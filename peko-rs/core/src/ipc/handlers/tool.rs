//! `tool` domain request handler (F6 step 3 / F8 completion).
//!
//! Owns the daemon-side tool execution IPC variants: `AsyncSpawn`,
//! `AsyncCancel`, and `ExecuteTool` (ADR-061 phase 1 — the synchronous
//! workflow callback). The handler holds a narrow [`ToolHost`]
//! port; the daemon-side implementation (`AppState`) is reached only
//! through the trait, so this module never imports
//! `crate::daemon::state::AppState` directly.
//!
//! Boundary rules:
//! - Dependency inversion: the consumer (`ipc::handlers::tool`) defines
//!   the [`ToolHost`] trait; the producer (`daemon::state`) implements
//!   it (same pattern as `SystemHost` and `AuthHost`).
//! - F6: this module must not import any other `ipc::handlers::*` module.
//!
//! Security (ADR-042 + F8 invariant): capability grants passed to
//! `ToolRuntime::execute_tool_with_workspace` are **always** derived
//! server-side from the session's owning Principal. They are never
//! accepted from the IPC packet itself — that would be privilege
//! escalation. The resolution chain is:
//!
//!   `parse_session_key()` → `principal_manager.get_by_name(parts.agent)`
//!   → `Principal.capabilities().to_strings()` (+ active extensions
//!   from the daemon-side `ExtensionStore`).
//!
//! If the principal cannot be resolved (e.g. an unknown session key
//! raced the principal reload, or the daemon hasn't finished warming
//! up), the handler **fails closed** — it falls back to `None, None`,
//! matching the system-wide invariant that an empty grant set denies
//! capability-gated tools (`engine::tool_runtime`,
//! `framework::core::tool_registry`, `agents::agent`,
//! `engine::agentic_loop` all treat `None`/empty as deny-all). The
//! `*_fail_closed_without_principal_id` gate tests pin this contract.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::engine::tool_runtime::ToolRuntime;
use crate::extensions::framework::async_exec::executor::{
    AsyncExecutor, AsyncTaskId, AsyncToolConfig,
};
use crate::extensions::framework::store::ExtensionStore;
use crate::ipc::handlers::RequestHandler;
use crate::ipc::packet::{RequestPacket, ResponsePacket};
use crate::ipc::response_sink::ResponseSink;
use crate::ipc::send_response::send_response;
use crate::ipc::server::PeerAddr;
use crate::principal::manager::PrincipalManager;
use peko_auth::caller::CallerContext;
use peko_session::key::parse_session_key;

/// Narrow port the `tool` handler uses to reach daemon state.
///
/// `AppState` is the sole implementor. All methods are sync: they
/// return cheap references / `Arc` clones, so the trait is trivially
/// object-safe and the handler pays no `async_trait` overhead. The
/// per-request grant resolution (the `async` part of the F8 invariant)
/// lives in the handler and uses these accessors to do its own awaits.
pub(crate) trait ToolHost: Send + Sync {
    /// Principal manager used to resolve the session's owning
    /// principal (ADR-042).
    fn principal_manager(&self) -> &Arc<PrincipalManager>;

    /// Extension store used to source the principal's active extension
    /// IDs for capability-gated tools.
    fn extension_store(&self) -> &Arc<ExtensionStore>;

    /// Async tool runtime used to actually execute the spawned task.
    fn tool_runtime(&self) -> Arc<ToolRuntime>;

    /// Async task executor that owns the spawned task's lifecycle
    /// (cancellation, completion delivery).
    fn async_task_executor(&self) -> Arc<AsyncExecutor>;
}

/// `tool` domain request handler. Constructed with an `Arc<dyn ToolHost>`
/// (typically `Arc::new(app_state.clone())` from the dispatcher).
pub(crate) struct ToolHandler {
    host: Arc<dyn ToolHost>,
}

