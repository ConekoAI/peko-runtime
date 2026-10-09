//! `tool` domain request handler (F6 step 3 / F8 completion).
//!
//! Owns the daemon-side tool execution IPC variant `ExecuteTool`
//! (ADR-061 phase 1 — the synchronous workflow callback). The
//! `Async action spawn`/`AsyncCancel` variants were retired 2026-09-27 with
//! the rest of the dead IPC async-spawn path (ADR-063
//! §3-D1): the CLI never executes tools (ADR-021), so nothing
//! produced those packets. Async task control lives in the
//! per-principal `Async*` tool family instead. The handler holds a
//! narrow [`ToolHost`]
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
//! Security (ADR-042 + F8 invariant): the attribution passed to
//! `ToolRuntime::execute_tool_full_with_workspace` is **always** derived
//! server-side from the session's owning Principal. It is never
//! accepted from the IPC packet itself — that would be privilege
//! escalation. The resolution chain is:
//!
//!   `parse_session_key()` → `principal_manager.get_by_name(parts.agent)`
//!   → principal identity (id + name).
//!
//! ADR-066 P2 deleted the capability gate this attribution used to feed;
//! what remains is identity threading for principal-scoped tools and
//! audit.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::engine::tool_runtime::ToolRuntime;
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

    /// Async tool runtime used to execute the requested tool.
    fn tool_runtime(&self) -> Arc<ToolRuntime>;

    /// Run-token registry (ADR-061 phase 2b, D6) that authenticates
    /// `ExecuteTool` requests carrying a `PEKO_RUN_TOKEN`.
    fn run_token_registry(&self) -> Arc<crate::ipc::run_tokens::RunTokenRegistry>;
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
        matches!(request, RequestPacket::ExecuteTool { .. })
    }

    async fn handle(
        &self,
        request: RequestPacket,
        _caller: &CallerContext,
        sink: &dyn ResponseSink,
        _peer: &PeerAddr,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::ExecuteTool {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
                run_token,
            } => {
                self.handle_execute_tool(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    run_token,
                    sink,
                )
                .await?;
            }

            // `matches()` returned true, so the exhaustive list above
            // covers every owned variant. This arm is unreachable.
            _ => unreachable!("ToolHandler::matches allowed an unhandled variant"),
        }
        Ok(())
    }
}

/// Server-side attribution for one `session_key` (ADR-042 + F8 +
/// ADR-057). Everything in here is derived from the resolved principal —
/// never from the packet. On any resolution failure every field is
/// `None`. ADR-066 P2 removed the capability/active-extension fields
/// (there is no grant gate to feed); what remains is identity for
/// audit + D9 ownership.
struct SessionAttribution {
    /// Stable principal id + human-readable name, threaded into the
    /// funnel so principal-scoped tools (e.g. ADR-061 `ModelCall`,
    /// cron) can resolve per-principal state at handle time.
    principal_id: Option<String>,
    principal_name: Option<String>,
}

impl ToolHandler {
    /// Resolve the owning principal's identity server-side from a
    /// session key (ADR-042 + F8 — see the module-level doc for the full
    /// chain). `ExecuteTool` uses this attribution path (ADR-061).
    ///
    /// The principal name is the session's `agent` segment per ADR-042;
    /// `principal_manager` never returns a stale entry — it holds
    /// `Arc<Principal>` and reloads are coordinated by
    /// `PrincipalManager` itself.
    ///
    /// Any resolution failure returns all-`None` fields and warns so
    /// the mismatch is visible in tracing without leaking data to the
    /// caller. `caller` labels the warn line with the owning variant
    /// ("ExecuteTool").
    async fn resolve_session_grants(
        &self,
        session_key: &str,
        caller: &'static str,
    ) -> SessionAttribution {
        let parts = parse_session_key(session_key);
        let principal_agent = parts.agent;

        let principal = self
            .host
            .principal_manager()
            .get_by_name(principal_agent)
            .await;

        match principal {
            Some(principal) => SessionAttribution {
                principal_id: Some(principal.id.0.clone()),
                principal_name: Some(principal.name().await),
            },
            None => {
                warn!(
                    "{caller} session_key={session_key} resolved to unknown principal \
                     '{principal_agent}'; no attribution",
                );
                SessionAttribution {
                    principal_id: None,
                    principal_name: None,
                }
            }
        }
    }

