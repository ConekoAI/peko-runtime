//! The last run that failed in a session (ADR-089 D5).
//!
//! A failed or disrupted run leaves nothing in the conversation or the
//! session — no "Run failed" post, no notice for the model. What
//! happened is kept here instead, beside the conversation, until the
//! session's next run succeeds, and status surfaces (`principal_log`,
//! `peko log`) show it.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::common::disruption::{
    disrupted, failure_kind, FAILED, PEKO_LIMIT, PROVIDER_QUOTA, PROVIDER_UNREACHABLE,
};

/// What went wrong in a session's last run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailure {
    pub at: DateTime<Utc>,
    /// `peko_limit`, `provider_quota`, `provider_unreachable` (ADR-089
    /// D1), or `failed` — a failure waiting will not fix.
    pub kind: String,
    pub message: String,
}

impl RunFailure {
    #[must_use]
    pub fn new(error: &anyhow::Error) -> Self {
        Self {
            at: Utc::now(),
            kind: failure_kind(error).to_string(),
            message: error.to_string(),
        }
    }
}

/// The key a conversation's last failure is kept under: its DM channel
/// (what `principal_log` reads), or the session when it has none.
#[must_use]
pub fn conversation_key(channel: Option<&peko_channel::ChannelId>, session_id: &str) -> String {
    channel.map_or_else(|| session_id.to_string(), |c| c.0.clone())
}

/// Last failures by conversation ([`conversation_key`]).
#[derive(Debug, Default)]
pub struct RunFailures(Mutex<HashMap<String, RunFailure>>);

impl RunFailures {
    pub fn record(&self, session_id: &str, failure: RunFailure) {
        self.0
            .lock()
            .expect("run failures lock poisoned")
            .insert(session_id.to_string(), failure);
    }

    /// The session's run succeeded: its last failure is history.
    pub fn clear(&self, session_id: &str) {
        self.0
            .lock()
            .expect("run failures lock poisoned")
            .remove(session_id);
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<RunFailure> {
        self.0
            .lock()
            .expect("run failures lock poisoned")
            .get(session_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_success_clears_the_last_failure() {
        let failures = RunFailures::default();
        failures.record(
            "s1",
            RunFailure::new(&anyhow::anyhow!("HTTP error 503: down")),
        );
        assert_eq!(failures.get("s1").unwrap().kind, "provider_unreachable");
        failures.clear("s1");
        assert!(failures.get("s1").is_none());
    }
}
