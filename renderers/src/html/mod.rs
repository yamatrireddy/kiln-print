//! HTML renderer: lays HTML/CSS out with a sandboxed headless Chromium-family browser
//! (Microsoft Edge or Google Chrome) and produces a PDF, which then follows the PDF path.
//!
//! Supported: CSS (including `@page` size and margins), system fonts and fonts embedded as
//! `data:` URIs, page breaks (`break-before`/`break-after`), headers/footers via templates,
//! barcodes/QR codes as inline SVG or `data:` images. Not supported by design: remote
//! resources of any kind, and JavaScript unless an administrator enables it (see
//! `cdp.rs` for the isolation model).

mod cdp;

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use kiln_core::error::{ErrorCode, PrintError, Result};
use kiln_core::model::{
    Document, DocumentType, HtmlDocument, MarginsMm, Orientation, PaperRequest, Placement,
};
use kiln_core::provider::{PayloadKind, PdfPayload, PrintPayload};
use kiln_core::renderer::{DocumentRenderer, RenderTarget};

use crate::pdf::DEFAULT_MAX_DPI;

#[derive(Debug, Clone)]
pub struct HtmlConfig {
    /// Browser executable. Auto-detected (Edge, then Chrome, then Chromium) when `None`.
    pub browser: Option<PathBuf>,
    pub timeout: Duration,
    /// Allow document JavaScript. Network access stays blocked either way.
    pub javascript: bool,
    /// Largest PDF the browser may return.
    pub max_pdf_bytes: usize,
}

