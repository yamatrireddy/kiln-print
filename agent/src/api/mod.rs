//! Local API: WebSocket (`/v1/ws`) and REST (`/v1/...`) over the same listener.

pub mod rest;
pub mod service;
pub mod sessions;
pub mod wire;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::get;
use kiln_core::engine::PrintEngine;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::config::AgentConfig;
use crate::persistence::Database;
use crate::security::{ClientAuthenticator, RateLimiter, RequestGuard};

pub struct AppState {
    pub engine: PrintEngine,
    pub auth: Arc<dyn ClientAuthenticator>,
    pub guard: RequestGuard,
    pub db: Arc<Database>,
    pub limiter: RateLimiter,
    pub sessions: sessions::SessionRegistry,
    pub config: AgentConfig,
    pub connection_slots: Arc<Semaphore>,
    pub shutdown: CancellationToken,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    let body_limit = state.config.limits.max_message_bytes();
    Router::new()
        .route("/v1/health", get(rest::health))
        .route("/v1/ws", get(ws::upgrade))
        .nest("/v1", rest::routes())
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(state)
}
