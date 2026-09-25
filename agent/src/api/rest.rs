//! REST transport (`/v1/...`) for native clients, backends and management tools.
//!
//! Authentication: `Authorization: Bearer <token>`. Browser requests are subject to the
//! same Origin binding as WebSocket clients. No CORS headers are emitted, so browsers
//! cannot read REST responses cross-origin; browser apps use the WebSocket API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use kiln_core::error::{ErrorCode, PrintError};
use kiln_core::model::{PrinterId, PrinterSelector};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::wire::*;
use super::{AppState, service};
use crate::security::{Credentials, Principal, PrincipalKind};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/printers", get(printers))
        .route("/printers/default", get(default_printer))
        .route("/printers/{id}", get(printer))
        .route("/printers/{id}/capabilities", get(capabilities))
        .route("/print", post(print))
        .route("/print/raw", post(print_raw))
        .route("/print/text", post(print_text))
        .route("/print/pdf", post(print_pdf))
        .route("/print/html", post(print_html))
        .route("/print/image", post(print_image))
        .route("/print/{kind}", post(print_unsupported))
        .route("/jobs", get(jobs))
        .route("/jobs/{id}", get(job).delete(cancel_job))
        .route("/queue", get(queue_summary))
        .route("/queue/{printer_id}", get(queue))
        .route("/clients", get(clients))
        .route("/audit", get(audit))
}

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "protocolVersions": SUPPORTED_PROTOCOL_VERSIONS }))
}

/// Error response in the same shape as WebSocket responses.
#[derive(Debug)]
pub struct ApiError(pub PrintError);

impl From<PrintError> for ApiError {
    fn from(err: PrintError) -> Self {
        Self(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.0.error_code {
            ErrorCode::PrinterNotFound | ErrorCode::JobNotFound => StatusCode::NOT_FOUND,
            ErrorCode::AuthenticationRequired | ErrorCode::ClientNotTrusted => {
                StatusCode::UNAUTHORIZED
            }
            ErrorCode::AccessDenied => StatusCode::FORBIDDEN,
            ErrorCode::InvalidPayload => StatusCode::BAD_REQUEST,
            ErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::UnsupportedDocument
            | ErrorCode::UnsupportedOperation
            | ErrorCode::UnsupportedProtocolVersion => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::InvalidJobState => StatusCode::CONFLICT,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::QueueFull
            | ErrorCode::PrinterBusy
            | ErrorCode::PrinterOffline
            | ErrorCode::PaperOut
            | ErrorCode::PaperJam => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Timeout => StatusCode::GATEWAY_TIMEOUT,
            ErrorCode::ConnectionError | ErrorCode::SpoolerError | ErrorCode::PrintFailed => {
                StatusCode::BAD_GATEWAY
            }
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = json!({ "protocolVersion": PROTOCOL_VERSION, "ok": false, "error": self.0 });
        (status, Json(body)).into_response()
    }
}

type ApiResult = Result<Response, ApiError>;

fn ok(status: StatusCode, value: impl Serialize) -> ApiResult {
    let result = serde_json::to_value(value).map_err(PrintError::internal)?;
    Ok((
        status,
        Json(json!({ "protocolVersion": PROTOCOL_VERSION, "ok": true, "result": result })),
    )
        .into_response())
}

/// Authenticated caller, extracted from `Authorization: Bearer`.
#[derive(Debug)]
pub struct Authed(pub Principal);

impl FromRequestParts<Arc<AppState>> for Authed {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0)
            .ok_or_else(|| {
                ApiError(PrintError::internal("missing peer address")).into_response()
            })?;
        let conn = state
            .guard
            .check(peer, &parts.headers)
            .map_err(|(status, reason)| (status, reason).into_response())?;
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                ApiError(PrintError::new(
                    ErrorCode::AuthenticationRequired,
                    "missing bearer token",
                ))
                .into_response()
            })?;
        let principal = match state
            .auth
            .authenticate(&Credentials::Token(token.to_owned()), &conn)
        {
            Ok(p) => p,
            Err(err) => {
                state.db.audit(
                    "auth.failure",
                    None,
                    conn.origin.as_deref(),
                    &json!({ "errorCode": err.error_code, "transport": "rest" }),
                );
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                return Err(ApiError(err).into_response());
            }
        };
        if !state.limiter.check(&principal.client_id) {
            return Err(ApiError(PrintError::new(
                ErrorCode::RateLimited,
                "too many requests; slow down",
            ))
            .into_response());
        }
        Ok(Self(principal))
    }
}

fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, PrintError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| PrintError::invalid_payload(format!("request body is not valid JSON: {e}")))?;
    parse_params(value)
}

