//! Registry of live client sessions (for connected-client management and the dashboard).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: Uuid,
    pub client_id: String,
    /// Name from the agent's trust configuration.
    pub client_name: String,
    /// Name/version the application reported about itself (informational only).
    pub reported_name: String,
    pub reported_version: Option<String>,
    pub origin: Option<String>,
    pub user_agent: Option<String>,
    pub protocol_version: u32,
    pub connected_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: Mutex<HashMap<Uuid, SessionInfo>>,
}

impl SessionRegistry {
    pub fn insert(&self, info: SessionInfo) {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(info.session_id, info);
    }

    pub fn touch(&self, session_id: Uuid) {
        if let Some(s) = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&session_id)
        {
            s.last_activity = Utc::now();
        }
    }

    pub fn remove(&self, session_id: Uuid) -> Option<SessionInfo> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&session_id)
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut list: Vec<_> = self
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        list.sort_by_key(|s| s.connected_at);
        list
    }
}
