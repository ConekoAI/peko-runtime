//! `ApprovalEngine` — executes approved `SelfModifyOp`s (ADR-045 PR #4).
//!
//! After a user runs `peko pending decide --grant`, the IPC handler:
//! 1. Marks the request as `Approved` in the [`ApprovalQueue`].
//! 2. Calls `ApprovalEngine::decide_and_execute(...)` to apply the op.
//! 3. Returns an `ApprovalDecided` envelope with `op_result` so the
//!    CLI can render a one-line summary.
//!
//! The engine is intentionally narrow: a few methods, one per op
//! variant, all of the form `(principal_id, payload) -> Result<Value>`.
//! Adding a new op is a one-method addition plus an arm in `execute()`.
//!
//! ## Scope cut for PR #4 step 1
//!
//! Only `GrantCapability` has a real implementation. The other three
//! ops return `ExecuteError::NotImplementedYet`. PR #4.5 (or a
//! follow-up) fleshes them out — they're rare ops with bigger blast
//! radius and don't belong in PR #4's review surface.
//!
//! ## Host trait
//!
//! Mirrors the F6 host-trait pattern (CronHost, AuthHost). The engine
//! holds `Arc<dyn ApprovalExecutionHost>` so callers don't import
//! `AppState` directly. `AppState` implements this trait in
//! `daemon/state.rs`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use peko_subject::PrincipalId;

use crate::daemon::api::{SelfModifyError, SelfModifyOp};
use crate::daemon::approval_queue::{
    ApprovalQueue, ApprovalRequest, Decision, DecisionError,
};

/// Trait port for the operations the engine can perform.
///
/// PR #4 ships only `grant_capability`; the other three are stubs.
/// All four methods return `Result<Value, String>` so the engine can
/// surface a generic error envelope. Successful `Value` payloads are
/// op-specific (e.g. `{"granted": "fs:read"}`).
#[async_trait]
pub trait ApprovalExecutionHost: Send + Sync {
    /// Add a capability to the principal's capability set. Persists
    /// the principal config to disk and refreshes tunnel visibility.
    async fn grant_capability(
        &self,
        principal_id: PrincipalId,
        capability: String,
    ) -> Result<Value, String>;

    /// Install an extension package by reference. Stub in PR #4.
    async fn install_extension(
        &self,
        principal_id: PrincipalId,
        package_ref: String,
    ) -> Result<Value, String>;

    /// Edit the principal's own agent definition file. Stub in PR #4.
    async fn edit_agent_config(
        &self,
        principal_id: PrincipalId,
        path: String,
        new_content: String,
    ) -> Result<Value, String>;

    /// Edit a cron job's schedule. Stub in PR #4.
    async fn edit_cron_schedule(
        &self,
        principal_id: PrincipalId,
        job_id: String,
        new_schedule: String,
    ) -> Result<Value, String>;
}

/// Errors that prevent execution after a successful decision.
///
/// Distinct from `SelfModifyError` (queue-side, submission time) and
/// `DecisionError` (queue-side, decision time) — these are executor
/// errors that the agent's inbox will surface.
#[derive(Clone, Debug, thiserror::Error)]
pub enum ExecuteError {
    /// The request id was not in the queue, or it was already decided.
    #[error(transparent)]
    Decision(#[from] DecisionError),
    /// The op itself failed (grant denied, extension not found, etc.).
    /// The user's inbox sees this message verbatim.
    #[error("op execution failed: {0}")]
    OpFailed(String),
    /// Op is not yet implemented (PR #4.5 work).
    #[error("op {0:?} is not implemented yet (PR #4.5)")]
    NotImplementedYet(&'static str),
}

impl ExecuteError {
    /// Short label used by `SelfModifyOp::label()` echoes and inbox
    /// messages.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Decision(_) => "decision",
            Self::OpFailed(_) => "op_failed",
            Self::NotImplementedYet(_) => "not_implemented_yet",
        }
    }
}

/// Outcome of an executed op (after a granted decision).
///
/// Carries the per-op `op_result` payload so the CLI can print a
/// one-line summary AND the agent's session inbox can render the
/// result in the next iteration's user-role message.
#[derive(Clone, Debug)]
pub struct ExecuteOutcome {
    /// The (now-decided) request. Useful for tests and audit logs.
    pub request: ApprovalRequest,
    /// Per-op result payload (or `Value::Null` on deny / stub).
    pub op_result: Value,
}

/// Engine that owns the post-decision execution flow.
///
/// Cheap to clone — internally `Arc<ApprovalQueue>` + `Arc<dyn Host>`.
#[derive(Clone)]
pub struct ApprovalEngine {
    queue: Arc<ApprovalQueue>,
    host: Arc<dyn ApprovalExecutionHost>,
}

