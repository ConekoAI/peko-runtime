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
                    "description":"An address (principal:hub:<id>, peko:hub:<id>, user:hub:<id>, kind:local:<id>), a name you share a conversation with (ada, @ada), or a peko by its owner's PekoHub handle (@frank/ada)."}
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
            let binding = self.port.binding_for(&channel, id).await?;
            channels.push(json!({"channel":channel,"participants":participants,
                "kind":self.port.conversation_kind(&channel).await?,
                "binding":binding.as_ref().map(|b| &b.session),
                "wake":binding.map(|b| b.wake)}));
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

    /// The actors `stamp` sees, without the `reachable: false` dormant ones.
    async fn visible(&self, id: &PrincipalId, stamp: &ActingFor) -> anyhow::Result<Vec<Value>> {
        let (actors, _) = self.discover(id, stamp).await?;
        Ok(actors
            .into_values()
            .filter(|actor| actor["reachable"] != json!(false))
            .map(|mut actor| {
                if let Some(fields) = actor.as_object_mut() {
                    fields.remove("reachable");
                }
                actor
            })
            .collect())
    }

    /// An address, in either spelling (`peko:` or `principal:`).
    async fn resolve_address(
        &self,
        id: &PrincipalId,
        stamp: &ActingFor,
        subject: ActorRef,
    ) -> anyhow::Result<Value> {
        let route = subject
            .route()
            .ok_or_else(|| anyhow::anyhow!("Use kind:local:<id> or kind:hub:<id>"))?;
        let reference = subject.canonical_address();
        let (scope, identifier) = (route.scope(), route.id());
        if let Some(actor) = self.visible(id, stamp).await?.into_iter().find(|actor| {
            actor["address"] == json!(reference)
                || actor["hub_id"].as_str().is_some_and(|hub| {
                    reference == format!("{}:hub:{hub}", actor["kind"].as_str().unwrap_or(""))
                })
        }) {
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
            anyhow::ensure!(
                context
                    .principal_manager
                    .get(PrincipalId(identifier.into()))
                    .await
                    .is_some(),
                "Unknown local principal; use principal:hub:<id> to learn a remote principal"
            );
            let actor = self.port.resolve_actor(&reference).await?;
            self.remember(id, &actor, None).await?;
            return Ok(json!({"actor":actor}));
        }
        let hub_id = reference.strip_prefix("principal:hub:").ok_or_else(|| {
            anyhow::anyhow!("Unknown local actor; use kind:local:<id> or principal:hub:<id>")
        })?;
        let caller = self.caller_did(&context, id).await?;
        let result = match &caller {
            Some(did) => context.directory.resolve_by_hub_id_as(hub_id, did).await?,
            None => context.directory.resolve_by_hub_id(hub_id).await?,
        };
        anyhow::ensure!(
            result.hub_id == hub_id,
            "Directory substituted a different Hub identity"
        );
        self.learn(id, &context, &result).await
    }

    /// The caller's signing DID when it is registered, so the directory
    /// answers for it (ADR-073); `None` asks anonymously.
    async fn caller_did(
        &self,
        context: &CrossRuntimeA2aCtx,
        id: &PrincipalId,
    ) -> anyhow::Result<Option<String>> {
        let caller = context
            .principal_manager
            .get(id.clone())
            .await
            .ok_or_else(|| anyhow::anyhow!("principal not loaded"))?;
        let profile = self.port.actor_info(&ActorRef::from(&caller.id)).await;
        Ok(match profile.hub_id {
            Some(_) => Some(caller.did().await.0),
            None => None,
        })
    }

    /// Check a directory answer, register the actor it names, and keep it
    /// in the principal's contact book.
    async fn learn(
        &self,
        id: &PrincipalId,
        context: &CrossRuntimeA2aCtx,
        result: &crate::tunnel::hub_directory::AgentResolution,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            !result.hub_id.is_empty(),
            "Directory returned no Hub identity"
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
        let subject = context.actor_subject(result).await?;
        let actor = self
            .port
            .register_actor(
                &subject,
                Some(&result.hub_id),
                Some(&result.agent_did),
                Some(&result.identity_key),
                result.name.as_deref(),
                if result.exposure == crate::tunnel::hub_directory::ResolvedExposure::Public {
                    peko_channel::Registration::Public
                } else {
                    peko_channel::Registration::Private
                },
            )
            .await?;
        self.remember(id, &actor, Some(&result.agent_did)).await?;
        Ok(
            json!({"actor":actor,"runtime_id":result.runtime_id,"local":result.runtime_id==context.caller_runtime_id}),
        )
    }

    /// ADR-098 D1: `ada` / `@ada` — the one actor the run sees by that
    /// name. An owner run also finds the principals hosted here.
    async fn resolve_name(
        &self,
        id: &PrincipalId,
        stamp: &ActingFor,
        name: &str,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            !name.is_empty(),
            "Contacts resolve needs a name or an address"
        );
        let named = |actor: &Value| {
            actor["name"]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
                || actor["hub_id"].as_str() == Some(name)
                || actor["local_id"].as_str() == Some(name)
        };
        let mut found: Vec<Value> = self
            .visible(id, stamp)
            .await?
            .into_iter()
            .filter(named)
            .collect();
        if found.is_empty() && stamp.is_owner() {
            if let Some(context) = self.context() {
                for principal in context.principal_manager.list_all().await {
                    if principal.id == *id {
                        continue;
                    }
                    let actor = self.port.actor_info(&ActorRef::from(&principal.id)).await;
                    let value = json!(actor);
                    if named(&value) || principal.name().await.eq_ignore_ascii_case(name) {
                        self.remember(id, &actor, None).await?;
                        found.push(value);
                    }
                }
            }
        }
        match found.len() {
            1 => Ok(json!({ "actor": found.remove(0) })),
            0 => anyhow::bail!(
                "No one called {name} among your contacts and conversations. A peko can be \
                 found as @<owner handle>/<name> (its owner shares a group with yours, or it is \
                 public), by its Hub ID (principal:hub:<id>) from its owner, or by sharing a \
                 group with it (Channel join with an invite link)."
            ),
            _ => {
                let candidates: Vec<String> = found
                    .iter()
                    .map(|a| {
                        format!(
                            "{} ({} {})",
                            a["address"].as_str().unwrap_or_default(),
                            a["kind"].as_str().unwrap_or_default(),
                            a["name"].as_str().unwrap_or_default()
                        )
                    })
                    .collect();
                anyhow::bail!(
                    "Several actors are called {name}: {}. Resolve one by its address.",
                    candidates.join(", ")
                )
            }
        }
    }

    /// ADR-098 D1: `@owner/name` — a peko named `name` whose owner's PekoHub
    /// handle is `owner`. Pekos the run already sees come first, checked
    /// against the directory; an owner run then asks PekoHub by handle.
    async fn resolve_owned(
        &self,
        id: &PrincipalId,
        stamp: &ActingFor,
        owner: &str,
        name: &str,
    ) -> anyhow::Result<Value> {
        let owner = owner.trim_start_matches('@');
        anyhow::ensure!(
            !owner.is_empty() && !name.is_empty(),
            "Name a peko by its owner as @<owner handle>/<name>"
        );
        let context = self
            .context()
            .ok_or_else(|| anyhow::anyhow!("Actor directory is unavailable"))?;
        let caller = self.caller_did(&context, id).await?;
        for actor in self.visible(id, stamp).await? {
            let (Some(hub_id), true) = (
                actor["hub_id"].as_str(),
                actor["kind"] == json!("principal")
                    && actor["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name)),
            ) else {
                continue;
            };
            let resolved = match &caller {
                Some(did) => context.directory.resolve_by_hub_id_as(hub_id, did).await,
                None => context.directory.resolve_by_hub_id(hub_id).await,
            };
            if resolved.is_ok_and(|r| {
                r.hub_id == hub_id
                    && r.owner_handle
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(owner))
            }) {
                return Ok(json!({ "actor": actor }));
            }
        }
        anyhow::ensure!(
            stamp.is_owner(),
            "No peko @{owner}/{name} in this conversation; only the owner can learn new contacts"
        );
        let result = match &caller {
            Some(did) => {
                context
                    .directory
                    .resolve_by_handle_as(owner, name, did)
                    .await
            }
            None => context.directory.resolve_by_handle(owner, name).await,
        }
        .map_err(|e| match e {
            crate::tunnel::hub_directory::DirectoryError::NotFound => anyhow::anyhow!(
                "PekoHub shows you no peko @{owner}/{name}. A private peko is found by handle \
                 only by its owner and by people in a group with its owner; otherwise ask \
                 {owner} for its Hub ID or for a group's invite link."
            ),
            other => anyhow::anyhow!(other),
        })?;
        anyhow::ensure!(
            result
                .name
                .as_deref()
                .is_none_or(|n| n.eq_ignore_ascii_case(name)),
            "Directory answered for a different peko"
        );
        self.learn(id, &context, &result).await
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
        "Discover your contacts and conversations. list returns your own actor, current participants of your channels, and principals you explicitly learned (a run acting for anyone but the owner sees only its own conversation), with canonical addresses, known names and mention tokens. resolve accepts an address (principal: and peko: are the same), a name (ada or @ada) of someone in your contacts or conversations, or a peko by its owner's PekoHub handle (@frank/ada: found when its owner shares a group with yours, or it is public); several matches come back as candidates to pick by address. Use its canonical address with ChannelSend.to. You can learn a locally hosted principal by its exact local ID or a registered principal by its Hub ID. User/visitor resolution is limited to your channel participants; their opaque IDs are never inferred from names. Learned contacts are private to each principal, survive restarts, and travel with it; a contact this runtime cannot route to is listed with reachable: false.".into()
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
                    .ok_or_else(|| anyhow::anyhow!("Contacts resolve requires reference"))?
                    .trim();
                // ADR-098 D1: an address in either spelling, a name, or a
                // peko by its owner's handle.
                let address = reference.strip_prefix('@').unwrap_or(reference);
                if let Ok(subject) = ActorRef::parse_scoped(address) {
                    return self.resolve_address(&id, &stamp, subject).await;
                }
                if let Some((owner, name)) = address.split_once('/') {
                    return self.resolve_owned(&id, &stamp, owner, name).await;
                }
                self.resolve_name(&id, &stamp, address).await
            }
            _ => anyhow::bail!("Contacts action must be list or resolve"),
        }
    }
}

#[cfg(test)]
#[path = "contacts_tests.rs"]
mod tests;
