//! `DaemonApi` — the in-process API the runtime uses to talk to
//! daemon-owned state.
//!
//! ADR-045 "Layer 3 — `peko_self` tool + inbox flow":
//!
//! ```text
//! Agent's loop → peko_self tool → DaemonApi::request_self_modify
//!                                          │
//!                                          ├─ validate op scope
//!                                          ├─ persist to ~/.peko/runtime/pending-requests/<uuid>.json
//!                                          ├─ enqueue in ApprovalQueue (in-memory)
//!                                          └─ return request_id to caller
//! ```
//!
//! The runtime never holds a token that lets it execute. It can only
//! **request**. The daemon executes the privileged work in PR #4
//! (`ApprovalEngine::execute` after the user decides via the inbox).
//!
//! Note: this trait is NOT a global port-trait (`peko_cron::CronRuntime`
//! is). It lives on `AppState` directly because every call site already
//! has an `&AppState` handle. The trait exists purely to make the
//! `peko_self` tool unit-testable without a full daemon.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use peko_subject::PrincipalId;

/// Caller identity for a self-modify request.
///
/// Deliberately smaller than [`peko_auth::caller::CallerContext`]:
/// the `peko_self` tool runs **in-process** (no IPC), so the full
/// IPC auth surface (rate-limit bucket, auth method, API-key scopes)
/// is irrelevant. We only need the `PrincipalId` of the principal
/// that originated the request so the on-disk artifact and the
/// inbox UI (PR #4) can attribute it.
#[derive(Clone, Debug)]
pub struct SelfModifyContext {
    pub principal_id: PrincipalId,
}

impl SelfModifyContext {
    /// Convenience: build from a borrowed `PrincipalId`.
    #[must_use]
    pub fn for_principal(principal_id: PrincipalId) -> Self {
        Self { principal_id }
    }
}

/// Unique identifier for a pending self-modification request.
///
/// UUIDv4 — same namespace as `CronJob` ids and `invite_token` ids in
/// the codebase. 128 bits of entropy from `OsRng`.
pub type RequestId = Uuid;

/// One of the four self-modification operations an agent can request
/// via `peko_self`. Each variant carries enough context for the user
/// to make an informed decision and for the daemon to execute after
/// approval (PR #4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SelfModifyOp {
    /// Request that the principal be granted an additional capability.
    /// Meta-capabilities (`principal:*`, `runtime:*`) and tool caps
    /// (`tool:*`) are categorically rejected by the daemon before
    /// reaching the queue (ADR-045 §"Meta-capability immutability").
    GrantCapability {
        capability: String,
        reason: String,
    },
    /// Request that an extension package be installed.
    InstallExtension {
        package_ref: String,
        reason: String,
    },
    /// Request that the principal's own agent definition file be edited.
    EditAgentConfig {
        path: String,
        new_content: String,
        reason: String,
    },
    /// Request that a cron job's schedule be modified.
    EditCronSchedule {
        job_id: String,
        new_schedule: String,
        reason: String,
    },
}

impl SelfModifyOp {
    /// Short human-readable label for the request. Used in the
    /// pending-requests JSON file and (in PR #4) the user's inbox UI.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::GrantCapability { capability, .. } => format!("grant {capability}"),
            Self::InstallExtension { package_ref, .. } => format!("install {package_ref}"),
            Self::EditAgentConfig { path, .. } => format!("edit {path}"),
            Self::EditCronSchedule { job_id, .. } => format!("reschedule {job_id}"),
        }
    }
}

/// Errors that prevent a self-modify request from being queued.
///
/// These are returned synchronously to the agent — the agent should
/// treat them as terminal failures, not retryable conditions.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SelfModifyError {
    /// `principal:*` / `runtime:*` are categorically not self-grantable.
    /// The agent must not retry; the user must grant (if at all) via
    /// the terminal.
    #[error(
        "meta-capability {0:?} is not self-grantable; principal must request user approval \
         via the terminal (ADR-045 §\"Meta-capability immutability\")"
    )]
    MetaCapabilityForbidden(String),

    /// `tool:*` capabilities are user-grantable only — a principal
    /// cannot grow its own toolset via `peko_self`. The user must
    /// grant directly from their terminal.
    #[error(
        "tool capability {0:?} is user-grantable only, not self-grantable; \
         request user approval via the terminal"
    )]
    ToolCapabilityNotSelfGrantable(String),

    /// Capability string didn't parse as `domain:action`. We accept
    /// any non-empty string containing a colon to avoid lockstep
    /// with the catalog; the user decides if it looks valid.
    #[error("invalid capability format {0:?}: expected `domain:action`")]
    InvalidCapabilityFormat(String),

    /// The on-disk queue is at capacity. New requests are refused
    /// rather than silently dropped so the agent can surface the
    /// condition to the user.
    #[error("approval queue at capacity ({0} pending); retry after user decisions")]
    QueueFull(usize),

    /// Failed to persist the request to disk. The in-memory map is
    /// not updated on this error path so the queue stays consistent
    /// with what's on disk.
    #[error("failed to persist pending request: {0}")]
    PersistenceFailed(String),
}

