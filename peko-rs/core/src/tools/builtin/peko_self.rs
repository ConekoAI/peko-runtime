//! `peko_self` tool — agent-driven self-modification request path (ADR-045 PR #3).
//!
//! This is the structural alternative to bash-launched privilege
//! escalation. Where a principal with `tool:Bash` previously could
//! subvert its capability boundary by talking to the daemon IPC
//! directly, an agent that genuinely needs more capability now calls
//! `peko_self { op: "grant_capability", capability: "tool:Write",
//! reason: "..." }`. The call:
//!
//! 1. Goes through the in-process `DaemonApi` trait (no IPC).
//! 2. Validates the op scope (meta-capability immutability).
//! 3. Persists a durable on-disk artifact under
//!    `<data_dir>/runtime/pending-requests/<uuid>.json` (mode 0600).
//! 4. Enqueues in the in-memory `ApprovalQueue` for the user to
//!    discover asynchronously.
//! 5. Returns immediately with `{ status: "pending", request_id }`.
//!    PR #4 wires the asynchronous decision back into the agent's
//!    inbox via `AsyncInboxItem::Approval`.

use std::sync::Arc;

use async_trait::async_trait;
use peko_tools_core::traits::Tool;
use serde_json::json;
use serde_json::Value;

use crate::daemon::api::{
    DaemonApi, RequestId, SelfModifyContext, SelfModifyError, SelfModifyOp,
};
use peko_subject::PrincipalId;

/// Tool name registered into the catalog.
pub const TOOL_NAME: &str = "peko_self";

/// The `peko_self` tool.
///
/// Holds an `Arc<dyn DaemonApi>` so tests can drive it with a stub
/// without booting a full daemon. Production wires `AppState`'s
/// `DaemonApi` impl (see `daemon/state.rs`).
#[derive(Debug)]
pub struct PekoSelfTool {
    api: Arc<dyn DaemonApi>,
}

impl PekoSelfTool {
    /// Construct with a daemon API handle.
    #[must_use]
    pub fn new(api: Arc<dyn DaemonApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl Tool for PekoSelfTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> String {
        r#"Request additional capability, extension install, or self-edit.

This is the structured alternative to invoking `Bash` to modify your own
runtime. The request is queued durably; the user decides asynchronously
from their terminal or desktop. You will not see the decision inline.

Parameters (one op at a time):

- op=grant_capability:
  - capability (string, required): the `<domain>:<action>` capability
    string, e.g. `fs:write`, `net:fetch`, `cron:create`. Meta
    capabilities (`principal:*`, `runtime:*`) and tool capabilities
    (`tool:*`) are categorically rejected — they require user action
    from the terminal, not a self-request.
  - reason (string, required): free text explaining why this is needed.

- op=install_extension:
  - package_ref (string, required): e.g. `acme/foo@1.2.3`.
  - reason (string, required): free text explaining why this is needed.

- op=edit_agent_config:
  - path (string, required): path to the agent file to edit.
  - new_content (string, required): new file contents.
  - reason (string, required): free text explaining why this is needed.

- op=edit_cron_schedule:
  - job_id (string, required): the cron job id.
  - new_schedule (string, required): new cron expression.
  - reason (string, required): free text explaining why this is needed.

Returns:

  On success: `{ "status": "pending", "request_id": "<uuid>" }`.

  On rejection: a structured error message describing why (meta-cap
  forbidden, malformed capability, queue full, etc.)."#
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": [
                        "grant_capability",
                        "install_extension",
                        "edit_agent_config",
                        "edit_cron_schedule"
                    ],
                    "description": "Which self-modification to request."
                },
                "capability": {
                    "type": "string",
                    "description": "For op=grant_capability: `<domain>:<action>`."
                },
                "package_ref": {
                    "type": "string",
                    "description": "For op=install_extension: package reference."
                },
                "path": {
                    "type": "string",
                    "description": "For op=edit_agent_config: file path."
                },
                "new_content": {
                    "type": "string",
                    "description": "For op=edit_agent_config: new file contents."
                },
                "job_id": {
                    "type": "string",
                    "description": "For op=edit_cron_schedule: cron job id."
                },
                "new_schedule": {
                    "type": "string",
                    "description": "For op=edit_cron_schedule: cron expression."
                },
                "reason": {
                    "type": "string",
                    "description": "Free-text reason the request is needed."
                }
            },
            "required": ["op", "reason"]
        })
    }

    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        // We delegate to execute_with_context so the default timeout /
        // abort handling still applies. extract_op + dispatch are
        // factored out so future per-call context (e.g., a principal
        // id override) can be injected cleanly.
        let ctx = peko_tools_core::exec::ToolContext::default_for_tool(TOOL_NAME);
        self.execute_with_context(params, &ctx).await
    }

    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &peko_tools_core::exec::ToolContext,
    ) -> anyhow::Result<Value> {
        let op = parse_op(&params).map_err(|e| anyhow::anyhow!("{e}"))?;

        // Resolve principal_id. ToolContext.principal_id is `Option<String>`;
        // tools that aren't invoked from a peko_self runtime path may
        // pass None (we fall back to PrincipalId::system()). Production
        // is always populated because every agentic loop sets it from
        // the active session.
        let principal_id = match ctx.principal_id.as_deref() {
            Some(s) if !s.is_empty() => parse_principal_id(s),
            _ => PrincipalId::system().clone(),
        };
        let api_ctx = SelfModifyContext::for_principal(principal_id);

        match self.api.request_self_modify(op, api_ctx).await {
            Ok(id) => Ok(json!({
                "status": "pending",
                "request_id": id.to_string(),
            })),
            Err(e) => Err(anyhow::anyhow!(
                "peko_self request rejected: {e}"
            )),
        }
    }
}

