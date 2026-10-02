//! `ToolDispatcher` — the single tool-execution point (ADR-066 D2).
//!
//! One function executes a tool call: builds the `ToolContext`
//! (principal/session/agent identity, abort receiver), applies the
//! execution timeout and panic isolation + background detach (the
//! behavior kept from `transport::async_router`), bridges abort
//! signals, fires the observe-only `PreToolUse` / `PostToolUse` hooks
//! through the workspace dispatcher, and emits the `tool.call` audit event.
//!
//! Replaces the P2 stack (`BuiltinExecuteHandler` + `WorkspaceHookDispatcher`
//! priority dispatch + companion-hook codegen) for the execution path:
//! tools dispatch straight from the [`ToolCatalog`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use futures::FutureExt;
use serde_json::Value;
use tracing::{debug, warn};

use peko_extension_api::ToolCallSpec;
use peko_tools_core::ToolInterruptNotice;

use crate::extensions::framework::transport::async_router::{
    AsyncExecutionRouter, ToolExecutionContext,
};
use crate::extensions::framework::types::HookInput;
use crate::extensions::workspace_dispatcher::WorkspaceHookDispatcher;
use crate::extensions::workspace_dispatcher::WorkspaceHookPoint;
use crate::tools::catalog::ToolCatalog;

/// The single tool-execution point. Cheap to clone (every field is an
/// `Arc`).
#[derive(Clone)]
pub struct ToolDispatcher {
    catalog: Arc<ToolCatalog>,
    hooks: Arc<WorkspaceHookDispatcher>,
    router: Arc<AsyncExecutionRouter>,
    audit: Option<Arc<peko_observability::Observability>>,
}

impl std::fmt::Debug for ToolDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolDispatcher")
            .field("has_audit_sink", &self.audit.is_some())
            .finish()
    }
}

impl ToolDispatcher {
    /// Compose the dispatcher over the catalog + hook registry + the
    /// timeout/detach router. `audit` is the observability hub the
    /// `tool.call` event lands on (`None` for tests / standalone
    /// contexts — execution is unaffected).
    #[must_use]
    pub fn new(
        catalog: Arc<ToolCatalog>,
        hooks: Arc<WorkspaceHookDispatcher>,
        router: Arc<AsyncExecutionRouter>,
        audit: Option<Arc<peko_observability::Observability>>,
    ) -> Self {
        Self {
            catalog,
            hooks,
            router,
            audit,
        }
    }

    /// The catalog this dispatcher executes against.
    #[must_use]
    pub fn catalog(&self) -> &Arc<ToolCatalog> {
        &self.catalog
    }

    /// The timeout/detach router (for `wait_for_async_tasks`-style
    /// drains at shutdown).
    #[must_use]
    pub fn router(&self) -> &Arc<AsyncExecutionRouter> {
        &self.router
    }

    /// Execute one tool call. Returns the `(display, json, success)`
    /// triplet: an unknown tool, a schema violation, and a tool error
    /// all arrive as `success: false` data, never a transport error;
    /// only dispatcher-internal failures produce `Err`.
    pub async fn execute(&self, call: ToolCallSpec) -> Result<(String, Value, bool)> {
        let start = Instant::now();
        self.fire_observe(
            WorkspaceHookPoint::PreToolUse {
                tool_name: Some(call.tool_name.clone()),
            },
            &call,
        )
        .await;
        let result = self.execute_inner(&call).await;
        self.fire_observe(
            WorkspaceHookPoint::PostToolUse {
                tool_name: Some(call.tool_name.clone()),
            },
            &call,
        )
        .await;
        let success = result.as_ref().is_ok_and(|(_, _, success)| *success);
        self.emit_audit(&call, success, start.elapsed()).await;
        result
    }

