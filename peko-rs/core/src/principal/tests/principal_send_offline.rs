//! Same-runtime offline `ChannelSend` (principal branch) integration
//! test (sprint 4 — was sprint 3 Phase 12b rewrite).
//!
//! Verifies that `LocalFirstAgentDirectory` resolves a target principal
//! without consulting the hub, and that `ChannelSendTool`'s
//! same-runtime branch runs over the DM channels: the message is
//! durably posted to BOTH the caller's own DM channel (the `peko log`
//! mirror) and the target's DM channel (where the target's responder
//! would fire), and — with no live responder in this harness — the
//! reply await surfaces a structured timeout. Offline behavior is now
//! exactly "durable local post + await timeout".
//!
//! Originally `tests/principal_send_offline.rs` (gated by `--features
//! test-utils`). Moved inline as part of F9.3 so the gated surface can
//! narrow — the test only consumes `crate::principal::*`, `crate::tunnel::*`,
//! `peko_auth::*`, `peko_providers::*` etc., all of which stay `pub`.

use std::sync::Arc;
use std::time::Duration;

use crate::engine::tool_runtime::ToolRuntime;
use crate::extensions::framework::core::init_global_core;
use crate::principal::config::{Exposure, TransportPreference};
use crate::principal::{
    DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalConfig, PrincipalManager,
};
use crate::tools::builtin::channel::{ChannelSendResult, ChannelSendTool};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentDirectory, AgentResolution, DirectoryError};
use crate::tunnel::local_directory::LocalFirstAgentDirectory;
use crate::tunnel::TunnelChannelPort;
use async_trait::async_trait;
use peko_auth::Subject;
use peko_channel::{ChannelEvent, ChannelPort, Checkpoint};
use peko_providers::LlmResolver;
use peko_subject::PrincipalDID;
use peko_tools_core::Tool;

/// A directory client that panics if consulted. Wrapping it inside
/// `LocalFirstAgentDirectory` proves the hub fallback is never reached
/// for same-runtime `ChannelSend` principal branch.
struct PanicDirectory;

#[async_trait]
impl AgentDirectory for PanicDirectory {
    async fn resolve_by_did(&self, _did: &str) -> Result<AgentResolution, DirectoryError> {
        panic!("hub directory should not be consulted for same-runtime ChannelSend");
    }

    async fn resolve_by_handle(
        &self,
        _owner: &str,
        _name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        panic!("hub directory should not be consulted for same-runtime ChannelSend");
    }
}

async fn create_test_principal(
    manager: &PrincipalManager,
    workspace: &std::path::Path,
    name: &str,
    owner: Subject,
    transport_preference: TransportPreference,
) -> Arc<crate::principal::Principal> {
    let agents_dir = workspace.join(name).join("agents");
    tokio::fs::create_dir_all(&agents_dir).await.unwrap();
    let prompt_path = agents_dir.join("primary.md");
    let prompt_body = format!(
        "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
         You are {name}, a test assistant. Reply concisely.\n"
    );
    tokio::fs::write(&prompt_path, prompt_body).await.unwrap();

    let config = PrincipalConfig {
        name: name.to_string(),
        did: None,
        owner,
        identity: Default::default(),
        intent: Default::default(),
        governance: Default::default(),
        memory: Default::default(),
        routing: Default::default(),
        capabilities: Default::default(),
        exposure: Exposure::Public,
        status: None,
        permissions: Vec::new(),
        preferred_model_id: Some("mock".to_string()),
        transport_preference,
        quota: None,
        children: Default::default(),
    };
    manager.create(config).await.unwrap()
}

