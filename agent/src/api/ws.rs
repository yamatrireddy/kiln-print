//! WebSocket transport.
//!
//! Lifecycle: upgrade (peer/Host/Origin checks, connection cap) → `session.hello` within
//! the handshake timeout (version negotiation + authentication) → request/response and
//! event streaming → close. Nothing but `session.hello` is accepted before
//! authentication, and a failed hello closes the connection.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::events::EngineEvent;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::sessions::SessionInfo;
use super::wire::*;
use super::{AppState, service};
use crate::security::{ConnectionInfo, Credentials, Permission, Principal};

/// Outbound queue per connection. Responses wait for space; events are dropped (with a
/// `session.lagged` notice) when a slow client falls this far behind.
const OUTBOUND_CAPACITY: usize = 256;
const RECENT_REQUEST_IDS: usize = 512;
const CLOSE_POLICY: u16 = 1008;

pub async fn upgrade(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let conn = match state.guard.check(peer, &headers) {
        Ok(conn) => conn,
        Err((status, reason)) => {
            state.db.audit(
                "connection.rejected",
                None,
                header_origin(&headers).as_deref(),
                &json!({ "reason": reason }),
            );
            return (status, reason).into_response();
        }
    };
    // Refuse unknown websites before any protocol exchange.
    if let Some(origin) = &conn.origin {
        if !state.auth.origin_known(origin) {
            state.db.audit(
                "connection.rejected",
                None,
                Some(origin),
                &json!({ "reason": "unknown origin" }),
            );
            return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
    }
    let Ok(slot) = state.connection_slots.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "too many connections").into_response();
    };
    let max = state.config.limits.max_message_bytes();
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| session(state, socket, conn, slot))
}

fn header_origin(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(200).collect())
}

struct Outbound {
    tx: mpsc::Sender<Message>,
}

impl Outbound {
    async fn response(&self, id: Option<&str>, result: Result<Value, PrintError>) {
        let text =
            serde_json::to_string(&ResponseEnvelope::from_result(id, result)).unwrap_or_default();
        let _ = self.tx.send(Message::Text(text.into())).await;
    }

    fn event(&self, name: &str, seq: u64, data: Value) -> bool {
        let envelope = EventEnvelope {
            protocol_version: PROTOCOL_VERSION,
            kind: "event",
            event: name,
            seq,
            timestamp: Utc::now(),
            data,
        };
        let text = serde_json::to_string(&envelope).unwrap_or_default();
        self.tx.try_send(Message::Text(text.into())).is_ok()
    }

    async fn close(&self, code: u16, reason: &str) {
        let frame = CloseFrame {
            code,
            reason: reason.chars().take(120).collect::<String>().into(),
        };
        let _ = self.tx.send(Message::Close(Some(frame))).await;
    }
}

async fn session(
    state: Arc<AppState>,
    socket: WebSocket,
    conn: ConnectionInfo,
    _slot: OwnedSemaphorePermit,
) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOUND_CAPACITY);
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let closing = matches!(message, Message::Close(_));
            if sink.send(message).await.is_err() || closing {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let out = Arc::new(Outbound { tx });

    let timeout = Duration::from_secs(state.config.server.handshake_timeout_secs.max(1));
    let hello = tokio::time::timeout(timeout, handshake(&state, &mut stream, &out, &conn)).await;
    let (principal, info) = match hello {
        Ok(Some(ok)) => ok,
        Ok(None) => {
            drop(out);
            let _ = writer.await;
            return;
        }
        Err(_) => {
            out.close(CLOSE_POLICY, "authentication timeout").await;
            drop(out);
            let _ = writer.await;
            return;
        }
    };

    let session_id = info.session_id;
    state.sessions.insert(info);
    state.db.audit(
        "client.connected",
        Some(&principal.client_id),
        principal.origin.as_deref(),
        &json!({ "sessionId": session_id }),
    );
    info!(target: "kiln::api", %session_id, client_id = %principal.client_id, origin = principal.origin.as_deref(), "client connected");

    run(&state, &mut stream, &out, &principal, session_id).await;

    state.sessions.remove(session_id);
    state.db.audit(
        "client.disconnected",
        Some(&principal.client_id),
        principal.origin.as_deref(),
        &json!({ "sessionId": session_id }),
    );
    info!(target: "kiln::api", %session_id, client_id = %principal.client_id, "client disconnected");
    drop(out);
    let _ = writer.await;
}

