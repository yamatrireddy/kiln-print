//! Minimal, blocking Chrome DevTools Protocol client for one print-to-PDF operation.
//!
//! Each render launches a fresh headless Chromium-family browser (Edge or Chrome) with a
//! throw-away profile and the sandbox settings below, prints once and shuts it down. The
//! browser is killed if anything fails or the deadline passes.
//!
//! Isolation:
//! * all network traffic goes to a dead proxy (`127.0.0.1:9`), loopback included, so the
//!   HTML cannot reach the internet, the LAN or the agent itself;
//! * every request is additionally failed through `Fetch` interception (only inline
//!   `data:` resources load);
//! * the document is injected into `about:blank`, whose origin cannot read `file:` URLs;
//! * JavaScript is disabled unless the administrator enables it;
//! * the profile directory is deleted afterwards.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use kiln_core::error::{ErrorCode, PrintError, Result};
use serde_json::{Value, json};
use tungstenite::protocol::WebSocketConfig;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub(crate) struct PdfRequest<'a> {
    pub html: &'a str,
    pub landscape: bool,
    pub paper_in: (f64, f64),
    /// top, right, bottom, left
    pub margins_in: [f64; 4],
    pub scale: f64,
    pub print_background: bool,
    pub prefer_css_page_size: bool,
    pub page_ranges: Option<&'a str>,
    pub header: Option<&'a str>,
    pub footer: Option<&'a str>,
    pub javascript: bool,
    pub max_pdf_bytes: usize,
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        // The browser may still be releasing files for a moment after exit (Windows).
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.0).is_ok() || !self.0.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        tracing::warn!(target: "kiln::html", dir = %self.0.display(), "could not remove browser profile");
    }
}

struct Browser(Child);