impl ToolHandler {
    pub(crate) fn new(host: Arc<dyn ToolHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl RequestHandler for ToolHandler {
    fn domain(&self) -> &'static str {
        "tool"
    }

    fn matches(&self, request: &RequestPacket) -> bool {
        matches!(
            request,
            RequestPacket::AsyncSpawn { .. }
                | RequestPacket::AsyncCancel { .. }
                | RequestPacket::ExecuteTool { .. }
        )
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::AsyncSpawn {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
            } => {
                self.handle_async_spawn(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    sink,
                )
                .await?;
            }

            RequestPacket::ExecuteTool {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
            } => {
                self.handle_execute_tool(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    sink,
                )
                .await?;
            }

            RequestPacket::AsyncCancel {
                request_id,
                task_id,
            } => {
                self.handle_async_cancel(request_id, task_id, sink).await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ToolHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

impl ToolHandler {
    /// Resolve the owning principal's capability grants and active
    /// extension set server-side from a session key (ADR-042 + F8 —
    /// see the module-level doc for the full chain). Both `AsyncSpawn`
    /// and `ExecuteTool` share this attribution path (ADR-061).
    ///
    /// The principal name is the session's `agent` segment per ADR-042;
    /// `principal_manager` never returns a stale entry — it holds
    /// `Arc<Principal>` and reloads are coordinated by
    /// `PrincipalManager` itself. From the resolved principal we derive
    /// BOTH the granted capability set (`capabilities().to_strings()`)
    /// and the principal's active extension set, built by
    /// `PrincipalCatalog::build` from the principal's capabilities +
    /// `agent_prompts` and the daemon-wide `ExtensionStore::global_items()`.
    /// This is the same path `PrincipalManager::receive` uses when an
    /// agent session boots, so capability-gated tools see the identical
    /// enable set whether they were spawned by chat traffic or by this
    /// IPC path.
    ///
    /// Fail-closed: any resolution failure returns `(None, None)` —
    /// deny-all downstream — and warns so the mismatch is visible in
    /// tracing without leaking data to the caller. `caller` labels the
    /// warn line with the owning variant ("AsyncSpawn" / "ExecuteTool").
    async fn resolve_session_grants(
        &self,
        session_key: &str,
        caller: &'static str,
    ) -> (Option<Vec<String>>, Option<Vec<String>>) {
        let parts = parse_session_key(session_key);
        let principal_agent = parts.agent;

        let principal = self
            .host
            .principal_manager()
            .get_by_name(principal_agent)
            .await;

        match principal {
            Some(principal) => {
                let caps = principal.capabilities().await;
                let global_items = self.host.extension_store().global_items().await;
                let catalog = crate::principal::catalog::PrincipalCatalog::build(
                    &principal.workspace_path,
                    &caps,
                    &principal.agent_prompts,
                    &global_items,
                );
                (
                    Some(caps.to_strings()),
                    Some(catalog.active_extensions().to_vec()),
                )
            }
            None => {
                warn!(
                    "{caller} session_key={session_key} resolved to unknown principal \
                     '{principal_agent}'; falling back to fail-closed (no grants)",
                );
                (None, None)
            }
        }
    }

    /// Spawn an async tool task.
    ///
    /// Capability grants are derived server-side from the session's
    /// owning Principal (ADR-042). See the module-level doc for the
    /// resolution chain. If the principal cannot be resolved, we fall
    /// back to `None, None` (fail-closed) and log a warning so the
    /// mismatch is visible in tracing without leaking data to the
    /// caller.
    async fn handle_async_spawn(
        &self,
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let (resolved_capabilities, resolved_active_extensions) = self
            .resolve_session_grants(&session_key, "AsyncSpawn")
            .await;

        let tool_runtime = self.host.tool_runtime();
        let executor = self.host.async_task_executor();

        let config = AsyncToolConfig::default();
        let task_id = AsyncTaskId::new();

        let receipt = executor
            .execute(
                task_id,
                tool_name.clone(),
                params.clone(),
                session_key,
                config,
                move || {
                    let runtime = tool_runtime.clone();
                    let ws = workspace.clone();
                    let name = tool_name.clone();
                    let p = params.clone();
                    // Move the resolved grants into the closure so the
                    // executor owns the only copy (`'static` + `Send`).
                    let grants = resolved_capabilities;
                    let exts = resolved_active_extensions;
                    Box::pin(async move {
                        // `grants`/`exts` are `Option<Vec<String>>` —
                        // server-derived, never packet-supplied. On
                        // resolution failure they are `None`, which the
                        // tool runtime treats as deny-all (fail-closed).
                        runtime
                            .execute_tool_with_workspace(&name, p, &ws, grants, exts)
                            .await
                    })
                },
            )
            .await?;

        let response = ResponsePacket::AsyncReceipt {
            request_id,
            receipt,
        };
        send_response(sink, response).await?;

        Ok(())
    }

    /// Execute a tool synchronously and return the result (ADR-061
    /// phase 1). Unlike `AsyncSpawn` there is no executor handoff: the
    /// call awaits the tool inline (the tool's own context timeout
    /// applies, same as the agentic-loop path) and replies with a
    /// `ToolExecuted` packet carrying the funnel's
    /// `(content, result, success)` triplet. Gate denials and tool
    /// errors arrive as `success: false` data, not a transport error;
    /// only a funnel-internal failure produces `ResponsePacket::Error`.
    async fn handle_execute_tool(
        &self,
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let (resolved_capabilities, resolved_active_extensions) = self
            .resolve_session_grants(&session_key, "ExecuteTool")
            .await;

        let tool_runtime = self.host.tool_runtime();
        let triplet = tool_runtime
            .execute_tool_full_with_workspace(
                &tool_name,
                params,
                &workspace,
                resolved_capabilities,
                resolved_active_extensions,
            )
            .await;

        let response = match triplet {
            Ok((content, result, success)) => {
                tool_executed_packet(request_id, content, result, success)
            }
            Err(e) => ResponsePacket::Error {
                request_id,
                message: format!("ExecuteTool {tool_name} failed: {e}"),
            },
        };
        send_response(sink, response).await?;

        Ok(())
    }

    /// Cancel a running async task by id.
    async fn handle_async_cancel(
        &self,
        request_id: u64,
        task_id: String,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        let executor = self.host.async_task_executor();
        let cancelled = executor.cancel(&task_id).await.unwrap_or(false);

        let response = ResponsePacket::Done {
            request_id,
            success: cancelled,
            error: if cancelled {
                None
            } else {
                Some(format!("Task {task_id} not found or already completed"))
            },
        };
        send_response(sink, response).await?;

        Ok(())
    }
}

/// Marker appended to `ToolExecuted::content` when the payload had to
/// be clipped to fit the datagram budget.
const TRUNCATION_MARKER: &str = "\n\n[truncated by peko: result exceeded the IPC packet budget]";

/// Build a `ToolExecuted` packet that fits the datagram budget —
/// `ResponsePacket::to_bytes` refuses payloads over `MAX_PACKET_SIZE`,
/// and an unbounded tool result (e.g. a `Bash` stdout tail) would
/// otherwise kill the response entirely. On overflow the structured
/// `result` is dropped to null and `content` is clipped until the
/// packet fits, with [`TRUNCATION_MARKER`] appended and
/// `truncated: true` set. ADR-061's spill-to-workspace-file is a
/// phase-2 refinement; the spike ships marked truncation only.
fn tool_executed_packet(
    request_id: u64,
    content: String,
    result: serde_json::Value,
    success: bool,
) -> ResponsePacket {
    let untruncated = ResponsePacket::ToolExecuted {
        request_id,
        content: content.clone(),
        result,
        success,
        truncated: false,
    };
    if untruncated.to_bytes().is_ok() {
        return untruncated;
    }

    // JSON string escaping can inflate one character to 6 bytes, so no
    // fixed fraction of the budget is guaranteed to fit — halve the
    // retained prefix until the packet serializes under the cap.
    let mut keep = content.chars().count();
    loop {
        let clipped: String = content.chars().take(keep).collect();
        let packet = ResponsePacket::ToolExecuted {
            request_id,
            content: format!("{clipped}{TRUNCATION_MARKER}"),
            result: serde_json::Value::Null,
            success,
            truncated: true,
        };
        if keep == 0 || packet.to_bytes().is_ok() {
            return packet;
        }
        keep /= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::paths::PathResolver;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use peko_auth::Subject;
    use peko_extension_api::Capabilities;
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Minimal `ToolHost` double over real components: a
    /// `PrincipalManager` with one created principal, an empty
    /// `ExtensionStore`, a `ToolRuntime` with the built-in tools
    /// registered, and a standalone `AsyncExecutor`. Mirrors the
    /// `principal_log` fixture in `ipc::handlers::principal`.
    struct TestToolHost {
        manager: Arc<PrincipalManager>,
        extension_store: Arc<ExtensionStore>,
        tool_runtime: Arc<ToolRuntime>,
        executor: Arc<AsyncExecutor>,
    }

    impl ToolHost for TestToolHost {
        fn principal_manager(&self) -> &Arc<PrincipalManager> {
            &self.manager
        }
        fn extension_store(&self) -> &Arc<ExtensionStore> {
            &self.extension_store
        }
        fn tool_runtime(&self) -> Arc<ToolRuntime> {
            self.tool_runtime.clone()
        }
        fn async_task_executor(&self) -> Arc<AsyncExecutor> {
            self.executor.clone()
        }
    }

    #[derive(Default)]
    struct CollectSink {
        seen: Mutex<Vec<ResponsePacket>>,
    }

    #[async_trait]
    impl ResponseSink for CollectSink {
        async fn send_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
            let packet: ResponsePacket = serde_json::from_slice(bytes)
                .map_err(|e| std::io::Error::other(format!("decode: {e}")))?;
            self.seen.lock().unwrap().push(packet);
            Ok(())
        }
    }

    struct Fixture {
        _temp: TempDir,
        handler: ToolHandler,
        workspace: PathBuf,
    }

    fn test_principal_config(
        name: &str,
        capabilities: Capabilities,
    ) -> crate::principal::PrincipalConfig {
        crate::principal::PrincipalConfig {
            name: name.to_string(),
            id: None,
            did: None,
            owner: Subject::User("test-owner".to_string()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities,
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![],
            preferred_model_id: Some("mock".to_string()),
            quota: None,
            children: Default::default(),
        }
    }

    async fn fixture(name: &str, capabilities: Capabilities) -> Fixture {
        let temp = TempDir::new().expect("temp dir");
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let manager = Arc::new(PrincipalManager::with_path_resolver(
            path_resolver.clone(),
            Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        ));
        manager
            .create(test_principal_config(name, capabilities))
            .await
            .expect("create principal");

        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let tool_runtime = ToolRuntime::with_workspace(path_resolver, &workspace)
            .await
            .expect("tool runtime");

        let host = TestToolHost {
            manager,
            extension_store: Arc::new(ExtensionStore::new()),
            tool_runtime: Arc::new(tool_runtime),
            executor: Arc::new(AsyncExecutor::new(
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )),
        };
        Fixture {
            _temp: temp,
            handler: ToolHandler::new(Arc::new(host)),
            workspace,
        }
    }

    /// Drive one `ExecuteTool` request through the handler and return
    /// the single response packet it emitted.
    async fn execute_tool(
        handler: &ToolHandler,
        request_id: u64,
        tool_name: &str,
        params: serde_json::Value,
        session_key: &str,
        workspace: &std::path::Path,
    ) -> ResponsePacket {
        let sink = CollectSink::default();
        let caller = CallerContext::local();
        let peer = PeerAddr::Ip("127.0.0.1:11435".parse().unwrap());
        handler
            .handle(
                RequestPacket::ExecuteTool {
                    request_id,
                    tool_name: tool_name.to_string(),
                    params,
                    session_key: session_key.to_string(),
                    workspace: workspace.to_path_buf(),
                },
                &caller,
                &sink,
                &peer,
            )
            .await
            .expect("handle");
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "exactly one response packet");
        seen[0].clone()
    }

    /// A valid `session_key` resolves the owning principal server-side
    /// and the tool runs under its grants (the starter bundle's
    /// `tool:*` wildcard).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_executes_with_resolved_principal_capabilities() {
        let fx = fixture("attributed", Capabilities::starter_bundle()).await;
        std::fs::write(fx.workspace.join("hello.txt"), "hi").expect("seed file");

        let response = execute_tool(
            &fx.handler,
            1,
            "Glob",
            json!({"pattern": "*.txt", "path": fx.workspace.to_string_lossy()}),
            "agent:attributed:cli:default",
            &fx.workspace,
        )
        .await;

        let ResponsePacket::ToolExecuted {
            content,
            success,
            truncated,
            ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "tool:* grant should pass the gate: {content}");
        assert!(content.contains("hello.txt"), "content: {content}");
        assert!(!truncated);
    }

    /// An unknown `session_key` resolves to no principal; the handler
    /// fails closed to deny-all and the tool never runs (no side
    /// effects).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_unknown_session_key_fails_closed() {
        let fx = fixture("known", Capabilities::starter_bundle()).await;
        let marker = fx.workspace.join("should_not_exist");

        let response = execute_tool(
            &fx.handler,
            2,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            "agent:ghost:cli:default",
            &fx.workspace,
        )
        .await;

        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(
            !success,
            "fail-closed: unknown principal must not execute: {content}"
        );
        assert!(
            content.contains("currently disabled"),
            "deny-all gate message expected, got: {content}"
        );
        assert!(!marker.exists(), "denied tool must not have run");
    }

    /// A resolved principal without a `tool:Bash` grant is refused by
    /// the capability gate; its granted tools still run.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_denied_when_principal_lacks_grant() {
        let fx = fixture("limited", Capabilities::with_grants(["tool:Glob"])).await;
        let marker = fx.workspace.join("should_not_exist");

        let response = execute_tool(
            &fx.handler,
            3,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            "agent:limited:cli:default",
            &fx.workspace,
        )
        .await;

        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(!success, "missing tool:Bash grant must deny: {content}");
        assert!(
            content.contains("currently disabled"),
            "capability-gate message expected, got: {content}"
        );
        assert!(!marker.exists(), "denied tool must not have run");

        let response = execute_tool(
            &fx.handler,
            4,
            "Glob",
            json!({"pattern": "*", "path": fx.workspace.to_string_lossy()}),
            "agent:limited:cli:default",
            &fx.workspace,
        )
        .await;
        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(
            success,
            "tool:Glob grant should pass for the same principal: {content}"
        );
    }

    #[test]
    fn tool_executed_packet_oversized_result_is_truncated_to_budget() {
        let packet = tool_executed_packet(
            9,
            "x".repeat(200_000),
            json!({"also_big": "y".repeat(200_000)}),
            true,
        );
        let bytes = packet.to_bytes().expect("fits the datagram budget");
        assert!(bytes.len() <= peko_protocol::ipc::MAX_PACKET_SIZE);
        let ResponsePacket::ToolExecuted {
            content,
            result,
            success,
            truncated,
            ..
        } = packet
        else {
            panic!("wrong variant");
        };
        assert!(truncated);
        assert!(success);
        assert_eq!(result, serde_json::Value::Null);
        assert!(content.ends_with(TRUNCATION_MARKER), "content: {content}");
    }

    #[test]
    fn tool_executed_packet_escape_heavy_result_still_fits() {
        // JSON escaping inflates control chars up to 6x — the halving
        // loop must still land under the budget.
        let packet =
            tool_executed_packet(9, "\u{1}".repeat(40_000), serde_json::Value::Null, false);
        let bytes = packet.to_bytes().expect("fits the datagram budget");
        assert!(bytes.len() <= peko_protocol::ipc::MAX_PACKET_SIZE);
        let ResponsePacket::ToolExecuted { truncated, .. } = packet else {
            panic!("wrong variant");
        };
        assert!(truncated);
    }

    #[test]
    fn tool_executed_packet_small_payload_passes_through() {
        let packet = tool_executed_packet(9, "ok".to_string(), json!({"k": "v"}), true);
        let ResponsePacket::ToolExecuted {
            content,
            result,
            truncated,
            ..
        } = packet
        else {
            panic!("wrong variant");
        };
        assert!(!truncated);
        assert_eq!(content, "ok");
        assert_eq!(result, json!({"k": "v"}));
    }
}