/// Waits for `session.hello`. Returns `None` after telling the client why it failed.
async fn handshake(
    state: &AppState,
    stream: &mut SplitStream<WebSocket>,
    out: &Outbound,
    conn: &ConnectionInfo,
) -> Option<(Principal, SessionInfo)> {
    let text = loop {
        match stream.next().await? {
            Ok(Message::Text(text)) => break text,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            _ => return None,
        }
    };
    let request: RequestEnvelope = match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            out.response(
                None,
                Err(PrintError::invalid_payload(format!(
                    "malformed message: {e}"
                ))),
            )
            .await;
            out.close(CLOSE_POLICY, "malformed hello").await;
            return None;
        }
    };
    let id = request.id.clone();
    match authenticate(state, request, conn) {
        Ok((principal, info, result)) => {
            out.response(Some(&id), Ok(result)).await;
            Some((principal, info))
        }
        Err(err) => {
            state.db.audit(
                "auth.failure",
                None,
                conn.origin.as_deref(),
                &json!({ "errorCode": err.error_code, "transport": "websocket" }),
            );
            warn!(target: "kiln::audit", origin = conn.origin.as_deref(), error = %err, "authentication failed");
            // Slow down online guessing a little; tokens are 256-bit so this is belt and braces.
            tokio::time::sleep(Duration::from_millis(250)).await;
            let reason = err.error_code.as_str();
            out.response(Some(&id), Err(err)).await;
            out.close(CLOSE_POLICY, reason).await;
            None
        }
    }
}

fn authenticate(
    state: &AppState,
    request: RequestEnvelope,
    conn: &ConnectionInfo,
) -> Result<(Principal, SessionInfo, Value), PrintError> {
    if request.method != "session.hello" {
        return Err(PrintError::new(
            ErrorCode::AuthenticationRequired,
            "the first message must be session.hello",
        ));
    }
    let hello: HelloParams = parse_params(request.params)?;
    let version = hello
        .protocol_versions
        .iter()
        .copied()
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .max()
        .ok_or_else(|| {
            PrintError::new(
                ErrorCode::UnsupportedProtocolVersion,
                "no mutually supported protocol version",
            )
            .with_details(json!({ "supported": SUPPORTED_PROTOCOL_VERSIONS }))
        })?;
    let AuthParams::Token { token } = hello.auth;
    let principal = state.auth.authenticate(&Credentials::Token(token), conn)?;
    let now = Utc::now();
    let info = SessionInfo {
        session_id: Uuid::new_v4(),
        client_id: principal.client_id.clone(),
        client_name: principal.name.clone(),
        reported_name: hello.client.name.chars().take(100).collect(),
        reported_version: hello.client.version.map(|v| v.chars().take(50).collect()),
        origin: principal.origin.clone(),
        user_agent: conn.user_agent.clone(),
        protocol_version: version,
        connected_at: now,
        last_activity: now,
    };
    let limits = &state.config.limits;
    let result = json!({
        "protocolVersion": version,
        "sessionId": info.session_id,
        "agent": { "name": "kiln-agent", "version": env!("CARGO_PKG_VERSION") },
        "client": principal,
        "limits": {
            "maxDocumentBytes": limits.max_document_bytes,
            "maxMessageBytes": limits.max_message_bytes(),
            "maxCopies": limits.max_copies,
        },
        "features": {
            "documentTypes": state.engine.document_types(),
            "languages": state.engine.languages(),
        },
        "heartbeatSeconds": state.config.server.heartbeat_secs,
    });
    Ok((principal, info, result))
}

