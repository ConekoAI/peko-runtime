//! In-memory `SessionRuntime` implementation for tests and placeholder
//! use (CLI/test harnesses that don't have a real `SessionManager`).
//!
//! Replaces the legacy `SessionCache` from root's
//! `src/tools/builtin/session.rs`. Mirrors the same shape: keyed by
//! session_key, returns pre-loaded `SessionInfo` / `HistoryMessage` /
//! `SessionStatusResult` records.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    HistoryMessage, SessionInfo, SessionRuntime, SessionStatusResult, SharedSessionRuntime,
};

/// In-memory session cache for testing and placeholder use.
///
/// Backed by three `HashMap<String, T>` slots. The current session is
/// held by-value (no clone-on-read for that hot path). The session_key
/// field on `SessionInfo`/`HistoryMessage`/`SessionStatusResult` is
/// always the lookup key.
#[derive(Debug)]
pub struct SessionCache {
    current_session: String,
    sessions: Mutex<HashMap<String, SessionInfo>>,
    histories: Mutex<HashMap<String, Vec<HistoryMessage>>>,
    statuses: Mutex<HashMap<String, SessionStatusResult>>,
}

impl SessionCache {
    /// Create a new in-memory session cache.
    #[must_use]
    pub fn new(current_session: impl Into<String>) -> Self {
        Self {
            current_session: current_session.into(),
            sessions: Mutex::new(HashMap::new()),
            histories: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
        }
    }

    /// Add a session with its history and status.
    pub fn add_session(
        &self,
        key: String,
        info: SessionInfo,
        history: Vec<HistoryMessage>,
        status: SessionStatusResult,
    ) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(key.clone(), info);
        self.histories
            .lock()
            .expect("histories mutex poisoned")
            .insert(key.clone(), history);
        self.statuses
            .lock()
            .expect("statuses mutex poisoned")
            .insert(key, status);
    }

    /// Wrap into a `SharedSessionRuntime` for tool construction.
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedSessionRuntime {
        self as Arc<dyn SessionRuntime>
    }
}

#[async_trait]
impl SessionRuntime for SessionCache {
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        peer: Option<&peko_subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));

        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let filtered: Vec<SessionInfo> = sessions
            .values()
            .filter(|s| {
                let kind_match = kinds.map_or(true, |k| k.contains(&s.kind));
                let agent_match = agent_id.map_or(true, |a| s.agent_id.as_deref() == Some(a));
                let active_match = cutoff_ms.map_or(true, |_| {
                    chrono::DateTime::parse_from_rfc3339(&s.last_activity)
                        .map(|dt| dt.timestamp_millis() as u64 >= cutoff_ms.unwrap_or(0))
                        .unwrap_or(true)
                });
                let peer_match = peer_filter.as_ref().map_or(true, |(want_kind, want_id)| {
                    let (have_kind, have_id) = match (s.peer_type.as_deref(), s.peer_id.as_deref())
                    {
                        (Some(k), Some(i)) => (k, i),
                        _ => return false,
                    };
                    have_kind == want_kind.as_str() && have_id == want_id.as_str()
                });
                kind_match && peer_match && agent_match && active_match
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        _include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        let histories = self.histories.lock().expect("histories mutex poisoned");
        let history = histories
            .get(&session_key.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(history.into_iter().take(limit).collect())
    }

    async fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult> {
        self.statuses
            .lock()
            .expect("statuses mutex poisoned")
            .get(&session_key.to_string())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))
    }

    fn current_session_key(&self) -> String {
        self.current_session.clone()
    }
}
