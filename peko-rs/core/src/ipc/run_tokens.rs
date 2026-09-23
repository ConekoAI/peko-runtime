//! `PEKO_RUN_TOKEN` registry (ADR-061 D6, phase 2b) — short-lived,
//! per-spawn opaque tokens that authenticate `ExecuteTool` callbacks
//! independently of transport trust.
//!
//! The daemon holds one in-memory registry (on `AppState`); the
//! `Workflow` runner mints a token when it spawns a workflow process
//! and injects it as `PEKO_RUN_TOKEN`. The Python SDK echoes it back on
//! every `ExecuteTool` request; the handler then requires that the
//! token exists, is unexpired, and names the **same** `session_key` /
//! principal as the packet — the token *authenticates*, while
//! capability grants still derive server-side from the session key
//! (never from the token payload).
//!
//! Entries are removed lazily on access (no background sweeper): every
//! `verify` first drops expired entries. The registry dies with the
//! daemon process — a restart invalidates every outstanding token,
//! which is the intended semantics (a token outlives its run by at
//! most the TTL margin).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use rand::RngCore;

/// What a minted run token stands for. Compared field-by-field against
/// the `ExecuteTool` packet before the call is attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTokenEntry {
    /// Principal the workflow runs as (matches `principal.name`).
    pub principal_name: String,
    /// Session key the workflow's callbacks attribute to.
    pub session_key: String,
    /// Workflow nesting depth of the spawned process (0 = spawned by an
    /// agent turn directly). The `ExecuteTool` handler injects it into
    /// nested `Workflow` calls server-side so the recursion guard
    /// (`tools::builtin::workflow::MAX_WORKFLOW_DEPTH`) cannot be
    /// spoofed from the wire.
    pub workflow_depth: u32,
    /// The canonical session UUID of the tree node the `Workflow` tool
    /// was invoked FROM — the caller-awareness link. When `Some`, the
    /// `ExecuteTool` handler threads it into `ToolContext.session_id`
    /// (instead of the session-key string) so tree-relative tools
    /// (`Agent`, session-layer ownership guards) classify the workflow
    /// caller as that node — `Agent new` parents under it, ownership
    /// guards see its ancestors. `None` for no-context invocations
    /// (`workflow:direct`), which keep the dangling fail-closed
    /// behavior. In-memory only — never a wire field.
    pub caller_session_id: Option<String>,
    /// Absolute expiry (mint time + run timeout + margin).
    pub expires_at: DateTime<Utc>,
}

/// In-memory run-token registry. Cheap to clone — every clone shares
/// the same map.
#[derive(Debug, Default)]
pub struct RunTokenRegistry {
    inner: Mutex<HashMap<String, RunTokenEntry>>,
}

impl RunTokenRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a fresh token for `(principal_name, session_key)` with the
    /// given TTL. `caller_session_id` is the canonical session UUID of
    /// the tree node the caller was running in (`None` for no-context
    /// spawns). The token is 32 random bytes, base64-url encoded
    /// (no padding) — safe for env vars and JSON without escaping.
    pub fn mint(
        &self,
        principal_name: &str,
        session_key: &str,
        workflow_depth: u32,
        caller_session_id: Option<String>,
        ttl: Duration,
    ) -> String {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        let entry = RunTokenEntry {
            principal_name: principal_name.to_string(),
            session_key: session_key.to_string(),
            workflow_depth,
            caller_session_id,
            expires_at: Utc::now()
                + chrono::Duration::from_std(ttl).unwrap_or_else(|_| chrono::Duration::hours(1)),
        };
        self.inner
            .lock()
            .expect("run-token registry poisoned")
            .insert(token.clone(), entry);
        token
    }

    /// Look up a token, returning its entry iff it exists and is
    /// unexpired. Sweeps expired entries first (lazy GC).
    pub fn verify(&self, token: &str) -> Option<RunTokenEntry> {
        let mut map = self.inner.lock().expect("run-token registry poisoned");
        let now = Utc::now();
        map.retain(|_, e| e.expires_at > now);
        map.get(token).cloned()
    }

    /// Number of live entries (test support + diagnostics).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("run-token registry poisoned")
            .len()
    }

    /// True when no live entries remain (companion to [`Self::len`]).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_verify_roundtrips_entry() {
        let registry = RunTokenRegistry::new();
        let token = registry.mint(
            "peko-a",
            "agent:peko-a:workflow:abc",
            1,
            Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
            Duration::from_mins(1),
        );
        assert_eq!(token.len(), 43, "32 bytes → 43 url-safe chars (no pad)");
        let entry = registry.verify(&token).expect("fresh token verifies");
        assert_eq!(entry.principal_name, "peko-a");
        assert_eq!(entry.session_key, "agent:peko-a:workflow:abc");
        assert_eq!(entry.workflow_depth, 1);
        assert_eq!(
            entry.caller_session_id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn caller_session_id_defaults_to_none() {
        let registry = RunTokenRegistry::new();
        let token = registry.mint("p", "agent:p:workflow:x", 0, None, Duration::from_mins(1));
        let entry = registry.verify(&token).expect("verifies");
        assert_eq!(entry.caller_session_id, None);
    }

    #[test]
    fn tokens_are_unique_per_mint() {
        let registry = RunTokenRegistry::new();
        let a = registry.mint("p", "agent:p:workflow:x", 0, None, Duration::from_mins(1));
        let b = registry.mint("p", "agent:p:workflow:x", 0, None, Duration::from_mins(1));
        assert_ne!(a, b);
    }

    #[test]
    fn unknown_token_fails_closed() {
        let registry = RunTokenRegistry::new();
        assert!(registry.verify("nope").is_none());
    }

    #[test]
    fn expired_token_fails_closed_and_is_swept() {
        let registry = RunTokenRegistry::new();
        // Zero TTL → expired by the time verify runs (expiry must be
        // strictly in the future).
        let token = registry.mint("p", "agent:p:workflow:x", 0, None, Duration::ZERO);
        assert!(registry.verify(&token).is_none(), "expired must not verify");
        assert_eq!(registry.len(), 0, "lazy sweep removed the expired entry");
    }
}
