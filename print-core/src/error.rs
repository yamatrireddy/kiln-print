//! Standardised error model.
//!
//! [`PrintError`] is the only error type that crosses the agent boundary. It is designed to
//! be serialised straight to SDK clients, so it must never carry stack traces, file paths
//! or raw OS diagnostics in `message`. Internal detail is logged, not returned — use
//! [`PrintError::internal`] for unexpected failures.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Machine-readable error codes. Serialised as `SCREAMING_SNAKE_CASE`.
///
/// Codes are part of the public protocol: never rename or remove one, only add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCode {
    PrinterNotFound,
    PrinterOffline,
    PrinterBusy,
    PaperOut,
    PaperJam,
    AccessDenied,
    ClientNotTrusted,
    AuthenticationRequired,
    InvalidPayload,
    PayloadTooLarge,
    UnsupportedDocument,
    UnsupportedOperation,
    UnsupportedProtocolVersion,
    PrintFailed,
    SpoolerError,
    ConnectionError,
    Timeout,
    QueueFull,
    RateLimited,
    JobNotFound,
    InvalidJobState,
    InternalError,
}

impl ErrorCode {
    /// Whether the condition behind this code can clear without the request changing
    /// (e.g. paper is loaded, the queue drains).
    ///
    /// `recoverable` is *not* a statement that re-sending is safe: a job that reached a
    /// spooler may already have produced output. The engine itself never retries a
    /// submission (see `docs/print-job-lifecycle.md`).
    pub const fn default_recoverable(self) -> bool {
        matches!(
            self,
            Self::PrinterOffline
                | Self::PrinterBusy
                | Self::PaperOut
                | Self::PaperJam
                | Self::ConnectionError
                | Self::Timeout
                | Self::QueueFull
                | Self::RateLimited
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrinterNotFound => "PRINTER_NOT_FOUND",
            Self::PrinterOffline => "PRINTER_OFFLINE",
            Self::PrinterBusy => "PRINTER_BUSY",
            Self::PaperOut => "PAPER_OUT",
            Self::PaperJam => "PAPER_JAM",
            Self::AccessDenied => "ACCESS_DENIED",
            Self::ClientNotTrusted => "CLIENT_NOT_TRUSTED",
            Self::AuthenticationRequired => "AUTHENTICATION_REQUIRED",
            Self::InvalidPayload => "INVALID_PAYLOAD",
            Self::PayloadTooLarge => "PAYLOAD_TOO_LARGE",
            Self::UnsupportedDocument => "UNSUPPORTED_DOCUMENT",
            Self::UnsupportedOperation => "UNSUPPORTED_OPERATION",
            Self::UnsupportedProtocolVersion => "UNSUPPORTED_PROTOCOL_VERSION",
            Self::PrintFailed => "PRINT_FAILED",
            Self::SpoolerError => "SPOOLER_ERROR",
            Self::ConnectionError => "CONNECTION_ERROR",
            Self::Timeout => "TIMEOUT",
            Self::QueueFull => "QUEUE_FULL",
            Self::RateLimited => "RATE_LIMITED",
            Self::JobNotFound => "JOB_NOT_FOUND",
            Self::InvalidJobState => "INVALID_JOB_STATE",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned to clients and recorded on failed jobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrintError {
    pub error_code: ErrorCode,
    /// Human-readable, client-safe message.
    pub message: String,
    pub job_id: Option<Uuid>,
    pub printer_id: Option<String>,
    pub recoverable: bool,
    /// Structured, client-safe context (e.g. `{"outcome": "UNKNOWN"}`).
    pub details: Option<serde_json::Value>,
}

pub type Result<T, E = PrintError> = std::result::Result<T, E>;

impl PrintError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error_code: code,
            message: message.into(),
            job_id: None,
            printer_id: None,
            recoverable: code.default_recoverable(),
            details: None,
        }
    }

    /// Wraps an unexpected failure. The context is logged; the client only sees a
    /// generic message so internal state never leaks through the API.
    pub fn internal(context: impl fmt::Display) -> Self {
        tracing::error!(target: "kiln::internal", error = %context, "internal error");
        Self::new(ErrorCode::InternalError, "internal agent error")
    }

    pub fn invalid_payload(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidPayload, message)
    }

    pub fn printer_not_found(printer: impl fmt::Display) -> Self {
        Self::new(
            ErrorCode::PrinterNotFound,
            format!("printer '{printer}' was not found"),
        )
    }

    pub fn job_not_found(job_id: Uuid) -> Self {
        Self::new(
            ErrorCode::JobNotFound,
            format!("job {job_id} was not found"),
        )
        .with_job(job_id)
    }

    pub fn with_job(mut self, job_id: Uuid) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn with_printer(mut self, printer_id: impl Into<String>) -> Self {
        self.printer_id = Some(printer_id.into());
        self
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn recoverable(mut self, recoverable: bool) -> Self {
        self.recoverable = recoverable;
        self
    }
}

impl fmt::Display for PrintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.error_code, self.message)
    }
}

impl std::error::Error for PrintError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_with_protocol_field_names() {
        let err = PrintError::new(ErrorCode::PaperOut, "out of paper").with_printer("win-1");
        let json = serde_json::to_value(&err).expect("serialise");
        assert_eq!(json["errorCode"], "PAPER_OUT");
        assert_eq!(json["printerId"], "win-1");
        assert_eq!(json["recoverable"], true);
        assert!(json["jobId"].is_null());
    }

    #[test]
    fn as_str_matches_serde() {
        for code in [
            ErrorCode::InvalidJobState,
            ErrorCode::UnsupportedProtocolVersion,
        ] {
            let json = serde_json::to_value(code).expect("serialise");
            assert_eq!(json, code.as_str());
        }
    }

    #[test]
    fn internal_errors_hide_context() {
        let err = PrintError::internal("C:\\secret\\path failed at line 7");
        assert_eq!(err.error_code, ErrorCode::InternalError);
        assert!(!err.message.contains("secret"));
    }
}
