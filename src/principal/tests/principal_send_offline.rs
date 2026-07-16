//! Same-runtime offline `principal_send` integration test.
//!
//! Verifies that `LocalFirstAgentDirectory` resolves a target principal
//! without consulting the hub, and that `PrincipalSendTool::execute`
//! short-circuits locally via `PrincipalManager::receive`. This test is
//! self-contained and runs in the regular unit/integration job.
//!
//! Originally `tests/principal_send_offline.rs` (gated by `--features
//! test-utils`). Moved inline as part of F9.3 so the gated surface can
//! narrow — the test only consumes `crate::principal::*`, `crate::tunnel::*`,
//! `crate::auth::*`, `crate::providers::*` etc., all of which stay `pub`.

use std::sync::Arc;
use std::time::Duration;

use crate::auth::Subject;
use crate::engine::tool_runtime::ToolRuntime;
use crate::extensions::framework::core::init_global_core;
use crate::principal::config::{Exposure, TransportPreference};
use crate::principal::{
    DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalConfig, PrincipalManager,
};
use crate::providers::LlmResolver;
use crate::subject::PrincipalDID;
use crate::tools::Tool;
use crate::tunnel::a2a_pending::PendingA2aResponses;
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::direct::DirectConnectionManager;
use crate::tunnel::hub_directory::{AgentDirectory, AgentResolution, DirectoryError};
use crate::tunnel::known_runtimes::KnownRuntimes;
use crate::tunnel::local_directory::LocalFirstAgentDirectory;
use crate::tunnel::principal_send_tool::{PrincipalSendResult, PrincipalSendTool};
use async_trait::async_trait;
use ed25519_dalek::SigningKey;
use tokio::sync::RwLock;

/// A directory client that panics if consulted. Wrapping it inside
/// `LocalFirstAgentDirectory` proves the hub fallback is never reached
/// for same-runtime principals.
struct PanicDirectory;

#[async_trait]
impl AgentDirectory for PanicDirectory {
    async fn resolve_by_did(&self, _did: &str) -> Result<AgentResolution, DirectoryError> {
        panic!("hub directory should not be consulted for same-runtime principal_send");
    }

    async fn resolve_by_handle(
        &self,
        _owner: &str,
        _name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        panic!("hub directory should not be consulted for same-runtime principal_send");
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
    };
    manager.create(config).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn same_runtime_principal_send_short_circuits_offline() {
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
    let (resolver, adapter) =
        LlmResolver::mock(crate::providers::MockAdapter::new(), &catalog_path).await;

    let principal_manager = Arc::new(
        PrincipalManager::with_path_resolver(
            workspace,
            path_resolver,
            Arc::new(DefaultPrincipalMemoryFactory),
            Arc::new(DefaultPrincipalRouterFactory),
        )
        .with_resolver(resolver),
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

    // Target principal — owned by the caller so the permission check passes.
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
    let signing_key = Arc::new(SigningKey::from_bytes(&[9u8; 32]));
    let pending = Arc::new(PendingA2aResponses::new());

    let ctx = Arc::new(CrossRuntimeA2aCtx {
        directory: Arc::new(LocalFirstAgentDirectory::new(
            caller_runtime_id.clone(),
            principal_manager.clone(),
            Arc::new(PanicDirectory),
        )),
        pending: pending.clone(),
        signing_key: signing_key.clone(),
        caller_runtime_id,
        tunnel: Arc::new(RwLock::new(None)),
        direct_manager: Arc::new(DirectConnectionManager::new(
            signing_key,
            "did:key:test-runtime".to_string(),
            false,
            pending,
        )),
        known_runtimes: Arc::new(RwLock::new(KnownRuntimes::new())),
        principal_manager,
        response_timeout: Duration::from_secs(5),
    });

    let tool = PrincipalSendTool::new(caller_did, ctx);

    adapter.queue_text("mock offline response");

    let result = tool
        .execute(serde_json::json!({
            "target_principal": target_did,
            "message": "ping"
        }))
        .await
        .expect("execute should not throw");

    let parsed: PrincipalSendResult = serde_json::from_value(result).expect("parse result");
    assert!(parsed.success, "principal_send should succeed offline");
    assert_eq!(parsed.response, "mock offline response");
}
