//! End-to-end tests: a real agent on an ephemeral loopback port, mock printers, real
//! WebSocket and HTTP clients. CI-safe.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use kiln_agent::config::{AgentConfig, ClientConfig};
use kiln_agent::security::{Permission, hash_token};
use kiln_agent::{RunningAgent, StartOptions};
use kiln_core::provider::{PrintPayload, ProviderJobState};
use kiln_provider_mock::{MockPrinter, MockProvider};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

const ADMIN_TOKEN: &str = "kiln_test_admin_token_0123456789abcdef";
const LAB_TOKEN: &str = "kiln_test_lab_token_0123456789abcdef";
const LAB_ORIGIN: &str = "https://lab.example.com";

struct TestAgent {
    agent: Option<RunningAgent>,
    addr: SocketAddr,
    mock: Arc<MockProvider>,
    dir: PathBuf,
}

impl TestAgent {
    async fn stop(self) {
        let dir = self.dir.clone();
        self.stop_keep_data().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn stop_keep_data(mut self) {
        if let Some(agent) = self.agent.take() {
            agent.shutdown().await;
        }
    }
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kiln-agent-test-{}", uuid::Uuid::new_v4()))
}

fn base_config(dir: &std::path::Path) -> AgentConfig {
    let mut config = AgentConfig::default();
    config.server.bind = "127.0.0.1:0".parse().expect("addr");
    config.server.heartbeat_secs = 30;
    config.providers.windows = false;
    config.storage.data_dir = Some(dir.to_path_buf());
    config.jobs.monitor_interval_ms = 50;
    config.jobs.monitor_max_interval_ms = 100;
    config.security.clients = vec![ClientConfig {
        id: "lab".into(),
        name: "Lab Application".into(),
        token_sha256: hex::encode(hash_token(LAB_TOKEN)),
        origins: vec![LAB_ORIGIN.into()],
        permissions: vec![
            Permission::PrintersRead,
            Permission::Print,
            Permission::JobsRead,
        ],
        printers: vec!["Zebra".into()],
    }];
    config
}

fn default_mock() -> Arc<MockProvider> {
    Arc::new(MockProvider::new(vec![
        MockPrinter::new("Zebra").raw_only(),
        MockPrinter::new("Laser").default_printer(),
    ]))
}

async fn start_with(config: AgentConfig, mock: Arc<MockProvider>) -> TestAgent {
    let dir = config.storage.data_dir.clone().expect("dir");
    let agent = kiln_agent::start(
        config,
        StartOptions {
            extra_providers: vec![mock.clone()],
            admin_token: Some(ADMIN_TOKEN.into()),
        },
    )
    .await
    .expect("agent starts");
    TestAgent {
        addr: agent.local_addr,
        agent: Some(agent),
        mock,
        dir,
    }
}

async fn start() -> TestAgent {
    start_with(base_config(&temp_dir()), default_mock()).await
}

// ------------------------------------------------------------------ WebSocket client

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Client {
    ws: Socket,
    next_id: u64,
    events: Vec<Value>,
}

async fn open(
    addr: SocketAddr,
    origin: Option<&str>,
    host: Option<&str>,
) -> Result<Socket, tungstenite::Error> {
    let mut request = format!("ws://{addr}/v1/ws").into_client_request()?;
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().expect("header"));
    }
    if let Some(host) = host {
        request
            .headers_mut()
            .insert("Host", host.parse().expect("header"));
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(ws, _)| ws)
}

impl Client {
    async fn raw(addr: SocketAddr, origin: Option<&str>) -> Self {
        Self {
            ws: open(addr, origin, None).await.expect("upgrade"),
            next_id: 0,
            events: Vec::new(),
        }
    }

    /// Connects and authenticates; panics on failure.
    async fn connect(addr: SocketAddr, token: &str, origin: Option<&str>) -> Self {
        let mut client = Self::raw(addr, origin).await;
        let hello = client.hello(token, &[1]).await;
        assert_eq!(hello["ok"], true, "hello failed: {hello}");
        client
    }

    async fn hello(&mut self, token: &str, versions: &[u32]) -> Value {
        self.call(
            "session.hello",
            json!({ "protocolVersions": versions, "client": { "name": "test", "version": "1" }, "auth": { "type": "token", "token": token } }),
        )
        .await
    }

