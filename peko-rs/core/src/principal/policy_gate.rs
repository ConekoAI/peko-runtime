//! The principal side of ADR-078 D7: the reviewer that judges a
//! non-owner run's sensitive calls against `POLICY.md`, and the approval
//! desk that hands unclear calls to the owner.
//!
//! - **Review.** One non-streaming completion, no tools, against the
//!   policy's `judge_model` (else the principal's own model), charged to
//!   the principal's quota. The reviewer sees the policy, the stamp, and
//!   the call — never the conversation that produced it.
//! - **Approvals.** A request is stored in the principal's local tier
//!   (`approvals/<id>.json`) and announced in the owner's conversation.
//!   The owner answers with `approve <id>`, `approve <id> always`, or
//!   `deny <id>` — handled by the runtime before any agent turn, so no
//!   model can approve on the owner's behalf. An approved call replays as
//!   a background task acting for the original requester, and its result
//!   wakes the session that asked.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_auth::ActorRef;
use peko_subject::PrincipalId;
use peko_tools_core::{ActingFor, Origin};
use serde::{Deserialize, Serialize};

use crate::principal::manager::PrincipalManager;
use crate::principal::Principal;
use crate::security::judge::{self, GatedCall, PolicyGate, Verdict};
use crate::security::policy;

/// How long a request waits for the owner.
pub const APPROVAL_TTL_HOURS: i64 = 24;

/// Where a request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
}

/// One stored request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub call: GatedCall,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: ApprovalStatus,
    /// The digest of the call the owner saw; a replay must match it.
    pub digest: String,
}

fn approvals_dir(principal: &Principal) -> PathBuf {
    // `<local>/sessions` is the sessions dir; approvals sit beside it.
    let sessions = principal.memory.sessions_dir();
    sessions.parent().map_or_else(
        || sessions.join("approvals"),
        |local| local.join("approvals"),
    )
}

fn record_path(dir: &Path, id: &str) -> Option<PathBuf> {
    let valid = id.starts_with("apr_") && id[4..].chars().all(|c| c.is_ascii_alphanumeric());
    valid.then(|| dir.join(format!("{id}.json")))
}

fn load_record(dir: &Path, id: &str) -> Option<ApprovalRecord> {
    let path = record_path(dir, id)?;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

fn save_record(dir: &Path, record: &ApprovalRecord) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let path = record_path(dir, &record.id).context("invalid approval id")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(record)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn pending_records(dir: &Path) -> Vec<ApprovalRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let now = Utc::now();
    let mut records: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            serde_json::from_slice::<ApprovalRecord>(&std::fs::read(e.path()).ok()?).ok()
        })
        .filter(|r| r.status == ApprovalStatus::Pending && r.expires_at > now)
        .collect();
    records.sort_by_key(|r| r.created_at);
    records
}