impl Drop for Browser {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            // Give Browser.close a moment, then kill.
            for _ in 0..20 {
                if !matches!(self.0.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn render_error(message: impl Into<String>) -> PrintError {
    PrintError::new(ErrorCode::PrintFailed, message).recoverable(false)
}

fn timeout() -> PrintError {
    PrintError::new(ErrorCode::Timeout, "HTML rendering timed out")
        .recoverable(true)
        .with_details(json!({ "outcome": "NOT_PRINTED" }))
}

pub(crate) fn print_to_pdf(
    browser_path: &Path,
    req: &PdfRequest<'_>,
    limit: Duration,
) -> Result<Vec<u8>> {
    let deadline = Instant::now() + limit;
    let profile = TempDir(std::env::temp_dir().join(format!("kiln-html-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&profile.0).map_err(PrintError::internal)?;

    let child = Command::new(browser_path)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-sync",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-default-apps",
            "--disable-breakpad",
            "--disable-crash-reporter",
            "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication,Prerender2",
            "--no-pings",
            "--mute-audio",
            "--hide-scrollbars",
            "--proxy-server=127.0.0.1:9",
            "--proxy-bypass-list=<-loopback>",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
        ])
        .arg(format!("--user-data-dir={}", profile.0.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            tracing::error!(target: "kiln::html", error = %e, browser = %browser_path.display(), "browser launch failed");
            PrintError::new(ErrorCode::UnsupportedOperation, "could not start the HTML rendering engine")
        })?;
    let _browser = Browser(child);

    let endpoint = wait_for_endpoint(&profile.0, deadline)?;
    let mut cdp = Cdp::connect(&endpoint, deadline, req.max_pdf_bytes)?;

    let target = cdp.call("Target.createTarget", json!({ "url": "about:blank" }))?;
    let target_id = target["targetId"]
        .as_str()
        .ok_or_else(|| render_error("no target id"))?
        .to_owned();
    let attached = cdp.call(
        "Target.attachToTarget",
        json!({ "targetId": target_id, "flatten": true }),
    )?;
    cdp.session = attached["sessionId"].as_str().map(str::to_owned);

    cdp.call("Page.enable", json!({}))?;
    if !req.javascript {
        cdp.call(
            "Emulation.setScriptExecutionDisabled",
            json!({ "value": true }),
        )?;
    }
    cdp.call(
        "Fetch.enable",
        json!({ "patterns": [{ "urlPattern": "*" }] }),
    )?;
    let tree = cdp.call("Page.getFrameTree", json!({}))?;
    let frame_id = tree["frameTree"]["frame"]["id"]
        .as_str()
        .ok_or_else(|| render_error("no frame id"))?
        .to_owned();
    cdp.call(
        "Page.setDocumentContent",
        json!({ "frameId": frame_id, "html": req.html }),
    )?;
    // Inline resources (data: images, fonts) load asynchronously; wait for the load event,
    // then for fonts, but never past the deadline.
    cdp.wait_event("Page.loadEventFired", Duration::from_secs(3));
    let _ = cdp.call(
        "Runtime.evaluate",
        json!({ "expression": "document.fonts.ready.then(() => true)", "awaitPromise": true, "timeout": 3000 }),
    );

    let display_header_footer = req.header.is_some() || req.footer.is_some();
    let [top, right, bottom, left] = req.margins_in;
    let mut params = json!({
        "landscape": req.landscape,
        "printBackground": req.print_background,
        "scale": req.scale,
        "paperWidth": req.paper_in.0,
        "paperHeight": req.paper_in.1,
        "marginTop": top,
        "marginRight": right,
        "marginBottom": bottom,
        "marginLeft": left,
        "preferCSSPageSize": req.prefer_css_page_size,
        "displayHeaderFooter": display_header_footer,
        "headerTemplate": req.header.unwrap_or("<span></span>"),
        "footerTemplate": req.footer.unwrap_or("<span></span>"),
        "transferMode": "ReturnAsBase64",
    });
    if let Some(ranges) = req.page_ranges {
        params["pageRanges"] = json!(ranges);
    }
    let printed = cdp.call("Page.printToPDF", params)?;
    let data = printed["data"]
        .as_str()
        .ok_or_else(|| render_error("no PDF data returned"))?;
    let pdf = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| render_error("invalid PDF data returned"))?;
    if pdf.len() > req.max_pdf_bytes {
        return Err(PrintError::new(
            ErrorCode::PayloadTooLarge,
            "rendered HTML exceeds the document size limit",
        ));
    }

    cdp.session = None;
    let _ = cdp.send("Browser.close", json!({}));
    Ok(pdf)
}

/// Reads `DevToolsActivePort` (port, then browser target path) written by the browser.
fn wait_for_endpoint(profile: &Path, deadline: Instant) -> Result<String> {
    let file = profile.join("DevToolsActivePort");
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                if port.trim().parse::<u16>().is_ok() && path.starts_with('/') {
                    return Ok(format!("ws://127.0.0.1:{}{}", port.trim(), path.trim()));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(timeout());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct Cdp {
    ws: WebSocket<MaybeTlsStream<std::net::TcpStream>>,
    next_id: u64,
    session: Option<String>,
    deadline: Instant,
    events: Vec<Value>,
}

impl Cdp {
    fn connect(url: &str, deadline: Instant, max_bytes: usize) -> Result<Self> {
        // base64 inflates by 4/3; allow for the envelope.
        let limit = max_bytes / 3 * 4 + 1024 * 1024;
        let config = WebSocketConfig::default()
            .max_message_size(Some(limit))
            .max_frame_size(Some(limit));
        let (ws, _) = tungstenite::client::connect_with_config(url, Some(config), 0)
            .map_err(|e| render_error(format!("could not connect to the rendering engine: {e}")))?;
        Ok(Self {
            ws,
            next_id: 0,
            session: None,
            deadline,
            events: Vec::new(),
        })
    }

    fn send(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let mut message = json!({ "id": self.next_id, "method": method, "params": params });
        if let Some(session) = &self.session {
            message["sessionId"] = json!(session);
        }
        self.ws
            .send(Message::text(message.to_string()))
            .map_err(|e| render_error(format!("rendering engine connection failed: {e}")))?;
        Ok(self.next_id)
    }

    fn read(&mut self, until: Instant) -> Result<Option<Value>> {
        let remaining = until.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        if let MaybeTlsStream::Plain(stream) = self.ws.get_mut() {
            let _ = stream.set_read_timeout(Some(remaining));
        }
        match self.ws.read() {
            Ok(Message::Text(text)) => {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|_| render_error("malformed engine message"))?;
                self.intercept(&value)?;
                Ok(Some(value))
            }
            Ok(_) => Ok(Some(Value::Null)),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(render_error(format!(
                "rendering engine connection failed: {e}"
            ))),
        }
    }

    /// Blocks every network request the page makes (see module docs).
    fn intercept(&mut self, value: &Value) -> Result<()> {
        if value["method"] == "Fetch.requestPaused" {
            let url = value["params"]["request"]["url"]
                .as_str()
                .unwrap_or_default();
            tracing::debug!(target: "kiln::html", url = %url.chars().take(100).collect::<String>(), "blocked request from HTML document");
            let request_id = value["params"]["requestId"].clone();
            self.send(
                "Fetch.failRequest",
                json!({ "requestId": request_id, "errorReason": "BlockedByClient" }),
            )?;
        }
        Ok(())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send(method, params)?;
        loop {
            let Some(message) = self.read(self.deadline)? else {
                return Err(timeout());
            };
            if message["id"].as_u64() == Some(id) {
                if let Some(error) = message.get("error") {
                    let text = error["message"].as_str().unwrap_or("unknown error");
                    return Err(render_error(format!("{method} failed: {text}")));
                }
                return Ok(message["result"].clone());
            }
            if message.get("method").is_some() {
                self.events.push(message);
            }
        }
    }

    /// Waits up to `max` for an event; returns whether it arrived.
    fn wait_event(&mut self, method: &str, max: Duration) -> bool {
        if self.events.iter().any(|e| e["method"] == method) {
            return true;
        }
        let until = (Instant::now() + max).min(self.deadline);
        while let Ok(Some(message)) = self.read(until) {
            if message["method"] == method {
                return true;
            }
        }
        false
    }
}