    async fn send_text(&mut self, text: String) {
        self.ws
            .send(Message::Text(text.into()))
            .await
            .expect("send");
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);
        self.send_text(json!({ "protocolVersion": 1, "type": "request", "id": id, "method": method, "params": params }).to_string())
            .await;
        self.response(&id).await
    }

    async fn next_message(&mut self) -> Option<Value> {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), self.ws.next())
                .await
                .expect("message within 20s")
            {
                Some(Ok(Message::Text(t))) => return Some(serde_json::from_str(&t).expect("json")),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                _ => return None,
            }
        }
    }

    async fn response(&mut self, id: &str) -> Value {
        loop {
            let msg = self.next_message().await.expect("connection open");
            if msg["type"] == "event" {
                self.events.push(msg);
            } else if msg["id"] == id || msg["id"].is_null() {
                return msg;
            }
        }
    }

    async fn wait_event(&mut self, name: &str, job_id: &str) -> Value {
        if let Some(pos) = self
            .events
            .iter()
            .position(|e| e["event"] == name && e["data"]["jobId"] == job_id)
        {
            return self.events.remove(pos);
        }
        loop {
            let msg = self.next_message().await.expect("connection open");
            if msg["type"] == "event" && msg["event"] == name && msg["data"]["jobId"] == job_id {
                return msg;
            }
            if msg["type"] == "event" {
                self.events.push(msg);
            }
        }
    }

    /// True if the server closes the connection (or it errors) within 5 seconds.
    async fn closed(&mut self) -> bool {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.ws.next()).await {
                Err(_) => return false,
                Ok(None | Some(Err(_)) | Some(Ok(Message::Close(_)))) => return true,
                Ok(Some(Ok(_))) => continue,
            }
        }
    }
}

fn error_code(response: &Value) -> &str {
    response["error"]["errorCode"].as_str().unwrap_or("<none>")
}

// ------------------------------------------------------------------ HTTP client

