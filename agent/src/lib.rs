//! Kiln Print agent library: wires configuration, persistence, security, the print engine
//! and the local API together. The `kiln-agent` binary is a thin CLI over [`start`].

#![forbid(unsafe_code)]

pub mod api;
pub mod config;
pub mod logging;
pub mod persistence;
pub mod security;
pub mod sources;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use kiln_core::engine::PrintEngine;
use kiln_core::provider::PrintProvider;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::api::AppState;
use crate::config::AgentConfig;
use crate::persistence::Database;
use crate::security::{RateLimiter, RequestGuard, TokenAuthenticator, load_or_create_admin_token};

#[derive(Default)]
pub struct StartOptions {
    /// Additional providers (tests, embedders).
    pub extra_providers: Vec<Arc<dyn PrintProvider>>,
    /// Use this admin token instead of the token file (tests).
    pub admin_token: Option<String>,
}

impl std::fmt::Debug for StartOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartOptions")
            .field("extra_providers", &self.extra_providers.len())
            .finish_non_exhaustive()
    }
}

/// Builds the engine with every provider enabled by configuration.
pub fn build_engine(
    config: &AgentConfig,
    repository: Arc<dyn kiln_core::repository::JobRepository>,
    extra_providers: Vec<Arc<dyn PrintProvider>>,
) -> anyhow::Result<PrintEngine> {
    let mut builder = PrintEngine::builder()
        .config(config.engine_config())
        .repository(repository);
    for renderer in kiln_renderers::builtin() {
        builder = builder.renderer(renderer);
    }
    builder = builder.renderer(Arc::new(kiln_renderers::html::HtmlRenderer::new(
        kiln_renderers::html::HtmlConfig {
            browser: config.html.browser_path.clone(),
            timeout: std::time::Duration::from_secs(config.html.timeout_secs.max(1)),
            javascript: config.html.javascript,
            max_pdf_bytes: usize::try_from(config.limits.max_document_bytes).unwrap_or(usize::MAX),
        },
    )));
    for protocol in kiln_protocols::builtin() {
        builder = builder.protocol(protocol);
    }
    #[cfg(windows)]
    if config.providers.windows {
        builder = builder.provider(Arc::new(kiln_provider_windows::WindowsPrintProvider::new()));
    }
    if config.providers.mock {
        builder = builder.provider(Arc::new(kiln_provider_mock::MockProvider::demo()));
    }
    for provider in extra_providers {
        builder = builder.provider(provider);
    }
    Ok(builder.build()?)
}

#[derive(Debug)]
pub struct RunningAgent {
    pub local_addr: SocketAddr,
    pub engine: PrintEngine,
    pub state: Arc<AppState>,
    server: JoinHandle<std::io::Result<()>>,
}

impl RunningAgent {
    /// Graceful stop: closes sessions, stops the listener, settles the engine.
    pub async fn shutdown(self) {
        self.state.shutdown.cancel();
        match tokio::time::timeout(Duration::from_secs(10), self.server).await {
            Ok(Ok(Err(err))) => {
                warn!(target: "kiln::api", error = %err, "server error during shutdown")
            }
            Err(_) => warn!(target: "kiln::api", "timed out waiting for connections to close"),
            _ => {}
        }
        self.engine.shutdown().await;
        self.state.db.audit("agent.stopped", None, None, &json!({}));
        info!(target: "kiln::agent", "agent stopped");
    }

    /// Resolves when the server task ends (e.g. after `shutdown` was requested).
    pub fn shutdown_token(&self) -> CancellationToken {
        self.state.shutdown.clone()
    }
}

pub async fn start(config: AgentConfig, options: StartOptions) -> anyhow::Result<RunningAgent> {
    config.validate()?;
    let data_dir = config.data_dir()?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let db = Arc::new(Database::open(&data_dir.join("kiln.db")).context("opening the database")?);

    let admin_token = match options.admin_token {
        Some(token) => token,
        None => {
            let path = config
                .security
                .admin_token_file
                .clone()
                .unwrap_or_else(|| data_dir.join("admin.token"));
            load_or_create_admin_token(&path)?
        }
    };
    let auth = TokenAuthenticator::new(Some(&admin_token), &config.security)?;

    let engine = build_engine(&config, db.clone(), options.extra_providers)?;
    engine.start().await?;

    let listener = TcpListener::bind(config.server.bind)
        .await
        .with_context(|| format!("binding {}", config.server.bind))?;
    let local_addr = listener.local_addr()?;

    let state = Arc::new(AppState {
        engine: engine.clone(),
        auth: Arc::new(auth),
        guard: RequestGuard {
            allow_non_loopback: config.server.allow_non_loopback,
        },
        db: db.clone(),
        limiter: RateLimiter::new(
            config.security.rate_limit.requests_per_second,
            config.security.rate_limit.burst,
        ),
        sessions: Default::default(),
        connection_slots: Arc::new(Semaphore::new(config.server.max_connections)),
        shutdown: CancellationToken::new(),
        config,
    });

    let app = api::router(state.clone());
    let stop = state.shutdown.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { stop.cancelled().await })
        .await
    });
    tokio::spawn(audit_retention(state.clone()));

    db.audit(
        "agent.started",
        None,
        None,
        &json!({ "version": env!("CARGO_PKG_VERSION"), "address": local_addr.to_string() }),
    );
    info!(target: "kiln::agent", address = %local_addr, version = env!("CARGO_PKG_VERSION"), "agent listening");
    Ok(RunningAgent {
        local_addr,
        engine,
        state,
        server,
    })
}

async fn audit_retention(state: Arc<AppState>) {
    let days = state.config.storage.audit_retention_days;
    if days == 0 {
        return;
    }
    loop {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
        if let Err(err) = state.db.purge_audit_before(cutoff) {
            warn!(target: "kiln::audit", error = %err, "audit purge failed");
        }
        tokio::select! {
            _ = state.shutdown.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(6 * 3600)) => {}
        }
    }
}
