//! `ChannelSend` principal-branch tests over real DM channels.
//!
//! The call posts and returns; posting never starts a run in the target
//! (ADR-072), which reads the message on its DM channel.
//!
//! Same runtime: `LocalFirstAgentDirectory` resolves the target without
//! the hub; the message is posted once to their shared pair DM, with
//! independent bindings to the caller's and target's peer sessions.
//!
//! Remote runtime: a `FakeAgentDirectory` resolves the target to another
//! runtime and a captured `TunnelHandle` records outbound traffic.
//! Covers first-contact invite (once), fan-out, and directory refusals.

use std::sync::Arc;

use crate::engine::tool_runtime::ToolRuntime;

use crate::principal::config::Exposure;
use crate::principal::{
    DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, PrincipalConfig, PrincipalManager,
};
use crate::tools::builtin::channel::{build_channel_send_tool, ChannelSendResult};
use crate::tunnel::cross_runtime::CrossRuntimeA2aCtx;
use crate::tunnel::hub_directory::{AgentDirectory, AgentResolution, DirectoryError};
use crate::tunnel::local_directory::LocalFirstAgentDirectory;
use crate::tunnel::TunnelChannelPort;
use async_trait::async_trait;
use peko_auth::Subject;
use peko_channel::{ChannelEvent, ChannelPort, Checkpoint};
use peko_providers::LlmResolver;
use peko_subject::PrincipalDID;

/// A directory client that panics if consulted. Wrapping it inside
/// `LocalFirstAgentDirectory` proves the hub fallback is never reached
/// for same-runtime `ChannelSend` principal branch.
const CALLER_RUNTIME: &str = "did:key:test-runtime";
const REMOTE_RUNTIME: &str = "did:key:remote-runtime";
const REMOTE_TARGET: &str = "did:key:z6MkRemoteTarget";

struct PanicDirectory;

#[async_trait]
impl AgentDirectory for PanicDirectory {
    async fn resolve_by_hub_id(&self, _did: &str) -> Result<AgentResolution, DirectoryError> {
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
) -> Arc<crate::principal::Principal> {
    let roles_dir = workspace.join(name).join("roles");
    tokio::fs::create_dir_all(&roles_dir).await.unwrap();
    let prompt_path = roles_dir.join("primary.md");
    let prompt_body = format!(
        "---\ndescription: \"Test assistant for {name}\"\n---\n\n\
         You are {name}, a test assistant. Reply concisely.\n"
    );
    tokio::fs::write(&prompt_path, prompt_body).await.unwrap();

    let config = PrincipalConfig {
        hub_id: None,
        actor_key: None,
        name: name.to_string(),
        id: None,
        did: None,
        owner,
        identity: Default::default(),
        intent: Default::default(),
        governance: Default::default(),
        memory: Default::default(),
        routing: Default::default(),
        exposure: Exposure::Public,
        status: None,
        boot_state: None,
        permissions: Vec::new(),
        preferred_model_id: Some("mock".to_string()),
        quota: None,
        children: Default::default(),
    };
    manager.create(config).await.unwrap()
}

/// The `(text, via)` of every `Posted` event on `principal`'s DM channel
/// with `peer`.
async fn posted_via(
    port: &Arc<dyn ChannelPort>,
    principal: &Arc<crate::principal::Principal>,
    peer: &Subject,
) -> Vec<(String, Option<String>)> {
    let peer = port.actor_info(peer).await.subject();
    let slug = crate::principal::peer_children::peer_child_slug(&peer).unwrap();
    let channel =
        crate::principal::peer_dm::find_peer_dm_channel(port, &principal.id, &format!("/{slug}"))
            .await
            .expect("dm lookup")
            .expect("DM channel exists after ChannelSend");
    port.peek(&channel, &Checkpoint::default())
        .await
        .expect("peek")
        .into_iter()
        .filter_map(|ev| match ev {
            ChannelEvent::Posted { text, via, .. } => Some((text, via)),
            _ => None,
        })
        .collect()
}

/// Two same-runtime principals, a caller-bound `ChannelSend`, and the
/// shared channel port, wired like the daemon.
struct Fixture {
    _temp: tempfile::TempDir,
    vault: Arc<crate::common::vault::Vault>,
    tooling: Arc<crate::tools::runtime::ToolingRuntime>,
    manager: Arc<PrincipalManager>,
    tunnel_port: TunnelChannelPort,
    channel_port: Arc<dyn ChannelPort>,
    caller: Arc<crate::principal::Principal>,
    caller_did: String,
    target: Arc<crate::principal::Principal>,
    target_did: String,
    tool: Arc<dyn peko_tools_core::Tool>,
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn local_directory_waits_for_busy_config_without_hub_fallback() {
    let fx = Fixture::new().await;
    let directory = Arc::new(LocalFirstAgentDirectory::new(
        CALLER_RUNTIME,
        fx.manager.clone(),
        Arc::new(PanicDirectory),
    ));
    let id = fx.target.id.0.clone();
    let guard = fx.target.config.write().await;
    let lookup = tokio::spawn(async move { directory.resolve_by_handle("", &id).await });
    tokio::task::yield_now().await;
    assert!(!lookup.is_finished());
    drop(guard);
    assert_eq!(lookup.await.unwrap().unwrap().local_id, fx.target.id.0);
}

impl Fixture {
    async fn new() -> Self {
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
        let workspace = temp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let (resolver, _adapter) = LlmResolver::mock(
            peko_providers::MockAdapter::new(),
            &temp.path().join("models.toml"),
        )
        .await;

        // One store shared by the manager (DM provisioning) and the
        // cross-runtime ctx (posts + reply subscription), as in the daemon.
        let store = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: temp.path().join("runtime"),
                shared_dir: None,
            },
        ));
        let tunnel_port = TunnelChannelPort::new(store);
        let channel_port: Arc<dyn ChannelPort> = Arc::new(tunnel_port.clone());