    async fn execute_inner(&self, call: &ToolCallSpec) -> Result<(String, Value, bool)> {
        let tool_name = call.tool_name.clone();
        let principal_id = call
            .principal_id
            .as_deref()
            .map(|id| peko_subject::PrincipalId(id.to_string()))
            .unwrap_or_else(|| peko_subject::PrincipalId::system().clone());

        // 1. Resolve the tool from the catalog (presence =
        //    executability — ADR-066 D1).
        let Some((tool, metadata)) = self.catalog.get(&tool_name, &principal_id).await else {
            let text = format!("Tool '{tool_name}' not available");
            return Ok((text.clone(), Value::String(text), false));
        };

        // 2. F32b — validate LLM-emitted args against the tool's
        //    declared JSON Schema before any preprocessing.
        if let Err(msg) =
            AsyncExecutionRouter::validate_tool_args(&metadata.parameters, &call.params, &tool_name)
        {
            warn!(tool = %tool_name, "Tool arg validation failed; returning as tool failure");
            let text = format!("Error: {msg}");
            return Ok((text.clone(), Value::String(text), false));
        }

        // 4. Build the ToolContext once so the cancel watcher and the
        //    exec closure share the same abort receiver / identity
        //    fields.
        let base_ctx = peko_tools_core::ToolContext::for_hook_run("hook_run", "hook", &tool_name)
            .with_agent_id(call.agent_id.clone().unwrap_or_default())
            .with_session_id(call.session_id.clone().unwrap_or_default())
            .with_workspace(call.workspace.clone().unwrap_or_default())
            .with_principal_id(call.principal_id.clone().unwrap_or_default())
            .with_principal_name(call.principal_name.clone().unwrap_or_default());
        let tool_ctx = match call.abort_signal.as_ref() {
            Some(rx) => base_ctx.with_abort_signal(rx.clone()),
            None => base_ctx,
        };

        // Cancel watcher: if the abort signal fires, invoke
        // `on_interrupt` and stash the notice — the framework always
        // emits a notice on cancel, even for tools that do not
        // implement `InterruptibleTool` (the blanket impl supplies a
        // soft default).
        let cancel_fired = Arc::new(AtomicBool::new(false));
        let notice_slot: Arc<tokio::sync::Mutex<Option<ToolInterruptNotice>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let interrupt_watch = if let Some(mut rx) = call.abort_signal.clone() {
            let cancel_fired_w = cancel_fired.clone();
            let notice_slot_w = notice_slot.clone();
            let tool_w = tool.clone();
            let tool_ctx_w = tool_ctx.clone();
            Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(
                async move {
                    if *rx.borrow() {
                        // Already aborted before we started watching.
                    } else if rx.changed().await.is_err() || !*rx.borrow() {
                        // Sender dropped without a flip, or flipped to false
                        // — do nothing.
                        return;
                    }
                    cancel_fired_w.store(true, Ordering::SeqCst);
                    let notice = tool_w.on_interrupt("", &tool_ctx_w).await;
                    *notice_slot_w.lock().await = Some(notice);
                },
            )))
        } else {
            None
        };

        // 5. Route through the timeout/detach machinery.
        let tool_exec_ctx = ToolExecutionContext::new(
            call.agent_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            call.session_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            "hook_run".to_string(),
        )
        .with_workspace(call.workspace.clone().unwrap_or_else(|| ".".to_string()))
        .with_principal_id(call.principal_id.clone());

        let mut params = call.params.clone();
        apply_workspace_injection(&mut params, &tool_name, call.workspace.as_deref());

        let result = self
            .router
            .route(&tool_name, &mut params, &tool_exec_ctx, move |p| {
                let tool = tool.clone();
                let tool_ctx = tool_ctx.clone();
                async move {
                    let _interrupt_watch = interrupt_watch;
                    std::panic::AssertUnwindSafe(tool.execute_with_context(p, &tool_ctx))
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Tool panicked during execution")))
                }
            })
            .await;

        // 6. Cancel wins over a natural completion.
        let (text, json, success) = if cancel_fired.load(Ordering::SeqCst)
            || call.abort_signal.as_ref().is_some_and(|rx| *rx.borrow())
        {
            let notice = notice_slot
                .lock()
                .await
                .take()
                .unwrap_or_else(|| ToolInterruptNotice::soft_default("", &tool_name));
            let text = notice.to_tool_result_text();
            (text.clone(), Value::String(text), true)
        } else {
            match result {
                Ok(value) => {
                    let text = match &value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (text, value, true)
                }
                Err(e) => {
                    let text = format!("Error: {e}");
                    (text.clone(), Value::String(text), false)
                }
            }
        };

        Ok((text, json, success))
    }

    /// Fire an observe-only hook point for the call (PreToolUse /
    /// PostToolUse). The result is discarded by design; the 2s
    /// soft-fail budget applies to each handler in `WorkspaceHookDispatcher`.
    async fn fire_observe(&self, point: WorkspaceHookPoint, call: &ToolCallSpec) {
        let input = HookInput::ToolCall {
            tool_name: call.tool_name.clone(),
            params: call.params.clone(),
            workspace: call.workspace.clone(),
            agent_id: call.agent_id.clone(),
            session_id: call.session_id.clone(),
            caller_id: call.caller_id.clone(),
            principal_id: call.principal_id.clone(),
            principal_name: call.principal_name.clone(),
            abort_signal: None,
        };
        let _ = self.hooks.invoke_hook(point, input).await;
    }

    /// Emit the `tool.call` audit event (Info severity). Tool name,
    /// principal, session, caller, a digest of the params (never the
    /// params themselves), success, and duration.
    async fn emit_audit(&self, call: &ToolCallSpec, success: bool, elapsed: std::time::Duration) {
        let Some(audit) = self.audit.as_ref() else {
            return;
        };
        let params_digest = {
            use sha2::{Digest, Sha256};
            let canonical = call.params.to_string();
            let digest = Sha256::digest(canonical.as_bytes());
            format!("sha256:{}", hex_encode(&digest))
        };
        let caller = call.caller_id.as_deref().map(|id| {
            id.parse::<peko_subject::Subject>()
                .unwrap_or_else(|_| peko_subject::Subject::User(id.to_string()))
        });
        let result = audit
            .audit_with_caller(
                caller.as_ref(),
                "tool.call",
                call.agent_id.as_deref(),
                serde_json::json!({
                    "tool_name": call.tool_name,
                    "agent_id": call.agent_id,
                    "principal_id": call.principal_id,
                    "principal_name": call.principal_name,
                    "session_id": call.session_id,
                    "caller_id": call.caller_id,
                    "params_digest": params_digest,
                    "success": success,
                    "duration_ms": elapsed.as_millis() as u64,
                }),
            )
            .await;
        if let Err(e) = result {
            debug!("failed to emit tool.call audit event: {e}");
        }
    }
}

