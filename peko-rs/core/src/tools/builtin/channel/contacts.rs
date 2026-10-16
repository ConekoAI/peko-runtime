//! Explicit, scoped actor discovery and name resolution.
use crate::extensions::framework::core::ExtensionServices;
use crate::tunnel::CrossRuntimeA2aCtx;
use async_trait::async_trait;
use peko_channel::{ActorInfo, ChannelPort};
use peko_subject::{ActorRef, PrincipalId};
use peko_tools_core::{ActingFor, Tool, ToolContext};
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

    /// The principal's contact book, when the principal is loaded here.
    async fn book(
        &self,
        id: &PrincipalId,
    ) -> Option<(crate::principal::contacts::ContactBook, String)> {
        let manager = Arc::clone(&self.context()?.principal_manager);
        let principal = manager.get(id.clone()).await?;
        Some((
            crate::principal::contacts::ContactBook::of(&principal),
            manager.runtime_did().to_string(),
        ))
    }

    /// The actors and conversations `stamp` may see (ADR-078 D4): the
    /// owner sees every channel and every learned contact; any other run
    /// sees its own actor and the participants of its own conversation.
    /// Learned contacts the registry here cannot route to the same actor
    /// are dormant (ADR-081 D1): listed with `reachable: false`.
    async fn discover(
        &self,
        id: &PrincipalId,
        stamp: &ActingFor,
    ) -> anyhow::Result<(BTreeMap<String, Value>, Vec<Value>)> {
        let owner = stamp.is_owner();
        // An unreadable registry is an error, never an empty answer.
        self.port.known_actors().await?;
        let mut actors = BTreeMap::new();
        if owner {
            if let Some((book, runtime)) = self.book(id).await {
                for contact in book.list()? {
                    let Some(subject) = contact.subject_on(&runtime) else {
                        continue;
                    };
                    let entry = if contact.reachable_on(self.port.as_ref(), &runtime) {
                        let mut value = json!(self.port.actor_info(&subject).await);
                        value["reachable"] = json!(true);
                        value
                    } else {
                        json!({
                            "address": subject.canonical_address(),
                            "kind": contact.kind,
                            "hub_id": contact.hub_id,
                            "name": contact.name,
                            "registration": contact.registration,
                            "reachable": false,
                        })
                    };
                    actors.insert(
                        entry["address"].as_str().unwrap_or_default().to_string(),
                        entry,
                    );
                }
            }
        }
        let me = self.port.actor_info(&ActorRef::from(id)).await;
        actors.insert(me.address.clone(), json!(me));
        let mut channels = Vec::new();
        for channel in self.port.list_for_principal(id).await? {
            if !owner && stamp.compartment.channel() != Some(channel.as_str()) {
                continue;
            }
            let participants = self.port.participants(&channel).await?;
            for actor in &participants {
                actors.insert(actor.address.clone(), json!(actor));
            }
            channels.push(json!({"channel":channel,"participants":participants,
                "binding":self.port.binding_for(&channel,id).await?}));
        }
        Ok((actors, channels))
    }

    /// Learn `actor` into the principal's contact book.
    async fn remember(
        &self,
        id: &PrincipalId,
        actor: &ActorInfo,
        signing_did: Option<&str>,
    ) -> anyhow::Result<()> {
        let (book, runtime) = self
            .book(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("principal not loaded"))?;
        let mut record = crate::principal::contacts::ContactRecord::from_actor(actor, &runtime);
        record.signing_did = signing_did.map(str::to_owned);
        book.remember(record).await
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
        "Discover your contacts and conversations. list returns your own actor, current participants of your channels, and principals you explicitly learned (a run acting for anyone but the owner sees only its own conversation), with canonical addresses, known names and mention tokens. resolve accepts kind:local:<id> or kind:hub:<id>; use its canonical address with ChannelSend.to. You can learn a locally hosted principal by its exact local ID or a registered principal by its Hub ID. User/visitor resolution is limited to your channel participants; their opaque IDs are never inferred from names. Learned contacts are private to each principal, survive restarts, and travel with it; a contact this runtime cannot route to is listed with reachable: false.".into()
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
        let stamp = ctx.acting_for();
        match params.get("action").and_then(Value::as_str) {
            Some("list") => {
                let (actors, channels) = self.discover(&id, &stamp).await?;
                Ok(json!({"actors":actors.into_values().collect::<Vec<_>>(),"channels":channels}))
            }
            Some("resolve") => {
                let reference = params
                    .get("reference")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("Contacts resolve requires reference"))?;
                let subject = ActorRef::parse_scoped(reference)
                    .map_err(|_| anyhow::anyhow!("Use kind:local:<id> or kind:hub:<id>"))?;
                let route = subject
                    .route()
                    .ok_or_else(|| anyhow::anyhow!("Use kind:local:<id> or kind:hub:<id>"))?;
                let (scope, identifier) = (route.scope(), route.id());
                let (actors, _) = self.discover(&id, &stamp).await?;
                if let Some(actor) = actors.values().find(|actor| {
                    actor["reachable"] != json!(false)
                        && (actor["address"] == json!(reference)
                            || actor["hub_id"].as_str().is_some_and(|hub| {
                                reference
                                    == format!("{}:hub:{hub}", actor["kind"].as_str().unwrap_or(""))
                            }))
                }) {
                    let mut actor = actor.clone();
                    if let Some(fields) = actor.as_object_mut() {
                        fields.remove("reachable");
                    }
                    return Ok(json!({ "actor": actor }));
                }
                anyhow::ensure!(
                    matches!(subject, ActorRef::Principal(_)),
                    "Actor is not a contact of this principal"
                );
                // Learned contacts are the principal's own knowledge, shared by
                // all of its runs; only the owner's runs add to it.
                anyhow::ensure!(
                    stamp.is_owner(),
                    "Actor is not a contact of this conversation; only the owner can learn new contacts"
                );
                let context = self
                    .context()
                    .ok_or_else(|| anyhow::anyhow!("Actor directory is unavailable"))?;
                if scope == "local" {
                    anyhow::ensure!(context.principal_manager.get(PrincipalId(identifier.into())).await.is_some(),
                        "Unknown local principal; use principal:hub:<id> to learn a remote principal");
                    let actor = self.port.resolve_actor(reference).await?;
                    self.remember(&id, &actor, None).await?;
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
                let caller_profile = self.port.actor_info(&ActorRef::from(&caller.id)).await;
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
                let subject = context.actor_subject(&result).await?;
                let actor = self
                    .port
                    .register_actor(
                        &subject,
                        Some(hub_id),
                        Some(&result.agent_did),
                        Some(&result.identity_key),
                        result.name.as_deref(),
                        if result.exposure == crate::tunnel::hub_directory::ResolvedExposure::Public
                        {
                            peko_channel::Registration::Public
                        } else {
                            peko_channel::Registration::Private
                        },
                    )
                    .await?;
                self.remember(&id, &actor, Some(&result.agent_did)).await?;
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