        let vault = Arc::new(crate::common::vault::Vault::load(path_resolver.vault()).unwrap());
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_identity_vault(vault.clone())
            .with_tooling(tool_runtime.tooling().clone())
            .with_resolver(resolver)
            .with_channel_port(channel_port.clone()),
        );
        let caller =
            create_test_principal(&manager, &workspace, "offline-caller", Subject::Public).await;
        let caller_did = caller.config.read().await.did.as_ref().unwrap().0.clone();
        let target = create_test_principal(
            &manager,
            &workspace,
            "offline-target",
            Subject::Principal(PrincipalDID(caller_did.clone())),
        )
        .await;
        let target_did = target.config.read().await.did.as_ref().unwrap().0.clone();

        // Installed before any tunnel context exists, as on an offline daemon.
        let tooling = tool_runtime.tooling().clone();
        tooling.services().set_channel_port(channel_port.clone());
        let tool = build_channel_send_tool(&tooling, &caller_did).unwrap();
        Self {
            _temp: temp,
            vault,
            tooling,
            manager,
            tunnel_port,
            channel_port,
            caller,
            caller_did,
            target,
            target_did,
            tool,
        }
    }

    /// Connect the cross-runtime context; the installed tool picks it up
    /// on its next call without re-registration.
    fn connect(&self) {
        let runtime_id = "did:key:test-runtime".to_string();
        self.tooling
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(CrossRuntimeA2aCtx {
                directory: Arc::new(LocalFirstAgentDirectory::new(
                    runtime_id.clone(),
                    self.manager.clone(),
                    Arc::new(PanicDirectory),
                )),
                caller_runtime_id: runtime_id,
                principal_manager: self.manager.clone(),
                channel_port: Arc::new(self.tunnel_port.clone()),
            }));
    }

    /// Connect as a runtime with a live (captured) tunnel, where DIDs not
    /// loaded here resolve through `directory` — the remote-principal
    /// path. Returns the outbound tunnel traffic.
    async fn connect_remote(
        &self,
        directory: Arc<crate::tunnel::hub_directory::FakeAgentDirectory>,
    ) -> tokio::sync::mpsc::Receiver<crate::tunnel::TunnelMessage> {
        self.channel_port
            .register_actor(
                &Subject::from(&self.caller.id),
                Some("caller-hub"),
                Some(&self.caller_did),
                self.caller.config.read().await.actor_key.as_deref(),
                Some("caller"),
                "public",
            )
            .await
            .unwrap();
        self.caller.config.write().await.hub_id = Some("caller-hub".into());
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let tunnel = Arc::new(tokio::sync::RwLock::new(Some(
            crate::tunnel::client::TunnelHandle::new(tx),
        )));
        self.tunnel_port
            .set_ctx(Arc::new(crate::tunnel::CrossRuntimeChannelCtx {
                directory: directory.clone(),
                signing_key: Arc::new(ed25519_dalek::SigningKey::from_bytes(&[7u8; 32])),
                caller_runtime_id: CALLER_RUNTIME.into(),
                tunnel,
                principal_keys: {
                    use base64::Engine;
                    use secrecy::ExposeSecret;
                    let secret = self
                        .vault
                        .get_principal_identity_private_key(&format!("{}#keys-1", self.caller_did))
                        .unwrap();
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(secret.expose_secret())
                        .unwrap();
                    Arc::new(crate::tunnel::cross_runtime_channel::TestPrincipalKeys(
                        vec![(
                            self.caller.id.clone(),
                            Arc::new(ed25519_dalek::SigningKey::from_bytes(
                                &bytes.try_into().unwrap(),
                            )),
                        )],
                    ))
                },
            }))
            .await;
        self.tooling
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(CrossRuntimeA2aCtx {
                directory: Arc::new(LocalFirstAgentDirectory::new(
                    CALLER_RUNTIME.to_string(),
                    self.manager.clone(),
                    directory,
                )),
                caller_runtime_id: CALLER_RUNTIME.into(),
                principal_manager: self.manager.clone(),
                channel_port: Arc::new(self.tunnel_port.clone()),
            }));
        rx
    }

    async fn send_to(&self, target_did: &str, text: &str) -> ChannelSendResult {
        let ctx =
            peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
                .with_principal_id(self.caller.id.0.clone());
        let reference = if let Some(principal) = self.manager.find_by_did(target_did).await {
            format!("principal:local:{}", principal.id.0)
        } else {
            format!("principal:hub:{target_did}")
        };
        let result = self
            .tool
            .execute_with_context(serde_json::json!({"to": reference, "text": text}), &ctx)
            .await;
        match result {
            Ok(value) => serde_json::from_value(value).expect("parse result"),
            // A failed send is a tool error; keep its text for assertions.
            Err(error) => serde_json::from_value(serde_json::json!({
                "success": false,
                "error": error.to_string(),
            }))
            .expect("failure shape"),
        }
    }

    async fn send(&self, text: &str) -> serde_json::Value {
        let ctx =
            peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
                .with_principal_id(self.caller.id.0.clone());
        self.tool
            .execute_with_context(
                serde_json::json!({"to": format!("principal:local:{}", self.target.id.0), "text": text}),
                &ctx,
            )
            .await
            // A failed send is a tool error; keep its text for assertions.
            .unwrap_or_else(|error| serde_json::json!({"success": false, "error": error.to_string()}))
    }
}

