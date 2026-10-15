use super::*;
use crate::tools::builtin::test_harness::{Caller, ToolHarness};
use crate::tools::runtime::ToolingRuntime;
use crate::tunnel::hub_directory::{
    AgentDirectory, AgentResolution, DirectoryError, ResolvedExposure,
};
use peko_channel::{ChannelConfig, ChannelStore, CreateOpts};
use std::sync::Mutex;

fn context(principal: &PrincipalId) -> ToolContext {
    let mut ctx = ToolContext::default_for_tool("Contacts")
        .with_acting_for(peko_tools_core::ActingFor::runtime());
    ctx.principal_id = Some(principal.0.clone());
    ctx
}

async fn register(port: &dyn ChannelPort, actor: &Subject, name: &str) -> ActorInfo {
    port.register_actor(
        actor,
        Some(&format!("hub-{name}")),
        None,
        Some(&format!("key-{name}")),
        Some(name),
        "private",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn list_and_alias_resolution_are_scoped_to_current_channels() {
    let temp = tempfile::tempdir().unwrap();
    let port = Arc::new(ChannelStore::new(ChannelConfig {
        runtime_dir: temp.path().into(),
        shared_dir: None,
    }));
    let a = PrincipalId("prin_a".into());
    let b = PrincipalId("prin_b".into());
    let me = register(&*port, &Subject::from(&a), "Alice").await;
    register(&*port, &Subject::from(&b), "Bob").await;
    let human = register(&*port, &Subject::User("human".into()), "Human").await;
    let visitor = register(&*port, &Subject::Visitor("visitor".into()), "Visitor").await;
    let unrelated = register(&*port, &Subject::User("unrelated".into()), "Unrelated").await;
    let dm = port
        .ensure_dm(&a, &human.subject(), "/human")
        .await
        .unwrap();
    let group = port.create(&a, CreateOpts::runtime("team")).await.unwrap();
    port.invite(&group, &a, &visitor.subject()).await.unwrap();
    port.ensure_dm(&b, &unrelated.subject(), "/unrelated")
        .await
        .unwrap();
    let tool = ContactsTool::new(port.clone(), Weak::new());
    let ctx = context(&a);
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &ctx)
        .await
        .unwrap();
    let actors = listed["actors"].as_array().unwrap();
    assert_eq!(actors.len(), 3, "{listed}");
    for actor in [&me, &human, &visitor] {
        assert!(actors.contains(&serde_json::to_value(actor).unwrap()));
    }
    assert_eq!(listed["channels"].as_array().unwrap().len(), 2);
    let dm_result = listed["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["channel"] == dm.as_str())
        .unwrap();
    assert_eq!(dm_result["binding"], "/human");
    assert_eq!(dm_result["participants"].as_array().unwrap().len(), 2);
    assert_eq!(
        listed["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["channel"] == group.as_str())
            .unwrap()["binding"],
        Value::Null
    );
    assert_eq!(
        actors
            .iter()
            .find(|actor| actor["address"] == me.address)
            .unwrap()["mention"],
        format!("@{}", me.address)
    );
    assert!(!listed.to_string().contains("identity_key"));
    assert!(!listed.to_string().contains("signing_did"));
    for actor in [&me, &human, &visitor] {
        for reference in [actor.address.clone(), actor.hub_address().unwrap()] {
            let resolved = tool
                .execute_with_context(json!({"action":"resolve", "reference":reference}), &ctx)
                .await
                .unwrap();
            assert_eq!(resolved["actor"], serde_json::to_value(actor).unwrap());
        }
    }
    for reference in [&unrelated.address, &unrelated.hub_address().unwrap()] {
        assert!(tool
            .execute_with_context(json!({"action":"resolve", "reference":reference}), &ctx)
            .await
            .unwrap_err()
            .to_string()
            .contains("not a contact"));
    }
    port.leave(&group, &a).await.unwrap();
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &ctx)
        .await
        .unwrap();
    assert_eq!(listed["actors"].as_array().unwrap().len(), 2);
    assert!(!listed.to_string().contains(&visitor.address));
}

/// ADR-078 D4: a run acting for anyone but the owner sees only its own
/// conversation — not the principal's other channels, their rosters, or
/// the contacts it learned — and cannot learn new contacts.
#[tokio::test]
async fn non_owner_runs_see_only_their_own_conversation() {
    let temp = tempfile::tempdir().unwrap();
    let port = Arc::new(ChannelStore::new(ChannelConfig {
        runtime_dir: temp.path().into(),
        shared_dir: None,
    }));
    let a = PrincipalId("prin_a".into());
    let b = PrincipalId("prin_b".into());
    let me = register(&*port, &Subject::from(&a), "Alice").await;
    let learned = register(&*port, &Subject::from(&b), "Bob").await;
    let guest = register(&*port, &Subject::User("guest".into()), "Guest").await;
    let friend = register(&*port, &Subject::User("friend".into()), "Friend").await;
    let visitor = register(&*port, &Subject::Visitor("visitor".into()), "Visitor").await;
    port.remember_contact(&a, &learned.subject()).await.unwrap();
    let guest_dm = port
        .ensure_dm(&a, &guest.subject(), "/guest")
        .await
        .unwrap();
    let friend_dm = port
        .ensure_dm(&a, &friend.subject(), "/friend")
        .await
        .unwrap();
    let group = port.create(&a, CreateOpts::runtime("team")).await.unwrap();
    port.invite(&group, &a, &visitor.subject()).await.unwrap();
    let tool = ContactsTool::new(port.clone(), Weak::new());

    let mut ctx =
        ToolContext::default_for_tool("Contacts").with_acting_for(peko_tools_core::ActingFor {
            actor: guest.address.clone(),
            name: None,
            trust: peko_tools_core::Trust::Untrusted,
            compartment: peko_tools_core::Compartment::Conversation {
                channel: guest_dm.as_str().into(),
            },
            origin: peko_tools_core::Origin::Ingress { channel: None },
        });
    ctx.principal_id = Some(a.0.clone());
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &ctx)
        .await
        .unwrap();
    let channels = listed["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 1, "{listed}");
    assert_eq!(channels[0]["channel"], guest_dm.as_str());
    let actors = listed["actors"].as_array().unwrap();
    assert_eq!(actors.len(), 2, "{listed}");
    for actor in [&me, &guest] {
        assert!(actors.contains(&serde_json::to_value(actor).unwrap()));
    }
    let text = listed.to_string();
    for hidden in [&learned, &friend, &visitor] {
        assert!(!text.contains(&hidden.address), "{hidden:?} leaked: {text}");
    }
    for hidden in [friend_dm.as_str(), group.as_str()] {
        assert!(!text.contains(hidden), "{hidden} leaked: {text}");
    }

    // Resolution goes through the same scope, and never learns.
    let resolved = tool
        .execute_with_context(json!({"action":"resolve","reference":guest.address}), &ctx)
        .await
        .unwrap();
    assert_eq!(resolved["actor"], serde_json::to_value(&guest).unwrap());
    for hidden in [&friend, &visitor] {
        assert!(tool
            .execute_with_context(json!({"action":"resolve","reference":hidden.address}), &ctx)
            .await
            .unwrap_err()
            .to_string()
            .contains("not a contact"));
    }
    let err = tool
        .execute_with_context(
            json!({"action":"resolve","reference":learned.address}),
            &ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("only the owner"), "{err}");

    // A call without a stamp is unattributed: it sees no conversation.
    let mut bare = ToolContext::default_for_tool("Contacts");
    bare.principal_id = Some(a.0.clone());
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &bare)
        .await
        .unwrap();
    assert_eq!(listed["channels"], json!([]));
    assert_eq!(listed["actors"].as_array().unwrap().len(), 1);

    // The owner still sees everything.
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &context(&a))
        .await
        .unwrap();
    assert_eq!(listed["channels"].as_array().unwrap().len(), 3);
    assert!(listed.to_string().contains(&learned.address));
}

