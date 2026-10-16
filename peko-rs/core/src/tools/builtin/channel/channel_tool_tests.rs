use super::*;
use crate::tools::runtime::ToolingRuntime;
use peko_channel::{Binding, ChannelConfig, ChannelStore};
use peko_tools_core::{Compartment, Origin, Trust};

struct Fixture {
    _temp: tempfile::TempDir,
    /// Holds the services the tools reach through a weak handle.
    _tooling: Arc<ToolingRuntime>,
    port: Arc<dyn ChannelPort>,
    tool: ChannelTool,
    contacts: super::super::contacts::ContactsTool,
    caller: Arc<Principal>,
    friend: Arc<Principal>,
    stranger: Arc<Principal>,
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
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: temp.path().join("runtime"),
            shared_dir: None,
        }));
        let tunnel_port = Arc::new(crate::tunnel::TunnelChannelPort::new(store));
        let port: Arc<dyn ChannelPort> = tunnel_port.clone();
        tooling.services().set_channel_port(tunnel_port.clone());
        let vault = Arc::new(crate::common::vault::Vault::for_test(
            temp.path(),
            "channel-tool-test-passphrase",
        ));
        let manager = Arc::new(
            PrincipalManager::with_path_resolver(
                paths,
                Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
                Arc::new(crate::principal::DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_identity_vault(vault)
            .with_tooling(tooling.clone())
            .with_channel_port(Arc::clone(&port)),
        );
        let mut principals = Vec::new();
        for name in ["caller", "friend", "stranger"] {
            let principal = manager
                .create(serde_json::from_value(json!({ "name": name })).unwrap())
                .await
                .unwrap();
            principals.push(principal);
        }
        tooling
            .services()
            .set_cross_runtime_a2a_ctx(Arc::new(CrossRuntimeA2aCtx {
                directory: Arc::new(crate::tunnel::local_directory::OfflineDirectory),
                caller_runtime_id: "local-runtime".into(),
                principal_manager: manager,
                channel_port: tunnel_port,
            }));
        let services = Arc::downgrade(tooling.services());
        let stranger = principals.pop().unwrap();
        let friend = principals.pop().unwrap();
        let caller = principals.pop().unwrap();
        Self {
            _temp: temp,
            _tooling: tooling,
            tool: ChannelTool::new(Arc::clone(&port), services.clone()),
            contacts: super::super::contacts::ContactsTool::new(Arc::clone(&port), services),
            port,
            caller,
            friend,
            stranger,
        }
    }

    fn owner_ctx(&self) -> ToolContext {
        let mut ctx =
            ToolContext::default_for_tool(CHANNEL_TOOL_NAME).with_acting_for(ActingFor::runtime());
        ctx.principal_id = Some(self.caller.id.0.clone());
        ctx
    }

    fn guest_ctx(&self, channel: &ChannelId, trust: Trust) -> ToolContext {
        let mut ctx = ToolContext::default_for_tool(CHANNEL_TOOL_NAME).with_acting_for(ActingFor {
            actor: "user:hub:guest".into(),
            name: None,
            trust,
            compartment: Compartment::Conversation {
                channel: channel.as_str().into(),
            },
            origin: Origin::Ingress { channel: None },
        });
        ctx.principal_id = Some(self.caller.id.0.clone());
        ctx
    }

    async fn call(&self, params: Value) -> anyhow::Result<Value> {
        self.tool
            .execute_with_context(params, &self.owner_ctx())
            .await
    }

    async fn address(&self, principal: &Principal) -> String {
        self.port
            .actor_info(&ActorRef::from(&principal.id))
            .await
            .address
    }

    async fn owner_address(&self) -> String {
        let owner = self.caller.config.read().await.owner.clone();
        self.port.actor_info(&owner).await.address
    }

    /// The owner's run learns `principal` into the caller's contacts.
    async fn learn(&self, principal: &Principal) {
        let mut ctx = self.owner_ctx();
        ctx.tool_name = "Contacts".into();
        self.contacts
            .execute_with_context(
                json!({"action": "resolve", "reference": self.address(principal).await}),
                &ctx,
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_owner_creates_a_group_with_its_owner_and_contacts() {
    let fx = Fixture::new().await;
    fx.learn(&fx.friend).await;
    let owner = fx.owner_address().await;
    let friend = fx.address(&fx.friend).await;

    let created = fx
        .call(
            json!({"action": "create", "name": "launch", "slug": "launch",
            "members": [owner, friend]}),
        )
        .await
        .unwrap();
    assert_eq!(created["channel"], "group:launch");
    assert_eq!(created["invited"], json!([owner, friend]));

    let channel = ChannelId::for_group("launch");
    let members = fx.port.list_members(&channel).await.unwrap();
    assert_eq!(members.len(), 3, "{members:?}");
    // The creator's group session is provisioned and bound (D2).
    let binding = fx
        .port
        .binding_for(&channel, &fx.caller.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(binding.wake, WakePolicy::Mention);

    let info = fx
        .call(json!({"action": "info", "channel": "group:launch"}))
        .await
        .unwrap();
    assert_eq!(info["kind"], "group");
    assert_eq!(info["creator"], fx.address(&fx.caller).await);
    assert_eq!(info["participants"].as_array().unwrap().len(), 3);
    assert_eq!(info["you"]["wake"], "mention");
    assert_eq!(info["you"]["session"], binding.session);
}

#[tokio::test]
async fn invitees_are_the_owner_and_learned_principals_only() {
    let fx = Fixture::new().await;
    fx.call(json!({"action": "create", "name": "team", "slug": "team"}))
        .await
        .unwrap();

    let stranger = fx.address(&fx.stranger).await;
    let err = fx
        .call(json!({"action": "invite", "channel": "group:team", "actor": stranger}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not in your contacts"), "{err}");

    let err = fx
        .call(json!({"action": "invite", "channel": "group:team", "actor": "user:local:someone"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("only its owner"), "{err}");

    fx.learn(&fx.stranger).await;
    let invited = fx
        .call(json!({"action": "invite", "channel": "group:team", "actor": stranger}))
        .await
        .unwrap();
    assert_eq!(invited["invited"], json!([stranger]));
}

#[tokio::test]
async fn wake_and_leave_change_only_the_callers_membership() {
    let fx = Fixture::new().await;
    fx.call(json!({"action": "create", "name": "ops", "slug": "ops"}))
        .await
        .unwrap();
    let channel = ChannelId::for_group("ops");

    let set = fx
        .call(json!({"action": "wake", "channel": "group:ops", "policy": "off"}))
        .await
        .unwrap();
    assert_eq!(set["wake"], "off");
    assert_eq!(
        fx.port
            .binding_for(&channel, &fx.caller.id)
            .await
            .unwrap()
            .map(|b: Binding| b.wake),
        Some(WakePolicy::Off)
    );

    fx.call(json!({"action": "leave", "channel": "group:ops"}))
        .await
        .unwrap();
    assert!(fx.port.list_members(&channel).await.unwrap().is_empty());
    let err = fx
        .call(json!({"action": "info", "channel": "group:ops"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a member"), "{err}");
}

#[tokio::test]
async fn a_dm_is_read_only_here() {
    let fx = Fixture::new().await;
    let dm = fx
        .port
        .ensure_dm(&fx.caller.id, &ActorRef::User("bob".into()), "/user-bob")
        .await
        .unwrap();
    let info = fx
        .call(json!({"action": "info", "channel": dm.as_str()}))
        .await
        .unwrap();
    assert_eq!(info["kind"], "dm");
    assert_eq!(info["you"]["wake"], "human");
    for params in [
        json!({"action": "leave", "channel": dm.as_str()}),
        json!({"action": "wake", "channel": dm.as_str(), "policy": "off"}),
        json!({"action": "invite", "channel": dm.as_str(), "actor": fx.owner_address().await}),
    ] {
        let err = fx.call(params).await.unwrap_err().to_string();
        assert!(err.contains("is a DM"), "{err}");
    }
}

/// D6: a non-owner run reaches only its own conversation's channel, and
/// its `info` never names the principal's session.
#[tokio::test]
async fn non_owner_runs_stay_in_their_conversation() {
    let fx = Fixture::new().await;
    for slug in ["here", "elsewhere"] {
        fx.call(json!({"action": "create", "name": slug, "slug": slug}))
            .await
            .unwrap();
    }
    let here = ChannelId::for_group("here");
    let ctx = fx.guest_ctx(&here, Trust::Trusted);

    let err = fx
        .tool
        .execute_with_context(
            json!({"action": "info", "channel": "group:elsewhere"}),
            &ctx,
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not found"), "{err}");

    let info = fx
        .tool
        .execute_with_context(json!({"action": "info", "channel": "group:here"}), &ctx)
        .await
        .unwrap();
    assert!(info["you"].get("session").is_none(), "{info}");

    fx.tool
        .execute_with_context(
            json!({"action": "wake", "channel": "group:here", "policy": "human"}),
            &ctx,
        )
        .await
        .unwrap();
    fx.tool
        .execute_with_context(json!({"action": "leave", "channel": "group:here"}), &ctx)
        .await
        .unwrap();
    assert!(fx.port.list_members(&here).await.unwrap().is_empty());
}
