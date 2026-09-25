//! Agent configuration (TOML). Every field has a safe default; an absent file means
//! "loopback only, no browser origins, admin token for native clients".

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail, ensure};
use kiln_core::engine::EngineConfig;
use serde::{Deserialize, Serialize};

use crate::security::{Permission, normalise_origin};

pub const DEFAULT_PORT: u16 = 18731;
const APP_DIR: &str = "KilnPrint";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    pub server: ServerConfig,
    pub security: SecurityConfig,
    pub limits: LimitsConfig,
    pub jobs: JobsConfig,
    pub discovery: DiscoveryConfig,
    pub providers: ProvidersConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Listen address. Must be loopback unless `allow_non_loopback` is set.
    pub bind: SocketAddr,
    /// Permits binding to a non-loopback address. Off by default: the agent exposes
    /// hardware access and must not be reachable from the network unless deliberately
    /// configured (and, from Phase 4, protected by TLS).
    pub allow_non_loopback: bool,
    pub max_connections: usize,
    /// Time a new WebSocket has to complete `session.hello`.
    pub handshake_timeout_secs: u64,
    pub heartbeat_secs: u64,
    /// Concurrent requests a single connection may have in flight.
    pub max_in_flight_per_connection: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)),
            allow_non_loopback: false,
            max_connections: 64,
            handshake_timeout_secs: 10,
            heartbeat_secs: 30,
            max_in_flight_per_connection: 16,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    /// File holding the local admin token (created on first start). Defaults to
    /// `<data_dir>/admin.token`.
    pub admin_token_file: Option<PathBuf>,
    /// Browser origins allowed to use the admin token. Empty by default: the admin token
    /// is for native tools and the local dashboard, not for web pages.
    pub admin_origins: Vec<String>,
    /// Pre-registered clients (Phase 1 trust model; Phase 4 adds interactive pairing).
    pub clients: Vec<ClientConfig>,
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// Stable client id recorded on every job (`[a-z0-9._-]`, 1-64 chars).
    pub id: String,
    pub name: String,
    /// SHA-256 (hex) of the client's token. The token itself is never stored.
    pub token_sha256: String,
    /// Browser origins this client may connect from. Native clients send no `Origin`.
    #[serde(default)]
    pub origins: Vec<String>,
    #[serde(default = "default_client_permissions")]
    pub permissions: Vec<Permission>,
    /// Printer ids or names this client may use; `["*"]` means all.
    #[serde(default = "all_printers")]
    pub printers: Vec<String>,
}

fn default_client_permissions() -> Vec<Permission> {
    vec![
        Permission::PrintersRead,
        Permission::Print,
        Permission::JobsRead,
        Permission::JobsCancel,
        Permission::QueueRead,
    ]
}