/// The `(msg_id, text)` of every root post on `principal`'s DM channel
/// with `peer`.
async fn root_ids(
    port: &Arc<dyn ChannelPort>,
    principal: &Arc<crate::principal::Principal>,
    peer: &Subject,
) -> Vec<(Option<String>, String)> {
    let peer = port.actor_info(peer).await.subject();
    let slug = crate::principal::peer_children::peer_child_slug(&peer).unwrap();
    let channel =
        crate::principal::peer_dm::find_peer_dm_channel(port, &principal.id, &format!("/{slug}"))
            .await
            .unwrap()
            .unwrap();
    port.peek(&channel, &Checkpoint::default())
        .await
        .unwrap()
        .into_iter()
        .filter_map(|ev| match ev {
            ChannelEvent::Posted {
                parent: None,
                msg_id,
                text,
                ..
            } => Some((msg_id, text)),
            _ => None,
        })
        .collect()
}

/// Same runtime: the request is recorded on the caller's DM channel and
/// delivered to the target's under ONE message id, and the call returns
/// at once — it does not wait for an answer.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn same_runtime_send_records_and_delivers_and_returns_at_once() {
    let fx = Fixture::new().await;
    let offline = fx.send("ping").await;
    assert!(offline["error"]
        .as_str()
        .unwrap()
        .contains("cross-runtime context"));

    fx.connect();
    let result = fx.send_to(&fx.target_did, "ping").await;
    assert!(result.success, "{:?}", result.error);
    assert_eq!(result.delivery.as_deref(), Some("delivered"));
    assert!(result
        .note
        .unwrap()
        .contains("does not wake the other principal"));
    assert!(!result.task_id.is_empty(), "the request's id on dm_channel");

    let record = root_ids(
        &fx.channel_port,
        &fx.caller,
        &Subject::Principal(PrincipalDID(fx.target_did.clone())),
    )
    .await;
    let delivered = root_ids(
        &fx.channel_port,
        &fx.target,
        &Subject::Principal(PrincipalDID(fx.caller_did.clone())),
    )
    .await;
    assert_eq!(record.len(), 1, "{record:?}");
    assert_eq!(record, delivered, "same text, same message id on both");
    assert!(record[0].0.is_some());
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn three_local_principals_can_send_and_thread_replies_without_a_hub() {
    let fx = Fixture::new().await;
    fx.connect(); // local directory; the Hub fallback panics if called
    let third = create_test_principal(
        &fx.manager,
        &fx._temp.path().join("principals"),
        "third",
        Subject::Public,
    )
    .await;
    let third_tool = build_channel_send_tool(&fx.tooling, &third.did().await.0).unwrap();
    let third_ctx = peko_tools_core::ToolContext::for_hook_run("test", "tool", "ChannelSend")
        .with_principal_id(third.id.0.clone());
    let first = fx.send_to(&fx.target_did, "first").await;
    assert!(first.success);
    let second = third_tool
        .execute_with_context(
            serde_json::json!({"to":format!("principal:local:{}", fx.target.id.0),"text":"second"}),
            &third_ctx,
        )
        .await
        .unwrap();
    assert_ne!(first.channel, second["channel"].as_str().unwrap());
    let target_tool = build_channel_send_tool(&fx.tooling, &fx.target_did).unwrap();
    let target_ctx = peko_tools_core::ToolContext::for_hook_run("test", "reply", "ChannelSend")
        .with_principal_id(fx.target.id.0.clone());
    target_tool
        .execute_with_context(
            serde_json::json!({"to":first.channel,"parent":first.task_id,"text":"answer"}),
            &target_ctx,
        )
        .await
        .unwrap();
    let events = fx
        .channel_port
        .peek(
            &peko_channel::ChannelId(first.channel),
            &Checkpoint::default(),
        )
        .await
        .unwrap();
    assert!(events.iter().any(|e|matches!(e,ChannelEvent::Posted{author,parent:Some(_),text,..} if author==&format!("principal:local:{}",fx.target.id.0) && text=="answer")));
    assert_eq!(
        root_ids(
            &fx.channel_port,
            &fx.caller,
            &Subject::Principal(fx.target_did.into())
        )
        .await
        .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn private_local_delivery_requires_the_canonical_principal_grant() {
    let fx = Fixture::new().await;
    fx.connect();
    {
        let mut config = fx.target.config.write().await;
        config.exposure = Exposure::Private;
        config.owner = Subject::User("other".into());
        config.permissions.clear();
    }
    let denied = fx.send_to(&fx.target_did, "denied").await;
    assert!(!denied.success);
    fx.target
        .config
        .write()
        .await
        .permissions
        .push(peko_auth::ownership::PermissionGrant {
            subject: Subject::Principal(fx.caller_did.clone().into()),
            permission: peko_auth::ownership::Permission::Chat,
            granted_at: "2026-10-12T00:00:00Z".into(),
            granted_by: Subject::User("other".into()),
        });
    assert!(fx.send_to(&fx.target_did, "allowed").await.success);
}

/// Every root post a call makes carries the calling session's slug path
/// as `via` (attribution). Calls without a resolvable session stay
/// unattributed.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn root_posts_carry_the_calling_sessions_slug_path() {
    let fx = Fixture::new().await;
    fx.connect();

    // The first exchange provisions the caller's peer child; the second
    // is made from inside it, as the child's own agent would.
    let first = fx.send_to(&fx.target_did, "one").await;
    assert!(first.success, "{:?}", first.error);
    let ctx = peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
        .with_principal_id(fx.caller.id.0.clone())
        .with_session_id(first.session_id.clone())
        .with_workspace(fx.caller.workspace_path.to_string_lossy());
    let send_from = |ctx: peko_tools_core::ToolContext, text: &'static str| {
        let fx = &fx;
        async move {
            let result: ChannelSendResult = serde_json::from_value(
                fx.tool
                    .execute_with_context(
                        serde_json::json!({"to": format!("principal:local:{}", fx.target.id.0), "text": text}),
                        &ctx,
                    )
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(result.success, "{text}: {:?}", result.error);
        }
    };
    send_from(ctx.clone(), "two").await;
    // A session id the store doesn't know is not attributed.
    send_from(
        ctx.with_session_id(peko_session::SessionId::from("not-in-store").to_string()),
        "three",
    )
    .await;

    let target_peer = Subject::from(&fx.target.id);
    let slug = crate::principal::peer_children::peer_child_slug(&target_peer).unwrap();
    let via = Some(format!("{}/{slug}", peko_session::path::SCHEME_PREFIX));
    let expected = vec![
        ("one".to_string(), None),
        ("two".to_string(), via),
        ("three".to_string(), None),
    ];
    assert_eq!(
        posted_via(&fx.channel_port, &fx.caller, &target_peer).await,
        expected
    );
    let caller_peer = Subject::from(&fx.caller.id);
    assert_eq!(
        posted_via(&fx.channel_port, &fx.target, &caller_peer).await,
        expected
    );
}

/// A `ChannelSend` bound to a principal this runtime has not loaded
/// refuses before touching any channel.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn caller_not_loaded_on_this_runtime_is_an_error() {
    let fx = Fixture::new().await;
    fx.connect();
    let ghost = build_channel_send_tool(&fx.tooling, "did:key:zGhostCaller").unwrap();
    let ctx = peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
        .with_principal_id("ghost");
    let error = ghost
        .execute_with_context(
            serde_json::json!({"to": format!("principal:local:{}", fx.target.id.0), "text": "hi"}),
            &ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("caller principal is not loaded"), "{error}");
}