/// Hex-encode bytes (lowercase, no separators).
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            write!(out, "{b:02x}").expect("writing to String cannot fail");
            out
        })
}

/// Inject the workspace into filesystem-tool params and give `Agent`
/// the longer default timeout — the live behavior of the P2
/// `BuiltinExecuteHandler` preprocessor.
fn apply_workspace_injection(params: &mut Value, tool_name: &str, workspace: Option<&str>) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    // Subagent spawn inherently takes longer than simple tools because
    // the subagent runs a full agentic loop with its own LLM calls.
    // Inject a longer default timeout for blocking Agent if none
    // is provided by the caller.
    if tool_name == "Agent" && !obj.contains_key("_timeout") {
        obj.insert("_timeout".to_string(), Value::Number(300.into()));
    }

    // Inject agent workspace into tool parameters for filesystem tools.
    if let Some(ws) = workspace {
        match tool_name {
            "Glob" => {
                if !obj.contains_key("directory") {
                    obj.insert("directory".to_string(), Value::String(ws.to_string()));
                }
            }
            "Grep" => {
                if !obj.contains_key("path") {
                    obj.insert("path".to_string(), Value::String(ws.to_string()));
                }
            }
            "Bash" => {
                if !obj.contains_key("cwd") {
                    obj.insert("cwd".to_string(), Value::String(ws.to_string()));
                }
            }
            "Write" | "Edit" | "Read" => {
                if let Some(path_str) = obj.get("file_path").and_then(|v| v.as_str()) {
                    let path_buf = std::path::PathBuf::from(path_str);
                    if !path_buf.is_absolute() {
                        let resolved = std::path::PathBuf::from(ws).join(path_str);
                        obj.insert(
                            "file_path".to_string(),
                            Value::String(resolved.to_string_lossy().to_string()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::framework::types::ToolSource;
    use peko_tools_core::{Tool, ToolContext};

    struct ProbeTool;
    #[async_trait::async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            "Read"
        }
        fn description(&self) -> String {
            "dispatcher probe".into()
        }
        fn parameters(&self) -> Value {
            serde_json::json!({"type":"object","properties":{"file_path":{"type":"string"},"panic":{"type":"boolean"}},"required":["file_path"]})
        }
        async fn execute(&self, _: Value) -> Result<Value> {
            unreachable!("context required")
        }
        async fn execute_with_context(&self, params: Value, ctx: &ToolContext) -> Result<Value> {
            assert_eq!(ctx.agent_id.as_deref(), Some("did:agent"));
            assert_eq!(ctx.session_id.as_deref(), Some("session"));
            assert_eq!(ctx.principal_id.as_deref(), Some("principal"));
            assert!(params["panic"] != true, "fixture panic");
            Ok(params)
        }
    }

    #[tokio::test]
    async fn attributed_dispatch_audits_success_validation_unknown_and_panic_once() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Arc::new(
            peko_observability::Observability::with_audit_dir("test", dir.path().to_path_buf())
                .unwrap(),
        );
        let catalog = Arc::new(ToolCatalog::new());
        catalog
            .register_system(Arc::new(ProbeTool), ToolSource::BuiltIn)
            .await;
        let dispatcher = ToolDispatcher::new(
            catalog,
            Arc::new(WorkspaceHookDispatcher::new()),
            Arc::new(AsyncExecutionRouter::new()),
            Some(Arc::clone(&audit)),
        );
        for (name, params, expected) in [
            ("Read", serde_json::json!({"file_path":"secret"}), true),
            ("Read", serde_json::json!({}), false),
            ("Unknown", serde_json::json!({}), false),
            (
                "Read",
                serde_json::json!({"file_path":"secret","panic":true}),
                false,
            ),
        ] {
            let mut call = ToolCallSpec::new(name, params);
            call.agent_id = Some("did:agent".into());
            call.session_id = Some("session".into());
            call.principal_id = Some("principal".into());
            call.caller_id = Some("user:alice".into());
            call.workspace = Some("/tmp/workspace".into());
            let (_, result, success) = dispatcher.execute(call).await.unwrap();
            assert_eq!(success, expected);
            if success {
                assert_eq!(result["file_path"], "/tmp/workspace/secret");
            }
        }
        let events = audit.get_audit_log(10).await;
        assert_eq!(events.len(), 4);
        for event in &events {
            assert_eq!(event.event_type, "tool.call");
            assert_eq!(event.agent_did.as_deref(), Some("did:agent"));
            assert_eq!(event.details["principal_id"], "principal");
            assert_eq!(event.details["session_id"], "session");
            assert_eq!(event.details["caller_id"], "user:alice");
            assert!(!event.details.to_string().contains("secret"));
            assert_eq!(event.details["params_digest"].as_str().unwrap().len(), 71);
        }
        let contents: String = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|file| std::fs::read_to_string(file.unwrap().path()).unwrap())
            .collect();
        assert_eq!(
            contents.lines().count(),
            4,
            "each dispatch is durably audited once"
        );
    }
}