/// The in-process API the runtime uses to talk to daemon-owned state.
///
/// Implementor: `AppState` (see `peko-rs/core/src/daemon/state.rs`).
/// Consumer: `peko_self` tool (see `peko-rs/core/src/tools/builtin/peko_self.rs`).
///
/// This trait is intentionally minimal — just the one method the
/// `peko_self` tool needs. Adding more methods is a deliberate API
/// change that should be reviewed against the ADR's "two-channel"
/// mental model: everything that mutates principal state goes
/// through here, never via IPC.
#[async_trait]
pub trait DaemonApi: Send + Sync + fmt::Debug {
    /// Submit a self-modification request.
    ///
    /// The caller (agent) is unblocked once the request is queued.
    /// The user decides asynchronously; the result is delivered to
    /// the agent's session inbox in PR #4 (`AsyncInboxItem::Approval`).
    async fn request_self_modify(
        &self,
        op: SelfModifyOp,
        ctx: SelfModifyContext,
    ) -> Result<RequestId, SelfModifyError>;
}

// =============================================================================
// Global daemon-api registry (ADR-045 PR #3)
// =============================================================================
//
// One process-global slot for the `DaemonApi` impl. The daemon
// populates it once at startup; `peko_self` registration (via
// `register_builtins` and `register_globals`) reads from it. This
// avoids threading the handle through 4 layers of constructor
// signatures for a tool that always needs the same per-process
// handle.
//
// In tests, the slot is a `RwLock` so multiple tests in the same
// binary can re-set it without `OnceLock::set` poisoning.
//
// Reading returns `None` if the daemon hasn't initialized — peko_self
// simply isn't registered in that case (the tool gets a clean
// "not found" error rather than a panic), and the CLI side can run
// without the daemon.

use std::sync::OnceLock;

#[cfg(not(test))]
static GLOBAL_DAEMON_API: OnceLock<Arc<dyn DaemonApi>> = OnceLock::new();
#[cfg(test)]
static GLOBAL_DAEMON_API: std::sync::RwLock<Option<Arc<dyn DaemonApi>>> =
    std::sync::RwLock::new(None);

/// Initialize the process-global `DaemonApi` handle.
///
/// Called once by the daemon at startup. Subsequent calls are
/// no-ops in release builds (the `OnceLock` rejects duplicates); in
/// tests, the slot is `RwLock<Option<…>>` so tests can re-set it.
pub fn init_global_daemon_api(api: Arc<dyn DaemonApi>) {
    #[cfg(not(test))]
    {
        let _ = GLOBAL_DAEMON_API.set(api);
    }
    #[cfg(test)]
    {
        let mut g = GLOBAL_DAEMON_API.write().expect("daemon api global poisoned");
        *g = Some(api);
    }
}

/// Get the process-global `DaemonApi` handle, if the daemon has
/// initialized one.
#[must_use]
pub fn global_daemon_api() -> Option<Arc<dyn DaemonApi>> {
    #[cfg(not(test))]
    {
        GLOBAL_DAEMON_API.get().cloned()
    }
    #[cfg(test)]
    {
        GLOBAL_DAEMON_API.read().expect("daemon api global poisoned").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_modify_context_for_principal_carries_id() {
        let pid = PrincipalId::system().clone();
        let ctx = SelfModifyContext::for_principal(pid.clone());
        assert_eq!(ctx.principal_id, pid);
    }

    #[test]
    fn self_modify_op_label_for_grant() {
        let op = SelfModifyOp::GrantCapability {
            capability: "fs:read".into(),
            reason: "test".into(),
        };
        assert_eq!(op.label(), "grant fs:read");
    }

    #[test]
    fn self_modify_op_label_for_install() {
        let op = SelfModifyOp::InstallExtension {
            package_ref: "acme/foo@1.2.3".into(),
            reason: "test".into(),
        };
        assert_eq!(op.label(), "install acme/foo@1.2.3");
    }

    #[test]
    fn self_modify_op_round_trip_via_serde() {
        let op = SelfModifyOp::EditCronSchedule {
            job_id: "job-123".into(),
            new_schedule: "0 * * * *".into(),
            reason: "test".into(),
        };
        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains("\"op\":\"edit_cron_schedule\""));
        let parsed: SelfModifyOp = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, op);
    }

    #[test]
    fn self_modify_error_displays_helpfully() {
        let e = SelfModifyError::MetaCapabilityForbidden("principal:create".into());
        let msg = e.to_string();
        assert!(msg.contains("principal:create"));
        assert!(msg.contains("not self-grantable"));
    }
}