fn remote_resolution(
    exposure: crate::tunnel::hub_directory::ResolvedExposure,
    agent_did: &str,
) -> crate::tunnel::hub_directory::AgentResolution {
    crate::tunnel::hub_directory::AgentResolution {
        name: None,
        hub_id: agent_did.into(),
        local_id: "prin_remote".into(),
        identity_key: "remote-actor-key".into(),
        runtime_id: REMOTE_RUNTIME.into(),
        instance_id: "inst-remote".into(),
        agent_did: agent_did.into(),
        owner_principal: Subject::Public,
        exposure,
    }
}

/// Record the kind of every outbound tunnel message.
fn spawn_remote_peer(
    mut outbound: tokio::sync::mpsc::Receiver<crate::tunnel::TunnelMessage>,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Vec<&'static str>>>,
) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_w = Arc::clone(&seen);
    let handle = tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            let kind = match message {
                crate::tunnel::TunnelMessage::TunnelChannelInvite { .. } => "invite",
                crate::tunnel::TunnelMessage::TunnelChannelEvent { .. } => "event",
                _ => continue,
            };
            seen_w.lock().unwrap().push(kind);
        }
    });
    (handle, seen)
}

/// Remote principal: first contact invites the target's runtime over the
/// tunnel, the root post fans out, and the call returns at once. Later
/// requests do not re-invite.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn remote_request_invites_once_fans_out_and_returns_at_once() {
    use crate::tunnel::hub_directory::ResolvedExposure;
    let fx = Fixture::new().await;
    let directory = Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::new());
    directory.register_hub_id(
        REMOTE_TARGET,
        remote_resolution(ResolvedExposure::Public, REMOTE_TARGET),
    );
    let outbound = fx.connect_remote(directory).await;
    let (peer, seen) = spawn_remote_peer(outbound);

    let first = fx.send_to(REMOTE_TARGET, "ping").await;
    assert!(first.success, "{:?}", first.error);
    // The captured tunnel never answers, so nothing confirms delivery.
    assert_eq!(first.delivery.as_deref(), Some("queued"));
    assert!(first.note.unwrap().contains("resent until it is delivered"));
    let expected = peko_channel::dm_channel_id(
        &fx.caller.config.read().await.actor_key.clone().unwrap(),
        "remote-actor-key",
    );
    assert_eq!(first.channel, expected.as_str());
    assert_eq!(first.dm_channel.as_deref(), Some(expected.as_str()));
    let second = fx.send_to(REMOTE_TARGET, "again").await;
    assert!(second.success, "{:?}", second.error);
    // Let the captured tunnel drain.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    peer.abort();

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter().filter(|k| **k == "invite").count(),
        1,
        "{seen:?}"
    );
    assert!(
        seen.iter().filter(|k| **k == "event").count() >= 2,
        "{seen:?}"
    );
    let remote_members = fx
        .tunnel_port
        .local()
        .list_remote_members(&expected)
        .await
        .unwrap();
    assert_eq!(remote_members.len(), 1);
    assert_eq!(remote_members[0].runtime_id, REMOTE_RUNTIME);
}