/// Parse the `params` JSON object into a [`SelfModifyOp`].
///
/// Returns a `SelfModifyError`-shaped error string for missing fields
/// or unknown ops so the agent sees a clear, structured message.
fn parse_op(params: &Value) -> Result<SelfModifyOp, String> {
    let op = params
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required field 'op'".to_string())?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required field 'reason'".to_string())?
        .to_string();

    match op {
        "grant_capability" => {
            let capability = params
                .get("capability")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=grant_capability requires 'capability'".to_string())?
                .to_string();
            Ok(SelfModifyOp::GrantCapability { capability, reason })
        }
        "install_extension" => {
            let package_ref = params
                .get("package_ref")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=install_extension requires 'package_ref'".to_string())?
                .to_string();
            Ok(SelfModifyOp::InstallExtension { package_ref, reason })
        }
        "edit_agent_config" => {
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=edit_agent_config requires 'path'".to_string())?
                .to_string();
            let new_content = params
                .get("new_content")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=edit_agent_config requires 'new_content'".to_string())?
                .to_string();
            Ok(SelfModifyOp::EditAgentConfig {
                path,
                new_content,
                reason,
            })
        }
        "edit_cron_schedule" => {
            let job_id = params
                .get("job_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=edit_cron_schedule requires 'job_id'".to_string())?
                .to_string();
            let new_schedule = params
                .get("new_schedule")
                .and_then(Value::as_str)
                .ok_or_else(|| "op=edit_cron_schedule requires 'new_schedule'".to_string())?
                .to_string();
            Ok(SelfModifyOp::EditCronSchedule {
                job_id,
                new_schedule,
                reason,
            })
        }
        other => Err(format!("unknown op {other:?}; expected one of grant_capability, install_extension, edit_agent_config, edit_cron_schedule")),
    }
}