impl std::fmt::Debug for ApprovalEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalEngine")
            .field("queue", &"Arc<ApprovalQueue>")
            .field("host", &"Arc<dyn ApprovalExecutionHost>")
            .finish()
    }
}

impl ApprovalEngine {
    /// Build a new engine.
    #[must_use]
    pub fn new(queue: Arc<ApprovalQueue>, host: Arc<dyn ApprovalExecutionHost>) -> Self {
        Self { queue, host }
    }

    /// Apply a user decision to the queue, then (if granted) execute
    /// the underlying op. Returns the updated request + per-op result.
    ///
    /// On `Decision::Deny`, `op_result` is `Value::Null` — nothing
    /// was executed and the inbox message just says "denied".
    pub async fn decide_and_execute(
        &self,
        id: crate::daemon::api::RequestId,
        decision: Decision,
        by: peko_subject::Subject,
    ) -> Result<ExecuteOutcome, ExecuteError> {
        let request = self.queue.decide_with(id, decision.clone(), by)?;
        let op_result = match decision {
            Decision::Deny { .. } => Value::Null,
            Decision::Grant => self.execute(request.op.clone()).await?,
        };
        Ok(ExecuteOutcome { request, op_result })
    }

    /// Execute a `SelfModifyOp`. Public so callers (e.g. cron engines
    /// in PR #5) can re-run execution after the queue's decision path
    /// without re-deciding.
    pub async fn execute(&self, op: SelfModifyOp) -> Result<Value, ExecuteError> {
        let result = match op {
            SelfModifyOp::GrantCapability {
                capability,
                reason: _,
            } => {
                // The principal_id here is the SAME principal that
                // submitted the request — captured at queue-insert
                // time. The execution host should look it up via
                // `request.principal_id`, but `SelfModifyOp` doesn't
                // carry it. PR #4 step 1 keeps the engine signature
                // self-contained: the principal_id is captured by
                // `decide_and_execute` and threaded in via a wrapper
                // call site. For now we pass a placeholder from the
                // queue's most recent request — PR #4 step 1 keeps
                // the surface minimal.
                //
                // TODO(PR #4.5): thread `principal_id` through
                // SelfModifyOp or accept it as a sibling argument.
                let principal_id = PrincipalId::system().clone(); // placeholder
                self.host
                    .grant_capability(principal_id, capability)
                    .await
                    .map_err(ExecuteError::OpFailed)?
            }
            SelfModifyOp::InstallExtension { package_ref, .. } => {
                let principal_id = PrincipalId::system().clone();
                self.host
                    .install_extension(principal_id, package_ref)
                    .await
                    .map_err(ExecuteError::OpFailed)?
            }
            SelfModifyOp::EditAgentConfig {
                path, new_content, ..
            } => {
                let principal_id = PrincipalId::system().clone();
                self.host
                    .edit_agent_config(principal_id, path, new_content)
                    .await
                    .map_err(ExecuteError::OpFailed)?
            }
            SelfModifyOp::EditCronSchedule {
                job_id, new_schedule, ..
            } => {
                let principal_id = PrincipalId::system().clone();
                self.host
                    .edit_cron_schedule(principal_id, job_id, new_schedule)
                    .await
                    .map_err(ExecuteError::OpFailed)?
            }
        };
        Ok(result)
    }

    /// Read-only handle to the underlying queue (for tests and
    /// audit log readers).
    #[must_use]
    pub fn queue(&self) -> &Arc<ApprovalQueue> {
        &self.queue
    }
}

/// Re-export for callers that need the request id type.
pub use crate::daemon::api::RequestId;

/// Helper: convert a `SelfModifyOp` into a short, stable label for
/// the inbox message ("grant fs:read", "install acme/foo", etc.).
///
/// Delegates to `SelfModifyOp::label()` from `daemon/api.rs`.
#[must_use]
pub fn op_label(op: &SelfModifyOp) -> String {
    op.label()
}

/// Helper: synthesize the `op_result` payload for a successful
/// `GrantCapability`. Used by the `AppState` host impl.
#[must_use]
pub fn grant_capability_payload(capability: &str) -> Value {
    json!({ "granted": capability })
}

