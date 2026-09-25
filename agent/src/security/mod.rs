//! Authentication, authorisation and request-origin checks.
//!
//! Phase 1 trust model (see `docs/security-model.md`):
//!
//! * The agent listens on loopback only.
//! * Every connection must authenticate with a token before any other request. There is
//!   no anonymous API apart from `GET /v1/health`.
//! * Browser connections (those with an `Origin` header) are accepted only from origins
//!   explicitly configured for the presenting client. A random website therefore cannot
//!   print even if the user has the agent running.
//! * `Host` must be a loopback name, which defeats DNS-rebinding attacks.
//! * Each client carries explicit permissions and a printer allow-list.
//!
//! Phase 4 replaces pre-shared client tokens with interactive pairing ("Application X
//! wants to use your printers") and short-lived session tokens, behind the same
//! [`ClientAuthenticator`] interface.

mod origin;
mod rate_limit;
mod token;

use std::collections::BTreeSet;
use std::net::SocketAddr;

use kiln_core::engine::{PrinterScope, Submitter};
use kiln_core::error::{ErrorCode, PrintError};
use serde::{Deserialize, Serialize};

pub use origin::{RequestGuard, normalise_origin};
pub use rate_limit::RateLimiter;
pub use token::{TokenAuthenticator, generate_token, hash_token, load_or_create_admin_token};

pub const ADMIN_CLIENT_ID: &str = "local-admin";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "printers.read")]
    PrintersRead,
    #[serde(rename = "print")]
    Print,
    /// Read the client's own jobs.
    #[serde(rename = "jobs.read")]
    JobsRead,
    /// Read every client's jobs (dashboards, administrators).
    #[serde(rename = "jobs.read.all")]
    JobsReadAll,
    #[serde(rename = "jobs.cancel")]
    JobsCancel,
    #[serde(rename = "jobs.cancel.all")]
    JobsCancelAll,
    #[serde(rename = "queue.read")]
    QueueRead,
    /// See connected clients.
    #[serde(rename = "clients.read")]
    ClientsRead,
}

impl Permission {
    pub const ALL: [Self; 8] = [
        Self::PrintersRead,
        Self::Print,
        Self::JobsRead,
        Self::JobsReadAll,
        Self::JobsCancel,
        Self::JobsCancelAll,
        Self::QueueRead,
        Self::ClientsRead,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PrincipalKind {
    Admin,
    Client,
}

/// An authenticated caller.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    pub client_id: String,
    pub name: String,
    pub kind: PrincipalKind,
    pub permissions: BTreeSet<Permission>,
    /// `["*"]` or explicit printer ids/names.
    pub printers: Vec<String>,
    pub origin: Option<String>,
}

impl Principal {
    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    pub fn require(&self, permission: Permission) -> Result<(), PrintError> {
        if self.has(permission) {
            Ok(())
        } else {
            Err(PrintError::new(
                ErrorCode::AccessDenied,
                format!(
                    "this client lacks the '{}' permission",
                    permission_name(permission)
                ),
            ))
        }
    }

    pub fn printer_scope(&self) -> PrinterScope {
        if self.printers.iter().any(|p| p == "*") {
            PrinterScope::All
        } else {
            PrinterScope::Only(self.printers.clone())
        }
    }

    pub fn submitter(&self) -> Submitter {
        Submitter {
            client_id: self.client_id.clone(),
            printers: self.printer_scope(),
        }
    }
}

fn permission_name(p: Permission) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Facts about the transport connection, established before authentication.
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    pub peer: SocketAddr,
    /// Normalised `Origin` header; `None` for native (non-browser) clients.
    pub origin: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Credentials {
    Token(String),
}

/// Pluggable authentication. Implementations must be constant-time with respect to
/// secrets and must enforce origin binding.
pub trait ClientAuthenticator: Send + Sync {
    fn authenticate(
        &self,
        credentials: &Credentials,
        connection: &ConnectionInfo,
    ) -> Result<Principal, PrintError>;

    /// Whether any client may connect from `origin`. Used to refuse WebSocket upgrades
    /// from unknown websites before any protocol exchange.
    fn origin_known(&self, origin: &str) -> bool;
}
