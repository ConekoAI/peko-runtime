//! Explicit, scoped actor discovery and name resolution.
use crate::extensions::framework::core::ExtensionServices;
use crate::tunnel::CrossRuntimeA2aCtx;
use async_trait::async_trait;
use peko_channel::ChannelPort;
use peko_subject::{PrincipalId, Subject};
use peko_tools_core::{Tool, ToolContext};
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

pub struct ContactsTool {
    port: Arc<dyn ChannelPort>,
    services: Weak<ExtensionServices>,
}
impl ContactsTool {
    pub fn new(port: Arc<dyn ChannelPort>, services: Weak<ExtensionServices>) -> Self {
        Self { port, services }
    }
    fn context(&self) -> Option<Arc<CrossRuntimeA2aCtx>> {
        Arc::downcast(self.services.upgrade()?.cross_runtime_a2a_ctx()?).ok()
    }
}
#[async_trait]
impl Tool for ContactsTool {
    fn parallelizable(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "Contacts"
    }
    fn description(&self) -> String {
        "Discover known actors and conversations. list returns locally hosted principals and participants of your channels, with canonical addresses, known names and mention tokens. resolve accepts principal:local:<id> or principal:hub:<id>; use its canonical address with ChannelSend.to. User/visitor IDs are opaque identities from authenticated ingress, never inferred from names. This does not enumerate all Hub users.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object", "properties": {"action":{"type":"string","enum":["list","resolve"]}, "reference":{"type":"string"}}, "required":["action"], "oneOf":[{"properties":{"action":{"const":"list"}},"not":{"required":["reference"]}},{"properties":{"action":{"const":"resolve"}},"required":["reference"]}]})
    }
    async fn execute(&self, _params: Value) -> anyhow::Result<Value> {
        anyhow::bail!("Contacts requires principal context")
    }
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        let id = PrincipalId(
            ctx.principal_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Contacts requires principal context"))?,
        );
        match params.get("action").and_then(Value::as_str) {
            Some("list") => {
                let mut actors = self
                    .port
                    .known_actors()
                    .await?
                    .into_iter()
                    .map(|a| (a.address.clone(), a))
                    .collect::<std::collections::BTreeMap<_, _>>();
                if let Some(context) = self.context() {
                    for principal in context.principal_manager.list_all().await {
                        let actor = self.port.actor_info(&Subject::from(&principal.id)).await;
                        actors.insert(actor.address.clone(), actor);
                    }
                }
                let mut channels = Vec::new();
                for channel in self.port.list_for_principal(&id).await? {
                    let participants = self.port.participants(&channel).await?;
                    for actor in &participants {
                        actors.insert(actor.address.clone(), actor.clone());
                    }
                    channels.push(json!({"channel":channel,"participants":participants,"binding":self.port.binding_for(&channel,&id).await?}));
                }
                Ok(json!({"actors":actors.into_values().collect::<Vec<_>>(),"channels":channels}))
            }
            Some("resolve") => {
                let reference = params
                    .get("reference")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Contacts resolve requires reference"))?;
                if let Ok(actor) = self.port.resolve_actor(reference).await {
                    return Ok(json!({"actor":actor}));
                }
                let context = self
                    .context()
                    .ok_or_else(|| anyhow::anyhow!("Actor directory is unavailable"))?;
                let hub_id = reference.strip_prefix("principal:hub:").ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown local actor; use kind:local:<id> or principal:hub:<id>"
                    )
                })?;
                let caller = context
                    .principal_manager
                    .get(id)
                    .await
                    .ok_or_else(|| anyhow::anyhow!("principal not loaded"))?;
                let caller_profile = self.port.actor_info(&Subject::from(&caller.id)).await;
                let result = if caller_profile.hub_id.is_some() {
                    context
                        .directory
                        .resolve_by_hub_id_as(hub_id, &caller.did().await.0)
                        .await?
                } else {
                    context.directory.resolve_by_hub_id(hub_id).await?
                };
                anyhow::ensure!(
                    result.hub_id == hub_id,
                    "Directory substituted a different Hub identity"
                );
                let local_id = if result.runtime_id == context.caller_runtime_id {
                    result.local_id.clone()
                } else {
                    format!("local_{hub_id}")
                };
                let actor = self
                    .port
                    .register_actor(
                        &Subject::Principal(local_id.into()),
                        Some(hub_id),
                        Some(&result.agent_did),
                        Some(&result.identity_key),
                        result.name.as_deref(),
                        if result.exposure == crate::tunnel::hub_directory::ResolvedExposure::Public
                        {
                            "public"
                        } else {
                            "private"
                        },
                    )
                    .await?;
                Ok(
                    json!({"actor":actor,"runtime_id":result.runtime_id,"local":result.runtime_id==context.caller_runtime_id}),
                )
            }
            _ => anyhow::bail!("Contacts action must be list or resolve"),
        }
    }
}