#[tokio::test]
async fn strict_arguments_and_principal_context_are_required() {
    let h = ToolHarness::new().await;
    let tool = ContactsTool::new(h.channels.clone(), Weak::new());
    for params in [
        json!({}),
        json!({"action":"invalid"}),
        json!({"action":false}),
        json!({"action":"resolve"}),
        json!({"action":"resolve","reference":""}),
        json!({"action":"resolve","reference":null}),
        json!({"action":"list","reference":"x"}),
        json!({"action":"list","limti":1}),
        json!({"action":"resolve","reference":"principal:hub:x","extra":true}),
    ] {
        h.call("Contacts", params.clone()).await.err("");
        assert!(tool
            .execute_with_context(params, &context(&PrincipalId("prin_a".into())))
            .await
            .is_err());
    }
    assert!(tool
        .execute(json!({"action":"list"}))
        .await
        .unwrap_err()
        .to_string()
        .contains("principal context"));
    assert!(tool
        .execute_with_context(
            json!({"action":"list"}),
            &ToolContext::default_for_tool("Contacts")
                .with_acting_for(peko_tools_core::ActingFor::runtime())
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("principal context"));
    let empty = h
        .call_as(
            &Caller::principal("prin_empty"),
            "Contacts",
            json!({"action":"list"}),
        )
        .await
        .ok();
    assert_eq!(empty["channels"], json!([]));
    assert_eq!(empty["actors"].as_array().unwrap().len(), 1);
    for reference in [
        "principal:hub:",
        "principal:local:",
        "principal:name",
        "public",
        "unknown:local:id",
    ] {
        assert!(tool
            .execute_with_context(
                json!({"action":"resolve","reference":reference}),
                &context(&PrincipalId("prin_a".into()))
            )
            .await
            .is_err());
    }
}

struct Directory {
    response: Mutex<AgentResolution>,
    calls: Mutex<Vec<Option<String>>>,
    forbidden: std::sync::atomic::AtomicBool,
}
impl Directory {
    fn result(&self, caller: Option<&str>) -> Result<AgentResolution, DirectoryError> {
        self.calls.lock().unwrap().push(caller.map(str::to_owned));
        if self.forbidden.load(std::sync::atomic::Ordering::SeqCst) {
            Err(DirectoryError::Forbidden)
        } else {
            Ok(self.response.lock().unwrap().clone())
        }
    }
}
#[async_trait]
impl AgentDirectory for Directory {
    async fn resolve_by_hub_id(&self, _id: &str) -> Result<AgentResolution, DirectoryError> {
        self.result(None)
    }
    async fn resolve_by_hub_id_as(
        &self,
        _id: &str,
        caller: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        self.result(Some(caller))
    }
    async fn resolve_by_handle(
        &self,
        _owner: &str,
        _name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        Err(DirectoryError::NotFound)
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    port: Arc<ChannelStore>,
    tooling: Arc<ToolingRuntime>,
    manager: Arc<crate::principal::PrincipalManager>,
    caller: Arc<crate::principal::Principal>,
    other: Arc<crate::principal::Principal>,
    directory: Arc<Directory>,
    tool: ContactsTool,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tooling = ToolingRuntime::standalone();
        let port = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: temp.path().join("runtime"),
            shared_dir: None,
        }));
        let tunnel_port = Arc::new(crate::tunnel::TunnelChannelPort::new(port.clone()));
        tooling.services().set_channel_port(tunnel_port.clone());
        let vault = Arc::new(crate::common::vault::Vault::for_test(
            temp.path(),
            "contacts-test-passphrase",
        ));
        let (resolver, _) = peko_providers::LlmResolver::mock(
            peko_providers::MockAdapter::new(),
            temp.path().join("models.toml"),
        )
        .await;
        let manager = Arc::new(
            crate::principal::PrincipalManager::with_path_resolver(
                paths,
                Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
                Arc::new(crate::principal::DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_identity_vault(vault)
            .with_tooling(tooling.clone())
            .with_resolver(resolver)
            .with_channel_port(tunnel_port.clone()),
        );
        let caller = manager
            .create(
                serde_json::from_value(json!({"name":"caller", "preferred_model_id":"mock"}))
                    .unwrap(),
            )
            .await
            .unwrap();
        let other = manager
            .create(
                serde_json::from_value(json!({"name":"other", "preferred_model_id":"mock"}))
                    .unwrap(),
            )
            .await
            .unwrap();
        for principal in [&caller, &other] {
            let did = principal.did().await.0;
            crate::tools::installation::install_principal_services(
                &tooling,
                &principal.id,
                crate::tools::installation::PrincipalBindings {
                    sessions_dir: None,
                    plan: None,
                    model_catalog: None,
                    caller_did: Some(&did),
                },
            )
            .await
            .unwrap();
        }
        let directory = Arc::new(Directory {
            response: Mutex::new(AgentResolution {
                name: Some("Remote".into()),
                hub_id: "remote-hub".into(),
                local_id: "prin_foreign".into(),
                identity_key: "remote-key".into(),
                runtime_id: "remote-runtime".into(),
                instance_id: "remote-instance".into(),
                agent_did: "did:key:remote".into(),
                owner_principal: peko_auth::Subject::Public,
                exposure: ResolvedExposure::Public,
            }),
            calls: Mutex::new(Vec::new()),
            forbidden: std::sync::atomic::AtomicBool::new(false),
        });
        tooling
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(CrossRuntimeA2aCtx {
                directory: directory.clone(),
                caller_runtime_id: "local-runtime".into(),
                principal_manager: manager.clone(),
                channel_port: tunnel_port.clone(),
            }));
        let tool = ContactsTool::new(tunnel_port, Arc::downgrade(tooling.services()));
        Self {
            temp,
            port,
            tooling,
            manager,
            caller,
            other,
            directory,
            tool,
        }
    }
    async fn resolve(&self, id: &PrincipalId, reference: &str) -> anyhow::Result<Value> {
        self.tool
            .execute_with_context(
                json!({"action":"resolve","reference":reference}),
                &context(id),
            )
            .await
    }
    async fn list(&self, id: &PrincipalId) -> Value {
        self.tool
            .execute_with_context(json!({"action":"list"}), &context(id))
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn learned_remote_contacts_persist_without_leaking_to_other_principals() {
    let fx = Fixture::new().await;
    assert_eq!(
        fx.list(&fx.caller.id).await["actors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut call = peko_engine::ToolCallSpec::new(
        "Contacts",
        json!({
            "action":"resolve", "reference":"principal:hub:remote-hub"
        }),
    );
    call.acting_for = Some(peko_tools_core::ActingFor::runtime());
    call.principal_id = Some(fx.caller.id.0.clone());
    let (_, resolved, success) = fx.tooling.dispatcher().execute(call).await.unwrap();
    assert!(success, "{resolved}");
    assert_eq!(resolved["actor"]["address"], "principal:hub:remote-hub");
    assert_eq!(resolved["actor"]["hub_id"], "remote-hub");
    assert!(resolved["actor"]["local_id"].is_null());
    assert_eq!(resolved["actor"]["registration"], "public");
    assert_eq!(resolved["runtime_id"], "remote-runtime");
    assert_eq!(resolved["local"], false);
    assert_eq!(*fx.directory.calls.lock().unwrap(), vec![None]);
    let cached = fx
        .resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .unwrap();
    assert_eq!(cached["actor"], resolved["actor"]);
    assert_eq!(fx.directory.calls.lock().unwrap().len(), 1);
    assert!(fx
        .resolve(&fx.caller.id, "principal:local:local_remote-hub")
        .await
        .is_err());
    assert_eq!(
        fx.list(&fx.caller.id).await["actors"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fx.list(&fx.other.id).await["actors"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(fx
        .resolve(&fx.other.id, "principal:local:local_remote-hub")
        .await
        .is_err());
    fx.directory
        .forbidden
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(fx
        .resolve(&fx.other.id, "principal:hub:remote-hub")
        .await
        .unwrap_err()
        .to_string()
        .contains("not permitted"));
    assert_eq!(fx.port.known_contacts(&fx.other.id).await.unwrap().len(), 0);
    let restarted = Arc::new(ChannelStore::new(ChannelConfig {
        runtime_dir: fx.temp.path().join("runtime"),
        shared_dir: None,
    }));
    let tool = ContactsTool::new(restarted, Weak::new());
    let listed = tool
        .execute_with_context(json!({"action":"list"}), &context(&fx.caller.id))
        .await
        .unwrap();
    assert!(listed["actors"]
        .as_array()
        .unwrap()
        .contains(&resolved["actor"]));
    assert!(
        fx.tooling
            .catalog()
            .get("Contacts", &fx.caller.id)
            .await
            .is_some(),
        "principal service installation"
    );
}

#[tokio::test]
async fn registered_callers_authenticate_directory_lookups() {
    let fx = Fixture::new().await;
    let config = fx.caller.config.read().await;
    fx.port
        .register_actor(
            &Subject::from(&fx.caller.id),
            Some("caller-hub"),
            Some(&fx.caller.did().await.0),
            config.actor_key.as_deref(),
            Some("caller"),
            "private",
        )
        .await
        .unwrap();
    drop(config);
    fx.directory.response.lock().unwrap().exposure = ResolvedExposure::Private;
    let result = fx
        .resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .unwrap();
    assert_eq!(result["actor"]["registration"], "private");
    let caller_did = fx.caller.did().await.0;
    assert_eq!(*fx.directory.calls.lock().unwrap(), vec![Some(caller_did)]);
}

#[tokio::test]
async fn invalid_directory_responses_do_not_poison_the_registry() {
    let fx = Fixture::new().await;
    let valid = fx.directory.response.lock().unwrap().clone();
    let original = std::fs::read(fx.temp.path().join("runtime/actors.json")).unwrap();
    for field in [
        "hub_id",
        "identity_key",
        "agent_did",
        "runtime_id",
        "local_id",
        "exposure",
    ] {
        let mut bad = valid.clone();
        match field {
            "hub_id" => bad.hub_id = "substituted-hub".into(),
            "identity_key" => bad.identity_key.clear(),
            "agent_did" => bad.agent_did.clear(),
            "runtime_id" => bad.runtime_id.clear(),
            "local_id" => bad.local_id.clear(),
            "exposure" => bad.exposure = ResolvedExposure::LocalOnly,
            _ => unreachable!(),
        }
        *fx.directory.response.lock().unwrap() = bad;
        assert!(
            fx.resolve(&fx.caller.id, "principal:hub:remote-hub")
                .await
                .is_err(),
            "{field}"
        );
        assert_eq!(
            std::fs::read(fx.temp.path().join("runtime/actors.json")).unwrap(),
            original,
            "{field} must not write"
        );
    }
    *fx.directory.response.lock().unwrap() = valid;
    fx.resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .unwrap();
    assert_eq!(
        fx.port
            .resolve_actor("principal:hub:remote-hub")
            .await
            .unwrap()
            .identity_key,
        "remote-key"
    );
}

#[tokio::test]
async fn exact_local_lookup_learns_only_for_the_requesting_principal() {
    let fx = Fixture::new().await;
    let reference = format!("principal:local:{}", fx.other.id.0);
    let result = fx.resolve(&fx.caller.id, &reference).await.unwrap();
    assert_eq!(result["actor"]["address"], reference);
    assert_eq!(result["actor"]["registration"], "local_only");
    assert!(fx.directory.calls.lock().unwrap().is_empty());
    assert_eq!(
        fx.port.known_contacts(&fx.caller.id).await.unwrap().len(),
        1
    );
    assert!(fx
        .port
        .known_contacts(&fx.other.id)
        .await
        .unwrap()
        .is_empty());
    fx.directory.response.lock().unwrap().runtime_id = "local-runtime".into();
    fx.directory.response.lock().unwrap().local_id = fx.other.id.0.clone();
    let other_config = fx.other.config.read().await;
    fx.directory.response.lock().unwrap().identity_key = other_config.actor_key.clone().unwrap();
    let other_did = fx.other.did().await.0;
    fx.directory.response.lock().unwrap().agent_did = other_did;
    drop(other_config);
    let resolved = fx
        .resolve(&fx.other.id, "principal:hub:remote-hub")
        .await
        .unwrap();
    assert_eq!(resolved["actor"]["address"], reference);
    assert_eq!(resolved["local"], true);
    assert_eq!(fx.manager.list_all().await.len(), 2);
}

#[tokio::test]
async fn storage_errors_are_not_treated_as_directory_misses() {
    let fx = Fixture::new().await;
    std::fs::write(fx.temp.path().join("runtime/actors.json"), "invalid json").unwrap();
    assert!(fx
        .resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .is_err());
    assert!(fx.directory.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_runtime_directory_claims_must_match_a_hosted_identity() {
    let fx = Fixture::new().await;
    let path = fx.temp.path().join("runtime/actors.json");
    let before = std::fs::read(&path).unwrap();
    fx.directory.response.lock().unwrap().runtime_id = "local-runtime".into();
    let unknown = fx
        .resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .unwrap_err();
    assert!(unknown.to_string().contains("unknown local principal"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    fx.directory.response.lock().unwrap().local_id = fx.other.id.0.clone();
    let mismatched = fx
        .resolve(&fx.caller.id, "principal:hub:remote-hub")
        .await
        .unwrap_err();
    assert!(mismatched
        .to_string()
        .contains("does not match the hosted principal"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(fx
        .port
        .known_contacts(&fx.caller.id)
        .await
        .unwrap()
        .is_empty());
}