fn by_id(id: String) -> PrinterSelector {
    PrinterSelector::Id(PrinterId(id))
}

async fn printers(State(s): State<Arc<AppState>>, Authed(p): Authed) -> ApiResult {
    ok(StatusCode::OK, service::list_printers(&s, &p).await?)
}

async fn default_printer(State(s): State<Arc<AppState>>, Authed(p): Authed) -> ApiResult {
    ok(StatusCode::OK, service::default_printer(&s, &p).await?)
}

async fn printer(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    ok(
        StatusCode::OK,
        service::get_printer(&s, &p, &by_id(id)).await?,
    )
}

async fn capabilities(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    ok(
        StatusCode::OK,
        service::capabilities(&s, &p, &by_id(id)).await?,
    )
}

async fn print(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    p.require(crate::security::Permission::Print)?;
    let job =
        match body::<GenericPrintParams>(&bytes)?.into_typed(s.config.limits.max_document_bytes)? {
            TypedPrint::Ready(request) => service::submit(&s, &p, request).await?,
            TypedPrint::Pending(pending) => service::submit_pending(&s, &p, pending).await?,
        };
    ok(StatusCode::ACCEPTED, job)
}

async fn print_pdf(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    let pending = body::<PdfPrintParams>(&bytes)?.into_pending()?;
    ok(
        StatusCode::ACCEPTED,
        service::submit_pending(&s, &p, pending).await?,
    )
}

async fn print_image(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    let pending = body::<ImagePrintParams>(&bytes)?.into_pending()?;
    ok(
        StatusCode::ACCEPTED,
        service::submit_pending(&s, &p, pending).await?,
    )
}

async fn print_html(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    let request =
        body::<HtmlPrintParams>(&bytes)?.into_request(s.config.limits.max_document_bytes)?;
    ok(
        StatusCode::ACCEPTED,
        service::submit(&s, &p, request).await?,
    )
}

async fn print_raw(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    let request =
        body::<RawPrintParams>(&bytes)?.into_request(s.config.limits.max_document_bytes)?;
    ok(
        StatusCode::ACCEPTED,
        service::submit(&s, &p, request).await?,
    )
}

async fn print_text(State(s): State<Arc<AppState>>, Authed(p): Authed, bytes: Bytes) -> ApiResult {
    let request =
        body::<TextPrintParams>(&bytes)?.into_request(s.config.limits.max_document_bytes)?;
    ok(
        StatusCode::ACCEPTED,
        service::submit(&s, &p, request).await?,
    )
}

async fn print_unsupported(Authed(_): Authed, Path(kind): Path<String>) -> ApiResult {
    Err(PrintError::new(
        ErrorCode::UnsupportedDocument,
        format!("unknown document type '{}'", kind.to_ascii_uppercase()),
    )
    .into())
}

async fn jobs(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let mut params = serde_json::Map::new();
    for (key, value) in q {
        let value = match key.as_str() {
            "limit" | "offset" => value
                .parse::<u64>()
                .map(Value::from)
                .map_err(|_| PrintError::invalid_payload(format!("{key} must be a number")))?,
            _ => Value::String(value),
        };
        params.insert(key, value);
    }
    ok(
        StatusCode::OK,
        service::list_jobs(&s, &p, parse_params(Value::Object(params))?)?,
    )
}

async fn job(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    let job_id = JobIdParams { job_id: id }.job_id()?;
    ok(StatusCode::OK, service::get_job(&s, &p, job_id)?)
}

async fn cancel_job(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    let job_id = JobIdParams { job_id: id }.job_id()?;
    ok(StatusCode::OK, service::cancel_job(&s, &p, job_id).await?)
}

async fn queue_summary(State(s): State<Arc<AppState>>, Authed(p): Authed) -> ApiResult {
    ok(StatusCode::OK, service::queue_summary(&s, &p).await?)
}

async fn queue(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    ok(StatusCode::OK, service::queue(&s, &p, &by_id(id)).await?)
}

async fn clients(State(s): State<Arc<AppState>>, Authed(p): Authed) -> ApiResult {
    ok(StatusCode::OK, service::list_clients(&s, &p)?)
}

async fn audit(
    State(s): State<Arc<AppState>>,
    Authed(p): Authed,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    if p.kind != PrincipalKind::Admin {
        return Err(PrintError::new(
            ErrorCode::AccessDenied,
            "the audit log is only available to the local administrator",
        )
        .into());
    }
    let limit = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(100);
    ok(StatusCode::OK, s.db.audit_entries(limit)?)
}
