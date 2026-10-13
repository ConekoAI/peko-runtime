//! Explicit, scoped actor discovery and name resolution.
use crate::extensions::framework::core::ExtensionServices;
use crate::tunnel::CrossRuntimeA2aCtx;
use async_trait::async_trait;
use peko_channel::{ActorInfo, ChannelPort};
use peko_subject::{PrincipalId, Subject};
use peko_tools_core::{Tool, ToolContext};
use serde_json::{json, Value};
use std::collections::BTreeMap;
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

    fn tool_parameters() -> Value {
        peko_tools_core::schema::action_schema(&[
            (
                "list",
                json!({"type":"object", "properties":{}, "additionalProperties":false}),
            ),
            (
                "resolve",
                json!({"type":"object", "properties":{
                "reference":{"type":"string", "minLength":1,
                    "description":"Explicit kind:local:<id> or kind:hub:<id> actor reference."}
            }, "required":["reference"], "additionalProperties":false}),
            ),
        ])
    }

    fn validate_params(params: &Value) -> anyhow::Result<()> {
        static SCHEMA: std::sync::OnceLock<crate::tools::action_schema::ActionSchema> =
            std::sync::OnceLock::new();
        SCHEMA
            .get_or_init(|| {
                crate::tools::action_schema::ActionSchema::new("Contacts", Self::tool_parameters())
            })
            .validate(params)
    }

    async fn discover(
        &self,
        id: &PrincipalId,
    ) -> anyhow::Result<(BTreeMap<String, ActorInfo>, Vec<Value>)> {
        let mut actors = self
            .port
            .known_contacts(id)
            .await?
            .into_iter()
            .map(|actor| (actor.address.clone(), actor))
            .collect::<BTreeMap<_, _>>();
        let me = self.port.actor_info(&Subject::from(id)).await;
        actors.insert(me.address.clone(), me);
        let mut channels = Vec::new();
        for channel in self.port.list_for_principal(id).await? {
            let participants = self.port.participants(&channel).await?;
            for actor in &participants {
                actors.insert(actor.address.clone(), actor.clone());
            }
            channels.push(json!({"channel":channel,"participants":participants,
                "binding":self.port.binding_for(&channel,id).await?}));
        }
        Ok((actors, channels))
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
        "Discover your contacts and conversations. list returns your own actor, current participants of your channels, and principals you explicitly learned, with canonical addresses, known names and mention tokens. resolve accepts kind:local:<id> or kind:hub:<id>; use its canonical address with ChannelSend.to. You can learn a locally hosted principal by its exact local ID or a registered principal by its Hub ID. User/visitor resolution is limited to your channel participants; their opaque IDs are never inferred from names. Learned contacts are private to each principal and survive restarts.".into()
    }
    fn parameters(&self) -> Value {
        Self::tool_parameters()
    }
    async fn execute(&self, params: Value) -> anyhow::Result<Value> {
        Self::validate_params(&params)?;
        anyhow::bail!("Contacts requires principal context")
    }
    async fn execute_with_context(
        &self,
        params: Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<Value> {
        Self::validate_params(&params)?;
        let id = PrincipalId(
            ctx.principal_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Contacts requires principal context"))?,
        );
        match params.get("action").and_then(Value::as_str) {
            Some("list") => {
                let (actors, channels) = self.discover(&id).await?;
                Ok(json!({"actors":actors.into_values().collect::<Vec<_>>(),"channels":channels}))
            }
            Some("resolve") => {
                let reference = params
                    .get("reference")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Contacts resolve requires reference"))?;
                let subject: Subject = reference.parse()?;
                let identifier = subject.subject_id();
                let (scope, identifier) = identifier
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("Use kind:local:<id> or kind:hub:<id>"))?;
                anyhow::ensure!(
                    matches!(scope, "local" | "hub") && !identifier.is_empty(),
                    "Use kind:local:<id> or kind:hub:<id>"
                );
                let (actors, _) = self.discover(&id).await?;
                if let Some(actor) = actors.values().find(|actor| {
                    actor.address == reference || actor.hub_address().as_deref() == Some(reference)
                }) {
                    return Ok(json!({"actor":actor}));
                }
                anyhow::ensure!(
                    matches!(subject, Subject::Principal(_)),
                    "Actor is not a contact of this principal"
                );
                let context = self
                    .context()
                    .ok_or_else(|| anyhow::anyhow!("Actor directory is unavailable"))?;
                if scope == "local" {
                    anyhow::ensure!(context.principal_manager.get(PrincipalId(identifier.into())).await.is_some(),
                        "Unknown local principal; use principal:hub:<id> to learn a remote principal");
                    let actor = self.port.resolve_actor(reference).await?;
                    self.port.remember_contact(&id, &actor.subject()).await?;
                    return Ok(json!({"actor":actor}));
                }
                let hub_id = reference.strip_prefix("principal:hub:").ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown local actor; use kind:local:<id> or principal:hub:<id>"
                    )
                })?;
                let caller = context
                    .principal_manager
                    .get(id.clone())
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
                anyhow::ensure!(
                    !result.agent_did.is_empty(),
                    "Directory returned an empty target signing DID"
                );
                anyhow::ensure!(
                    !result.identity_key.is_empty(),
                    "Directory returned no immutable actor key"
                );
                anyhow::ensure!(
                    !result.runtime_id.is_empty(),
                    "Directory returned no target runtime"
                );
                anyhow::ensure!(
                    !result.local_id.is_empty(),
                    "Directory returned no local actor ID"
                );
                anyhow::ensure!(
                    result.exposure != crate::tunnel::hub_directory::ResolvedExposure::LocalOnly,
                    "Target is local_only"
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
                self.port.remember_contact(&id, &actor.subject()).await?;
                Ok(
                    json!({"actor":actor,"runtime_id":result.runtime_id,"local":result.runtime_id==context.caller_runtime_id}),
                )
            }
            _ => anyhow::bail!("Contacts action must be list or resolve"),
        }
    }
}

#[cfg(test)]
#[path = "contacts_tests.rs"]
mod tests;