async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (u16, Value) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        request.push_str(&format!("Host: {addr}\r\n"));
    }
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    for (k, v) in headers {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status");
    let body = serde_json::from_str(body).unwrap_or(Value::String(body.to_owned()));
    (status, body)
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn health_is_public_but_minimal() {
    let t = start().await;
    let (status, body) = http(t.addr, "GET", "/v1/health", None, None, &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body["status"], "ok");
    t.stop().await;
}

#[tokio::test]
async fn raw_print_round_trip_over_websocket() {
    let t = start().await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;

    let printers = c.call("printers.list", json!({})).await;
    let names: Vec<_> = printers["result"]
        .as_array()
        .expect("list")
        .iter()
        .map(|p| p["name"].clone())
        .collect();
    assert_eq!(names.len(), 2);

    let zpl = "^XA^FO50,50^A0N,50,50^FDKiln^FS^XZ";
    let submitted = c
        .call("print.raw", json!({ "printer": "Zebra", "language": "ZPL", "encoding": "utf8", "data": zpl, "copies": 2 }))
        .await;
    assert_eq!(submitted["ok"], true, "{submitted}");
    let job = &submitted["result"];
    assert_eq!(job["status"], "QUEUED");
    assert_eq!(job["delivery"], "REQUEST_ACCEPTED");
    let job_id = job["jobId"].as_str().expect("jobId").to_owned();

    let done = c.wait_event("job.completed", &job_id).await;
    assert_eq!(done["data"]["completion"], "SPOOLER_REPORTED_PRINTED");
    assert!(done["seq"].as_u64().is_some());

    let fetched = c.call("jobs.get", json!({ "jobId": job_id })).await;
    assert_eq!(fetched["result"]["status"], "COMPLETED");
    assert_eq!(fetched["result"]["copies"], 2);

    let subs = t.mock.submissions();
    let PrintPayload::Raw(raw) = &subs[0].payload else {
        panic!("raw")
    };
    assert_eq!(&raw.bytes[..], zpl.as_bytes());
    t.stop().await;
}

#[tokio::test]
async fn base64_payload_is_byte_exact_end_to_end() {
    use base64::Engine;
    let t = start().await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let data: Vec<u8> = (0u8..=255).cycle().take(70_000).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
    let r = c
        .call(
            "print.raw",
            json!({ "printer": "Zebra", "encoding": "base64", "data": encoded }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    c.wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    let PrintPayload::Raw(raw) = &t.mock.submissions()[0].payload else {
        panic!("raw")
    };
    assert!(raw.bytes[..] == data[..]);
    t.stop().await;
}

#[tokio::test]
async fn first_message_must_be_hello() {
    let t = start().await;
    let mut c = Client::raw(t.addr, None).await;
    let r = c.call("printers.list", json!({})).await;
    assert_eq!(error_code(&r), "AUTHENTICATION_REQUIRED");
    assert!(c.closed().await);
    t.stop().await;
}

#[tokio::test]
async fn untrusted_token_is_rejected_and_audited() {
    let t = start().await;
    let mut c = Client::raw(t.addr, None).await;
    let r = c.hello("kiln_wrong", &[1]).await;
    assert_eq!(error_code(&r), "CLIENT_NOT_TRUSTED");
    assert!(c.closed().await);

    let (status, audit) = http(t.addr, "GET", "/v1/audit", Some(ADMIN_TOKEN), None, &[]).await;
    assert_eq!(status, 200);
    let events: Vec<_> = audit["result"]
        .as_array()
        .expect("audit")
        .iter()
        .map(|e| e["event"].clone())
        .collect();
    assert!(events.contains(&json!("auth.failure")));
    t.stop().await;
}

#[tokio::test]
async fn protocol_version_is_negotiated() {
    let t = start().await;
    let mut c = Client::raw(t.addr, None).await;
    let r = c.hello(ADMIN_TOKEN, &[99]).await;
    assert_eq!(error_code(&r), "UNSUPPORTED_PROTOCOL_VERSION");

    let mut c = Client::raw(t.addr, None).await;
    let r = c.hello(ADMIN_TOKEN, &[1, 2, 7]).await;
    assert_eq!(r["result"]["protocolVersion"], 1);
    assert!(
        r["result"]["features"]["languages"]
            .as_array()
            .expect("languages")
            .len()
            >= 7
    );
    t.stop().await;
}

#[tokio::test]
async fn silent_connections_are_closed_after_handshake_timeout() {
    let mut config = base_config(&temp_dir());
    config.server.handshake_timeout_secs = 1;
    let t = start_with(config, default_mock()).await;
    let mut c = Client::raw(t.addr, None).await;
    assert!(c.closed().await);
    t.stop().await;
}

#[tokio::test]
async fn unknown_websites_cannot_even_open_a_socket() {
    let t = start().await;
    let err = open(t.addr, Some("https://evil.example"), None)
        .await
        .expect_err("rejected");
    assert!(
        matches!(err, tungstenite::Error::Http(ref r) if r.status() == 403),
        "{err:?}"
    );
    let err = open(t.addr, Some("null"), None)
        .await
        .expect_err("opaque origin rejected");
    assert!(matches!(err, tungstenite::Error::Http(ref r) if r.status() == 403));
    t.stop().await;
}

#[tokio::test]
async fn dns_rebinding_host_is_rejected() {
    let t = start().await;
    let host = format!("attacker.example:{}", t.addr.port());
    let err = open(t.addr, None, Some(&host)).await.expect_err("rejected");
    assert!(matches!(err, tungstenite::Error::Http(ref r) if r.status() == 403));
    let (status, _) = http(
        t.addr,
        "GET",
        "/v1/printers",
        Some(ADMIN_TOKEN),
        None,
        &[("Host", &host)],
    )
    .await;
    assert_eq!(status, 403);
    t.stop().await;
}

#[tokio::test]
async fn browser_client_is_bound_to_its_origin() {
    let t = start().await;
    // The lab app from its own origin works.
    let mut lab = Client::connect(t.addr, LAB_TOKEN, Some(LAB_ORIGIN)).await;
    let r = lab.call("session.ping", json!({})).await;
    assert_eq!(r["ok"], true);
    // The admin token is not usable from a web page, even a known one.
    let mut c = Client::raw(t.addr, Some(LAB_ORIGIN)).await;
    let r = c.hello(ADMIN_TOKEN, &[1]).await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");
    t.stop().await;
}

#[tokio::test]
async fn permissions_and_printer_scope_are_enforced() {
    let t = start().await;
    let mut lab = Client::connect(t.addr, LAB_TOKEN, Some(LAB_ORIGIN)).await;
    let mut admin = Client::connect(t.addr, ADMIN_TOKEN, None).await;

    // Scope: the lab app sees only its printer and cannot print elsewhere.
    let printers = lab.call("printers.list", json!({})).await;
    let list = printers["result"].as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "Zebra");
    let laser_id = admin.call("printers.list", json!({})).await["result"]
        .as_array()
        .expect("list")
        .iter()
        .find(|p| p["name"] == "Laser")
        .expect("laser")["id"]
        .clone();
    let r = lab
        .call("printers.get", json!({ "printerId": laser_id }))
        .await;
    assert_eq!(
        error_code(&r),
        "PRINTER_NOT_FOUND",
        "out-of-scope printers are invisible"
    );
    let r = lab
        .call("print.raw", json!({ "printer": "Laser", "data": "eA==" }))
        .await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");

    // Jobs: each client sees its own; the lab app cannot cancel (no permission).
    let lab_job = lab
        .call("print.raw", json!({ "printer": "Zebra", "data": "eA==" }))
        .await["result"]["jobId"]
        .clone();
    let admin_job = admin
        .call("print.raw", json!({ "printer": "Laser", "data": "eA==" }))
        .await["result"]["jobId"]
        .clone();
    let r = lab.call("jobs.get", json!({ "jobId": admin_job })).await;
    assert_eq!(error_code(&r), "JOB_NOT_FOUND");
    let r = lab.call("jobs.cancel", json!({ "jobId": lab_job })).await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");
    let r = lab
        .call("jobs.list", json!({ "clientId": "local-admin" }))
        .await;
    assert!(
        r["result"]
            .as_array()
            .expect("list")
            .iter()
            .all(|j| j["clientId"] == "lab")
    );
    let r = admin.call("jobs.list", json!({ "clientId": "lab" })).await;
    assert_eq!(
        r["result"].as_array().expect("list").len(),
        2,
        "both lab jobs (incl. the denied one) are recorded"
    );

    // Events: the lab app never receives the admin's job events.
    admin
        .wait_event("job.completed", admin_job.as_str().expect("id"))
        .await;
    lab.wait_event("job.completed", lab_job.as_str().expect("id"))
        .await;
    assert!(
        lab.events
            .iter()
            .all(|e| e["data"]["clientId"] != "local-admin")
    );

    let r = lab.call("clients.list", json!({})).await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");
    let clients = admin.call("clients.list", json!({})).await;
    let lab_entry = clients["result"]
        .as_array()
        .expect("clients")
        .iter()
        .find(|c| c["clientId"] == "lab")
        .expect("lab")
        .clone();
    assert_eq!(lab_entry["connected"], true);
    t.stop().await;
}

#[tokio::test]
async fn malformed_and_duplicate_requests() {
    let t = start().await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    c.send_text("{not json".into()).await;
    let r = c.next_message().await.expect("response");
    assert_eq!(error_code(&r), "INVALID_PAYLOAD");

    c.send_text(json!({"id": "dup", "method": "session.ping"}).to_string())
        .await;
    assert_eq!(c.response("dup").await["ok"], true);
    c.send_text(json!({"id": "dup", "method": "session.ping"}).to_string())
        .await;
    assert_eq!(error_code(&c.response("dup").await), "INVALID_PAYLOAD");

    let r = c
        .call(
            "print.raw",
            json!({ "printer": "Zebra", "data": "eA==", "copys": 3 }),
        )
        .await;
    assert_eq!(
        error_code(&r),
        "INVALID_PAYLOAD",
        "unknown fields are rejected"
    );
    let r = c.call("no.such.method", json!({})).await;
    assert_eq!(error_code(&r), "UNSUPPORTED_OPERATION");
    let r = c
        .call(
            "print.submit",
            json!({ "type": "SPREADSHEET", "printer": "Laser", "data": "" }),
        )
        .await;
    assert_eq!(error_code(&r), "UNSUPPORTED_DOCUMENT");
    let r = c
        .call(
            "print.pdf",
            json!({ "printer": "Laser", "data": "bm90IGEgcGRm" }),
        )
        .await;
    assert_eq!(
        error_code(&r),
        "INVALID_PAYLOAD",
        "non-PDF data is rejected"
    );
    // The connection survives all of the above.
    assert_eq!(c.call("session.ping", json!({})).await["ok"], true);
    t.stop().await;
}

#[tokio::test]
async fn rate_limit_applies_per_client() {
    let mut config = base_config(&temp_dir());
    config.security.rate_limit.requests_per_second = 0.01;
    config.security.rate_limit.burst = 3;
    let t = start_with(config, default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    for _ in 0..3 {
        assert_eq!(c.call("session.ping", json!({})).await["ok"], true);
    }
    assert_eq!(
        error_code(&c.call("session.ping", json!({})).await),
        "RATE_LIMITED"
    );
    t.stop().await;
}

#[tokio::test]
async fn oversized_documents_are_rejected() {
    let mut config = base_config(&temp_dir());
    config.limits.max_document_bytes = 16;
    let t = start_with(config, default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let r = c
        .call(
            "print.raw",
            json!({ "printer": "Zebra", "encoding": "utf8", "data": "x".repeat(17) }),
        )
        .await;
    assert_eq!(error_code(&r), "PAYLOAD_TOO_LARGE");
    assert!(t.mock.submissions().is_empty());
    t.stop().await;
}

#[tokio::test]
async fn idempotent_resubmission_returns_the_same_job() {
    let t = start().await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let params =
        json!({ "printer": "Zebra", "data": "eA==", "idempotencyKey": "order-1001/label" });
    let a = c.call("print.raw", params.clone()).await;
    // Simulate a reconnect before resubmitting.
    drop(c);
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let b = c.call("print.raw", params).await;
    assert_eq!(a["result"]["jobId"], b["result"]["jobId"]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.mock.submissions().len(), 1);
    t.stop().await;
}

#[tokio::test]
async fn rest_api() {
    let t = start().await;
    let (status, body) = http(t.addr, "GET", "/v1/printers", None, None, &[]).await;
    assert_eq!(status, 401);
    assert_eq!(body["error"]["errorCode"], "AUTHENTICATION_REQUIRED");
    let (status, _) = http(t.addr, "GET", "/v1/printers", Some("nope"), None, &[]).await;
    assert_eq!(status, 401);

    let (status, body) = http(t.addr, "GET", "/v1/printers", Some(ADMIN_TOKEN), None, &[]).await;
    assert_eq!(status, 200);
    let zebra_id = body["result"]
        .as_array()
        .expect("list")
        .iter()
        .find(|p| p["name"] == "Zebra")
        .expect("zebra")["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let (status, body) = http(
        t.addr,
        "GET",
        &format!("/v1/printers/{zebra_id}"),
        Some(ADMIN_TOKEN),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"]["capabilities"]["raw"], true);
    let (status, body) = http(
        t.addr,
        "GET",
        "/v1/printers/default",
        Some(ADMIN_TOKEN),
        None,
        &[],
    )
    .await;
    assert_eq!(
        (status, body["result"]["name"].as_str()),
        (200, Some("Laser"))
    );

    let (status, body) = http(
        t.addr,
        "POST",
        "/v1/print/raw",
        Some(ADMIN_TOKEN),
        Some(json!({ "printerId": zebra_id, "data": "^XA^XZ", "encoding": "utf8", "language": "ZPL" })),
        &[],
    )
    .await;
    assert_eq!(status, 202, "{body}");
    let job_id = body["result"]["jobId"].as_str().expect("id").to_owned();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, body) = http(
            t.addr,
            "GET",
            &format!("/v1/jobs/{job_id}"),
            Some(ADMIN_TOKEN),
            None,
            &[],
        )
        .await;
        if body["result"]["status"] == "COMPLETED" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job did not complete: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, body) = http(
        t.addr,
        "DELETE",
        &format!("/v1/jobs/{job_id}"),
        Some(ADMIN_TOKEN),
        None,
        &[],
    )
    .await;
    assert_eq!(
        (status, body["error"]["errorCode"].as_str()),
        (409, Some("INVALID_JOB_STATE"))
    );

    let (status, body) = http(
        t.addr,
        "GET",
        "/v1/jobs?status=COMPLETED&limit=5",
        Some(ADMIN_TOKEN),
        None,
        &[],
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["result"].as_array().expect("jobs").len(), 1);

    let (status, _) = http(
        t.addr,
        "POST",
        "/v1/print/spreadsheet",
        Some(ADMIN_TOKEN),
        Some(json!({})),
        &[],
    )
    .await;
    assert_eq!(status, 422);
    let (status, body) = http(
        t.addr,
        "POST",
        "/v1/print/raw",
        Some(ADMIN_TOKEN),
        Some(json!({ "printer": "Nope", "data": "eA==" })),
        &[],
    )
    .await;
    assert_eq!(
        (status, body["error"]["errorCode"].as_str()),
        (404, Some("PRINTER_NOT_FOUND"))
    );
    assert!(
        body["error"]["jobId"].is_string(),
        "failed requests still create a job"
    );

    let (status, body) = http(t.addr, "GET", "/v1/queue", Some(ADMIN_TOKEN), None, &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body["result"].as_array().expect("queues").len(), 2);

    // Browser requests from unknown origins are refused even with a valid token.
    let (status, _) = http(
        t.addr,
        "GET",
        "/v1/printers",
        Some(LAB_TOKEN),
        None,
        &[("Origin", "https://evil.example")],
    )
    .await;
    assert_eq!(status, 403);
    // The lab client may not read the audit log.
    let (status, _) = http(t.addr, "GET", "/v1/audit", Some(LAB_TOKEN), None, &[]).await;
    assert_eq!(status, 403);
    t.stop().await;
}

#[tokio::test]
async fn jobs_survive_an_agent_restart() {
    let dir = temp_dir();
    let mock = Arc::new(MockProvider::new(vec![
        MockPrinter::new("Zebra").script(vec![ProviderJobState::Pending]),
    ]));
    let t = start_with(base_config(&dir), mock).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let r = c
        .call("print.raw", json!({ "printer": "Zebra", "data": "eA==" }))
        .await;
    let job_id = r["result"]["jobId"].as_str().expect("id").to_owned();
    c.wait_event("job.spooled", &job_id).await;
    drop(c);
    t.stop_keep_data().await;

    // A fresh agent over the same database: the spooler no longer has the job (a new mock),
    // so monitoring resumes and settles it without re-sending anything.
    let fresh = Arc::new(MockProvider::new(vec![MockPrinter::new("Zebra")]));
    let t = start_with(base_config(&dir), fresh.clone()).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, body) = http(
            t.addr,
            "GET",
            &format!("/v1/jobs/{job_id}"),
            Some(ADMIN_TOKEN),
            None,
            &[],
        )
        .await;
        if body["result"]["status"] == "COMPLETED" {
            assert_eq!(body["result"]["completion"], "SPOOLER_JOB_RETIRED");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job was not reconciled: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        fresh.submissions().is_empty(),
        "nothing is re-sent after a restart"
    );
    t.stop().await;
    let _ = std::fs::remove_dir_all(dir);
}

// ------------------------------------------------------------------ Phase 2 documents

/// A 2x1 PNG (red, blue).
fn tiny_png() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::ImageBuffer::from_fn(2, 1, |x, _| {
        if x == 0 {
            image::Rgb([255, 0, 0])
        } else {
            image::Rgb([0, 0, 255])
        }
    }))
    .write_to(&mut out, image::ImageFormat::Png)
    .expect("png");
    out.into_inner()
}

