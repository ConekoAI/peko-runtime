//! `ChannelSend` principal-branch tests over real DM channels.
//!
//! Same runtime: `LocalFirstAgentDirectory` resolves the target without
//! the hub; the message is posted to both the caller's DM channel (the
//! `peko log` record) and the target's, and the reply await times out
//! cleanly with no responder. A fake target responder (`spawn_responder`)
//! covers the reply path and per-target request serialization.
//!
//! Remote runtime: a `FakeAgentDirectory` resolves the target to another
//! runtime and a captured `TunnelHandle` records outbound traffic; a fake
//! peer (`spawn_remote_peer`) mirrors the reply back into the caller's DM
//! channel. Covers first-contact invite (once), fan-out, reply, timeout,
//! and directory refusals.

use std::sync::Arc;
use std::time::Duration;

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
use peko_channel::{ChannelEvent, ChannelId, ChannelPort, Checkpoint, PostMsg};
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

/// The `(author, parent, text)` rows of every `Posted` event on the
/// channel bound to `binding` for `principal` (find-only — the sends
/// above already provisioned it).
async fn dm_posted_rows(
    port: &Arc<dyn ChannelPort>,
    principal: &Arc<crate::principal::Principal>,
    peer: &Subject,
) -> Vec<(String, Option<String>, String)> {
    let slug = crate::principal::peer_children::peer_child_slug(peer).unwrap();
    let channel =
        crate::principal::peer_dm::find_peer_dm_channel(port, &principal.id, &format!("/{slug}"))
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

/// Two same-runtime principals, a caller-bound `ChannelSend`, and the
/// shared channel port, wired like the daemon.
struct Fixture {
    _temp: tempfile::TempDir,
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

        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(DefaultPrincipalMemoryFactory),
                Arc::new(DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
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
    fn connect(&self, response_timeout: Duration) {
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
                response_timeout,
            }));
    }

    /// Connect as a runtime with a live (captured) tunnel, where DIDs not
    /// loaded here resolve through `directory` — the remote-principal
    /// path. Returns the outbound tunnel traffic.
    async fn connect_remote(
        &self,
        directory: Arc<crate::tunnel::hub_directory::FakeAgentDirectory>,
        response_timeout: Duration,
    ) -> tokio::sync::mpsc::Receiver<crate::tunnel::TunnelMessage> {
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
                principal_keys: Arc::new(crate::tunnel::cross_runtime_channel::NoPrincipalKeys),
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
                response_timeout,
            }));
        rx
    }

    async fn send_to(&self, target_did: &str, text: &str) -> ChannelSendResult {
        let ctx =
            peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
                .with_principal_id(self.caller.id.0.clone());
        let result = self
            .tool
            .execute_with_context(
                serde_json::json!({"channel": format!("principal:{target_did}"), "text": text}),
                &ctx,
            )
            .await
            .expect("execute_with_context should not throw");
        serde_json::from_value(result).expect("parse result")
    }

    async fn send(&self, text: &str) -> serde_json::Value {
        let ctx =
            peko_tools_core::ToolContext::for_hook_run("test-run", "test-tool", "ChannelSend")
                .with_principal_id(self.caller.id.0.clone());
        self.tool
            .execute_with_context(
                serde_json::json!({"channel": format!("principal:{}", self.target_did), "text": text}),
                &ctx,
            )
            .await
            .expect("execute_with_context should not throw")
    }

    /// The target's DM channel for the caller, provisioned the way the
    /// tool provisions it (idempotent).
    async fn target_channel(&self) -> ChannelId {
        let peer = Subject::Principal(PrincipalDID(self.caller_did.clone()));
        self.manager
            .ensure_peer_child_ingress(&self.target, &peer)
            .await
            .expect("provision target-side DM")
            .dm_channel
            .expect("DM channel")
    }

    /// Stand in for the target's responder: answer each caller-authored
    /// root post on the target's DM channel with `pong: <text>` after
    /// `delay`, authored by the target. Replies are produced in arrival
    /// order, one at a time.
    async fn spawn_responder(&self, delay: Duration) -> tokio::task::JoinHandle<()> {
        let channel = self.target_channel().await;
        let port = self.channel_port.clone();
        let caller_raw = self.caller.id.0.clone();
        let target = Subject::from(&self.target.id);
        let mut rx = port.subscribe_events(&channel).await;
        tokio::spawn(async move {
            let mut answered = 0usize;
            while rx.recv().await.is_ok() {
                let roots: Vec<_> = port
                    .peek_with_ids(&channel, &Checkpoint::default())
                    .await
                    .unwrap()
                    .into_iter()
                    .filter_map(|(id, ev)| match ev {
                        ChannelEvent::Posted {
                            author,
                            parent: None,
                            text,
                            ..
                        } if author == caller_raw => Some((id, text)),
                        _ => None,
                    })
                    .collect();
                for (id, text) in roots.into_iter().skip(answered) {
                    tokio::time::sleep(delay).await;
                    port.post(
                        &channel,
                        &target,
                        PostMsg::reply(id, format!("pong: {text}")),
                    )
                    .await
                    .unwrap();
                    answered += 1;
                }
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn same_runtime_channel_send_principal_branch_posts_and_times_out() {
    let fx = Fixture::new().await;
    let offline = fx.send("ping").await;
    assert!(offline["error"]
        .as_str()
        .unwrap()
        .contains("cross-runtime context"));

    fx.connect(Duration::from_millis(200));
    let result = fx.send("ping").await;

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
    let caller_peer = Subject::Principal(PrincipalDID(fx.target_did.clone()));
    let caller_rows = dm_posted_rows(&fx.channel_port, &fx.caller, &caller_peer).await;
    assert_eq!(
        caller_rows,
        vec![(fx.caller.id.0.clone(), None, "ping".to_string())],
        "caller's DM channel must hold the self-authored outbound post"
    );

    // 2. the target's channel (caller-authored root — the post the
    //    target's responder would fire on).
    let target_peer = Subject::Principal(PrincipalDID(fx.caller_did.clone()));
    let target_rows = dm_posted_rows(&fx.channel_port, &fx.target, &target_peer).await;
    assert_eq!(
        target_rows,
        vec![(fx.caller.id.0.clone(), None, "ping".to_string())],
        "target's DM channel must hold the caller's root post"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn same_runtime_reply_is_returned_and_mirrored_to_the_callers_log() {
    let fx = Fixture::new().await;
    fx.connect(Duration::from_secs(10));
    let responder = fx.spawn_responder(Duration::ZERO).await;

    let parsed: ChannelSendResult =
        serde_json::from_value(fx.send("ping").await).expect("parse result");
    responder.abort();
    assert!(parsed.success, "{:?}", parsed.error);
    assert_eq!(parsed.response, "pong: ping");
    assert_eq!(parsed.kind.as_deref(), Some("principal"));
    assert_eq!(parsed.channel, format!("principal:{}", fx.target_did));
    assert!(
        !parsed.session_id.is_empty(),
        "caller-side child session id"
    );

    // The caller's log holds the full exchange: its root, then the reply
    // attributed to the target and parented on that root.
    let caller_peer = Subject::Principal(PrincipalDID(fx.target_did.clone()));
    let rows = dm_posted_rows(&fx.channel_port, &fx.caller, &caller_peer).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0], (fx.caller.id.0.clone(), None, "ping".to_string()));
    assert_eq!(rows[1].0, fx.target.id.0, "reply attributed to the target");
    assert!(
        rows[1].1.is_some(),
        "reply is parented on the outbound root"
    );
    assert_eq!(rows[1].2, "pong: ping");
}

/// Replies carry no correlation id, so the per-target await lock is what
/// keeps overlapping requests from claiming each other's replies.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn concurrent_requests_to_one_target_each_receive_their_own_reply() {
    let fx = Fixture::new().await;
    fx.connect(Duration::from_secs(10));
    let responder = fx.spawn_responder(Duration::from_millis(150)).await;

    let (a, b, c) = tokio::join!(fx.send("a"), fx.send("b"), fx.send("c"));
    responder.abort();
    for (sent, result) in [("a", a), ("b", b), ("c", c)] {
        let parsed: ChannelSendResult = serde_json::from_value(result).expect("parse result");
        assert!(parsed.success, "{sent}: {:?}", parsed.error);
        assert_eq!(
            parsed.response,
            format!("pong: {sent}"),
            "{sent} got another reply"
        );
    }
    // Each request lands exactly once on the target's channel: one root,
    // one reply. (A slug collision once resolved the caller's own DM to
    // the target's, double-posting every request after the first.)
    let target_peer = Subject::Principal(PrincipalDID(fx.caller_did.clone()));
    let rows = dm_posted_rows(&fx.channel_port, &fx.target, &target_peer).await;
    let roots: Vec<_> = rows
        .iter()
        .filter(|r| r.1.is_none())
        .map(|r| r.2.as_str())
        .collect();
    assert_eq!(roots.len(), 3, "one root per request: {rows:?}");
    assert_eq!(rows.len(), 6, "one reply per request: {rows:?}");
}

fn remote_resolution(
    exposure: crate::tunnel::hub_directory::ResolvedExposure,
    agent_did: &str,
) -> crate::tunnel::hub_directory::AgentResolution {
    crate::tunnel::hub_directory::AgentResolution {
        runtime_id: REMOTE_RUNTIME.into(),
        instance_id: "inst-remote".into(),
        agent_did: agent_did.into(),
        owner_principal: Subject::Public,
        exposure,
    }
}

/// Play the remote runtime: for each outbound channel event (a fanned-out
/// root post), mirror a `pong: <text>` reply from the remote principal
/// into the caller's DM channel, as the inbound tunnel mirror would.
/// Records every outbound message kind.
fn spawn_remote_peer(
    fx: &Fixture,
    mut outbound: tokio::sync::mpsc::Receiver<crate::tunnel::TunnelMessage>,
) -> (
    tokio::task::JoinHandle<()>,
    Arc<std::sync::Mutex<Vec<&'static str>>>,
) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_w = Arc::clone(&seen);
    let store = Arc::clone(fx.tunnel_port.local());
    let caller = Subject::from(&fx.caller.id);
    let caller_raw = fx.caller.id.0.clone();
    let channel = ChannelId::for_principal(REMOTE_TARGET);
    let handle = tokio::spawn(async move {
        let mut answered = 0usize;
        while let Some(message) = outbound.recv().await {
            match message {
                crate::tunnel::TunnelMessage::TunnelChannelInvite { .. } => {
                    seen_w.lock().unwrap().push("invite");
                }
                crate::tunnel::TunnelMessage::TunnelChannelEvent { .. } => {
                    seen_w.lock().unwrap().push("event");
                    let roots: Vec<_> = store
                        .peek_with_ids(&channel, &Checkpoint::default())
                        .await
                        .unwrap()
                        .into_iter()
                        .filter_map(|(id, ev)| match ev {
                            ChannelEvent::Posted {
                                author,
                                parent: None,
                                text,
                                ..
                            } if author == caller_raw => Some((id, text)),
                            _ => None,
                        })
                        .collect();
                    for (id, text) in roots.into_iter().skip(answered) {
                        store
                            .post_attributed(
                                &channel,
                                &caller,
                                REMOTE_TARGET,
                                PostMsg::reply(id, format!("pong: {text}")),
                            )
                            .await
                            .unwrap();
                        answered += 1;
                    }
                }
                _ => {}
            }
        }
    });
    (handle, seen)
}

/// Remote principal: first contact invites the target's runtime over the
/// tunnel, the root post fans out, and the reply mirrored back into the
/// caller's DM channel is returned. Later requests do not re-invite.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn remote_request_invites_once_fans_out_and_returns_the_mirrored_reply() {
    use crate::tunnel::hub_directory::ResolvedExposure;
    let fx = Fixture::new().await;
    let directory = Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::new());
    directory.register_did(
        REMOTE_TARGET,
        remote_resolution(ResolvedExposure::Public, REMOTE_TARGET),
    );
    let outbound = fx.connect_remote(directory, Duration::from_secs(10)).await;
    let (peer, seen) = spawn_remote_peer(&fx, outbound);

    let first = fx.send_to(REMOTE_TARGET, "ping").await;
    assert!(first.success, "{:?}", first.error);
    assert_eq!(first.response, "pong: ping");
    assert_eq!(first.channel, format!("principal:{REMOTE_TARGET}"));
    let second = fx.send_to(REMOTE_TARGET, "again").await;
    assert!(second.success, "{:?}", second.error);
    assert_eq!(second.response, "pong: again");
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
        .list_remote_members(&ChannelId::for_principal(REMOTE_TARGET))
        .await
        .unwrap();
    assert_eq!(remote_members.len(), 1);
    assert_eq!(remote_members[0].runtime_id, REMOTE_RUNTIME);
}