fn all_printers() -> Vec<String> {
    vec!["*".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Sustained requests per second per client.
    pub requests_per_second: f64,
    /// Short burst allowance per client.
    pub burst: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_second: 50.0,
            burst: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsConfig {
    pub max_document_bytes: u64,
    pub max_queued_bytes: u64,
    pub queue_capacity_per_printer: usize,
    pub max_copies: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        let engine = EngineConfig::default();
        Self {
            max_document_bytes: engine.max_document_bytes,
            max_queued_bytes: engine.max_queued_bytes,
            queue_capacity_per_printer: engine.queue_capacity_per_printer,
            max_copies: engine.max_copies,
        }
    }
}

impl LimitsConfig {
    /// Largest accepted WebSocket message / HTTP body: a base64-encoded maximum-size
    /// document plus JSON envelope overhead.
    pub fn max_message_bytes(&self) -> usize {
        usize::try_from(self.max_document_bytes.div_ceil(3) * 4 + 64 * 1024).unwrap_or(usize::MAX)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    pub submit_timeout_secs: u64,
    pub monitor_interval_ms: u64,
    pub monitor_max_interval_ms: u64,
    /// Days of job history to keep. 0 keeps history forever.
    pub retention_days: u32,
    pub strict_languages: bool,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            submit_timeout_secs: 120,
            monitor_interval_ms: 1000,
            monitor_max_interval_ms: 10_000,
            retention_days: 30,
            strict_languages: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryConfig {
    pub interval_secs: u64,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self { interval_secs: 5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    /// Windows spooler provider (ignored on other platforms).
    pub windows: bool,
    /// Simulated printers for development and demos.
    pub mock: bool,
}

impl Default for ProvidersConfig {
    fn default() -> Self {
        Self {
            windows: true,
            mock: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Database, token and log location. Defaults to the per-user local data directory.
    pub data_dir: Option<PathBuf>,
    pub audit_retention_days: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: None,
            audit_retention_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` filter, e.g. `info` or `info,kiln::monitor=debug`. `RUST_LOG` overrides.
    pub level: String,
    /// Write daily-rotated JSON logs to `<data_dir>/logs`.
    pub file: bool,
    pub console: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            file: true,
            console: true,
        }
    }
}

impl AgentConfig {
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join(APP_DIR).join("agent.toml"))
    }

    /// Loads `path`, or the default location, or built-in defaults if neither exists.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let path = match path {
            Some(p) => Some(p.to_path_buf()),
            None => Self::default_path().filter(|p| p.exists()),
        };
        let config = match &path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("parsing {}", p.display()))?
            }
            None => Self::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.server.bind.ip().is_loopback() && !self.server.allow_non_loopback {
            bail!(
                "server.bind = {} is not a loopback address; set server.allow_non_loopback = true to expose the agent to the network",
                self.server.bind
            );
        }
        ensure!(
            self.server.max_connections > 0,
            "server.max_connections must be > 0"
        );
        ensure!(
            self.server.max_in_flight_per_connection > 0,
            "server.max_in_flight_per_connection must be > 0"
        );
        ensure!(
            self.limits.max_document_bytes > 0,
            "limits.max_document_bytes must be > 0"
        );
        ensure!(self.limits.max_copies > 0, "limits.max_copies must be > 0");
        ensure!(
            self.security.rate_limit.requests_per_second > 0.0,
            "rate_limit.requests_per_second must be > 0"
        );
        for origin in &self.security.admin_origins {
            ensure!(
                normalise_origin(origin).is_some(),
                "invalid admin origin '{origin}'"
            );
        }
        let mut ids = HashSet::new();
        for client in &self.security.clients {
            ensure!(
                !client.id.is_empty()
                    && client.id.len() <= 64
                    && client
                        .id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)),
                "client id '{}' must be 1-64 characters of [a-z0-9._-]",
                client.id
            );
            ensure!(
                client.id != crate::security::ADMIN_CLIENT_ID,
                "client id '{}' is reserved",
                client.id
            );
            ensure!(
                ids.insert(client.id.as_str()),
                "duplicate client id '{}'",
                client.id
            );
            ensure!(
                client.token_sha256.len() == 64
                    && client.token_sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "client '{}': token_sha256 must be 64 hex characters",
                client.id
            );
            for origin in &client.origins {
                ensure!(
                    normalise_origin(origin).is_some(),
                    "client '{}': invalid origin '{origin}'",
                    client.id
                );
            }
        }
        Ok(())
    }

    pub fn data_dir(&self) -> anyhow::Result<PathBuf> {
        match &self.storage.data_dir {
            Some(dir) => Ok(dir.clone()),
            None => dirs::data_local_dir()
                .map(|d| d.join(APP_DIR))
                .context("could not determine the local data directory; set storage.data_dir"),
        }
    }

    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            queue_capacity_per_printer: self.limits.queue_capacity_per_printer,
            max_queued_bytes: self.limits.max_queued_bytes,
            max_document_bytes: self.limits.max_document_bytes,
            max_copies: self.limits.max_copies,
            submit_timeout: Duration::from_secs(self.jobs.submit_timeout_secs.max(1)),
            monitor_interval: Duration::from_millis(self.jobs.monitor_interval_ms.max(50)),
            monitor_max_interval: Duration::from_millis(self.jobs.monitor_max_interval_ms.max(50)),
            discovery_interval: Duration::from_secs(self.discovery.interval_secs.max(1)),
            strict_languages: self.jobs.strict_languages,
            job_retention: (self.jobs.retention_days > 0)
                .then(|| Duration::from_secs(u64::from(self.jobs.retention_days) * 86_400)),
            ..EngineConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_matches_defaults() {
        let example: AgentConfig =
            toml::from_str(include_str!("../agent.example.toml")).expect("example parses");
        example.validate().expect("example validates");
        let defaults = toml::to_string(&AgentConfig::default()).expect("serialise");
        assert_eq!(
            toml::to_string(&example).expect("serialise"),
            defaults,
            "agent.example.toml documents the defaults; keep them in sync"
        );
    }

    #[test]
    fn defaults_are_valid_and_loopback() {
        let config = AgentConfig::default();
        config.validate().expect("defaults validate");
        assert!(config.server.bind.ip().is_loopback());
        assert!(config.security.admin_origins.is_empty());
    }

    #[test]
    fn non_loopback_bind_requires_opt_in() {
        let mut config = AgentConfig::default();
        config.server.bind = "0.0.0.0:18731".parse().expect("addr");
        assert!(config.validate().is_err());
        config.server.allow_non_loopback = true;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn parses_client_config_and_rejects_typos() {
        let toml = r#"
            [[security.clients]]
            id = "lab-app"
            name = "Lab Application"
            token_sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
            origins = ["https://lab.example.com"]
            permissions = ["printers.read", "print"]
            printers = ["Zebra ZD421"]
        "#;
        let config: AgentConfig = toml::from_str(toml).expect("parse");
        config.validate().expect("valid");
        assert_eq!(
            config.security.clients[0].permissions,
            vec![Permission::PrintersRead, Permission::Print]
        );

        let typo = "[server]\nbnid = \"127.0.0.1:1\"\n";
        assert!(toml::from_str::<AgentConfig>(typo).is_err());
    }

    #[test]
    fn rejects_bad_clients() {
        let mut config = AgentConfig::default();
        config.security.clients.push(ClientConfig {
            id: "Bad Id".into(),
            name: "x".into(),
            token_sha256: "00".repeat(32),
            origins: vec![],
            permissions: vec![],
            printers: vec![],
        });
        assert!(config.validate().is_err());
    }

    #[test]
    fn message_limit_covers_base64_overhead() {
        let limits = LimitsConfig {
            max_document_bytes: 3 * 1024,
            ..LimitsConfig::default()
        };
        assert_eq!(limits.max_message_bytes(), 4 * 1024 + 64 * 1024);
    }
}