const TINY_PDF: &[u8] =
    b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";

fn b64(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[tokio::test]
async fn pdf_and_image_documents_reach_graphics_printers() {
    let t = start().await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;

    let pdf = c
        .call(
            "print.pdf",
            json!({
                "printer": "Laser",
                "data": b64(TINY_PDF),
                "options": { "pageRange": "1", "scale": "FIT", "paperSize": "A4", "duplex": "LONG_EDGE", "color": "MONOCHROME" },
                "copies": 2
            }),
        )
        .await;
    assert_eq!(pdf["ok"], true, "{pdf}");
    assert_eq!(pdf["result"]["documentType"], "PDF");
    c.wait_event(
        "job.completed",
        pdf["result"]["jobId"].as_str().expect("id"),
    )
    .await;

    let img = c
        .call(
            "print.submit",
            json!({ "type": "IMAGE", "printer": "Laser", "data": b64(&tiny_png()), "options": { "fit": "ORIGINAL", "rotate": 90, "dpi": 300 } }),
        )
        .await;
    assert_eq!(img["ok"], true, "{img}");
    c.wait_event(
        "job.completed",
        img["result"]["jobId"].as_str().expect("id"),
    )
    .await;

    let subs = t.mock.submissions();
    let PrintPayload::Pdf(p) = &subs[0].payload else {
        panic!("pdf payload")
    };
    assert_eq!(
        &p.bytes[..],
        TINY_PDF,
        "PDF bytes are passed through untouched"
    );
    assert_eq!(p.setup.duplex, Some(kiln_core::model::Duplex::LongEdge));
    assert_eq!(p.pages.as_ref().expect("range").indices(3), vec![0]);
    let PrintPayload::Image(i) = &subs[1].payload else {
        panic!("image payload")
    };
    assert_eq!(
        (i.image.width, i.image.height),
        (1, 2),
        "rotated 90 degrees"
    );
    assert_eq!(i.image.dpi_x, 300);

    // Label printers without a graphics driver refuse graphical documents up front.
    let r = c
        .call(
            "print.pdf",
            json!({ "printer": "Zebra", "data": b64(TINY_PDF) }),
        )
        .await;
    assert_eq!(error_code(&r), "UNSUPPORTED_DOCUMENT");
    let caps = c
        .call("printers.capabilities", json!({ "printer": "Laser" }))
        .await;
    let types: Vec<_> = caps["result"]["documentTypes"]
        .as_array()
        .expect("types")
        .to_vec();
    for t in ["RAW", "TEXT", "PDF", "IMAGE"] {
        assert!(types.contains(&json!(t)), "{t} in {types:?}");
    }
    t.stop().await;
}

#[tokio::test]
async fn html_is_rendered_to_pdf_before_printing() {
    // See renderers/tests/html.rs: Linux CI runners often cannot start Chrome's sandbox.
    let linux_opt_out = cfg!(target_os = "linux") && std::env::var_os("KILN_HTML_TESTS").is_none();
    if linux_opt_out || kiln_renderers::html::find_browser().is_none() {
        eprintln!("skipping: browser tests disabled or no Edge/Chrome/Chromium installed");
        return;
    }
    // Keep the render deadline (including one startup retry) inside the client's 20 s wait.
    let mut config = base_config(&temp_dir());
    config.html.timeout_secs = 15;
    let t = start_with(config, default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let r = c
        .call(
            "print.html",
            json!({
                "printer": "Laser",
                "html": "<h1>Invoice</h1><p style='break-before:page'>Terms</p>",
                "options": { "paperSize": "Letter", "footerHtml": "<span class=pageNumber></span>" }
            }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let job_id = r["result"]["jobId"].as_str().expect("id").to_owned();
    let done = c.wait_event("job.completed", &job_id).await;
    assert_eq!(done["data"]["documentType"], "HTML");
    let PrintPayload::Pdf(p) = &t.mock.submissions()[0].payload else {
        panic!("pdf payload")
    };
    assert!(p.bytes.starts_with(b"%PDF"));
    assert_eq!(p.placement, kiln_core::model::Placement::ActualSize);
    t.stop().await;
}

#[tokio::test]
async fn path_sources_obey_the_allowlist() {
    let dir = temp_dir();
    let docs = dir.join("docs");
    std::fs::create_dir_all(&docs).expect("mkdir");
    std::fs::write(docs.join("label.png"), tiny_png()).expect("write");
    std::fs::write(dir.join("private.pdf"), TINY_PDF).expect("write");

    // Disabled by default.
    let t = start_with(base_config(&dir), default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let path = docs.join("label.png").display().to_string();
    let r = c
        .call("print.image", json!({ "printer": "Laser", "path": path }))
        .await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");
    t.stop_keep_data().await;

    let mut config = base_config(&dir);
    config.sources.allowed_paths = vec![docs.clone()];
    let t = start_with(config, default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;
    let r = c
        .call("print.image", json!({ "printer": "Laser", "path": path }))
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let outside = dir.join("private.pdf").display().to_string();
    let r = c
        .call("print.pdf", json!({ "printer": "Laser", "path": outside }))
        .await;
    assert_eq!(error_code(&r), "ACCESS_DENIED");
    t.stop().await;
}

// ------------------------------------------------------------------ Phase 3 documents

fn label_fleet() -> Arc<MockProvider> {
    Arc::new(MockProvider::new(vec![
        MockPrinter::new("Zebra").raw_only().language("ZPL"),
        MockPrinter::new("Receipt").raw_only().language("ESC/POS"),
        MockPrinter::new("Epson LQ").language("ESC/P"),
        MockPrinter::new("Unknown Label").raw_only(),
    ]))
}

fn raw_bytes(t: &TestAgent, index: usize) -> Vec<u8> {
    match &t.mock.submissions()[index].payload {
        PrintPayload::Raw(raw) => raw.bytes.to_vec(),
        other => panic!("expected RAW, got {other:?}"),
    }
}

#[tokio::test]
async fn structured_documents_are_encoded_for_each_printer() {
    let t = start_with(base_config(&temp_dir()), label_fleet()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;

    let label = json!({
        "widthMm": 100, "heightMm": 50,
        "elements": [
            { "type": "TEXT", "xMm": 5, "yMm": 5, "text": "Ship to: Jane ^XZ", "heightMm": 5 },
            { "type": "BARCODE", "xMm": 5, "yMm": 15, "symbology": "CODE128", "data": "1Z999AA1" },
            { "type": "QR", "xMm": 70, "yMm": 5, "data": "https://example.com/t/1" }
        ]
    });
    let r = c
        .call(
            "print.label",
            json!({ "printer": "Zebra", "label": label, "copies": 2 }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let done = c
        .wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    assert_eq!(done["data"]["documentType"], "LABEL");
    assert_eq!(
        done["data"]["language"], "ZPL",
        "language taken from the printer hint"
    );
    let zpl = String::from_utf8(raw_bytes(&t, 0)).expect("utf8");
    assert!(zpl.starts_with("^XA") && zpl.trim_end().ends_with("^XZ"));
    assert_eq!(
        zpl.matches("^XZ").count(),
        1,
        "client text cannot terminate the label"
    );

    let r = c
        .call(
            "print.submit",
            json!({ "type": "LABEL", "printer": "Zebra", "label": { "widthMm": 60, "heightMm": 40, "language": "TSPL",
                    "elements": [{ "type": "TEXT", "xMm": 2, "yMm": 2, "text": "override" }] } }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    c.wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    assert!(raw_bytes(&t, 1).starts_with(b"SIZE 60 mm,40 mm"));

    let r = c
        .call(
            "print.receipt",
            json!({ "printer": "Receipt", "receipt": { "widthChars": 32, "codePage": "ibm858", "items": [
                { "type": "TEXT", "text": "KILN CAFE", "align": "CENTER", "bold": true, "doubleHeight": true },
                { "type": "COLUMNS", "left": "Latte", "right": "3.80€" },
                { "type": "QR", "data": "receipt/42", "align": "CENTER" }
            ] } }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    c.wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    let escpos = raw_bytes(&t, 2);
    assert!(escpos.starts_with(&[0x1B, b'@', 0x1B, b't', 19]));
    assert!(escpos.ends_with(&[0x1D, b'V', 65, 3]), "cut at the end");

    let r = c
        .call(
            "print.dotmatrix",
            json!({ "printer": "Epson LQ", "document": { "cpi": 17, "formLengthInches": 12, "lines": [
                "INVOICE 2026-0042", { "type": "LINE", "text": "TOTAL 43.90", "bold": true }
            ] } }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    c.wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    let escp = raw_bytes(&t, 3);
    assert!(escp.starts_with(&[0x1B, b'@']));
    assert!(escp.windows(4).any(|w| w == [0x1B, b'C', 0, 12]));
    assert_eq!(escp.last(), Some(&0x0C));

    // No language anywhere: a clear error, not garbage on the printer.
    let r = c
        .call(
            "print.label",
            json!({ "printer": "Unknown Label", "label": label }),
        )
        .await;
    assert_eq!(error_code(&r), "UNSUPPORTED_DOCUMENT");
    // A receipt printer cannot take a label.
    let r = c
        .call(
            "print.label",
            json!({ "printer": "Receipt", "label": label }),
        )
        .await;
    assert_eq!(error_code(&r), "UNSUPPORTED_DOCUMENT");
    let r = c
        .call(
            "print.label",
            json!({ "printer": "Zebra", "label": { "widthMm": 50, "heightMm": 30, "elements": [
            { "type": "BARCODE", "xMm": 1, "yMm": 1, "symbology": "EAN13", "data": "12345" }] } }),
        )
        .await;
    assert_eq!(
        error_code(&r),
        "INVALID_PAYLOAD",
        "bad barcode data is caught before printing"
    );
    assert_eq!(t.mock.submissions().len(), 4);
    t.stop().await;
}

#[tokio::test]
async fn configured_network_printers_print_directly_over_tcp() {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let printer = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut data = Vec::new();
        stream.read_to_end(&mut data).expect("read");
        data
    });

    let dir = temp_dir();
    let mut config = base_config(&dir);
    config.network_printers = vec![kiln_agent::config::NetworkPrinterConfig {
        name: "Dock TSC".into(),
        host: "127.0.0.1".into(),
        port,
        language: Some("TSPL".into()),
        status: kiln_provider_tcp::StatusQuery::None,
        connect_timeout_ms: 1000,
        write_timeout_ms: 5000,
    }];
    let t = start_with(config, default_mock()).await;
    let mut c = Client::connect(t.addr, ADMIN_TOKEN, None).await;

    let printers = c.call("printers.list", json!({})).await;
    let dock = printers["result"]
        .as_array()
        .expect("list")
        .iter()
        .find(|p| p["name"] == "Dock TSC")
        .expect("dock")
        .clone();
    assert_eq!(dock["type"], "NETWORK");
    assert_eq!(dock["language"], "TSPL");
    assert_eq!(dock["port"], format!("tcp://127.0.0.1:{port}"));

    let r = c
        .call(
            "print.label",
            json!({ "printerId": dock["id"], "label": { "widthMm": 50, "heightMm": 25,
                    "elements": [{ "type": "TEXT", "xMm": 2, "yMm": 2, "text": "direct" }] } }),
        )
        .await;
    assert_eq!(r["ok"], true, "{r}");
    let done = c
        .wait_event("job.completed", r["result"]["jobId"].as_str().expect("id"))
        .await;
    assert_eq!(done["data"]["delivery"], "DEVICE_DELIVERED");
    assert_eq!(done["data"]["completion"], "BYTES_DELIVERED");
    let received = printer.join().expect("printer thread");
    assert!(received.starts_with(b"SIZE 50 mm,25 mm"));
    assert!(received.ends_with(b"PRINT 1\r\n"));

    // Only configured hosts exist: a client cannot address a network location itself.
    let r = c
        .call(
            "print.raw",
            json!({ "printer": "tcp://10.0.0.1:9100", "data": "eA==" }),
        )
        .await;
    assert_eq!(error_code(&r), "PRINTER_NOT_FOUND");
    t.stop().await;
}