/// The `(author, parent, text)` rows of every `Posted` event on the
/// channel bound to `binding` for `principal` (find-only — the sends
/// above already provisioned it).
async fn dm_posted_rows(
    port: &Arc<dyn ChannelPort>,
    principal: &Arc<crate::principal::Principal>,
    peer: &Subject,
) -> Vec<(String, Option<String>, String)> {
    let slug = crate::principal::peer_children::peer_child_slug(peer).unwrap();
    let channel = crate::principal::peer_dm::find_peer_dm_channel(
        port,
        &principal.id,
        &format!("/{slug}"),
    )
    .await
    .expect("dm lookup")
    .expect("DM channel exists after ChannelSend");
    port.peek(&channel, &Checkpoint::default())
        .await
        .expect("peek")
        .iter()
        .filter_map(|ev| match ev {
            ChannelEvent::Posted {
                author,
                parent,
                text,
                ..
            } => Some((author.clone(), parent.clone(), text.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn same_runtime_channel_send_principal_branch_posts_and_times_out() {
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("PEKO_HOME", temp.path());

    let path_resolver = crate::common::paths::PathResolver::with_dirs(
        temp.path().join("config"),
        temp.path().join("data"),
        temp.path().join("cache"),
    );
    let tool_runtime = ToolRuntime::with_workspace(path_resolver.clone(), temp.path())
        .await
        .expect("tool runtime should initialize");
    init_global_core(tool_runtime.extension_core().clone());

    let workspace = temp.path().join("principals");
    let workspace_ref = workspace.clone();
    tokio::fs::create_dir_all(&workspace).await.unwrap();

    let catalog_path = temp.path().join("models.toml");
    let (resolver, _adapter) =
        LlmResolver::mock(peko_providers::MockAdapter::new(), &catalog_path).await;

    // The channel port both the manager (DM provisioning) and the
    // ctx (posts + reply subscription) share — one underlying store,
    // exactly like the daemon wiring.
    let store = Arc::new(peko_channel::ChannelStore::new(
        peko_channel::ChannelConfig {
            runtime_dir: temp.path().join("runtime"),
            shared_dir: None,
        },
    ));
    let tunnel_port = TunnelChannelPort::new(store);
    let channel_port: Arc<dyn ChannelPort> = Arc::new(tunnel_port.clone());

    let principal_manager = Arc::new(
        PrincipalManager::with_path_resolver(
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver)
        .with_channel_port(channel_port.clone()),
    );

    // Caller principal — its DID becomes the owner of the target.
    let caller = create_test_principal(
        &principal_manager,
        &workspace_ref,
        "offline-caller",
        Subject::Public,
        TransportPreference::Auto,
    )
    .await;

    let caller_did = {
        let cfg = caller.config.read().await;
        cfg.did.as_ref().unwrap().0.clone()
    };

    // Target principal — owned by the caller.
    let target = create_test_principal(
        &principal_manager,
        &workspace_ref,
        "offline-target",
        Subject::Principal(PrincipalDID(caller_did.clone())),
        TransportPreference::Direct,
    )
    .await;

    let target_did = {
        let cfg = target.config.read().await;
        cfg.did.as_ref().unwrap().0.clone()
    };

    let caller_runtime_id = "did:key:test-runtime".to_string();
    let ctx = Arc::new(CrossRuntimeA2aCtx {
        directory: Arc::new(LocalFirstAgentDirectory::new(
            caller_runtime_id.clone(),
            principal_manager.clone(),
            Arc::new(PanicDirectory),
        )),
        caller_runtime_id,
        principal_manager: principal_manager.clone(),
        channel_port: Arc::new(tunnel_port),
        response_timeout: Duration::from_millis(200),
    });

    // Sprint 4: ChannelSendTool replaces SendPeerTool. The principal
    // branch is selected by the `principal:<did>` wire form on the
    // `channel` parameter — exactly the same dispatch shape the
    // LLM-facing tool will use. The principal branch needs a
    // `ToolContext` (the F37 funnel supplies one in production); the
    // test stands up a minimal context with the caller's principal id
    // bound.
    let tool = ChannelSendTool::new_with_peer(channel_port.clone(), caller_did.clone(), ctx);
    let principal_id_string = caller.id.0.clone();
    let tool_ctx = peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
        .with_principal_id(principal_id_string);

    let result = tool
        .execute_with_context(
            serde_json::json!({
                "channel": format!("principal:{target_did}"),
                "text": "ping",
            }),
            &tool_ctx,
        )
        .await
        .expect("execute_with_context should not throw");

    // No live responder in this harness → the reply await times out
    // with a structured error.
    let parsed: ChannelSendResult = serde_json::from_value(result).expect("parse result");
    assert!(!parsed.success, "no responder → await must time out");
    let err = parsed.error.expect("timeout error must be set");
    assert!(
        err.contains("timed out"),
        "error must name the timeout; got: {err}"
    );

    // …but the message stands durably on BOTH DM channels:
    // 1. the caller's own channel (self-authored root — the `peko log`
    //    mirror);
    let caller_peer = Subject::Principal(PrincipalDID(target_did.clone()));
    let caller_rows = dm_posted_rows(&channel_port, &caller, &caller_peer).await;
    assert_eq!(
        caller_rows,
        vec![(caller.id.0.clone(), None, "ping".to_string())],
        "caller's DM channel must hold the self-authored outbound post"
    );

    // 2. the target's channel (caller-authored root — the post the
    //    target's responder would fire on).
    let target_peer = Subject::Principal(PrincipalDID(caller_did.clone()));
    let target_rows = dm_posted_rows(&channel_port, &target, &target_peer).await;
    assert_eq!(
        target_rows,
        vec![(caller.id.0.clone(), None, "ping".to_string())],
        "target's DM channel must hold the caller's root post"
    );
}
