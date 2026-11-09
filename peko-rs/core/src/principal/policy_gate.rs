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
//!   The owner decides with their own action — the desktop's Approve /
//!   Deny, or `peko approve` / `peko deny` (IPC `approval_decide`); from
//!   another of their computers, the desktop's buttons there (ADR-099) —
//!   never in a message, so no model reading the conversation can approve
//!   on the owner's behalf. An approved call replays as a background task
//!   acting for the original requester, and its result wakes the session
//!   that asked.

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

/// Whether `record` hands a task to the owner (ADR-100) rather than
/// asking about one reviewed call.
fn is_owner_task(call: &GatedCall) -> bool {
    crate::security::floor::is_owner_task(&call.tool_name, &call.params)
}

/// The brief of a task handed to the owner, as they read it.
fn owner_task_brief(call: &GatedCall) -> String {
    peko_tools_core::neutralize_runtime_tags(call.params["prompt"].as_str().unwrap_or_default())
}

/// Where a task handed to the owner was asked for, as the owner reads it:
/// "in “repo-team” (4 people)".
async fn owner_task_origin(manager: &PrincipalManager, call: &GatedCall) -> String {
    let Some(channel) = call.stamp.compartment.channel() else {
        return "in a shared conversation".into();
    };
    let Some(port) = manager.channel_port() else {
        return format!("in {channel}");
    };
    let id = peko_channel::ChannelId(channel.to_string());
    let name = port
        .membership(&id)
        .await
        .ok()
        .map(|m| m.name.trim().to_string())
        .filter(|n| !n.is_empty());
    // Who takes part, once each: `membership().members` also lists the
    // routes to remote members, so it counts some of them twice.
    let members = port.participants(&id).await.ok().map(|people| {
        let mut keys: Vec<String> = people.into_iter().map(|p| p.identity_key).collect();
        keys.sort();
        keys.dedup();
        keys.len()
    });
    let place = match name {
        Some(name) => format!(
            "\u{201c}{}\u{201d}",
            peko_tools_core::neutralize_runtime_tags(&name)
        ),
        None => channel.to_string(),
    };
    match members {
        Some(n) => format!("in {place} ({n} members)"),
        None => format!("in {place}"),
    }
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
    /// Models may think before answering even when not asked to, and
    /// thinking counts against the cap: an answer cut off at the cap
    /// is asked once more with more room.
    async fn ask_reviewer(
        manager: &PrincipalManager,
        principal: &Principal,
        policy: &policy::Policy,
        prompt: &str,
    ) -> Result<String> {
        let mut max_tokens = judge::REVIEWER_MAX_TOKENS;
        let mut retried = false;
        loop {
            let (text, truncated) = Self::complete(
                manager,
                principal,
                policy,
                judge::REVIEWER_SYSTEM,
                prompt,
                max_tokens,
            )
            .await?;
            if !truncated || retried {
                return Ok(text);
            }
            max_tokens *= judge::REVIEWER_RETRY_FACTOR;
            retried = true;
        }
    }

    /// One completion on the policy's `judge_model` (else the
    /// principal's own model), charged to the principal's quota, and
    /// whether the answer stopped at `max_tokens`.
    async fn complete(
        manager: &PrincipalManager,
        principal: &Principal,
        policy: &policy::Policy,
        system: &str,
        prompt: &str,
        max_tokens: u32,
    ) -> Result<(String, bool)> {
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
        crate::tools::builtin::model_call::refuse_if_exhausted(&principal.quota_meter)?;
        let provider = resolver.build_provider(&choice.config).await?;
        let options = peko_provider_api::ChatOptions {
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
        // The review is made and paid for: recorded, and its verdict kept.
        if let Err(e) = principal
            .quota_meter
            .charge_with_cost(&response.usage, cost)
            .await
        {
            tracing::info!("policy review crossed a limit ({e}); the next call is refused");
        }
        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                peko_message::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let truncated = response.stop_reason == peko_provider_api::StopReason::Length;
        Ok((text, truncated))
    }

    /// The owner, as the subject their conversation is keyed by
    /// (ADR-081 D8: the one owner's own conversation).
    async fn owner_subjects(principal: &Principal) -> Vec<ActorRef> {
        vec![principal.config.read().await.owner.clone()]
    }

    /// Tell the owner a request waits; best-effort (`approval_list`
    /// lists it either way).
    async fn notify_owner(principal: &Principal, record: &ApprovalRecord) {
        let Some(messenger) = crate::principal::messenger::global_messenger() else {
            return;
        };
        let call = &record.call;
        let peko = principal.name().await;
        let note = if is_owner_task(call) {
            format!(
                "📋 Approval needed ({id}): you asked {peko} for something {reason}, and it \
                 needs your full access. Approved, it runs as you, outside that conversation, \
                 with only this brief, and its report goes back there:\n\n{brief}\n\nApprove or \
                 deny it in the Peko app, or with `peko approve {peko} {id}` / `peko deny {peko} \
                 {id}`. Expires {exp}.",
                id = record.id,
                reason = record.reason,
                brief = owner_task_brief(call),
                exp = record.expires_at.format("%Y-%m-%d %H:%M UTC"),
            )
        } else {
            format!(
            "📋 Approval needed ({id}): {actor}{name} ({trust}) wants to run {what}. Reviewer: \
             {reason}. Approve or deny it in the Peko app, or with `peko approve {peko} {id}` \
             / `peko deny {peko} {id}`. Expires {exp}.",
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
            )
        };
        for owner in Self::owner_subjects(principal).await {
            let delivered = messenger
                .deliver_note(
                    &principal.id.0,
                    &owner,
                    &note,
                    None,
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
            return Verdict::Deny("the peko is not loaded; nothing can review the call".into());
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
        let (manager, principal) = self
            .principal(&call.principal_id)
            .await
            .context("principal not loaded")?;
        // ADR-100 D2: the owner learns where the task was asked for.
        let reason = if is_owner_task(call) {
            owner_task_origin(&manager, call).await
        } else {
            reason.to_string()
        };
        let now = Utc::now();
        let record = ApprovalRecord {
            id: format!("apr_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]),
            call: call.clone(),
            reason,
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
        // ADR-085 D4: review only while a run could carry someone else's
        // words, unless the owner says otherwise.
        let skip = |reason: &str| vec![ClaimCheck::Skipped(reason.into()); claims.len()];
        match policy.memory_review {
            policy::MemoryReview::Off => return skip("memory_review: off"),
            policy::MemoryReview::Guests
                if !others_spoke_recently(&principal, channels.clone()).await =>
            {
                return skip("no conversation with anyone but the owner this week");
            }
            _ => {}
        }
        let mut checks = Vec::with_capacity(claims.len());
        for batch in claims.chunks(memory_gate::CLAIMS_PER_REVIEW) {
            let mut items = Vec::with_capacity(batch.len());
            for claim in batch {
                let mut evidence = Vec::new();
                for source in &claim.sources {
                    let excerpt =
                        source_excerpt(&principal, channels.as_ref(), source, &claim.entry).await;
                    evidence.push((source.clone(), excerpt));
                }
                items.push((claim.clone(), evidence));
            }
            let prompt = memory_gate::review_prompt(&items);
            // ADR-085 D2: an answer that leaves a claim without a finding
            // is asked once more. Thinking counts against the cap, so a
            // cut-off answer gets more room; a malformed or degenerate one
            // gets the expected format restated. The retry fills only
            // what the first answer missed.
            let mut max_tokens = memory_gate::review_budget(batch.len());
            let mut asked = prompt.clone();
            let mut earlier: Option<Vec<ClaimCheck>> = None;
            let found = loop {
                let answer = Self::complete(
                    &manager,
                    &principal,
                    &policy,
                    memory_gate::REVIEW_SYSTEM,
                    &asked,
                    max_tokens,
                )
                .await;
                match answer {
                    Ok((text, truncated)) => {
                        let parsed = memory_gate::parse_review(&text, batch.len());
                        let found = match earlier.take() {
                            Some(first) => break memory_gate::merge_reviews(first, parsed),
                            None => parsed,
                        };
                        let missing = found.iter().any(|c| matches!(c, ClaimCheck::Unchecked(_)));
                        if !missing {
                            break found;
                        }
                        if truncated {
                            max_tokens *= memory_gate::REVIEW_RETRY_FACTOR;
                        }
                        asked = memory_gate::review_retry_prompt(&prompt, batch.len());
                        earlier = Some(found);
                    }
                    // A retry that cannot run keeps the first answer's findings.
                    Err(_) if earlier.is_some() => break earlier.take().unwrap_or_default(),
                    Err(e) => {
                        break batch
                            .iter()
                            .map(|_| {
                                ClaimCheck::Unchecked(format!("the reviewer could not run: {e:#}"))
                            })
                            .collect()
                    }
                }
            };
            checks.extend(found);
        }
        checks
    }
}

/// Whether any of the principal's conversations with someone besides the
/// owner had activity in the last
/// [`crate::security::memory_gate::GUEST_ACTIVITY_DAYS`] days. Unknown
/// is yes: a channel whose roster or log cannot be read counts.
pub(crate) async fn others_spoke_recently(
    principal: &Arc<Principal>,
    channels: Option<Arc<dyn peko_channel::ChannelPort>>,
) -> bool {
    let Some(port) = channels.clone() else {
        return false;
    };
    let Ok(list) = port.list_for_principal(&principal.id).await else {
        return true;
    };
    let cutoff = chrono::Utc::now()
        - chrono::Duration::days(crate::security::memory_gate::GUEST_ACTIVITY_DAYS);
    let stamps = crate::principal::trust::stamps_for(principal, channels);
    for channel in list {
        let compartment = peko_tools_core::Compartment::Conversation {
            channel: channel.0.clone(),
        };
        if stamps.ceiling(&compartment).await == peko_tools_core::Trust::Owner {
            continue;
        }
        let Ok(tail) = port.peek_tail(&channel, 1, None).await else {
            return true;
        };
        let recent = tail.events.last().is_some_and(|(_, event)| {
            chrono::DateTime::parse_from_rfc3339(event.at())
                .map_or(true, |at| at.with_timezone(&chrono::Utc) >= cutoff)
        });
        if recent {
            return true;
        }
    }
    false
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
/// bare channel quotes the posts sharing the most words with the claim,
/// the owner's first. A session bound to a conversation (the owner's DM,
/// a group) is quoted from that conversation's posts, as a bare channel
/// would be; any other session from its messages, leaving out what the
/// runtime injected (its context blocks, hook, cron and agent notes).
/// Empty when nothing is found.
pub(crate) async fn source_excerpt(
    principal: &Principal,
    channels: Option<&Arc<dyn peko_channel::ChannelPort>>,
    source: &str,
    claim: &str,
) -> String {
    use crate::security::memory_gate::{
        best_excerpts, channel_of, is_runtime_text, Passage, EXCERPT_BYTES_PER_SOURCE,
    };
    if let Some(channel) = channel_of(source) {
        let event = source.split_once('#').map(|(_, e)| e.trim());
        return channel_excerpt(principal, channels, channel, event, claim)
            .await
            .unwrap_or_default();
    }
    let Some((id, metas)) = resolve_session(principal, source).await else {
        return String::new();
    };
    if let peko_tools_core::Compartment::Conversation { channel } =
        crate::security::compartment_of(&metas, id)
    {
        if let Some(excerpt) = channel_excerpt(principal, channels, &channel, None, claim).await {
            return excerpt;
        }
    }
    let storage = peko_session::jsonl::SessionStorage::new(principal.memory.sessions_dir());
    let Ok(entries) = storage.load_normalized(&id.to_string()).await else {
        return String::new();
    };
    let passages: Vec<Passage> = entries
        .into_iter()
        .filter_map(|entry| {
            use peko_session::events::MessageSource;
            use peko_session::jsonl::NormalizedEntry as E;
            let (who, text) = match entry {
                E::UserMessage {
                    content, source, ..
                } if source == MessageSource::User && !is_runtime_text(&content) => {
                    ("user", content)
                }
                E::AssistantMessage { content, .. } => ("assistant", content),
                E::Compaction { summary, .. } => ("earlier summary", summary),
                _ => return None,
            };
            Some(Passage::new(who, text))
        })
        .collect();
    best_excerpts(claim, &passages, EXCERPT_BYTES_PER_SOURCE)
}

/// [`source_excerpt`] for a channel: `event` and the two posts on either
/// side, else the posts sharing the most words with `claim`, the
/// owner's first. Posts are labelled `[<event>] <author>`, the owner's
/// with `(owner)`. `None` when the channel cannot be read.
async fn channel_excerpt(
    principal: &Principal,
    channels: Option<&Arc<dyn peko_channel::ChannelPort>>,
    channel: &str,
    event: Option<&str>,
    claim: &str,
) -> Option<String> {
    use crate::security::memory_gate::{
        best_excerpts, clip, Passage, EXCERPT_BYTES_PER_SOURCE, PASSAGE_BYTES,
    };
    let port = channels?;
    let id = peko_channel::ChannelId(channel.to_string());
    let events = port
        .peek_with_ids(&id, &peko_channel::Checkpoint::default())
        .await
        .ok()?;
    let owner = principal.config.read().await.owner.clone();
    let keys = crate::security::actor_keys(Some(&**port));
    // (event id, passage)
    let posts: Vec<(String, Passage)> = events
        .into_iter()
        .filter_map(|(id, event)| match event {
            peko_protocol::channel::ChannelEvent::Posted { author, text, .. } => {
                let by_owner = author
                    .parse::<ActorRef>()
                    .is_ok_and(|actor| keys.same_actor(&actor, &owner));
                let label = if by_owner {
                    format!("[{id}] {author} (owner)")
                } else {
                    format!("[{id}] {author}")
                };
                Some((id, Passage::new(label, text).first(by_owner)))
            }
            _ => None,
        })
        .collect();
    if let Some(at) = event.and_then(|e| posts.iter().position(|(id, _)| id == e)) {
        let window = &posts[at.saturating_sub(2)..(at + 3).min(posts.len())];
        let text: Vec<String> = window
            .iter()
            .map(|(_, p)| clip(&format!("{}: {}", p.label, p.text), PASSAGE_BYTES))
            .collect();
        return Some(clip(&text.join("\n"), EXCERPT_BYTES_PER_SOURCE));
    }
    let passages: Vec<Passage> = posts.into_iter().map(|(_, passage)| passage).collect();
    Some(best_excerpts(claim, &passages, EXCERPT_BYTES_PER_SOURCE))
}

/// The owner's decision on a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run it; with `always`, also add an always-allow rule for the
    /// requester to `POLICY.md`.
    Approve {
        always: bool,
    },
    Deny,
}

/// The requests waiting for the owner, oldest first.
#[must_use]
pub fn pending_requests(principal: &Principal) -> Vec<ApprovalRecord> {
    let mut pending = pending_records(&approvals_dir(principal));
    pending.sort_by_key(|r| r.created_at);
    pending
}

/// One line on what a request asks: the tool and its target.
#[must_use]
pub fn request_summary(record: &ApprovalRecord) -> String {
    if is_owner_task(&record.call) {
        // ADR-100 D2: the owner reads the whole brief.
        return owner_task_brief(&record.call);
    }
    summarize(&record.call)
}

/// The request's kind on the wire: `owner_task` for a task handed to the
/// owner (ADR-100), absent for a reviewed call.
#[must_use]
pub fn request_kind(record: &ApprovalRecord) -> Option<String> {
    is_owner_task(&record.call).then(|| "owner_task".to_string())
}

/// Apply the owner's decision on request `id` and say what happened.
/// Called only for the owner's own action — the desktop's buttons or the
/// CLI (IPC `approval_decide`) — never for a message.
pub async fn decide_request(
    manager: &Arc<PrincipalManager>,
    principal: &Arc<Principal>,
    id: &str,
    decision: Decision,
) -> Result<String> {
    decide_request_via(manager, principal, id, decision, None).await
}

/// [`decide_request`] for a decision the owner made on another of their
/// computers (ADR-099 D2): `via` is that computer's runtime, kept in the
/// audit record.
pub async fn decide_request_via(
    manager: &Arc<PrincipalManager>,
    principal: &Arc<Principal>,
    id: &str,
    decision: Decision,
    via: Option<&str>,
) -> Result<String> {
    let dir = approvals_dir(principal);
    // ADR-100 D2: work run as the owner is approved one brief at a time.
    if matches!(decision, Decision::Approve { always: true })
        && load_record(&dir, id).is_some_and(|r| is_owner_task(&r.call))
    {
        anyhow::bail!(
            "Request {id} runs as you, so it is approved once, never always; approve it \
             without always."
        );
    }
    let reply = match decision {
        Decision::Deny => {
            let record = decide(&dir, id, ApprovalStatus::Denied)?;
            notify_requester(manager, &record, "The owner declined this request.").await;
            format!("Denied {id}.")
        }
        Decision::Approve { always } => {
            let record = decide(&dir, id, ApprovalStatus::Approved)?;
            let mut reply = format!("Approved {id}.");
            if always {
                reply.push_str(&remember_allowance(principal, &record));
            }
            // ADR-100 D5: work someone else has done or is doing meanwhile
            // is not done twice.
            let covered = if is_owner_task(&record.call) {
                owner_task_board(manager, principal, &record).await.err()
            } else {
                None
            };
            if let Some(reason) = covered {
                notify_requester(
                    manager,
                    &record,
                    &format!(
                        "The owner approved it, but it did not run: {reason}. Leave that work to \
                         them: release or drop your item, and say so in one line only if \
                         someone asked you for it."
                    ),
                )
                .await;
                reply.push_str(&format!(" It did not run: {reason}."));
            } else {
                match replay(manager, principal, &record).await {
                    Ok(task) if is_owner_task(&record.call) => reply.push_str(&format!(
                        " Running it as you now (task {task}); its report goes back to the \
                         conversation it was asked in."
                    )),
                    Ok(task) => reply.push_str(&format!(
                        " Running it for {} now (task {task}); the result goes back to \
                         their conversation.",
                        record.call.stamp.actor
                    )),
                    Err(e) => reply.push_str(&format!(" It could not be started: {e:#}")),
                }
            }
            reply
        }
    };
    if let Some(record) = load_record(&dir, id) {
        notify_owner_decided(principal, &record).await;
    }
    if let Some(obs) = manager.observability() {
        let _ = obs
            .audit_security(
                "policy.owner_decision",
                None,
                serde_json::json!({
                    "principal_id": principal.id.0,
                    "request_id": id,
                    "via_runtime": via,
                    "decision": match decision {
                        Decision::Approve { always: true } => "approve_always",
                        Decision::Approve { always: false } => "approve",
                        Decision::Deny => "deny",
                    },
                }),
            )
            .await;
    }
    Ok(reply)
}

/// Tell the owner's conversation that a request it announced was decided.
/// The decision is made outside any conversation (the app's buttons, the
/// CLI, another computer); without this note the peko still reads the
/// request as waiting there (2026-11-08 field test: it told its owner two
/// approved requests "are still waiting for you").
async fn notify_owner_decided(principal: &Principal, record: &ApprovalRecord) {
    let Some(messenger) = crate::principal::messenger::global_messenger() else {
        return;
    };
    let Some(note) = decision_note(record) else {
        return;
    };
    for owner in PrincipalPolicyGate::owner_subjects(principal).await {
        let delivered = messenger
            .deliver_note(
                &principal.id.0,
                &owner,
                &note,
                None,
                peko_session::events::MessageSource::Agent,
                Some("approvals"),
            )
            .await;
        if matches!(delivered, Ok(true)) {
            return;
        }
    }
}

/// The note [`notify_owner_decided`] leaves; `None` while pending.
fn decision_note(record: &ApprovalRecord) -> Option<String> {
    let what = if is_owner_task(&record.call) {
        let brief = owner_task_brief(&record.call);
        let first = brief.lines().next().unwrap_or_default();
        let mut short: String = first.chars().take(120).collect();
        if short.len() < brief.len() {
            short.push('…');
        }
        short
    } else {
        summarize(&record.call)
    };
    match record.status {
        ApprovalStatus::Approved => Some(format!(
            "✅ You approved {}: {what}. It is running now; its result goes back to the \
             conversation that asked.",
            record.id
        )),
        ApprovalStatus::Denied => Some(format!(
            "🚫 You denied {}: {what}. Nothing was done.",
            record.id
        )),
        ApprovalStatus::Pending => None,
    }
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

/// ADR-100 D5: the board item a task handed to the owner serves in its
/// group (its `item`, else the one item the peko is working on). `Err`
/// names what already covers the work: the item is done or someone else's,
/// or an item that looks like the same work is done, under way, or waiting
/// on its own approval since earlier. Otherwise the item is marked `doing`,
/// so a twin approved later on another computer sees it taken.
async fn owner_task_board(
    manager: &PrincipalManager,
    principal: &Principal,
    record: &ApprovalRecord,
) -> std::result::Result<(), String> {
    let Some(channel) = record
        .call
        .stamp
        .compartment
        .channel()
        .and_then(peko_channel::ChannelId::parse)
    else {
        return Ok(());
    };
    let Some(port) = manager.channel_port() else {
        return Ok(());
    };
    let me = ActorRef::from(&principal.id);
    let reference = record.call.params["item"].as_str();
    let served = match port.owner_task_item(&channel, reference, &me).await {
        Ok(Some(served)) => served,
        Ok(None) => return Ok(()),
        Err(e) => {
            tracing::warn!(%channel, "owner task: board check failed; running it: {e}");
            return Ok(());
        }
    };
    let mark = |state, note: String| {
        peko_channel::PostMsg::root(String::new()).with_board(
            peko_protocol::channel::BoardOp::Update {
                item: served.item.id.clone(),
                state,
                note: Some(note),
                check_at: None,
            },
        )
    };
    let Some(covered) = served.covered else {
        let update = mark(
            peko_protocol::channel::BoardState::Doing,
            "Approved; running as the owner".into(),
        );
        if let Err(e) = port.post(&channel, &me, update).await {
            tracing::warn!(%channel, "owner task: could not mark its item doing: {e}");
        }
        return Ok(());
    };
    let item = &covered.item;
    let what = format!("#{} \u{201c}{}\u{201d}", item.short_id(), item.title);
    let reason = if covered.done {
        format!("{what} is already done")
    } else {
        let holder = match item.claimant.as_deref().map(str::parse::<ActorRef>) {
            Some(Ok(actor)) => {
                let info = port.actor_info(&actor).await;
                info.name.unwrap_or(info.address)
            }
            _ => "someone else".to_string(),
        };
        format!("{holder} holds {what}")
    };
    // The board says so at once; the item's check-in brings it back to
    // the peko, which drops or releases it. Its own item already done or
    // someone else's is left as it is.
    if served.item.id != item.id {
        let update = mark(
            peko_protocol::channel::BoardState::Blocked,
            format!("Approved, but not run: {reason}"),
        );
        if let Err(e) = port.post(&channel, &me, update).await {
            tracing::warn!(%channel, "owner task: could not note its item: {e}");
        }
    }
    Err(reason)
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
    let mut params = call.params.clone();
    if is_owner_task(call) {
        // ADR-100 D3: the brief runs as the owner, in a new session of
        // the owner's own tree, with none of the conversation it was
        // asked in; its report still wakes that conversation.
        stamp.trust = peko_tools_core::Trust::Owner;
        stamp.compartment = peko_tools_core::Compartment::Internal;
        stamp.owner_request = false;
        params = owner_task_params(&params);
    }
    let mut context = crate::async_exec::executor::ToolDispatchContext::builder(
        call.tool_name.clone(),
        params,
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

/// The `Agent` call an approved owner task runs: no `as_owner`, and its
/// slug at the top of the owner's tree rather than under the
/// conversation it was asked in.
fn owner_task_params(params: &serde_json::Value) -> serde_json::Value {
    let mut params = params.clone();
    if let Some(map) = params.as_object_mut() {
        map.remove("as_owner");
        map.remove("item");
        let slug = map
            .get("path")
            .and_then(|p| p.as_str())
            .map(|p| p.trim_start_matches("sess:"))
            .filter(|p| !p.is_empty() && !p.contains('/'))
            .unwrap_or("owner-task")
            .to_string();
        map.insert(
            "path".into(),
            serde_json::Value::String(format!("sess:/{slug}")),
        );
    }
    params
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
    let key = peko_session::inbox_key(&record.call.principal_id, session);
    manager
        .shared_inbox_registry()
        .get_or_create(&key)
        .await
        .push(peko_session::SteeringMessage::new(note).into())
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::{Compartment, Trust};

    /// The owner's conversation hears each decision, so the peko never
    /// reads an approved request as still waiting (2026-11-08 field test).
    #[test]
    fn a_decided_request_leaves_a_note_for_the_owners_conversation() {
        let mut record = ApprovalRecord {
            id: "apr_0123456789ab".into(),
            call: call(),
            reason: "unclear".into(),
            created_at: Utc::now(),
            expires_at: Utc::now(),
            status: ApprovalStatus::Pending,
            digest: String::new(),
        };
        assert_eq!(decision_note(&record), None);
        record.status = ApprovalStatus::Approved;
        let approved = decision_note(&record).unwrap();
        assert!(
            approved.starts_with("✅ You approved apr_0123456789ab"),
            "{approved}"
        );
        record.status = ApprovalStatus::Denied;
        let denied = decision_note(&record).unwrap();
        assert!(
            denied.contains("You denied apr_0123456789ab") && denied.contains("Nothing was done")
        );
        // A task handed to the owner is named by its brief's first line.
        record.call.tool_name = "Agent".into();
        record.call.params = serde_json::json!({"prompt": "Run the tests.\nThen report.", "path": "t", "as_owner": true});
        assert!(decision_note(&record)
            .unwrap()
            .contains(": Run the tests.…"));
    }

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
                owner_request: false,
            },
            tool_name: "Read".into(),
            params: serde_json::json!({"file_path": "docs/q3.md"}),
        }
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