/// A short description of what a call does, for the owner.
fn summarize(call: &GatedCall) -> String {
    let p = &call.params;
    let detail = ["file_path", "path", "command", "to", "prompt", "action"]
        .iter()
        .find_map(|key| p[*key].as_str().map(|v| format!("{key}={v}")));
    let mut text = match detail {
        Some(detail) => format!("`{}` ({detail})", call.tool_name),
        None => format!("`{}`", call.tool_name),
    };
    if text.len() > 300 {
        let mut end = 300;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    peko_tools_core::neutralize_runtime_tags(&text)
}

/// The production [`PolicyGate`] over the loaded principals.
pub struct PrincipalPolicyGate {
    principals: Weak<PrincipalManager>,
}

impl PrincipalPolicyGate {
    #[must_use]
    pub fn new(principals: Weak<PrincipalManager>) -> Self {
        Self { principals }
    }

    async fn principal(&self, id: &str) -> Option<(Arc<PrincipalManager>, Arc<Principal>)> {
        let manager = self.principals.upgrade()?;
        let principal =
            crate::daemon::cron_engine::resolve_principal(&manager, &PrincipalId(id.to_string()))
                .await?;
        Some((manager, principal))
    }

    /// One reviewer completion; `Err` when no reviewer model could run.
    async fn ask_reviewer(
        manager: &PrincipalManager,
        principal: &Principal,
        policy: &policy::Policy,
        prompt: &str,
    ) -> Result<String> {
        Self::ask_model(
            manager,
            principal,
            policy,
            judge::REVIEWER_SYSTEM,
            prompt,
            300,
        )
        .await
    }

    /// One completion on the policy's `judge_model` (else the
    /// principal's own model), charged to the principal's quota.
    async fn ask_model(
        manager: &PrincipalManager,
        principal: &Principal,
        policy: &policy::Policy,
        system: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let resolver = manager
            .llm_resolver()
            .context("no model resolver on this runtime")?;
        let preferred = principal.config.read().await.preferred_model_id.clone();
        let choice = resolver
            .resolve(peko_providers::resolver::ResolveRequest {
                override_model: policy.judge_model.as_deref(),
                agent_model: preferred.as_deref(),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow::anyhow!("reviewer model: {e}"))?;
        let provider = resolver.build_provider(&choice.config).await?;
        let options = peko_provider_api::ChatOptions {
            temperature: Some(0.0),
            max_tokens: Some(max_tokens),
            ..Default::default()
        };
        let response = provider
            .chat_response_with_options(Some(system), prompt, &choice.model_id, &options)
            .await?;
        let cost = peko_engine::compute_cost_usd(
            choice.config.spec.and_then(|s| s.pricing),
            response.usage.input,
            response.usage.output,
        );
        principal
            .quota_meter
            .charge_with_cost(&response.usage, cost)
            .await
            .map_err(|e| anyhow::anyhow!("reviewer charge refused: {e}"))?;
        Ok(response
            .content
            .iter()
            .filter_map(|block| match block {
                peko_message::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect())
    }

    /// The owner, as the subject their conversation is keyed by
    /// (ADR-081 D8: the one owner's own conversation).
    async fn owner_subjects(principal: &Principal) -> Vec<ActorRef> {
        vec![principal.config.read().await.owner.clone()]
    }

    /// Tell the owner a request waits; best-effort (the `approvals`
    /// command lists it either way).
    async fn notify_owner(principal: &Principal, record: &ApprovalRecord) {
        let Some(messenger) = crate::principal::messenger::global_messenger() else {
            return;
        };
        let call = &record.call;
        let note = format!(
            "📋 Approval needed ({id}): {actor}{name} ({trust}) wants to run {what}. Reviewer: \
             {reason}. Reply `approve {id}`, `approve {id} always`, or `deny {id}`. Expires {exp}.",
            id = record.id,
            actor = call.stamp.actor,
            name = call
                .stamp
                .name
                .as_deref()
                .map(|n| format!(" \"{n}\""))
                .unwrap_or_default(),
            trust = call.stamp.trust.as_str(),
            what = summarize(call),
            reason = peko_tools_core::neutralize_runtime_tags(&record.reason),
            exp = record.expires_at.format("%Y-%m-%d %H:%M UTC"),
        );
        for owner in Self::owner_subjects(principal).await {
            let delivered = messenger
                .deliver_note(
                    &principal.id.0,
                    &owner,
                    &note,
                    peko_session::events::MessageSource::Agent,
                    Some("approvals"),
                )
                .await;
            if matches!(delivered, Ok(true)) {
                return;
            }
        }
        tracing::warn!(id = %record.id, "approval request stored; no owner conversation to announce it in");
    }
}

#[async_trait]
impl PolicyGate for PrincipalPolicyGate {
    async fn review(&self, call: &GatedCall) -> Verdict {
        let Some((manager, principal)) = self.principal(&call.principal_id).await else {
            return Verdict::Deny(
                "the principal is not loaded; nothing can review the call".into(),
            );
        };
        let policy = policy::load(&principal.workspace_path).unwrap_or_default();
        let keys = crate::security::shared_actor_keys(manager.channel_port());
        let rules = policy.for_actor(&call.stamp.actor, &*keys);
        let prompt = judge::reviewer_prompt(call, &rules);
        let verdict = match Self::ask_reviewer(&manager, &principal, &policy, &prompt).await {
            Ok(text) => judge::parse_verdict(&text),
            Err(e) => Verdict::AskOwner(format!("the reviewer could not run: {e:#}")),
        };
        if let Some(obs) = manager.observability() {
            let decision = match &verdict {
                Verdict::Allow => "allow".to_string(),
                Verdict::Deny(r) => format!("deny: {r}"),
                Verdict::AskOwner(r) => format!("ask_owner: {r}"),
            };
            let _ = obs
                .audit_security(
                    "policy.review",
                    call.agent_id.as_deref(),
                    serde_json::json!({
                        "principal_id": call.principal_id,
                        "tool_name": call.tool_name,
                        "acting_for": call.stamp,
                        "decision": decision,
                    }),
                )
                .await;
        }
        verdict
    }

    async fn ask_owner(&self, call: &GatedCall, reason: &str) -> Result<String> {
        let (_, principal) = self
            .principal(&call.principal_id)
            .await
            .context("principal not loaded")?;
        let now = Utc::now();
        let record = ApprovalRecord {
            id: format!("apr_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            call: call.clone(),
            reason: reason.to_string(),
            created_at: now,
            expires_at: now + chrono::Duration::hours(APPROVAL_TTL_HOURS),
            status: ApprovalStatus::Pending,
            digest: call.digest(),
        };
        save_record(&approvals_dir(&principal), &record)?;
        Self::notify_owner(&principal, &record).await;
        Ok(record.id)
    }

    async fn is_approved_replay(&self, call: &GatedCall) -> bool {
        let Origin::Approval { request_id } = &call.stamp.origin else {
            return false;
        };
        let Some((_, principal)) = self.principal(&call.principal_id).await else {
            return false;
        };
        load_record(&approvals_dir(&principal), request_id).is_some_and(|record| {
            record.status == ApprovalStatus::Approved && record.digest == call.digest()
        })
    }

    async fn review_reply(&self, stamp: &ActingFor, principal_id: &str, reply: &str) -> Verdict {
        if stamp.is_owner() {
            return Verdict::Allow;
        }
        let Some((manager, principal)) = self.principal(principal_id).await else {
            return Verdict::Allow;
        };
        let policy = policy::load(&principal.workspace_path).unwrap_or_default();
        if !policy.review_replies {
            return Verdict::Allow;
        }
        let keys = crate::security::shared_actor_keys(manager.channel_port());
        let rules = policy.for_actor(&stamp.actor, &*keys);
        let prompt = judge::reply_prompt(stamp, &rules, reply);
        match Self::ask_reviewer(&manager, &principal, &policy, &prompt).await {
            Ok(text) => match judge::parse_verdict(&text) {
                // A reply cannot wait for the owner; unclear is withheld.
                Verdict::AskOwner(reason) => Verdict::Deny(reason),
                other => other,
            },
            Err(e) => Verdict::Deny(format!("the reviewer could not run: {e:#}")),
        }
    }

    async fn source_class(
        &self,
        principal_id: &str,
        source: &str,
    ) -> crate::security::memory_gate::SourceClass {
        use crate::security::memory_gate::{channel_of, SourceClass};
        use peko_tools_core::{Compartment, Trust};
        let Some((manager, principal)) = self.principal(principal_id).await else {
            return SourceClass::Unknown;
        };
        let channels = manager.channel_port();
        let compartment = if let Some(channel) = channel_of(source) {
            // An existing channel; the event id, if any, is the reviewer's
            // concern (ADR-085).
            let Some(port) = &channels else {
                return SourceClass::Unknown;
            };
            let id = peko_channel::ChannelId(channel.to_string());
            if port.list_members(&id).await.is_err() {
                return SourceClass::Unknown;
            }
            Compartment::Conversation {
                channel: channel.to_string(),
            }
        } else {
            let Some((id, metas)) = resolve_session(&principal, source).await else {
                return SourceClass::Unknown;
            };
            crate::security::compartment_of(&metas, id)
        };
        let stamps = crate::principal::trust::stamps_for(&principal, channels);
        if stamps.ceiling(&compartment).await == Trust::Owner {
            SourceClass::Owner
        } else {
            SourceClass::Other(compartment)
        }
    }

    async fn check_claims(
        &self,
        principal_id: &str,
        claims: &[crate::security::memory_gate::Claim],
    ) -> Vec<crate::security::memory_gate::ClaimCheck> {
        use crate::security::memory_gate::{self, ClaimCheck};
        let unchecked = |reason: String| vec![ClaimCheck::Unchecked(reason); claims.len()];
        let Some((manager, principal)) = self.principal(principal_id).await else {
            return unchecked("the principal is not loaded".into());
        };
        let channels = manager.channel_port();
        let policy = policy::load(&principal.workspace_path).unwrap_or_default();
        let mut checks = Vec::with_capacity(claims.len());
        for batch in claims.chunks(memory_gate::CLAIMS_PER_REVIEW) {
            let mut items = Vec::with_capacity(batch.len());
            for claim in batch {
                let mut evidence = Vec::new();
                for source in &claim.sources {
                    let excerpt =
                        source_excerpt(&principal, channels.as_ref(), source, &claim.line).await;
                    evidence.push((source.clone(), excerpt));
                }
                items.push((claim.clone(), evidence));
            }
            let prompt = memory_gate::review_prompt(&items);
            let max_tokens = 40 + 40 * batch.len() as u32;
            match Self::ask_model(
                &manager,
                &principal,
                &policy,
                memory_gate::REVIEW_SYSTEM,
                &prompt,
                max_tokens,
            )
            .await
            {
                Ok(answer) => checks.extend(memory_gate::parse_review(&answer, batch.len())),
                Err(e) => {
                    checks.extend(batch.iter().map(|_| {
                        ClaimCheck::Unchecked(format!("the reviewer could not run: {e:#}"))
                    }))
                }
            }
        }
        checks
    }
}

/// The session a hot-memory source names, with the store's metadata.
/// Paths resolve from the trunk, like the memory run's own; a bare id
/// needs no anchor.
async fn resolve_session(
    principal: &Principal,
    source: &str,
) -> Option<(peko_session::SessionId, Vec<peko_session::SessionMetadata>)> {
    let mut sessions = peko_session::SessionManager::new()
        .with_sessions_dir_internal(principal.memory.sessions_dir());
    let metas = sessions.list_all_sessions(false).await.unwrap_or_default();
    let anchor = metas
        .iter()
        .find(|m| m.parent_session_id.is_none())
        .map_or(peko_session::SessionId(uuid::Uuid::nil()), |m| m.session_id);
    let id = peko_session::path::resolve_reference(&metas, anchor, source).ok()?;
    metas
        .iter()
        .any(|m| m.session_id == id)
        .then_some((id, metas))
}

/// What `source` says that bears on `claim` (ADR-085), capped at
/// [`crate::security::memory_gate::EXCERPT_BYTES_PER_SOURCE`]. A
/// `<channel>#<event>` source quotes that event and its neighbours; a
/// bare channel or a session quotes the passages sharing the most words
/// with the claim. Empty when nothing is found.
pub(crate) async fn source_excerpt(
    principal: &Principal,
    channels: Option<&Arc<dyn peko_channel::ChannelPort>>,
    source: &str,
    claim: &str,
) -> String {
    use crate::security::memory_gate::{
        best_excerpts, channel_of, clip, EXCERPT_BYTES_PER_SOURCE, PASSAGE_BYTES,
    };
    if let Some(channel) = channel_of(source) {
        let Some(port) = channels else {
            return String::new();
        };
        let id = peko_channel::ChannelId(channel.to_string());
        let Ok(events) = port
            .peek_with_ids(&id, &peko_channel::Checkpoint::default())
            .await
        else {
            return String::new();
        };
        // (event id, label, text)
        let posts: Vec<(String, String, String)> = events
            .into_iter()
            .filter_map(|(id, event)| match event {
                peko_protocol::channel::ChannelEvent::Posted { author, text, .. } => {
                    let label = format!("[{id}] {author}");
                    Some((id, label, text))
                }
                _ => None,
            })
            .collect();
        let event = source.split_once('#').map(|(_, e)| e.trim());
        if let Some(at) = event.and_then(|e| posts.iter().position(|(id, _, _)| id == e)) {
            let window = &posts[at.saturating_sub(2)..(at + 3).min(posts.len())];
            let text: Vec<String> = window
                .iter()
                .map(|(_, label, text)| clip(&format!("{label}: {text}"), PASSAGE_BYTES))
                .collect();
            return clip(&text.join("\n"), EXCERPT_BYTES_PER_SOURCE);
        }
        let passages: Vec<(String, String)> = posts
            .into_iter()
            .map(|(_, label, text)| (label, text))
            .collect();
        return best_excerpts(claim, &passages, EXCERPT_BYTES_PER_SOURCE);
    }
    let Some((id, _)) = resolve_session(principal, source).await else {
        return String::new();
    };
    let storage = peko_session::jsonl::SessionStorage::new(principal.memory.sessions_dir());
    let Ok(entries) = storage.load_normalized(&id.to_string()).await else {
        return String::new();
    };
    let passages: Vec<(String, String)> = entries
        .into_iter()
        .filter_map(|entry| {
            use peko_session::jsonl::NormalizedEntry as E;
            let (who, text) = match entry {
                E::UserMessage { content, .. } => ("user", content),
                E::AssistantMessage { content, .. } => ("assistant", content),
                E::Compaction { summary, .. } => ("earlier summary", summary),
                _ => return None,
            };
            Some((who.to_string(), text))
        })
        .collect();
    best_excerpts(claim, &passages, EXCERPT_BYTES_PER_SOURCE)
}

/// An owner's approval command, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnerCommand {
    Approve { id: String, always: bool },
    Deny { id: String },
    List,
}

fn parse_owner_command(text: &str) -> Option<OwnerCommand> {
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["approvals"] => Some(OwnerCommand::List),
        ["approve", id] => Some(OwnerCommand::Approve {
            id: (*id).to_string(),
            always: false,
        }),
        ["approve", id, "always"] => Some(OwnerCommand::Approve {
            id: (*id).to_string(),
            always: true,
        }),
        ["deny", id] => Some(OwnerCommand::Deny {
            id: (*id).to_string(),
        }),
        _ => None,
    }
    .filter(|command| match command {
        OwnerCommand::Approve { id, .. } | OwnerCommand::Deny { id } => id.starts_with("apr_"),
        OwnerCommand::List => true,
    })
}

/// Handle an owner's approval command, before any agent turn. `None`
/// when `text` is not one; the caller then runs the turn as usual. Only
/// call this for a message whose authority is the owner.
pub(crate) async fn handle_owner_command(
    manager: &Arc<PrincipalManager>,
    principal: &Arc<Principal>,
    text: &str,
) -> Option<String> {
    let command = parse_owner_command(text)?;
    let dir = approvals_dir(principal);
    let reply = match command {
        OwnerCommand::List => {
            let pending = pending_records(&dir);
            if pending.is_empty() {
                "No approval requests are waiting.".to_string()
            } else {
                pending
                    .iter()
                    .map(|r| {
                        format!(
                            "- {}: {} ({}) wants {}. Reviewer: {}",
                            r.id,
                            r.call.stamp.actor,
                            r.call.stamp.trust.as_str(),
                            summarize(&r.call),
                            r.reason
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        OwnerCommand::Deny { id } => match decide(&dir, &id, ApprovalStatus::Denied) {
            Ok(record) => {
                notify_requester(manager, &record, "The owner declined this request.").await;
                format!("Denied {id}.")
            }
            Err(e) => format!("{e}"),
        },
        OwnerCommand::Approve { id, always } => match decide(&dir, &id, ApprovalStatus::Approved) {
            Ok(record) => {
                let mut reply = format!("Approved {id}.");
                if always {
                    reply.push_str(&remember_allowance(principal, &record));
                }
                match replay(manager, principal, &record).await {
                    Ok(task) => reply.push_str(&format!(
                        " Running it for {} now (task {task}); the result goes back to \
                             their conversation.",
                        record.call.stamp.actor
                    )),
                    Err(e) => reply.push_str(&format!(" It could not be started: {e:#}")),
                }
                reply
            }
            Err(e) => format!("{e}"),
        },
    };
    if let Some(obs) = manager.observability() {
        let _ = obs
            .audit_security(
                "policy.owner_decision",
                None,
                serde_json::json!({ "principal_id": principal.id.0, "command": text.trim() }),
            )
            .await;
    }
    Some(reply)
}

/// Move a pending, unexpired request to `status`.
fn decide(dir: &Path, id: &str, status: ApprovalStatus) -> Result<ApprovalRecord> {
    let mut record =
        load_record(dir, id).ok_or_else(|| anyhow::anyhow!("No approval request {id}."))?;
    anyhow::ensure!(
        record.status == ApprovalStatus::Pending,
        "Request {id} was already decided."
    );
    anyhow::ensure!(record.expires_at > Utc::now(), "Request {id} has expired.");
    record.status = status;
    save_record(dir, &record)?;
    Ok(record)
}

/// Append the owner's *Always allow* to the actor's `POLICY.md` section.
fn remember_allowance(principal: &Principal, record: &ApprovalRecord) -> String {
    let path = principal.workspace_path.join(policy::POLICY_FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let updated =
        policy::Policy::with_allowance(&text, &record.call.stamp.actor, &summarize(&record.call));
    match std::fs::write(&path, updated) {
        Ok(()) => format!(
            " Added an always-allow rule for {} to POLICY.md.",
            record.call.stamp.actor
        ),
        Err(e) => format!(" POLICY.md could not be updated: {e}."),
    }
}

/// Run an approved call as a background task acting for its requester;
/// its completion wakes the session that asked.
async fn replay(
    manager: &Arc<PrincipalManager>,
    principal: &Arc<Principal>,
    record: &ApprovalRecord,
) -> Result<String> {
    let core = manager.tooling();
    let observability = manager
        .observability()
        .unwrap_or_else(|| Arc::new(peko_observability::Observability::new("principal")));
    crate::principal::context::install_principal_tooling(manager, principal, &core, observability)
        .await?;
    let call = &record.call;
    let session = call
        .session_id
        .clone()
        .context("the request has no session to report back to")?;
    let mut stamp = call.stamp.clone();
    stamp.origin = Origin::Approval {
        request_id: record.id.clone(),
    };
    let mut context = crate::async_exec::executor::ToolDispatchContext::builder(
        call.tool_name.clone(),
        call.params.clone(),
        session.clone(),
    )
    .for_principal(principal.id.0.clone())
    .with_session_id(session)
    .with_acting_for(stamp);
    context.principal_name = call.principal_name.clone();
    context.agent_id = call.agent_id.clone();
    context.workspace = call.workspace.clone();
    context.parent = Some(peko_tools_core::CallParent::Approval {
        id: record.id.clone(),
        call_id: call.call_id.clone(),
    });
    let config = crate::async_exec::executor::AsyncToolConfig {
        wake_on_completion: true,
        label: Some(format!("approved {}", record.id)),
        principal_id: principal.id.clone(),
        ..Default::default()
    };
    let executor = core
        .async_executor_for(&principal.id, manager.shared_inbox_registry())
        .await;
    let receipt = executor.dispatch_tool(&core, context, config).await?;
    Ok(receipt.task_id)
}

/// Leave a note in the requesting session's inbox; it reaches the run at
/// its next turn.
async fn notify_requester(manager: &PrincipalManager, record: &ApprovalRecord, text: &str) {
    let Some(session) = &record.call.session_id else {
        return;
    };
    let note = format!(
        "[runtime] Approval request {} for {}: {text}",
        record.id,
        summarize(&record.call)
    );
    manager
        .shared_inbox_registry()
        .get_or_create(session)
        .await
        .push(peko_session::SteeringMessage::new(note).into())
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::{Compartment, Trust};

    fn call() -> GatedCall {
        GatedCall {
            call_id: None,
            principal_id: "prin_x".into(),
            principal_name: None,
            session_id: Some("s".into()),
            agent_id: None,
            workspace: None,
            stamp: ActingFor {
                actor: "user:hub:alice".into(),
                name: None,
                trust: Trust::Untrusted,
                compartment: Compartment::Conversation {
                    channel: "dm_a".into(),
                },
                origin: Origin::Ingress { channel: None },
            },
            tool_name: "Read".into(),
            params: serde_json::json!({"file_path": "docs/q3.md"}),
        }
    }

    #[test]
    fn owner_commands_parse_strictly() {
        assert_eq!(
            parse_owner_command(" approve apr_abc "),
            Some(OwnerCommand::Approve {
                id: "apr_abc".into(),
                always: false
            })
        );
        assert_eq!(
            parse_owner_command("approve apr_abc always"),
            Some(OwnerCommand::Approve {
                id: "apr_abc".into(),
                always: true
            })
        );
        assert_eq!(
            parse_owner_command("deny apr_abc"),
            Some(OwnerCommand::Deny {
                id: "apr_abc".into()
            })
        );
        assert_eq!(parse_owner_command("approvals"), Some(OwnerCommand::List));
        assert_eq!(parse_owner_command("please approve apr_abc"), None);
        assert_eq!(parse_owner_command("approve everything"), None);
    }

    #[test]
    fn records_are_decided_once_and_never_after_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut record = ApprovalRecord {
            id: "apr_0123456789ab".into(),
            call: call(),
            reason: "unclear".into(),
            created_at: now,
            expires_at: now + chrono::Duration::hours(1),
            status: ApprovalStatus::Pending,
            digest: call().digest(),
        };
        save_record(dir.path(), &record).unwrap();
        assert_eq!(pending_records(dir.path()).len(), 1);
        let decided = decide(dir.path(), &record.id, ApprovalStatus::Approved).unwrap();
        assert_eq!(decided.status, ApprovalStatus::Approved);
        assert!(decide(dir.path(), &record.id, ApprovalStatus::Denied).is_err());
        assert!(pending_records(dir.path()).is_empty());

        record.id = "apr_ba9876543210".into();
        record.expires_at = now - chrono::Duration::minutes(1);
        save_record(dir.path(), &record).unwrap();
        assert!(decide(dir.path(), &record.id, ApprovalStatus::Approved).is_err());
        assert!(load_record(dir.path(), "../escape").is_none());
    }

    #[test]
    fn summaries_name_the_tool_and_target() {
        assert_eq!(summarize(&call()), "`Read` (file_path=docs/q3.md)");
    }
}