/// Directory refusals and unusable resolutions are structured errors,
/// and nothing is sent over the tunnel.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn remote_resolution_failures_are_errors() {
    use crate::tunnel::hub_directory::DirectoryErrorKind;
    use crate::tunnel::hub_directory::ResolvedExposure;
    let fx = Fixture::new().await;
    let directory = Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::new());
    directory.register_hub_id_err("did:key:missing", DirectoryErrorKind::NotFound);
    directory.register_hub_id_err("did:key:forbidden", DirectoryErrorKind::Forbidden);
    directory.register_hub_id(
        "did:key:hidden",
        remote_resolution(ResolvedExposure::LocalOnly, "did:key:hidden"),
    );
    let mut missing_signer = remote_resolution(ResolvedExposure::Public, "");
    missing_signer.hub_id = "did:key:nodid".into();
    directory.register_hub_id("did:key:nodid", missing_signer);
    directory.register_hub_id(
        "did:key:requested",
        remote_resolution(ResolvedExposure::Public, "did:key:substituted"),
    );
    let mut outbound = fx.connect_remote(directory).await;

    for (target, needle) in [
        ("did:key:missing", "target not found"),
        ("did:key:forbidden", "not permitted"),
        ("did:key:hidden", "local_only"),
        ("did:key:nodid", "empty target signing DID"),
        ("did:key:requested", "different Hub identity"),
    ] {
        let result = fx.send_to(target, "ping").await;
        assert!(!result.success, "{target}");
        let error = result.error.unwrap();
        assert!(error.contains(needle), "{target}: {error}");
    }
    assert!(outbound.try_recv().is_err(), "nothing sent over the tunnel");
}