impl Default for HtmlConfig {
    fn default() -> Self {
        Self {
            browser: None,
            timeout: Duration::from_secs(30),
            javascript: false,
            max_pdf_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HtmlRenderer {
    config: HtmlConfig,
    browser: Option<PathBuf>,
}

impl HtmlRenderer {
    pub fn new(config: HtmlConfig) -> Self {
        let browser = config
            .browser
            .clone()
            .filter(|p| p.is_file())
            .or_else(find_browser);
        match &browser {
            Some(path) => {
                tracing::info!(target: "kiln::html", browser = %path.display(), "HTML rendering engine found")
            }
            None => {
                tracing::warn!(target: "kiln::html", "no Edge/Chrome/Chromium found; HTML printing is unavailable")
            }
        }
        Self { config, browser }
    }

    pub fn browser(&self) -> Option<&Path> {
        self.browser.as_deref()
    }
}

/// Well-known install locations, most common first.
pub fn find_browser() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LocalAppData"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
                candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
                candidates.push(base.join(r"Chromium\Application\chrome.exe"));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push("/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge".into());
        candidates.push("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into());
        candidates.push("/Applications/Chromium.app/Contents/MacOS/Chromium".into());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for dir in ["/usr/bin", "/usr/local/bin", "/snap/bin"] {
            for name in [
                "microsoft-edge",
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
            ] {
                candidates.push(Path::new(dir).join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Common paper names, in millimetres (portrait). Names are matched case-insensitively.
const PAPERS: &[(&str, f32, f32)] = &[
    ("A3", 297.0, 420.0),
    ("A4", 210.0, 297.0),
    ("A5", 148.0, 210.0),
    ("A6", 105.0, 148.0),
    ("B5", 176.0, 250.0),
    ("Letter", 215.9, 279.4),
    ("Legal", 215.9, 355.6),
    ("Tabloid", 279.4, 431.8),
    ("Executive", 184.15, 266.7),
    ("4x6", 101.6, 152.4),
    ("4x4", 101.6, 101.6),
    ("2x1", 50.8, 25.4),
];

fn paper_mm(request: Option<&PaperRequest>, target: &RenderTarget) -> Result<(f32, f32)> {
    match request {
        Some(PaperRequest::Custom {
            width_mm,
            height_mm,
        }) => Ok((*width_mm, *height_mm)),
        Some(PaperRequest::Named(name)) => {
            let wanted = name.trim().replace(['"', ' ', '-'], "");
            PAPERS
                .iter()
                .find(|(n, ..)| n.eq_ignore_ascii_case(&wanted) || n.eq_ignore_ascii_case(name.trim()))
                .map(|(_, w, h)| (*w, *h))
                .ok_or_else(|| {
                    PrintError::invalid_payload(format!(
                        "paper size '{name}' is not known for HTML layout; use one of {} or give widthMm/heightMm",
                        PAPERS.iter().map(|p| p.0).collect::<Vec<_>>().join(", ")
                    ))
                })
        }
        None => Ok(target.default_paper_mm.unwrap_or((210.0, 297.0))),
    }
}

impl DocumentRenderer for HtmlRenderer {
    fn document_type(&self) -> DocumentType {
        DocumentType::Html
    }

    fn output_kinds(&self) -> &'static [PayloadKind] {
        &[PayloadKind::Pdf]
    }

    fn validate(&self, document: &Document) -> Result<()> {
        let Document::Html(HtmlDocument { html, options }) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected an HTML document",
            ));
        };
        if html.trim().is_empty() {
            return Err(PrintError::invalid_payload("html is empty"));
        }
        if self.browser.is_none() {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "HTML printing needs Microsoft Edge, Google Chrome or Chromium on this machine",
            ));
        }
        options.validate()
    }

    fn render(&self, document: Document, target: &RenderTarget) -> Result<PrintPayload> {
        let Document::Html(HtmlDocument { html, options }) = document else {
            return Err(PrintError::new(
                ErrorCode::UnsupportedDocument,
                "expected an HTML document",
            ));
        };
        let browser = self.browser.as_deref().ok_or_else(|| {
            PrintError::new(
                ErrorCode::UnsupportedDocument,
                "no HTML rendering engine is available",
            )
        })?;
        if !target.accepted.contains(&PayloadKind::Pdf) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "printer '{}' cannot print rendered documents",
                    target.printer.name
                ),
            )
            .with_printer(target.printer.id.as_str()));
        }
        let (w, h) = paper_mm(options.paper_size.as_ref(), target)?;
        let margins = options.margins_mm.unwrap_or(MarginsMm::default());
        let inch = |mm: f32| f64::from(mm) / 25.4;
        let request = cdp::PdfRequest {
            html: &html,
            landscape: options.orientation == Some(Orientation::Landscape),
            paper_in: (inch(w.min(h)), inch(w.max(h))),
            margins_in: [
                inch(margins.top),
                inch(margins.right),
                inch(margins.bottom),
                inch(margins.left),
            ],
            scale: f64::from(options.scale) / 100.0,
            print_background: options.print_background,
            prefer_css_page_size: options.prefer_css_page_size,
            page_ranges: options.page_range.as_deref(),
            header: options.header_html.as_deref(),
            footer: options.footer_html.as_deref(),
            javascript: self.config.javascript,
            max_pdf_bytes: self.config.max_pdf_bytes,
        };
        let started = std::time::Instant::now();
        let deadline = started + self.config.timeout;
        let pdf = match cdp::print_to_pdf(browser, &request, self.config.timeout) {
            // A browser that failed to start is replaced once; nothing has printed yet.
            Err(e) if cdp::is_startup_failure(&e) && deadline > std::time::Instant::now() => {
                tracing::warn!(target: "kiln::html", error = %e, "rendering engine did not start; retrying once");
                cdp::print_to_pdf(
                    browser,
                    &request,
                    deadline.saturating_duration_since(std::time::Instant::now()),
                )?
            }
            other => other?,
        };
        tracing::info!(target: "kiln::html", bytes = pdf.len(), elapsed_ms = started.elapsed().as_millis() as u64, "HTML rendered to PDF");

        // Margins are already inside the PDF pages, so print them at actual size: a page
        // laid out for the printer's paper lands exactly on it.
        let mut setup = options.page_setup();
        setup.margins_mm = None;
        Ok(PrintPayload::Pdf(PdfPayload {
            bytes: Bytes::from(pdf),
            pages: None,
            placement: Placement::ActualSize,
            setup,
            max_dpi: options.dpi.unwrap_or(DEFAULT_MAX_DPI),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(paper: Option<(f32, f32)>) -> RenderTarget {
        RenderTarget {
            printer: kiln_core::model::Printer {
                id: "t".into(),
                name: "t".into(),
                display_name: "t".into(),
                provider: "t".into(),
                connection: kiln_core::model::ConnectionType::Local,
                driver: None,
                port: None,
                location: None,
                default: false,
                online: true,
                status: kiln_core::model::PrinterState::Ready,
                conditions: vec![],
                queued_jobs: None,
                language: None,
                capabilities: None,
            },
            accepted: vec![PayloadKind::Pdf],
            default_paper_mm: paper,
        }
    }

    #[test]
    fn paper_resolution() {
        let t = target(Some((215.9, 279.4)));
        assert_eq!(paper_mm(None, &t).expect("default"), (215.9, 279.4));
        assert_eq!(
            paper_mm(None, &target(None)).expect("fallback"),
            (210.0, 297.0)
        );
        assert_eq!(
            paper_mm(Some(&PaperRequest::Named("a4".into())), &t).expect("a4"),
            (210.0, 297.0)
        );
        assert_eq!(
            paper_mm(Some(&PaperRequest::Named("4 x 6".into())), &t).expect("4x6"),
            (101.6, 152.4)
        );
        assert!(paper_mm(Some(&PaperRequest::Named("Foolscap".into())), &t).is_err());
    }
}