    /// Execute a tool synchronously and return the result (ADR-061
    /// phase 1). There is no executor handoff: the
    /// call awaits the tool inline (the tool's own context timeout
    /// applies, same as the agentic-loop path) and replies with a
    /// `ToolExecuted` packet carrying the funnel's
    /// `(content, result, success)` triplet. Gate denials and tool
    /// errors arrive as `success: false` data, not a transport error;
    /// only a funnel-internal failure produces `ResponsePacket::Error`.
    ///
    /// Phase 2b (ADR-061 D6): when the packet carries a `run_token`,
    /// it is authenticated against the daemon's `RunTokenRegistry`
    /// BEFORE attribution — an unknown, expired, or session-mismatched
    /// token is refused with a transport-level `Error` (fail closed).
    /// The token only authenticates; grants still derive server-side
    /// from `session_key` via [`Self::resolve_session_grants`], never
    /// from the token payload. A validated token also lets the handler
    /// stamp the server-recorded workflow depth into nested `Workflow`
    /// calls (`_workflow_depth`), so the recursion guard can't be
    /// spoofed from the wire — a tokenless request has the key stripped.
    #[allow(clippy::too_many_arguments)]
    async fn handle_execute_tool(
        &self,
        request_id: u64,
        tool_name: String,
        mut params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
        run_token: Option<String>,
        sink: &dyn ResponseSink,
    ) -> anyhow::Result<()> {
        // Run-token authentication (phase 2b). Never log the token.
        let mut token_depth = None;
        // Caller-awareness: the tree node the calling `Workflow` was
        // invoked from, resolved from the server-minted token record —
        // NEVER from the packet. Threaded into `ToolContext.session_id`
        // in place of the session-key string so tree-relative tools
        // classify the workflow caller as that node.
        let mut token_caller_session_id: Option<String> = None;
        if let Some(token) = run_token.as_deref() {
            let caller_principal = parse_session_key(&session_key).agent.to_string();
            let validated = self
                .host
                .run_token_registry()
                .verify(token)
                .filter(|entry| {
                    entry.session_key == session_key && entry.principal_name == caller_principal
                });
            match validated {
                Some(entry) => {
                    token_depth = Some(entry.workflow_depth);
                    token_caller_session_id = entry.caller_session_id;
                }
                None => {
                    let response = ResponsePacket::Error {
                        request_id,
                        message: "ExecuteTool refused: run_token is unknown, expired, or does \
                                  not match the packet's session_key"
                            .to_string(),
                    };
                    send_response(sink, response).await?;
                    return Ok(());
                }
            }
        }

        // `_workflow_depth` is server-owned: strip whatever the wire
        // carried, then set it from the validated token's record when
        // the call targets the Workflow runner. A tokenless request
        // (local-trust path) always runs at depth 0.
        if tool_name == crate::tools::builtin::WORKFLOW_TOOL_NAME {
            if let Some(obj) = params.as_object_mut() {
                obj.remove("_workflow_depth");
                if let Some(depth) = token_depth {
                    obj.insert("_workflow_depth".to_string(), depth.into());
                }
            }
        }

        let attribution = self
            .resolve_session_grants(&session_key, "ExecuteTool")
            .await;
        // ADR-061 D2: attribution failure fails closed. The capability
        // gate's deny-all used to provide this; with the gate retired
        // (ADR-066) the refusal is explicit, so no call — and no
        // background task it spawns — runs without an owning principal.
        if attribution.principal_id.is_none() {
            send_response(
                sink,
                ResponsePacket::Error {
                    request_id,
                    message: format!(
                        "ExecuteTool refused: session_key '{session_key}' does not resolve \
                         to a loaded principal"
                    ),
                },
            )
            .await?;
            return Ok(());
        }

        // Session-id selection: a valid token carrying the calling node
        // id threads the real session UUID; a token without one (or no
        // token at all) threads the session-key string — the pre-fix
        // dangling behavior. The id flows through the session layer's
        // existing `caller_context` classification, which treats
        // unknown/stale ids as dangling — no new privilege logic.
        let threaded_session_id = token_caller_session_id.unwrap_or_else(|| session_key.clone());

        let tool_runtime = self.host.tool_runtime();
        let triplet = tool_runtime
            .execute_tool_full_with_workspace(
                &tool_name,
                params,
                &workspace,
                Some(threaded_session_id),
                attribution.principal_id,
                attribution.principal_name,
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
    use crate::async_exec::executor::AsyncExecutor;
    use crate::common::paths::PathResolver;
    use crate::principal::config::{
        PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
        PrincipalMemoryConfig, PrincipalRoutingConfig,
    };
    use peko_auth::Subject;

    use peko_tools_core::Tool as _;
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Minimal `ToolHost` double over real components: a
    /// `PrincipalManager` with one created principal, a `ToolRuntime`
    /// with the built-in tools registered, and a standalone
    /// `AsyncExecutor`. Mirrors the
    /// `principal_log` fixture in `ipc::handlers::principal`.
    struct TestToolHost {
        manager: Arc<PrincipalManager>,
        tool_runtime: Arc<ToolRuntime>,
        run_tokens: Arc<crate::ipc::run_tokens::RunTokenRegistry>,
    }

    impl ToolHost for TestToolHost {
        fn principal_manager(&self) -> &Arc<PrincipalManager> {
            &self.manager
        }
        fn tool_runtime(&self) -> Arc<ToolRuntime> {
            self.tool_runtime.clone()
        }
        fn run_token_registry(&self) -> Arc<crate::ipc::run_tokens::RunTokenRegistry> {
            self.run_tokens.clone()
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
        run_tokens: Arc<crate::ipc::run_tokens::RunTokenRegistry>,
        manager: Arc<PrincipalManager>,
        tool_runtime: Arc<ToolRuntime>,
    }

    fn test_principal_config(name: &str) -> crate::principal::PrincipalConfig {
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
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: vec![],
            preferred_model_id: Some("mock".to_string()),
            quota: None,
            children: Default::default(),
        }
    }

    async fn fixture(name: &str) -> Fixture {
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
            crate::async_exec::executor::standalone_inbox_registry(),
        ));
        manager
            .create(test_principal_config(name))
            .await
            .expect("create principal");

        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        let tool_runtime = ToolRuntime::with_workspace(path_resolver, &workspace)
            .await
            .expect("tool runtime");

        let run_tokens = Arc::new(crate::ipc::run_tokens::RunTokenRegistry::new());
        let tool_runtime = Arc::new(tool_runtime);
        let host = TestToolHost {
            manager: Arc::clone(&manager),
            tool_runtime: Arc::clone(&tool_runtime),
            run_tokens: Arc::clone(&run_tokens),
        };
        Fixture {
            _temp: temp,
            handler: ToolHandler::new(Arc::new(host)),
            workspace,
            run_tokens,
            manager,
            tool_runtime,
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
        execute_tool_with_token(
            handler,
            request_id,
            tool_name,
            params,
            session_key,
            workspace,
            None,
        )
        .await
    }

    /// `execute_tool` + a `run_token` on the packet (ADR-061 phase 2b).
    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_with_token(
        handler: &ToolHandler,
        request_id: u64,
        tool_name: &str,
        params: serde_json::Value,
        session_key: &str,
        workspace: &std::path::Path,
        run_token: Option<String>,
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
                    run_token,
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
    async fn execute_tool_executes_with_resolved_principal() {
        let fx = fixture("attributed").await;
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

    /// ADR-061 D2: an unknown `session_key` resolves to no principal, so
    /// the call fails closed — nothing runs unattributed.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_unknown_session_key_fails_closed() {
        let fx = fixture("known").await;
        let marker = fx.workspace.join("created-by-unattributed-call");

        let response = execute_tool(
            &fx.handler,
            2,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            "agent:ghost:cli:default",
            &fx.workspace,
        )
        .await;

        let ResponsePacket::Error { message, .. } = response else {
            panic!("expected Error, got {response:?}");
        };
        assert!(message.contains("does not resolve"), "message: {message}");
        assert!(!marker.exists(), "refused tool must not have run");
    }

    /// ADR-066 P2: a principal with no grants at all executes Bash —
    /// presence = executability.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_executes_without_grants() {
        let fx = fixture("unrestricted").await;
        let marker = fx.workspace.join("created-without-grants");

        let response = execute_tool(
            &fx.handler,
            3,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            "agent:unrestricted:cli:default",
            &fx.workspace,
        )
        .await;

        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "no grants needed: {content}");
        assert!(marker.exists(), "the tool ran");
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

    // ── ADR-061 phase 2b: run-token authentication ──────────────────

    /// A validated `run_token` authenticates the call; attribution and
    /// grants still derive from the session key (the tool runs).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_with_valid_run_token_executes() {
        let fx = fixture("tokprincipal").await;
        std::fs::write(fx.workspace.join("hello.txt"), "hi").expect("seed file");
        let token = fx.run_tokens.mint(
            "tokprincipal",
            "agent:tokprincipal:cli:default",
            1,
            None,
            std::time::Duration::from_mins(1),
        );

        let response = execute_tool_with_token(
            &fx.handler,
            10,
            "Glob",
            json!({"pattern": "*.txt", "path": fx.workspace.to_string_lossy()}),
            "agent:tokprincipal:cli:default",
            &fx.workspace,
            Some(token),
        )
        .await;

        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "valid token + grants should run: {content}");
        assert!(content.contains("hello.txt"), "content: {content}");
    }

    /// An unknown token fails closed with a transport-level `Error` —
    /// the tool never runs.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_unknown_run_token_fails_closed() {
        let fx = fixture("tokunknown").await;
        let marker = fx.workspace.join("should_not_exist");

        let response = execute_tool_with_token(
            &fx.handler,
            11,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            "agent:tokunknown:cli:default",
            &fx.workspace,
            Some("not-a-real-token".to_string()),
        )
        .await;

        let ResponsePacket::Error { message, .. } = response else {
            panic!("expected Error, got {response:?}");
        };
        assert!(message.contains("run_token"), "message: {message}");
        assert!(!marker.exists(), "refused tool must not have run");
    }

    /// A token minted for session A does not authenticate a packet
    /// naming session B — even when both resolve to the same principal.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_mismatched_session_key_fails_closed() {
        let fx = fixture("tokmismatch").await;
        let marker = fx.workspace.join("should_not_exist");
        let token = fx.run_tokens.mint(
            "tokmismatch",
            "agent:tokmismatch:workflow:run-1",
            1,
            None,
            std::time::Duration::from_mins(1),
        );

        let response = execute_tool_with_token(
            &fx.handler,
            12,
            "Bash",
            json!({"command": format!("touch {}", marker.display())}),
            // Same principal, different session key than the token's.
            "agent:tokmismatch:cli:default",
            &fx.workspace,
            Some(token),
        )
        .await;

        let ResponsePacket::Error { message, .. } = response else {
            panic!("expected Error, got {response:?}");
        };
        assert!(message.contains("run_token"), "message: {message}");
        assert!(!marker.exists(), "refused tool must not have run");
    }

    /// An expired token fails closed (zero-TTL mint).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_expired_run_token_fails_closed() {
        let fx = fixture("tokexpired").await;
        let token = fx.run_tokens.mint(
            "tokexpired",
            "agent:tokexpired:cli:default",
            1,
            None,
            std::time::Duration::ZERO,
        );

        let response = execute_tool_with_token(
            &fx.handler,
            13,
            "Glob",
            json!({"pattern": "*", "path": fx.workspace.to_string_lossy()}),
            "agent:tokexpired:cli:default",
            &fx.workspace,
            Some(token),
        )
        .await;

        let ResponsePacket::Error { message, .. } = response else {
            panic!("expected Error, got {response:?}");
        };
        assert!(message.contains("run_token"), "message: {message}");
    }

    /// A token minted for principal A does not authenticate a packet
    /// whose session_key names principal B.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_cross_principal_token_fails_closed() {
        let fx = fixture("toka").await;
        // Same fixture manager hosts a second principal.
        fx.manager
            .create(test_principal_config("tokb"))
            .await
            .expect("create second principal");
        let token = fx.run_tokens.mint(
            "toka",
            "agent:toka:cli:default",
            1,
            None,
            std::time::Duration::from_mins(1),
        );

        let response = execute_tool_with_token(
            &fx.handler,
            14,
            "Glob",
            json!({"pattern": "*", "path": fx.workspace.to_string_lossy()}),
            "agent:tokb:cli:default",
            &fx.workspace,
            Some(token),
        )
        .await;

        let ResponsePacket::Error { message, .. } = response else {
            panic!("expected Error, got {response:?}");
        };
        assert!(message.contains("run_token"), "message: {message}");
    }

    /// Depth injection (ADR-061 D8): a validated run token stamps the
    /// server-recorded `workflow_depth` into nested `Workflow` calls as
    /// `_workflow_depth`; a workflow at MAX depth is refused. The depth
    /// is server-derived — the wire cannot spoof it.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_stamps_workflow_depth_from_run_token() {
        let fx = fixture("deepwf").await;
        // Register the Workflow runner on the fixture's core (daemon
        // state.rs does this in production).
        fx.tool_runtime
            .tooling()
            .catalog()
            .register(
                Arc::new(crate::tools::builtin::WorkflowTool::new(
                    Arc::downgrade(&fx.manager),
                    Arc::clone(&fx.run_tokens),
                )),
                crate::tools::metadata::ToolSource::BuiltIn,
                peko_subject::PrincipalId::system(),
            )
            .await;

        // A workflow running at MAX depth calls ExecuteTool("Workflow")
        // — refused by the recursion guard.
        let token = fx.run_tokens.mint(
            "deepwf",
            "agent:deepwf:workflow:run-1",
            crate::tools::builtin::MAX_WORKFLOW_DEPTH,
            None,
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            15,
            "Workflow",
            json!({"path": "nonexistent.py"}),
            "agent:deepwf:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(!success, "depth-MAX spawn must be refused: {content}");
        assert!(
            content.contains("nesting depth"),
            "expected the recursion-guard refusal, got: {content}"
        );

        // A client-supplied `_workflow_depth` on a TOKENLESS call is
        // stripped server-side: the same "refusal" does not happen at
        // depth 0 — the failure surfaces later (path guard), proving
        // the wire value was ignored.
        let response = execute_tool(
            &fx.handler,
            16,
            "Workflow",
            json!({"path": "nonexistent.py", "_workflow_depth": 99}),
            "agent:deepwf:cli:default",
            &fx.workspace,
        )
        .await;
        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(!success);
        assert!(
            !content.contains("nesting depth") && content.contains("workflows"),
            "depth 99 from the wire must be stripped (fail later at the path guard), got: {content}"
        );
    }

    // ── Caller-aware workflow runs (run token carries the node) ─────

    /// Probe tool: classifies the caller from the threaded
    /// `ToolContext.session_id` through the REAL ownership layer
    /// (`session::ownership::caller_context`) over a real
    /// store-produced metadata slice — the same classification the
    /// `Agent` tool and session guards apply. Returns the threaded id
    /// plus the classification so tests can compare a workflow call
    /// against a direct call field-by-field.
    struct ClassifyProbeTool {
        metas: Vec<peko_session::SessionMetadata>,
    }

    #[async_trait]
    impl peko_tools_core::Tool for ClassifyProbeTool {
        fn name(&self) -> &'static str {
            "ClassifyProbe"
        }
        fn description(&self) -> String {
            "test probe".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            anyhow::bail!("probe only supports execute_with_context")
        }
        async fn execute_with_context(
            &self,
            _params: serde_json::Value,
            ctx: &peko_tools_core::ToolContext,
        ) -> anyhow::Result<serde_json::Value> {
            let id = ctx.session_id.clone().unwrap_or_default();
            let caller = crate::session::ownership::caller_context(&id, &self.metas);
            Ok(json!({
                "threaded": id,
                "dangling": caller.dangling,
                "is_base": caller.is_base,
                "ancestors": caller.ancestors,
            }))
        }
    }

    /// Seed a real session store with trunk + child; returns the
    /// metadata slice and the canonical ids.
    async fn seed_tree() -> (TempDir, Vec<peko_session::SessionMetadata>, String, String) {
        let temp = TempDir::new().expect("tempdir");
        let mut manager = peko_session::SessionManager::new()
            .with_sessions_dir_internal(temp.path())
            .with_agent_name("wf-agent");
        let peer = Subject::User("alice".to_string());
        manager
            .create_session(
                "wf-agent",
                &peer,
                peko_session::SessionCreateOptions::new().with_session_id("trunk"),
            )
            .await
            .expect("create trunk");
        let trunk_id = peko_session::SessionId::from("trunk").to_string();
        manager
            .create_session(
                "wf-agent",
                &peer,
                peko_session::SessionCreateOptions::new()
                    .with_session_id("child")
                    .with_parent(trunk_id.clone()),
            )
            .await
            .expect("create child");
        let child_id = peko_session::SessionId::from("child").to_string();
        let metas = manager
            .list_all_sessions(false)
            .await
            .expect("list sessions");
        (temp, metas, trunk_id, child_id)
    }

    async fn fixture_with_probe(
        name: &str,
        metas: Vec<peko_session::SessionMetadata>,
    ) -> (Fixture, Arc<ClassifyProbeTool>) {
        let fx = fixture(name).await;
        let probe = Arc::new(ClassifyProbeTool { metas });
        fx.tool_runtime
            .tooling()
            .catalog()
            .register(
                probe.clone(),
                crate::tools::metadata::ToolSource::BuiltIn,
                peko_subject::PrincipalId::system(),
            )
            .await;
        (fx, probe)
    }

    /// THE equivalence test: a workflow callback whose run token
    /// carries the calling node id is threaded that node id and
    /// classifies through the ownership layer EXACTLY as a direct call
    /// from that node does — same id, non-dangling, same ancestors.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_with_caller_node_behaves_as_direct_call_from_that_node() {
        let (_t, metas, trunk_id, child_id) = seed_tree().await;
        let (fx, probe) = fixture_with_probe("calleraware", metas).await;

        // The workflow side: ExecuteTool with a token minted against
        // the child node.
        let token = fx.run_tokens.mint(
            "calleraware",
            "agent:calleraware:workflow:run-1",
            1,
            Some(child_id.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            20,
            "ClassifyProbe",
            json!({}),
            "agent:calleraware:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success);

        // The direct side: the same tool driven with the node id, as
        // the engine does on the agent-loop path.
        let direct = probe
            .execute_with_context(
                json!({}),
                &peko_tools_core::ToolContext::default_for_tool("ClassifyProbe")
                    .with_session_id(child_id.clone()),
            )
            .await
            .expect("direct call");

        // Field-by-field equality: the workflow call IS the node.
        assert_eq!(result, direct);
        // And the classification is real: child of trunk, not dangling.
        assert_eq!(result["threaded"], json!(child_id));
        assert_eq!(result["dangling"], json!(false));
        assert_eq!(result["is_base"], json!(false));
        assert_eq!(result["ancestors"], json!([trunk_id]));
    }

    /// A token WITHOUT a caller node keeps the pre-fix behavior: the
    /// session-key string is threaded and classifies dangling.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_without_caller_node_threads_key_string() {
        let (_t, metas, _trunk_id, _child_id) = seed_tree().await;
        let (fx, _probe) = fixture_with_probe("noctx", metas).await;

        let token = fx.run_tokens.mint(
            "noctx",
            "agent:noctx:workflow:direct",
            1,
            None,
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            21,
            "ClassifyProbe",
            json!({}),
            "agent:noctx:workflow:direct",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success);
        assert_eq!(result["threaded"], json!("agent:noctx:workflow:direct"));
        assert_eq!(result["dangling"], json!(true));
    }

    /// Tokenless (local-trust) calls are unchanged: the session-key
    /// string is threaded and classifies dangling.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_tokenless_threads_key_string_unchanged() {
        let (_t, metas, _trunk_id, _child_id) = seed_tree().await;
        let (fx, _probe) = fixture_with_probe("plain", metas).await;

        let response = execute_tool(
            &fx.handler,
            22,
            "ClassifyProbe",
            json!({}),
            "agent:plain:workflow:direct",
            &fx.workspace,
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success);
        assert_eq!(result["threaded"], json!("agent:plain:workflow:direct"));
        assert_eq!(result["dangling"], json!(true));
    }

    /// A node id that no longer exists in the store degrades to
    /// dangling: the handler passes the id through unchanged and the
    /// session layer's existing classification decides — no new
    /// privilege logic.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_stale_caller_node_degrades_to_dangling() {
        let (_t, metas, _trunk_id, _child_id) = seed_tree().await;
        let (fx, _probe) = fixture_with_probe("stale", metas).await;

        let stale = "11111111-2222-3333-4444-555555555555".to_string();
        let token = fx.run_tokens.mint(
            "stale",
            "agent:stale:workflow:run-9",
            1,
            Some(stale.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            23,
            "ClassifyProbe",
            json!({}),
            "agent:stale:workflow:run-9",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success);
        assert_eq!(
            result["threaded"],
            json!(stale),
            "handler passes the token's node id through"
        );
        assert_eq!(
            result["dangling"],
            json!(true),
            "session layer classifies the stale node as dangling"
        );
    }

    // ── Caller-aware `Session` tool on the ExecuteTool path ─────────

    /// Seed the fixture principal's REAL store with trunk + child
    /// (slugs t/c), ids prefixed for cross-principal distinctness.
    async fn seed_principal_tree(fx: &Fixture, principal: &str, prefix: &str) -> (String, String) {
        let sessions_dir = fx
            .manager
            .get_by_name(principal)
            .await
            .expect("principal")
            .memory
            .sessions_dir()
            .clone();
        let mut manager = peko_session::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .with_agent_name(principal);
        let peer = Subject::User("alice".to_string());
        manager
            .create_session(
                principal,
                &peer,
                peko_session::SessionCreateOptions::new()
                    .with_session_id(format!("{prefix}-trunk")),
            )
            .await
            .expect("trunk");
        let trunk = peko_session::SessionId::from(format!("{prefix}-trunk").as_str()).to_string();
        manager
            .create_session(
                principal,
                &peer,
                peko_session::SessionCreateOptions::new()
                    .with_session_id(format!("{prefix}-child"))
                    .with_parent(trunk.clone()),
            )
            .await
            .expect("child");
        let child = peko_session::SessionId::from(format!("{prefix}-child").as_str()).to_string();
        manager
            .set_session_slug(&trunk, Some("t".to_string()))
            .await
            .expect("slug t");
        manager
            .set_session_slug(&child, Some("c".to_string()))
            .await
            .expect("slug c");
        (trunk, child)
    }

    /// Register the daemon-side caller-aware `Session` tool on the
    /// fixture core (daemon/state.rs does this in production).
    async fn register_daemon_session_tool(fx: &Fixture) {
        fx.tool_runtime
            .tooling()
            .catalog()
            .register(
                Arc::new(crate::tools::builtin::CallerAwareSessionTool::for_daemon(
                    Arc::downgrade(&fx.manager),
                    crate::async_exec::executor::standalone_inbox_registry(),
                )),
                crate::tools::metadata::ToolSource::BuiltIn,
                peko_subject::PrincipalId::system(),
            )
            .await;
    }

    /// Pre-change evidence: without the daemon-side registration (and
    /// no booted agent), `ExecuteTool("Session")` resolves to nothing.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_unresolvable_without_daemon_registration() {
        let fx = fixture("nosess").await;
        let response = execute_tool(
            &fx.handler,
            30,
            "Session",
            json!({"action": "list"}),
            "agent:nosess:cli:default",
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
            "unregistered session tool must not execute: {content}"
        );
    }

    /// THE headline test: a `session status` call through `ExecuteTool`
    /// with a token bound to the child node returns EXACTLY what a
    /// direct in-run call as the child returns — the calling node's
    /// status, parented under the trunk.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_status_matches_direct_call_from_token_node() {
        let fx = fixture("sesshead").await;
        register_daemon_session_tool(&fx).await;
        let (trunk, child) = seed_principal_tree(&fx, "sesshead", "head").await;

        let token = fx.run_tokens.mint(
            "sesshead",
            "agent:sesshead:workflow:run-1",
            1,
            Some(child.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            31,
            "Session",
            json!({"action": "status"}),
            "agent:sesshead:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "status via token must succeed: {result}");

        // The direct side: the stock tool as the child itself (the
        // loop-path construction, cell = child).
        let sessions_dir = fx
            .manager
            .get_by_name("sesshead")
            .await
            .expect("principal")
            .memory
            .sessions_dir()
            .clone();
        let manager = peko_session::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .with_agent_name("sesshead");
        let runtime = crate::session::session_runtime_impl::SessionManagerRuntime::new(
            Arc::new(tokio::sync::RwLock::new(manager)),
            Arc::new(tokio::sync::RwLock::new(Some(child.clone()))),
            "sesshead".to_string(),
            None,
            None,
        );
        let direct = crate::tools::builtin::SessionTool::new(
            Arc::new(runtime) as crate::tools::builtin::session::SharedSessionRuntime
        )
        .execute(json!({"action": "status"}))
        .await
        .expect("direct status");

        assert_eq!(result["session_id"], direct["session_id"]);
        assert_eq!(result["parent_session"], direct["parent_session"]);
        assert_eq!(result["message_count"], direct["message_count"]);
        assert_eq!(result["session_id"], json!(child));
        assert_eq!(result["parent_session"], json!(trunk));
    }

    /// A token bound to the TRUNK node resolves the trunk (parentless
    /// base caller — the whole-store identity).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_trunk_token_resolves_trunk() {
        let fx = fixture("sesstrunk").await;
        register_daemon_session_tool(&fx).await;
        let (trunk, _child) = seed_principal_tree(&fx, "sesstrunk", "base").await;

        let token = fx.run_tokens.mint(
            "sesstrunk",
            "agent:sesstrunk:workflow:run-1",
            1,
            Some(trunk.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            32,
            "Session",
            json!({"action": "status"}),
            "agent:sesstrunk:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "got: {result}");
        assert_eq!(result["session_id"], json!(trunk));
        assert_eq!(result["parent_session"], serde_json::Value::Null);
    }

    /// Token without a node: the session-key string threads → dangling
    /// — a destructive op is refused with the structured dangling
    /// message and has no side effect.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_without_node_degrades_to_dangling() {
        let fx = fixture("sessdang").await;
        register_daemon_session_tool(&fx).await;
        let (_trunk, _child) = seed_principal_tree(&fx, "sessdang", "dang").await;

        let token = fx.run_tokens.mint(
            "sessdang",
            "agent:sessdang:workflow:direct",
            1,
            None,
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            33,
            "Session",
            json!({"action": "remove", "path": "sess:/t/c"}),
            "agent:sessdang:workflow:direct",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(!success, "dangling caller must be refused: {content}");
        assert!(
            content.contains("no entry in the session store"),
            "structured dangling refusal expected, got: {content}"
        );

        // No side effect: the child is still in the store.
        let sessions_dir = fx
            .manager
            .get_by_name("sessdang")
            .await
            .expect("principal")
            .memory
            .sessions_dir()
            .clone();
        let mut manager =
            peko_session::SessionManager::new().with_sessions_dir_internal(sessions_dir);
        let metas = manager.list_all_sessions(false).await.expect("list");
        assert!(
            metas
                .iter()
                .any(|m| m.session_id.to_string().ends_with('c') || m.slug.as_deref() == Some("c")),
            "child must survive the refused remove"
        );
    }

    /// Tokenless (local-trust) calls degrade identically: key string
    /// threaded, dangling refusal on destructive ops.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_tokenless_degrades_to_dangling() {
        let fx = fixture("sessplain").await;
        register_daemon_session_tool(&fx).await;
        let (_trunk, _child) = seed_principal_tree(&fx, "sessplain", "plain").await;

        let response = execute_tool(
            &fx.handler,
            34,
            "Session",
            json!({"action": "remove", "path": "sess:/t/c"}),
            "agent:sessplain:workflow:direct",
            &fx.workspace,
        )
        .await;
        let ResponsePacket::ToolExecuted {
            content, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(!success);
        assert!(
            content.contains("no entry in the session store"),
            "got: {content}"
        );
    }

    /// Principal isolation: a token for principal A's node never
    /// touches principal B's store — the per-call resolution scopes to
    /// the principal resolved from the session key.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_session_isolates_principal_stores() {
        let fx = fixture("sessA").await;
        fx.manager
            .create(test_principal_config("sessB"))
            .await
            .expect("create sessB");
        register_daemon_session_tool(&fx).await;
        let (_ta, child_a) = seed_principal_tree(&fx, "sessA", "alpha").await;
        let (trunk_b, _child_b) = seed_principal_tree(&fx, "sessB", "beta").await;

        let token = fx.run_tokens.mint(
            "sessA",
            "agent:sessA:workflow:run-1",
            1,
            Some(child_a.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            35,
            "Session",
            json!({"action": "list"}),
            "agent:sessA:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "got: {result}");
        let listed = result["sessions"].to_string();
        assert!(listed.contains(&child_a), "A's node listed: {listed}");
        assert!(
            !listed.contains(&trunk_b),
            "B's store must never appear: {listed}"
        );
    }

    /// Async* per-call stamping on the workflow path: `Async action spawn` via
    /// `ExecuteTool` with a node-carrying run token stamps the task's
    /// `parent_session_key` with the token's node id — and the
    /// completion event is delivered to THAT session's inbox
    /// (wake-on-completion), not a stale cell value.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn execute_tool_async_spawn_stamps_token_node_and_delivers_there() {
        use crate::async_exec::executor::AsyncExecutorRuntime;
        use crate::tools::builtin::async_control::AsyncRuntime as _;

        let fx = fixture("asyncwf").await;
        let inbox_registry = crate::async_exec::executor::standalone_inbox_registry();
        let executor = Arc::new(AsyncExecutor::new(Arc::clone(&inbox_registry)));
        let principal_id = fx
            .manager
            .get_by_name("asyncwf")
            .await
            .expect("principal")
            .id
            .0
            .clone();
        let runtime = Arc::new(AsyncExecutorRuntime::new(
            Arc::clone(&executor),
            Arc::downgrade(fx.tool_runtime.tooling()),
            None, // no agent-DID cell — the request must carry the parent
            peko_subject::PrincipalId(principal_id.clone()),
        ));
        fx.tool_runtime
            .tooling()
            .catalog()
            .register(
                Arc::new(crate::tools::builtin::AsyncTool::new(
                    Arc::clone(&runtime).as_shared(),
                )),
                crate::tools::metadata::ToolSource::BuiltIn,
                peko_subject::PrincipalId::system(),
            )
            .await;

        let node = "550e8400-e29b-41d4-a716-446655440000".to_string();
        let token = fx.run_tokens.mint(
            "asyncwf",
            "agent:asyncwf:workflow:run-1",
            1,
            Some(node.clone()),
            std::time::Duration::from_mins(1),
        );
        let response = execute_tool_with_token(
            &fx.handler,
            40,
            "Async", json!({"action":"spawn", "tool": "Glob", "params": {"pattern": "*.nothing", "path": fx.workspace.to_string_lossy()}}),
            "agent:asyncwf:workflow:run-1",
            &fx.workspace,
            Some(token),
        )
        .await;
        let ResponsePacket::ToolExecuted {
            result, success, ..
        } = response
        else {
            panic!("expected ToolExecuted, got {response:?}");
        };
        assert!(success, "spawn must succeed: {result}");
        let task_id = result["task_id"].as_str().expect("task_id");

        // The task record is stamped with the token's node id.
        let view = runtime.lookup(task_id).await.expect("task registered");
        assert_eq!(view.parent_session_key, node);

        // Wake-on-completion: the CompletionEvent lands in the node's
        // inbox (the stamped origin), proving the delivery key is the
        // per-call stamp.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let delivered = loop {
            if let Some(inbox) = inbox_registry.peek_inbox(&node).await {
                if inbox.len().await > 0 {
                    break inbox.drain_all().await;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "completion never arrived in the node's inbox"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        };
        assert!(
            delivered.iter().any(|item| matches!(
                item,
                peko_session::AsyncInboxItem::Completion(env)
                    if env.parent_session_key == node && env.tool_name == "Glob"
            )),
            "completion for the spawned Glob must land in the node's inbox: {delivered:?}"
        );
    }
}