async fn run(
    state: &Arc<AppState>,
    stream: &mut SplitStream<WebSocket>,
    out: &Arc<Outbound>,
    principal: &Principal,
    session_id: Uuid,
) {
    let mut events = state.engine.subscribe();
    let in_flight = Arc::new(Semaphore::new(
        state.config.server.max_in_flight_per_connection,
    ));
    let heartbeat = Duration::from_secs(state.config.server.heartbeat_secs.max(1));
    let mut ping = tokio::time::interval(heartbeat);
    ping.tick().await;
    let mut last_seen = Instant::now();
    let mut seq: u64 = 0;
    let mut recent_ids: (HashSet<String>, VecDeque<String>) = Default::default();

    loop {
        tokio::select! {
            message = stream.next() => {
                let Some(Ok(message)) = message else { break };
                last_seen = Instant::now();
                state.sessions.touch(session_id);
                match message {
                    Message::Text(text) => {
                        handle_text(state, out, principal, &in_flight, &mut recent_ids, text.as_str()).await;
                    }
                    Message::Binary(_) => {
                        out.response(None, Err(PrintError::invalid_payload(
                            "binary frames are not supported; send JSON text frames",
                        ))).await;
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            event = events.recv() => match event {
                Ok(event) => {
                    if let Some(data) = visible_event(principal, &event) {
                        seq += 1;
                        if !out.event(event.name(), seq, data) {
                            debug!(target: "kiln::api", %session_id, "dropping event for slow client");
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    seq += 1;
                    out.event("session.lagged", seq, json!({ "missedEvents": missed }));
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > heartbeat * 3 {
                    out.close(CLOSE_POLICY, "heartbeat timeout").await;
                    break;
                }
                let _ = out.tx.try_send(Message::Ping(Default::default()));
            }
            _ = state.shutdown.cancelled() => {
                out.close(1001, "agent shutting down").await;
                break;
            }
        }
    }
}

async fn handle_text(
    state: &Arc<AppState>,
    out: &Arc<Outbound>,
    principal: &Principal,
    in_flight: &Arc<Semaphore>,
    recent_ids: &mut (HashSet<String>, VecDeque<String>),
    text: &str,
) {
    let request: RequestEnvelope = match serde_json::from_str(text) {
        Ok(r) => r,
        Err(e) => {
            let id = serde_json::from_str::<Value>(text)
                .ok()
                .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_owned));
            out.response(
                id.as_deref(),
                Err(PrintError::invalid_payload(format!(
                    "malformed message: {e}"
                ))),
            )
            .await;
            return;
        }
    };
    let id = request.id.clone();
    if let Err(err) = check_envelope(&request) {
        out.response(Some(&id), Err(err)).await;
        return;
    }
    // Replay guard within the session: a request id is processed once.
    let (seen, order) = recent_ids;
    if !seen.insert(id.clone()) {
        out.response(
            Some(&id),
            Err(PrintError::invalid_payload("duplicate request id")),
        )
        .await;
        return;
    }
    order.push_back(id.clone());
    if order.len() > RECENT_REQUEST_IDS {
        if let Some(old) = order.pop_front() {
            seen.remove(&old);
        }
    }
    if !state.limiter.check(&principal.client_id) {
        out.response(
            Some(&id),
            Err(PrintError::new(
                ErrorCode::RateLimited,
                "too many requests; slow down",
            )),
        )
        .await;
        return;
    }
    let Ok(permit) = in_flight.clone().try_acquire_owned() else {
        out.response(
            Some(&id),
            Err(PrintError::new(
                ErrorCode::RateLimited,
                "too many concurrent requests on this connection",
            )),
        )
        .await;
        return;
    };
    let (state, out, principal) = (state.clone(), out.clone(), principal.clone());
    tokio::spawn(async move {
        let _permit = permit;
        let result = service::dispatch(&state, &principal, &request.method, request.params).await;
        out.response(Some(&id), result).await;
    });
}

fn check_envelope(request: &RequestEnvelope) -> Result<(), PrintError> {
    if request.id.is_empty() || request.id.len() > 128 {
        return Err(PrintError::invalid_payload("id must be 1-128 characters"));
    }
    if request.kind.as_deref().is_some_and(|k| k != "request") {
        return Err(PrintError::invalid_payload("type must be \"request\""));
    }
    if let Some(v) = request.protocol_version {
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&v) {
            return Err(PrintError::new(
                ErrorCode::UnsupportedProtocolVersion,
                format!("protocol version {v} is not supported"),
            ));
        }
    }
    Ok(())
}

/// Event payload if `principal` may see it.
fn visible_event(principal: &Principal, event: &EngineEvent) -> Option<Value> {
    match event {
        EngineEvent::Job { job, .. } => {
            let allowed = principal.has(Permission::JobsReadAll)
                || (principal.has(Permission::JobsRead) && job.client_id == principal.client_id);
            allowed.then(|| serde_json::to_value(job).ok()).flatten()
        }
        EngineEvent::Printer { printer, .. } => {
            let allowed = principal.has(Permission::PrintersRead)
                && principal.printer_scope().allows(printer);
            allowed
                .then(|| serde_json::to_value(printer).ok())
                .flatten()
        }
    }
}