/// Without a reply the remote request fails with a timeout naming the
/// remote runtime; the invite routing state is still recorded.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn remote_request_without_a_reply_times_out_cleanly() {
    use crate::tunnel::hub_directory::ResolvedExposure;
    let fx = Fixture::new().await;
    let directory = Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::new());
    directory.register_did(
        REMOTE_TARGET,
        remote_resolution(ResolvedExposure::Public, REMOTE_TARGET),
    );
    let _outbound = fx
        .connect_remote(directory, Duration::from_millis(200))
        .await;
    let result = fx.send_to(REMOTE_TARGET, "ping").await;
    assert!(!result.success);
    let error = result.error.unwrap();
    assert!(error.contains("timed out"), "{error}");
    assert!(error.contains(REMOTE_RUNTIME), "{error}");
}

/// Directory refusals and unusable resolutions are structured errors,
/// and nothing is sent over the tunnel.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial]
async fn remote_resolution_failures_are_structured_errors() {
    use crate::tunnel::hub_directory::DirectoryErrorKind;
    use crate::tunnel::hub_directory::ResolvedExposure;
    let fx = Fixture::new().await;
    let directory = Arc::new(crate::tunnel::hub_directory::FakeAgentDirectory::new());
    directory.register_did_err("did:key:missing", DirectoryErrorKind::NotFound);
    directory.register_did_err("did:key:forbidden", DirectoryErrorKind::Forbidden);
    directory.register_did(
        "did:key:hidden",
        remote_resolution(ResolvedExposure::Unexposed, "did:key:hidden"),
    );
    directory.register_did(
        "did:key:nodid",
        remote_resolution(ResolvedExposure::Public, ""),
    );
    let mut outbound = fx
        .connect_remote(directory, Duration::from_millis(200))
        .await;

    for (target, needle) in [
        ("did:key:missing", "not found in hub directory"),
        ("did:key:forbidden", "denied resolution"),
        ("did:key:hidden", "unexposed"),
        ("did:key:nodid", "empty target DID"),
    ] {
        let result = fx.send_to(target, "ping").await;
        assert!(!result.success, "{target}");
        let error = result.error.unwrap();
        assert!(error.contains(needle), "{target}: {error}");
    }
    assert!(outbound.try_recv().is_err(), "nothing sent over the tunnel");
}