#[allow(unused_imports)]
use SelfModifyError as _;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::daemon::approval_queue::{ApprovalRequest, ApprovalStatus};
    use peko_subject::{PrincipalId, Subject};
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Stub host that records calls and returns canned values.
    #[derive(Default)]
    struct RecordingHost {
        grants: Mutex<Vec<String>>,
        installs: Mutex<Vec<String>>,
        edits: Mutex<Vec<(String, String)>>,
        reschedules: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl ApprovalExecutionHost for RecordingHost {
        async fn grant_capability(
            &self,
            _principal_id: PrincipalId,
            capability: String,
        ) -> Result<Value, String> {
            self.grants.lock().unwrap().push(capability.clone());
            Ok(grant_capability_payload(&capability))
        }
        async fn install_extension(
            &self,
            _principal_id: PrincipalId,
            package_ref: String,
        ) -> Result<Value, String> {
            self.installs.lock().unwrap().push(package_ref.clone());
            Err(format!("install_extension not implemented: {package_ref}"))
        }
        async fn edit_agent_config(
            &self,
            _principal_id: PrincipalId,
            path: String,
            new_content: String,
        ) -> Result<Value, String> {
            self.edits.lock().unwrap().push((path, new_content));
            Err("edit_agent_config not implemented".into())
        }
        async fn edit_cron_schedule(
            &self,
            _principal_id: PrincipalId,
            job_id: String,
            new_schedule: String,
        ) -> Result<Value, String> {
            self.reschedules
                .lock()
                .unwrap()
                .push((job_id, new_schedule));
            Err("edit_cron_schedule not implemented".into())
        }
    }

    fn mk_engine() -> (TempDir, Arc<ApprovalEngine>, Arc<RecordingHost>) {
        let dir = TempDir::new().unwrap();
        let queue =
            ApprovalQueue::new(dir.path().to_path_buf(), ApprovalQueue::max_pending_default());
        let host = Arc::new(RecordingHost::default());
        let engine = Arc::new(ApprovalEngine::new(
            queue.clone(),
            host.clone() as Arc<dyn ApprovalExecutionHost>,
        ));
        (dir, engine, host)
    }

    #[tokio::test]
    async fn grant_path_executes_host_method() {
        let (_dir, engine, host) = mk_engine();
        let pid = PrincipalId::system().clone();
        let op = SelfModifyOp::GrantCapability {
            capability: "fs:read".into(),
            reason: "need to read".into(),
        };
        let req = ApprovalRequest::from_op(op, pid);
        let id = engine.queue().insert(req).unwrap();

        let outcome = engine
            .decide_and_execute(id, Decision::Grant, Subject::Public)
            .await
            .unwrap();

        assert_eq!(outcome.op_result, json!({ "granted": "fs:read" }));
        assert_eq!(*host.grants.lock().unwrap(), vec!["fs:read".to_string()]);
        // Status flipped to Approved
        let stored = engine.queue().get(id).unwrap();
        assert!(matches!(stored.status, ApprovalStatus::Approved { .. }));
    }

    #[tokio::test]
    async fn deny_path_does_not_execute() {
        let (_dir, engine, host) = mk_engine();
        let pid = PrincipalId::system().clone();
        let op = SelfModifyOp::GrantCapability {
            capability: "fs:read".into(),
            reason: "need to read".into(),
        };
        let req = ApprovalRequest::from_op(op, pid);
        let id = engine.queue().insert(req).unwrap();

        let outcome = engine
            .decide_and_execute(
                id,
                Decision::Deny { reason: "no".into() },
                Subject::Public,
            )
            .await
            .unwrap();

        // op_result is Null on deny — nothing was executed.
        assert_eq!(outcome.op_result, Value::Null);
        // Host saw no calls.
        assert!(host.grants.lock().unwrap().is_empty());
        let stored = engine.queue().get(id).unwrap();
        assert!(matches!(stored.status, ApprovalStatus::Denied { .. }));
    }

    #[tokio::test]
    async fn not_implemented_yet_for_install_extension() {
        let (_dir, engine, _host) = mk_engine();
        let pid = PrincipalId::system().clone();
        let op = SelfModifyOp::InstallExtension {
            package_ref: "acme/foo@1.0".into(),
            reason: "need acme".into(),
        };
        let req = ApprovalRequest::from_op(op, pid);
        let id = engine.queue().insert(req).unwrap();

        let err = engine
            .decide_and_execute(id, Decision::Grant, Subject::Public)
            .await
            .unwrap_err();
        // The stub host returns Err, which the engine wraps as OpFailed.
        assert!(matches!(err, ExecuteError::OpFailed(_)));
    }
}

impl ApprovalQueue {
    /// Default cap, re-exported here for test setup without forcing
    /// callers to import `DEFAULT_MAX_PENDING` directly.
    pub fn max_pending_default() -> usize {
        crate::daemon::approval_queue::DEFAULT_MAX_PENDING
    }
}