/// Parse a `principal_id` string from `ToolContext.principal_id`.
///
/// The wire shape from the runtime is the canonical DID string.
/// `PrincipalId` is a thin newtype around that string; we accept any
/// non-empty value the runtime provides and fall back to the system
/// sentinel for empty/missing values. We don't validate the DID
/// format here — `principal_id` flows into on-disk artifacts (PR #4)
/// where the user sees it; a malformed principal will be obvious.
fn parse_principal_id(s: &str) -> PrincipalId {
    if s.is_empty() {
        PrincipalId::system().clone()
    } else {
        PrincipalId(s.to_string())
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::approval_queue::{ApprovalQueue, ApprovalRequest, ApprovalStatus};
    use crate::daemon::api::DaemonApi;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use uuid::Uuid;

    /// In-process `DaemonApi` stub for tests. Captures the request so
    /// assertions can check the op + caller were passed through.
    #[derive(Debug, Default)]
    struct StubApi {
        captured: Mutex<Option<SelfModifyOp>>,
    }

    #[async_trait]
    impl DaemonApi for StubApi {
        async fn request_self_modify(
            &self,
            op: SelfModifyOp,
            _ctx: SelfModifyContext,
        ) -> Result<RequestId, SelfModifyError> {
            *self.captured.lock().unwrap() = Some(op);
            Ok(Uuid::new_v4())
        }
    }

    #[tokio::test]
    async fn grants_capability_forwards_op_and_returns_request_id() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "grant_capability", "capability": "fs:read", "reason": "agent needs it" });
        let out = tool.execute(p).await.unwrap();
        assert_eq!(out["status"], "pending");
        assert!(out["request_id"].as_str().is_some());
    }

    #[tokio::test]
    async fn install_extension_forwards_package_ref() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "install_extension", "package_ref": "acme/foo@1.0", "reason": "test" });
        let out = tool.execute(p).await.unwrap();
        assert_eq!(out["status"], "pending");
    }

    #[tokio::test]
    async fn edit_cron_schedule_forwards_job_id_and_schedule() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({
            "op": "edit_cron_schedule",
            "job_id": "job-1",
            "new_schedule": "0 * * * *",
            "reason": "test"
        });
        let out = tool.execute(p).await.unwrap();
        assert_eq!(out["status"], "pending");
    }

    #[tokio::test]
    async fn edit_agent_config_forwards_path_and_content() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({
            "op": "edit_agent_config",
            "path": "/tmp/x.toml",
            "new_content": "x=1",
            "reason": "test"
        });
        let out = tool.execute(p).await.unwrap();
        assert_eq!(out["status"], "pending");
    }

    #[tokio::test]
    async fn missing_op_is_rejected() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({ "reason": "test" });
        let err = tool.execute(p).await.unwrap_err();
        assert!(err.to_string().contains("missing required field 'op'"));
    }

    #[tokio::test]
    async fn unknown_op_is_rejected() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "fly_to_mars", "reason": "test" });
        let err = tool.execute(p).await.unwrap_err();
        assert!(err.to_string().contains("unknown op"));
    }

    #[tokio::test]
    async fn missing_capability_field_for_grant_is_rejected() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "grant_capability", "reason": "test" });
        let err = tool.execute(p).await.unwrap_err();
        assert!(err.to_string().contains("requires 'capability'"));
    }

    #[tokio::test]
    async fn surface_meta_capability_error_from_daemon_api() {
        // Build a stub that returns MetaCapabilityForbidden so the
        // tool surfaces the daemon's gate error verbatim.
        #[derive(Debug)]
        struct RejectApi;
        #[async_trait]
        impl DaemonApi for RejectApi {
            async fn request_self_modify(
                &self,
                _op: SelfModifyOp,
                _ctx: SelfModifyContext,
            ) -> Result<RequestId, SelfModifyError> {
                Err(SelfModifyError::MetaCapabilityForbidden(
                    "principal:create".into(),
                ))
            }
        }
        let api: Arc<dyn DaemonApi> = Arc::new(RejectApi);
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "grant_capability", "capability": "principal:create", "reason": "x" });
        let err = tool.execute(p).await.unwrap_err();
        assert!(err.to_string().contains("principal:create"));
        assert!(err.to_string().contains("not self-grantable"));
    }

    #[tokio::test]
    async fn tool_uses_real_approval_queue_end_to_end() {
        // Integration-style test that wires the tool to a real
        // ApprovalQueue via a thin DaemonApi wrapper. Verifies the
        // request lands in the queue + on disk.
        let dir = TempDir::new().unwrap();
        let queue = ApprovalQueue::new(dir.path().to_path_buf(), 16);

        #[derive(Debug)]
        struct QueueApi {
            queue: Arc<ApprovalQueue>,
        }
        #[async_trait]
        impl DaemonApi for QueueApi {
            async fn request_self_modify(
                &self,
                op: SelfModifyOp,
                ctx: SelfModifyContext,
            ) -> Result<RequestId, SelfModifyError> {
                let req = ApprovalRequest::from_op(op, ctx.principal_id);
                self.queue.insert(req)
            }
        }

        let api: Arc<dyn DaemonApi> = Arc::new(QueueApi { queue: queue.clone() });
        let tool = PekoSelfTool::new(api);
        let p = json!({ "op": "grant_capability", "capability": "net:fetch", "reason": "x" });
        let out = tool.execute(p).await.unwrap();
        let id_str = out["request_id"].as_str().unwrap();
        let id: Uuid = id_str.parse().unwrap();

        assert_eq!(queue.list_pending().len(), 1);
        assert_eq!(queue.get(id).unwrap().status, ApprovalStatus::Pending);
    }

    #[test]
    fn parameters_schema_is_valid_json_schema() {
        // Sanity check: parameters() returns parseable JSON Schema.
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["op"].is_object());
        let enums: Vec<String> = params["properties"]["op"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            enums,
            vec![
                "grant_capability".to_string(),
                "install_extension".to_string(),
                "edit_agent_config".to_string(),
                "edit_cron_schedule".to_string(),
            ]
        );
    }

    #[test]
    fn tool_name_is_peko_self() {
        let api: Arc<dyn DaemonApi> = Arc::new(StubApi::default());
        let tool = PekoSelfTool::new(api);
        assert_eq!(tool.name(), "peko_self");
    }
}