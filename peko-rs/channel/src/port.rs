//! `ChannelPort` trait + supporting types.
//!
//! The port the CLI / daemon / `peko-engine` consume via `Arc<dyn
//! ChannelPort>`. The trait is consumer-defined (lives here, not in
//! `peko-engine`) so the dep direction stays acyclic — `peko-engine`
//! implements `ChannelResponder` (in `responder.rs`), but the ChannelPort
//! surface this crate owns.
//!
//! Convention mirrors: every other port trait in the codebase follows
//! the "no closures on the trait surface" rule
//! (`peko-rs/plan/src/plan_port.rs:9-16`). Pure async methods taking
//! `&self` + borrowed arguments; no `FnOnce` readers/writers.

use std::collections::HashSet;

use async_trait::async_trait;
use peko_protocol::channel::{
    ChannelEvent, ChannelId, ChannelMembership, ConversationKind, MemberProvenance,
};
use peko_subject::{ActorKey, ActorKeys, ActorRef, PrincipalId, Route};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// The board item an approved owner task serves
/// ([`ChannelPort::owner_task_item`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerTaskItem {
    pub item: crate::board::BoardItem,
    /// What already covers its work, if anything.
    pub covered: Option<CoveredBy>,
}

/// The item that covers an owner task's work: done, or someone else's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoveredBy {
    pub done: bool,
    pub item: crate::board::BoardItem,
}

/// Port trait for the multi-principal chat primitive.
///
/// `peko-rs/core` and `peko-rs/cli` hold `Arc<dyn ChannelPort>` on
/// their respective contexts. The concrete impl is
/// [`crate::ChannelStore`] (file-backed, append-only JSONL event log +
/// member set). Future impls — in-memory for tests, network-backed
/// for distributed deployments — slot in without changing call sites.
#[async_trait]
pub trait ChannelPort: Send + Sync + 'static {
    /// Register local storage identity → canonical actor identity. Storage keys
    /// remain authorization keys; callers see the returned canonical address.
    async fn register_principal(&self, _id: &PrincipalId, _did: &str, _name: &str) -> Result<()> {
        Ok(())
    }

    /// Release a hosted principal's local identity when it is removed. A known
    /// Hub actor remains a remote contact; local-only history remains local.
    async fn unhost_principal(&self, _id: &PrincipalId) -> Result<()> {
        Ok(())
    }

    async fn actor_info(&self, subject: &ActorRef) -> ActorInfo {
        ActorInfo::from_subject(subject, None)
    }

    /// The actor's immutable key (ADR-081 D1): every alias of a known actor
    /// maps to one key; an unknown actor's key is its canonical address.
    /// Synchronous so permission checks can use it without plumbing.
    fn key_of(&self, subject: &ActorRef) -> ActorKey {
        ActorKey(ActorInfo::from_subject(subject, None).identity_key)
    }

    /// Register a local actor or a verified Hub-only contact. Signing keys are
    /// transport credentials, never user-facing actor references.
    async fn register_actor(
        &self,
        subject: &ActorRef,
        hub_id: Option<&str>,
        signing_did: Option<&str>,
        identity_key: Option<&str>,
        name: Option<&str>,
        registration: Registration,
    ) -> Result<ActorInfo> {
        let mut actor = ActorInfo::from_subject(subject, name);
        actor.bind_hub_id(hub_id)?;
        actor.registration = registration;
        actor.identity_key = identity_key
            .or(signing_did)
            .unwrap_or(&actor.address)
            .into();
        Ok(actor)
    }

    async fn known_actors(&self) -> Result<Vec<ActorInfo>> {
        Ok(Vec::new())
    }

    async fn resolve_actor(&self, reference: &str) -> Result<ActorInfo> {
        let subject: ActorRef = reference
            .parse()
            .map_err(|e| ChannelError::Adapter(format!("actor reference: {e}")))?;
        Ok(self.actor_info(&subject).await)
    }

    async fn participants(&self, channel: &ChannelId) -> Result<Vec<ActorInfo>> {
        let mut out = Vec::new();
        for member in self.list_members(channel).await? {
            out.push(self.actor_info(&member).await);
        }
        Ok(out)
    }

    /// What the channel is (ADR-082 D1). The default is for adapters
    /// without stored metadata (`NoopChannelPort`, in-memory tests).
    async fn conversation_kind(&self, channel: &ChannelId) -> Result<ConversationKind> {
        let _ = channel;
        Ok(ConversationKind::Group)
    }

    /// `principal`'s binding on `channel` (ADR-082 D2): the session that
    /// handles its posts and the wake policy. `None` until the runtime
    /// provisions the principal's conversation session there.
    async fn binding_for(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<Option<Binding>> {
        let _ = (channel, principal);
        Ok(None)
    }

    /// Record that `session` handles `principal`'s posts on `channel`.
    /// Only the runtime calls this, right after creating that session in
    /// the channel's compartment. An existing binding keeps its wake
    /// policy; a new one starts at the kind's default.
    async fn bind(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
        session: &str,
    ) -> Result<Binding> {
        let _ = principal;
        let kind = self.conversation_kind(channel).await?;
        Ok(Binding::new(session, WakePolicy::default_for(kind)))
    }

    /// Set `principal`'s wake policy on a group it is bound to. A DM's
    /// policy is always [`WakePolicy::Human`].
    async fn set_wake(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
        wake: WakePolicy,
    ) -> Result<Binding> {
        let _ = (channel, principal, wake);
        Err(ChannelError::Adapter(
            "this channel port does not store bindings".into(),
        ))
    }

    /// Set the role `principal`'s turns run on `channel` (ADR-087 D4);
    /// `None` returns it to the principal's default. Works for both
    /// kinds. The caller checks that the role resolves.
    async fn set_role(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
        role: Option<String>,
    ) -> Result<Binding> {
        let _ = (channel, principal, role);
        Err(ChannelError::Adapter(
            "this channel port does not store bindings".into(),
        ))
    }

    /// ADR-092 D1: a group's home, owner and admins; `None` for a DM.
    async fn group_info(&self, channel: &ChannelId) -> Result<Option<crate::store::GroupInfo>> {
        let _ = channel;
        Ok(None)
    }

    /// ADR-092 D1: a group's owner (`by`) makes a member an admin, or not.
    async fn set_group_admin(
        &self,
        channel: &ChannelId,
        by: &PrincipalId,
        actor: &ActorRef,
        admin: bool,
    ) -> Result<()> {
        let _ = (channel, by, actor, admin);
        Err(ChannelError::Adapter(
            "this channel port does not keep group admins".into(),
        ))
    }

    /// ADR-092 D5: a group's owner or admin (`by`) registers it with
    /// PekoHub, or changes who may find it. Returns the registration.
    async fn register_group(
        &self,
        channel: &ChannelId,
        by: &PrincipalId,
        visibility: crate::store::GroupVisibility,
    ) -> Result<crate::store::HubGroup> {
        let _ = (channel, by, visibility);
        Err(ChannelError::Adapter(
            "this channel port does not register groups".into(),
        ))
    }

    /// ADR-098 D2: a peko hosted here (`peko`) joins a group by its invite
    /// link (or a public group by its ID), for itself. Returns the group's
    /// channel and name; the home's invitation completes the join.
    async fn join_group_as(&self, peko: &PrincipalId, link: &str) -> Result<(ChannelId, String)> {
        let _ = (peko, link);
        Err(ChannelError::Adapter(
            "this channel port does not join groups on PekoHub".into(),
        ))
    }

    /// ADR-092 D5: a group's owner or admin (`by`) mints an invite link
    /// good for `days`. Returns the link and its expiry (unix seconds).
    async fn group_invite_link(
        &self,
        channel: &ChannelId,
        by: &PrincipalId,
        days: u32,
    ) -> Result<(String, i64)> {
        let _ = (channel, by, days);
        Err(ChannelError::Adapter(
            "this channel port does not make invite links".into(),
        ))
    }

    /// ADR-092 D5: a group's owner or admin (`by`) stops every invite
    /// link from working. Returns how many were live.
    async fn revoke_group_links(&self, channel: &ChannelId, by: &PrincipalId) -> Result<usize> {
        let _ = (channel, by);
        Err(ChannelError::Adapter(
            "this channel port does not make invite links".into(),
        ))
    }

    /// Ensure an opaque pair DM and its principal-local session binding.
    async fn ensure_dm(
        &self,
        principal: &PrincipalId,
        peer: &ActorRef,
        binding: &str,
    ) -> Result<ChannelId> {
        let me = self.actor_info(&ActorRef::from(principal)).await;
        let other = self.actor_info(peer).await;
        let id = dm_channel_id(&me.dm_key(), &other.dm_key());
        if !self.list_for_principal(principal).await?.contains(&id) {
            self.create(
                principal,
                CreateOpts::runtime("direct conversation")
                    .with_id(id.clone())
                    .with_kind(ConversationKind::Dm),
            )
            .await?;
        }
        self.invite(&id, principal, &other.subject()).await?;
        self.bind(&id, principal, binding).await?;
        Ok(id)
    }

    /// Create a new channel. The creator is automatically added as the
    /// first member. Returns the generated [`ChannelId`].
    async fn create(&self, creator: &PrincipalId, opts: CreateOpts) -> Result<ChannelId>;

    /// Add an invitee to a channel. Returns [`ChannelError::NotMember`]
    /// if `inviter` isn't already a member; [`ChannelError::FanOutCap`]
    /// if the channel already has 8 principal members (PR-1 cap, now
    /// principal-counted per ADR-049 Phase 4; PR-3 may lift).
    ///
    /// ADR-049 Phase 1: the invitee is [`ActorRef`]-typed — both
    /// principals (`ActorRef::Principal`) and users (`ActorRef::User`)
    /// can be channel members. The inviter stays principal-typed (only
    /// principals hold agency to invite in this phase). User invitees
    /// are validated by wire form only; no `PrincipalManager`
    /// resolution happens at this layer.
    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &ActorRef,
    ) -> Result<()>;

    /// Post a message. The adapter enforces the at-most-one-parent
    /// convention — `msg.parent` references the line number of the
    /// message being replied to. Returns the new message's [`TaskId`]
    /// (the line number it was assigned in the channel's event log).
    ///
    /// ADR-049 Phase 1: `sender` is [`ActorRef`]-typed; membership
    /// itself is the write authorization, so a member user posts as
    /// themselves — no vouching member principal is required.
    async fn post(&self, channel: &ChannelId, sender: &ActorRef, msg: PostMsg) -> Result<TaskId>;

    /// Phase 11 (agent-session paradigm sprint): like [`Self::post`]
    /// but writes an explicit `author` string onto the event instead
    /// of deriving it from `sender`. Membership + parent validation
    /// are still enforced against `sender` — `author` is a display
    /// attribution, not an authority claim.
    ///
    /// Used by the per-peer DM channels: the inbound message is posted
    /// with `sender = principal.id` (the channel's creator/member —
    /// the human peer is deliberately not added to membership) and
    /// `author = peer.to_string()` (the ActorRef wire form, e.g.
    /// `user:alice` or `principal:did:...`), so the channel log reads
    /// as a natural two-party conversation.
    ///
    /// The default impl degrades to a plain `sender`-authored
    /// [`Self::post`], so adapters that don't distinguish attribution
    /// (`NoopChannelPort`, in-memory test ports) don't need to
    /// override.
    async fn post_attributed(
        &self,
        channel: &ChannelId,
        sender: &ActorRef,
        author: &str,
        msg: PostMsg,
    ) -> Result<TaskId> {
        let _ = author;
        self.post(channel, sender, msg).await
    }

    /// Walk the channel's event log starting from `since`, returning
    /// every event from that line on. The checkpoint is an inclusive
    /// line offset ("skip the first N lines"): the event AT `since` is
    /// returned, so callers holding the last id they saw drop it to get
    /// strictly newer events. An empty `Checkpoint` (default) returns
    /// the entire log.
    ///
    /// Prefer [`Self::peek_tail`] for the canonical chat read ("the
    /// newest N messages") — this method always scans from the start
    /// of the log and materializes every matching event.
    async fn peek(&self, channel: &ChannelId, since: &Checkpoint) -> Result<Vec<ChannelEvent>>;

    /// Backward-anchored read: return the newest `limit` events at
    /// or before `before` (a line-number [`TaskId`]; `None` = the tip
    /// of the log), oldest→newest, each carrying its source line
    /// number. [`TailPage::has_more`] is true when the log holds
    /// events older than the first returned one — pass that event's
    /// id back as `before` to page further back.
    ///
    /// This is the canonical "last N messages" chat read. Adapters
    /// backed by the JSONL store ([`crate::ChannelStore`]) override it
    /// with a tail scan that parses only the returned lines.
    ///
    /// The default impl walks the full log via [`Self::peek_with_ids`]
    /// and slices the tail — correct for any adapter, but O(history).
    async fn peek_tail(
        &self,
        channel: &ChannelId,
        limit: usize,
        before: Option<&TaskId>,
    ) -> Result<TailPage> {
        let all = self.peek_with_ids(channel, &Checkpoint::default()).await?;
        let end = match before {
            None => all.len(),
            Some(b) => {
                let b: u64 = b.parse().map_err(|_| {
                    ChannelError::Adapter(format!(
                        "invalid before cursor {b:?}: not a numeric line offset"
                    ))
                })?;
                all.partition_point(|(id, _)| id.parse::<u64>().map(|n| n < b).unwrap_or(true))
            }
        };
        let start = end.saturating_sub(limit);
        Ok(TailPage {
            events: all[start..end].to_vec(),
            has_more: start > 0,
        })
    }

    /// Search the channel's `Posted` events, scanning BACKWARD from
    /// `query.before` (or the tip of the log) and returning up to
    /// `query.limit` matching events oldest→newest, each with its
    /// line-number [`TaskId`].
    ///
    /// This is the chat "find a message" read. There is deliberately
    /// no index: the scan is bounded by `limit` and a per-call scan
    /// budget ([`crate::store::SEARCH_MAX_SCAN_LINES`] on the JSONL
    /// store), and [`SearchPage::resume_before`] lets the caller
    /// continue into older history on a miss-heavy query. When
    /// [`SearchPage::has_more`] is true, older history exists but was
    /// not fully scanned — pass `resume_before` back as
    /// `query.before`.
    ///
    /// The default impl walks the whole log via
    /// [`Self::peek_with_ids`] and filters in memory (O(history), no
    /// scan budget); [`crate::ChannelStore`] overrides with a
    /// page-aware backward scan.
    async fn search(&self, channel: &ChannelId, query: &ChannelQuery) -> Result<SearchPage> {
        let all = self.peek_with_ids(channel, &Checkpoint::default()).await?;
        let before: Option<u64> = match &query.before {
            None => None,
            Some(b) => Some(b.parse().map_err(|_| {
                ChannelError::Adapter(format!(
                    "invalid before cursor {b:?}: not a numeric line offset"
                ))
            })?),
        };
        let needle = query.text.as_deref().map(str::to_lowercase);
        let mut matches: Vec<(TaskId, ChannelEvent)> = all
            .into_iter()
            .filter(|(id, ev)| {
                if let Some(b) = before {
                    if id.parse::<u64>().map(|n| n >= b).unwrap_or(false) {
                        return false;
                    }
                }
                query_matches(ev, needle.as_deref(), query.author.as_deref())
            })
            .collect();
        let overflow = matches.len() > query.limit;
        if overflow {
            matches = matches.split_off(matches.len() - query.limit);
        }
        Ok(SearchPage {
            events: matches,
            has_more: overflow,
            resume_before: None,
        })
    }

    /// Like [`Self::peek`] but each item carries its source `TaskId`
    /// (the line number where the event was appended in the channel's
    /// JSONL log). Used by the subscription loop to advance cursors
    /// precisely without re-decoding the wire event.
    ///
    /// **No default body.** A previous default of `Ok(Vec::new())` was
    /// a silent footgun: an adapter that forgot to override would
    /// compile clean, but the subscription loop at
    /// `crate::subscription::Subscriber::poll` would see zero events
    /// and never advance its cursor — effectively turning the channel
    /// into a no-op at runtime. Implementors MUST override; both
    /// production ports (`ChannelStore`, `TunnelChannelPort`) already
    /// do.
    async fn peek_with_ids(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>>;

    /// The post carrying stable id `msg_id`, with its line, if this
    /// channel's log holds one. Newest first, so a message posted twice
    /// under one id (a mirror) resolves to its latest copy.
    async fn find_post(
        &self,
        channel: &ChannelId,
        msg_id: &str,
    ) -> Result<Option<(TaskId, ChannelEvent)>> {
        let events = self.peek_with_ids(channel, &Checkpoint::default()).await?;
        Ok(events.into_iter().rev().find(
            |(_, ev)| matches!(ev, ChannelEvent::Posted { msg_id: Some(id), .. } if id == msg_id),
        ))
    }

    /// Remove `principal` from the channel membership set. Emits a
    /// `MemberLeft` event. PR-1: leaves are always permitted.
    async fn leave(&self, channel: &ChannelId, principal: &PrincipalId) -> Result<()>;

    /// Remove any member — a person as well as a principal — from a
    /// group. A principal leaving this way also loses its binding, as
    /// with [`Self::leave`]. Idempotent for non-members; a DM refuses.
    async fn leave_actor(&self, channel: &ChannelId, actor: &ActorRef) -> Result<()> {
        match actor {
            ActorRef::Principal(route) => {
                self.leave(channel, &PrincipalId(route.id().to_string()))
                    .await
            }
            _ => Err(ChannelError::Adapter(
                "this channel port cannot remove people".into(),
            )),
        }
    }

    /// All current members of the channel. ADR-049 Phase 1: members
    /// are [`ActorRef`]-typed — the returned list can contain both
    /// principals and users. Legacy on-disk member rows (bare
    /// principal ids, no `kind:` prefix) parse as
    /// `ActorRef::Principal`.
    async fn list_members(&self, channel: &ChannelId) -> Result<Vec<ActorRef>>;

    /// All channels where `principal` is a member. Walks each channel's
    /// member set; cheap in PR-1's small fan-out cap (≤ 8 channels).
    async fn list_for_principal(&self, principal: &PrincipalId) -> Result<Vec<ChannelId>>;

    /// Optional: IPC-shaped snapshot of a channel's membership + name.
    /// Has a default impl that walks `peek` + member events, so impls
    /// don't need to override.
    async fn membership(&self, channel: &ChannelId) -> Result<ChannelMembership> {
        // Default: walk MemberJoined/MemberLeft from the full event log.
        let events = self.peek(channel, &Checkpoint::default()).await?;
        let mut name = String::new();
        let mut creator = String::new();
        let mut created_at = String::new();
        let mut joined: HashSet<String> = Default::default();
        let mut left: HashSet<String> = Default::default();
        let mut last_change: Option<String> = None;
        for ev in events {
            match ev {
                ChannelEvent::Created {
                    name: n,
                    creator: c,
                    at,
                    ..
                } => {
                    name = n;
                    creator = c;
                    created_at = at;
                }
                ChannelEvent::MemberJoined { member: m, at, .. } => {
                    joined.insert(m);
                    last_change = Some(at);
                }
                ChannelEvent::MemberLeft { member: m, at, .. } => {
                    left.insert(m);
                    last_change = Some(at);
                }
                _ => {}
            }
        }
        joined.retain(|p| !left.contains(p));
        Ok(ChannelMembership {
            channel: channel.clone(),
            name,
            creator,
            members: joined.into_iter().collect(),
            member_provenance: Vec::new(),
            created_at,
            last_membership_change: last_change,
        })
    }

    /// ADR-095 D7: key this runtime's person's DMs by their current DM
    /// key (see [`ChannelStore::rekey_local_user_dms`]). Returns `(old,
    /// new, bound principal ids)` per moved DM. Adapters without DMs on
    /// disk move nothing.
    ///
    /// [`ChannelStore::rekey_local_user_dms`]: crate::ChannelStore::rekey_local_user_dms
    async fn rekey_local_user_dms(&self) -> Result<Vec<(ChannelId, ChannelId, Vec<String>)>> {
        Ok(Vec::new())
    }

    /// Where `channel` lives now (ADR-095 D7): itself, or where a DM rekey
    /// moved it. Adapters that never move channels answer the id given.
    async fn forwarded(&self, channel: &ChannelId) -> ChannelId {
        channel.clone()
    }

    /// ADR-095 D4: fill a DM's copy from a snapshot — record where its
    /// other members live (`remote`) and merge `posts` by `msg_id`, as
    /// verified history (nothing wakes, nothing fans out). Returns how
    /// many posts were added. Adapters without a cross-runtime roster
    /// refuse.
    async fn restore_dm_copy(
        &self,
        channel: &ChannelId,
        remote: &[RemoteMember],
        posts: Vec<ChannelEvent>,
    ) -> Result<usize> {
        let _ = (channel, remote, posts);
        Err(ChannelError::Adapter(
            "this channel adapter cannot restore a DM copy".into(),
        ))
    }

    /// P1.2 attribution: return each member of `channel` paired with
    /// their runtime provenance (local vs remote). The default impl
    /// walks the event log and returns an empty `runtime_id` for
    /// every row, so single-runtime adapters don't need to override.
    ///
    /// Adapters backed by an authoritative `members.json`
    /// (e.g. [`crate::ChannelStore`]) should override this to surface
    /// remote members with their `runtime_id`.
    async fn members_with_attribution(&self, channel: &ChannelId) -> Result<Vec<MemberProvenance>> {
        let membership = self.membership(channel).await?;
        // The default event-log walk can't distinguish local vs
        // remote — every row is treated as local.
        Ok(membership
            .members
            .into_iter()
            .map(|p| MemberProvenance {
                principal: p,
                runtime_id: None,
            })
            .collect())
    }

    /// Copy an existing Runtime-tier channel into the adapter's
    /// Shared tier (PR-3d). Returns the absolute Shared path on
    /// success. COPY semantics — the Runtime source remains so the
    /// channel is still reachable from `peko channel show`. Adapters
    /// without a Shared dir (CLI fallback that only knows the
    /// runtime dir) must return `ChannelError::Adapter` with a
    /// clear message.
    async fn pin_to_shared(&self, channel: &ChannelId) -> Result<std::path::PathBuf>;

    /// The channel's file store (ADR-094): attachments that cross
    /// runtimes, one file per SHA-256 digest. Adapters without one
    /// refuse.
    async fn files_dir(&self, channel: &ChannelId) -> Result<std::path::PathBuf> {
        let _ = channel;
        Err(ChannelError::Adapter(
            "this channel port keeps no files".into(),
        ))
    }

    /// Per-session digest read position: the highest line-number
    /// [`TaskId`] `session_key` has observed on `channel`. Keyed by
    /// session-id string in the channel's `read_marks.json` — a
    /// sibling of the per-principal subscriber `cursors.json`, which
    /// can never represent "what has this session been shown" (the
    /// subscriber cursor advances on every observed event).
    ///
    /// The default impl returns `None` ("never observed") so adapters
    /// without read-mark persistence (`NoopChannelPort`, in-memory
    /// test ports) don't need to override; the digest handler treats
    /// `None` as first-observation.
    async fn read_mark(&self, channel: &ChannelId, session_key: &str) -> Result<Option<TaskId>> {
        let _ = (channel, session_key);
        Ok(None)
    }

    /// Advance `session_key`'s read position on `channel` to `mark`.
    /// Implementations backed by the JSONL store refuse to rewind a
    /// numeric line number (a backward-paging `ChannelRead` must not
    /// move the digest position backwards). The default impl is a
    /// no-op so in-memory adapters don't need to override.
    async fn advance_read_mark(
        &self,
        channel: &ChannelId,
        session_key: &str,
        mark: TaskId,
    ) -> Result<()> {
        let _ = (channel, session_key, mark);
        Ok(())
    }

    /// Posts by other principals on `channel` that `session_key` has
    /// been shown but not handled (see [`crate::attention`]). The
    /// default impl returns none, for adapters without persistence.
    async fn attention(
        &self,
        channel: &ChannelId,
        session_key: &str,
    ) -> Result<Vec<crate::AttentionItem>> {
        let _ = (channel, session_key);
        Ok(Vec::new())
    }

    /// Replace `session_key`'s waiting list on `channel` (an empty list
    /// clears it). The default impl is a no-op.
    async fn set_attention(
        &self,
        channel: &ChannelId,
        session_key: &str,
        items: Vec<crate::AttentionItem>,
    ) -> Result<()> {
        let _ = (channel, session_key, items);
        Ok(())
    }

    /// ADR-101: the group's board, folded from its log; `None` for a DM.
    /// Authority is the registry's keys and the group's owner and
    /// admins as this runtime knows them.
    async fn board(&self, channel: &ChannelId) -> Result<Option<crate::board::Board>> {
        if self.conversation_kind(channel).await? != ConversationKind::Group {
            return Ok(None);
        }
        let events = self.peek_with_ids(channel, &Checkpoint::default()).await?;
        let info = self.group_info(channel).await?;
        let keys = SelfKeys(self);
        let auth = crate::board::Authority::new(
            &keys,
            info.as_ref().map(|i| i.owner.as_str()),
            info.as_ref().map(|i| i.admins.as_slice()).unwrap_or(&[]),
        );
        Ok(Some(crate::board::Board::fold(
            events.iter().map(|(_, ev)| ev),
            &auth,
        )))
    }

    /// ADR-100 + ADR-101: the board item an approved owner task of
    /// `member` serves in group `channel`, and what already covers its work
    /// ([`crate::board::Board::covered`]). `item` names the item (its id or
    /// short id); without one, it is the one item `member` holds and is
    /// working on. `None` for a DM, or when no item is found.
    async fn owner_task_item(
        &self,
        channel: &ChannelId,
        item: Option<&str>,
        member: &ActorRef,
    ) -> Result<Option<OwnerTaskItem>> {
        if self.conversation_kind(channel).await? != ConversationKind::Group {
            return Ok(None);
        }
        let events = self.peek_with_ids(channel, &Checkpoint::default()).await?;
        let info = self.group_info(channel).await?;
        let keys = SelfKeys(self);
        let auth = crate::board::Authority::new(
            &keys,
            info.as_ref().map(|i| i.owner.as_str()),
            info.as_ref().map(|i| i.admins.as_slice()).unwrap_or(&[]),
        );
        let board = crate::board::Board::fold(events.iter().map(|(_, ev)| ev), &auth);
        let me = member.to_string();
        let served = match item {
            Some(reference) => board.find(reference).ok(),
            None => {
                let mut held = board
                    .items
                    .iter()
                    .filter(|i| i.is_active() && crate::board::Board::holds(i, &me, &auth));
                match (held.next(), held.next()) {
                    (Some(only), None) => Some(only),
                    _ => None,
                }
            }
        };
        Ok(served.map(|served| OwnerTaskItem {
            item: served.clone(),
            covered: board.covered(served, &me, &auth).map(|c| CoveredBy {
                done: matches!(c, crate::board::Covered::Done(_)),
                item: c.item().clone(),
            }),
        }))
    }

    /// ADR-101 D6: the check-ins that already brought one of
    /// `principal`'s items back on `channel` (fired once each). The
    /// default keeps none.
    async fn fired_check_ins(&self, channel: &ChannelId, principal: &str) -> Result<Vec<String>> {
        let _ = (channel, principal);
        Ok(Vec::new())
    }

    /// Replace `principal`'s fired check-ins on `channel`. The default
    /// is a no-op.
    async fn set_fired_check_ins(
        &self,
        channel: &ChannelId,
        principal: &str,
        fired: Vec<String>,
    ) -> Result<()> {
        let _ = (channel, principal, fired);
        Ok(())
    }

    /// PR-2b: subscribe to live events for `channel`. The returned
    /// receiver yields every event appended to the channel after this
    /// call (events appended before subscription are NOT replayed —
    /// use [`Self::peek`] for the from-cursor history). The default
    /// impl returns a receiver that never fires, so adapters without
    /// a broadcast registry (in-memory tests, `NoopChannelPort`)
    /// don't need to override.
    async fn subscribe_events(
        &self,
        channel: &ChannelId,
    ) -> tokio::sync::broadcast::Receiver<ChannelEvent> {
        // Drop the sender so the receiver returns `RecvError::Closed`
        // immediately on first await. The test impls that use the
        // default avoid needing a broadcast registry. Note we still
        // need a `let _ = channel;` to avoid an unused-arg warning
        // on the default-impl signature.
        let _ = channel;
        let (tx, _rx) = tokio::sync::broadcast::channel::<ChannelEvent>(1);
        drop(tx);
        let (_tx, rx) = tokio::sync::broadcast::channel::<ChannelEvent>(1);
        rx
    }
}

/// How an actor is reachable (ADR-073/081): a local-only actor, a hosted
/// principal registered privately or publicly, or a remote Hub actor whose
/// visibility this runtime does not track. Serializes as `local_only`,
/// `private`, `public`, `registered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    #[default]
    LocalOnly,
    Private,
    Public,
    Registered,
}

impl Registration {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalOnly => "local_only",
            Self::Private => "private",
            Self::Public => "public",
            Self::Registered => "registered",
        }
    }

    /// Whether the actor has a Hub route.
    #[must_use]
    pub fn is_registered(self) -> bool {
        !matches!(self, Self::LocalOnly)
    }
}

impl std::fmt::Display for Registration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for Registration {
    /// Parse the wire form; anything unknown reads as `local_only`.
    fn from(text: &str) -> Self {
        match text {
            "private" => Self::Private,
            "public" => Self::Public,
            "registered" => Self::Registered,
            _ => Self::LocalOnly,
        }
    }
}

impl PartialEq<&str> for Registration {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

/// Public, addressable identity in contact lists and channel rosters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActorInfo {
    pub address: String,
    /// Present only for identities hosted by this runtime.
    pub local_id: Option<String>,
    pub hub_id: Option<String>,
    pub kind: String,
    pub registration: Registration,
    #[serde(skip)]
    pub identity_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mention: Option<String>,
}

impl ActorInfo {
    pub fn from_subject(subject: &ActorRef, name: Option<&str>) -> Self {
        let address = subject.canonical_address();
        let route = subject.route();
        Self {
            // A route qualified with another runtime keeps its qualifier,
            // so it can never be rebuilt as a local actor here.
            local_id: route.filter(|r| !r.is_hub()).map(|r| match r.runtime() {
                Some(runtime) => format!("{}@{runtime}", r.id()),
                None => r.id().to_owned(),
            }),
            hub_id: route.filter(|r| r.is_hub()).map(|r| r.id().to_owned()),
            kind: subject.kind().to_string(),
            registration: if route.is_some_and(Route::is_hub) {
                Registration::Registered
            } else {
                Registration::LocalOnly
            },
            identity_key: address.clone(),
            mention: matches!(subject, ActorRef::Principal(_)).then(|| format!("@{address}")),
            address,
            name: name.map(str::to_owned),
        }
    }
    pub(crate) fn bind_hub_id(&mut self, hub_id: Option<&str>) -> Result<()> {
        if !matches!(self.kind.as_str(), "principal" | "user" | "visitor")
            || (self.local_id.is_none() && self.hub_id.is_none())
            || self
                .local_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.contains(':'))
            || self
                .hub_id
                .as_deref()
                .is_some_and(|id| id.is_empty() || id.contains(':'))
            || hub_id.is_some_and(|id| id.is_empty() || id.contains(':'))
        {
            return Err(ChannelError::Adapter("Invalid actor identity".into()));
        }
        if self.local_id.is_none() {
            if hub_id.is_some() && self.hub_id.as_deref() != hub_id {
                return Err(ChannelError::Adapter(
                    "Hub reference and identity disagree".into(),
                ));
            }
        } else {
            self.hub_id = hub_id.map(str::to_owned);
        }
        Ok(())
    }
    pub fn hub_address(&self) -> Option<String> {
        self.hub_id
            .as_ref()
            .map(|id| format!("{}:hub:{id}", self.kind))
    }
    /// What a DM's pair id hashes for this actor (ADR-095 D7): its
    /// immutable key, except that a registered runtime's own person is
    /// their Hub account, as every other runtime names them. The person's
    /// registry key stays `local@<runtime DID>` (ADR-081 D3).
    pub fn dm_key(&self) -> String {
        match &self.hub_id {
            Some(hub) if self.kind == "user" && self.identity_key.starts_with("local@") => {
                format!("user:hub:{hub}")
            }
            _ => self.identity_key.clone(),
        }
    }
    pub fn subject(&self) -> ActorRef {
        let route = match (&self.local_id, &self.hub_id) {
            (Some(id), _) => Route::parse(&format!("local:{id}")),
            (None, Some(hub)) => Route::hub(hub.clone()),
            (None, None) => unreachable!("actor has an address"),
        };
        match self.kind.as_str() {
            "user" => ActorRef::User(route),
            "visitor" => ActorRef::Visitor(route),
            "principal" => ActorRef::Principal(route),
            other => unreachable!("registered actor kind {other}"),
        }
    }
}

/// Any port's registry as [`ActorKeys`], for the trait's own default
/// methods (which cannot make a `&dyn ChannelPort` of `Self`).
struct SelfKeys<'a, P: ?Sized>(&'a P);

impl<P: ChannelPort + ?Sized> ActorKeys for SelfKeys<'_, P> {
    fn key_of(&self, subject: &ActorRef) -> ActorKey {
        self.0.key_of(subject)
    }
}

/// A [`ChannelPort`]'s registry as the runtime's single identity
/// comparison (ADR-081 D1).
pub struct PortKeys<'a>(pub &'a dyn ChannelPort);

impl ActorKeys for PortKeys<'_> {
    fn key_of(&self, subject: &ActorRef) -> ActorKey {
        self.0.key_of(subject)
    }
}

/// A short mention is safe only when no other principal can own that name.
pub fn short_name_unambiguous(roster: &[ActorInfo], name: &str, address: &str) -> bool {
    let is_me = |p: &ActorInfo| {
        p.address == address
            || p.local_id.as_deref()
                == Some(
                    address
                        .strip_prefix("principal:local:")
                        .or_else(|| address.strip_prefix("principal:"))
                        .unwrap_or(address),
                )
    };
    roster
        .iter()
        .filter(|p| p.address.starts_with("principal:"))
        .all(|p| is_me(p) || p.name.is_some())
        && roster
            .iter()
            .filter(|p| {
                is_me(p)
                    || p.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .count()
            == 1
}

/// What starts a turn for a bound principal (ADR-082 D3). Whatever the
/// policy, a principal-authored post or a reply never does (ADR-072).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakePolicy {
    /// Every human (user or visitor) root post. A DM's fixed policy.
    Human,
    /// A human root post mentioning `@<name>`. A group's default.
    Mention,
    /// Nothing; posts wait for `ChannelRead` or the unread summary.
    Off,
}

impl WakePolicy {
    /// The policy a new binding starts with.
    pub const fn default_for(kind: ConversationKind) -> Self {
        match kind {
            ConversationKind::Dm => Self::Human,
            ConversationKind::Group => Self::Mention,
        }
    }

    /// The tool / wire form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Mention => "mention",
            Self::Off => "off",
        }
    }

    /// Parse the tool / wire form.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Self::Human),
            "mention" => Some(Self::Mention),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// A principal's link to a channel (ADR-082 D2), runtime-local and
/// never on the wire. Persisted in `meta.json` under `bindings`, keyed
/// by local principal id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The principal's conversation session for this channel (an id or
    /// a bare `/slug` path).
    pub session: String,
    /// What starts a turn in that session.
    pub wake: WakePolicy,
    /// The role (`roles/<id>.md`) the principal's turns run here
    /// (ADR-087 D4). `None` is the principal's default conversation role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Binding {
    pub fn new(session: impl Into<String>, wake: WakePolicy) -> Self {
        Self {
            session: session.into(),
            wake,
            role: None,
        }
    }

    /// The same binding running `role`.
    pub fn with_role(mut self, role: Option<String>) -> Self {
        self.role = role;
        self
    }
}

/// A post as it leaves this runtime in a history page or a snapshot
/// (ADR-091 D3, ADR-095 D4): its author by the address other runtimes
/// know (`author`), no session path (`via`), no line-number `parent` (it
/// is this copy's own; `reply_to` carries the reply), and attachments
/// without their path on this disk. Other events pass unchanged.
pub fn portable_post(event: ChannelEvent, author: String) -> ChannelEvent {
    match event {
        ChannelEvent::Posted {
            channel,
            text,
            at,
            msg_id,
            reply_to,
            attachments,
            options,
            model,
            board,
            ..
        } => ChannelEvent::Posted {
            channel,
            author,
            parent: None,
            text,
            at,
            via: None,
            msg_id,
            reply_to,
            attachments: attachments
                .into_iter()
                .map(|a| peko_protocol::channel::Attachment {
                    path: String::new(),
                    ..a
                })
                .collect(),
            options,
            model,
            // ADR-101: a copy caught up from history folds the same board.
            board,
        },
        other => other,
    }
}

/// Pair identity is order-independent and includes both complete addresses.
pub fn dm_channel_id(a: &str, b: &str) -> ChannelId {
    use sha2::{Digest, Sha256};
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let mut hash = Sha256::new();
    hash.update(b"peko-dm-v1\0");
    for address in [a, b] {
        hash.update((address.len() as u64).to_be_bytes());
        hash.update(address.as_bytes());
    }
    ChannelId(format!("dm_{:x}", hash.finalize()))
}

// ---------------------------------------------------------------------------
// ChannelEvent / PostMsg / CreateOpts / Checkpoint / TaskId
// ---------------------------------------------------------------------------

/// A posted message. `text` is the message body. `parent` is the
/// message being replied to; `None` for the channel's root post. The
/// adapter enforces at-most-one parent (see `plan_channel.rs`).
#[derive(Debug, Clone)]
pub struct PostMsg {
    pub text: String,
    pub parent: Option<TaskId>,
    /// Optional agent attribution: the session slug path (e.g.
    /// `sess:/user-a/reminder`) of the posting agent within the sender's
    /// principal. Persisted verbatim onto `ChannelEvent::Posted::via`
    /// — audit metadata only, never an authority claim. `None` for
    /// human/CLI posts and peer-authored inbound projections.
    pub via: Option<String>,
    /// Stable message id to post under. `None` (the usual case) lets the
    /// store mint one; a caller posting the SAME message into two
    /// channels (a local cross-principal send's record + delivery
    /// copies, a reply mirror) passes one id to both.
    pub msg_id: Option<String>,
    /// Files sent with the post (ADR-088); empty for most posts.
    pub attachments: Vec<peko_protocol::channel::Attachment>,
    /// Suggested replies (ADR-093); empty for most posts. The store
    /// refuses a post whose options break [`validate_options`].
    pub options: Vec<String>,
    /// ADR-097 D2: the model the author asks the receiving peko to
    /// answer with, for this post only. `None` for most posts.
    pub model: Option<String>,
    /// ADR-101: the board change this post makes; `None` for most
    /// posts. The store refuses an op that breaks [`crate::board::validate_op`].
    pub board: Option<peko_protocol::channel::BoardOp>,
}

impl PostMsg {
    /// Construct a root post (no parent).
    pub fn root(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parent: None,
            via: None,
            msg_id: None,
            attachments: Vec::new(),
            options: Vec::new(),
            model: None,
            board: None,
        }
    }

    /// Construct a reply post.
    pub fn reply(parent: TaskId, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parent: Some(parent),
            via: None,
            msg_id: None,
            attachments: Vec::new(),
            options: Vec::new(),
            model: None,
            board: None,
        }
    }

    /// Post under an existing stable message id (see the field docs).
    #[must_use]
    pub fn with_msg_id(mut self, msg_id: impl Into<String>) -> Self {
        self.msg_id = Some(msg_id.into());
        self
    }

    /// Send files with the post (ADR-088).
    #[must_use]
    pub fn with_attachments(
        mut self,
        attachments: Vec<peko_protocol::channel::Attachment>,
    ) -> Self {
        self.attachments = attachments;
        self
    }

    /// Offer suggested replies with the post (ADR-093).
    #[must_use]
    pub fn with_options(mut self, options: Vec<String>) -> Self {
        self.options = options;
        self
    }

    /// Make a board change with the post (ADR-101).
    #[must_use]
    pub fn with_board(mut self, op: peko_protocol::channel::BoardOp) -> Self {
        self.board = Some(op);
        self
    }

    /// Ask the receiving peko to answer with `model` (ADR-097 D2).
    #[must_use]
    pub fn with_model(mut self, model: Option<String>) -> Self {
        self.model = model;
        self
    }

    /// Stamp the posting agent's session slug path as the `via`
    /// attribution (see the field docs).
    #[must_use]
    pub fn with_via(mut self, via: impl Into<String>) -> Self {
        self.via = Some(via.into());
        self
    }
}

/// Fewest suggested replies a post may offer (ADR-093 D2).
pub const MIN_OPTIONS: usize = 2;
/// Most suggested replies a post may offer (ADR-093 D2).
pub const MAX_OPTIONS: usize = 4;
/// Longest suggested reply, in characters (ADR-093 D2).
pub const MAX_OPTION_CHARS: usize = 60;

/// Check a post's suggested replies (ADR-093 D2): none, or 2–4
/// distinct one-line options of 1–60 characters each (surrounding
/// whitespace does not count). Returns the options trimmed.
///
/// # Errors
/// A message naming the broken limit, written for the model that
/// offered the options.
pub fn validate_options(options: &[String]) -> std::result::Result<Vec<String>, String> {
    if options.is_empty() {
        return Ok(Vec::new());
    }
    if !(MIN_OPTIONS..=MAX_OPTIONS).contains(&options.len()) {
        return Err(format!(
            "offer {MIN_OPTIONS} to {MAX_OPTIONS} options, not {}",
            options.len()
        ));
    }
    let mut out: Vec<String> = Vec::with_capacity(options.len());
    for option in options {
        let option = option.trim();
        if option.is_empty() {
            return Err("an option is empty".to_string());
        }
        if option.contains(['\n', '\r']) {
            return Err(format!("option {option:?} spans more than one line"));
        }
        if option.chars().count() > MAX_OPTION_CHARS {
            return Err(format!(
                "option {option:?} is longer than {MAX_OPTION_CHARS} characters"
            ));
        }
        if out.iter().any(|o| o == option) {
            return Err(format!("option {option:?} is offered twice"));
        }
        out.push(option.to_string());
    }
    Ok(out)
}

/// Options for [`ChannelPort::create`].
#[derive(Debug, Default, Clone)]
pub struct CreateOpts {
    pub name: String,
    pub tier: Tier,
    /// Sprint 4 (`feat!`: consolidate `send_peer` into `ChannelSend`):
    /// optional explicit [`ChannelId`]. When `Some`, the store uses
    /// this id verbatim (after `parse`) instead of minting a fresh
    /// `chan_<8 base36>` via [`ChannelId::generate`]. Used by the
    /// peer-DM auto-provisioning path
    /// (`peko-rs/core/src/principal/peer_dm.rs`) to mint a
    /// deterministic `principal:<did>` channel id — both sides of a
    /// DM exchange derive the same id from the same DID.
    ///
    /// Collisions surface as [`ChannelError::Adapter`] (mirrors the
    /// idempotency check at `ChannelStore::join_remote`). The default
    /// `None` preserves the pre-PR `ChannelId::generate()` behavior.
    pub id: Option<ChannelId>,
    /// What the channel is (ADR-082 D1); immutable after create. A
    /// [`ConversationKind::Dm`] requires a pair id ([`dm_channel_id`]).
    pub kind: ConversationKind,
}

impl CreateOpts {
    /// Construct a Runtime-tier `CreateOpts` (PR-1 default).
    pub fn runtime(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tier: Tier::Runtime,
            id: None,
            kind: ConversationKind::Group,
        }
    }

    /// Construct a Shared-tier `CreateOpts` (PR-3d). The caller is
    /// responsible for the authority gate — the CLI does this via the
    /// Phase B `RuntimeAuthority::write_shared_channels` check.
    pub fn shared(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tier: Tier::Shared,
            id: None,
            kind: ConversationKind::Group,
        }
    }

    /// Set the channel's kind. See [`Self::kind`].
    pub fn with_kind(mut self, kind: ConversationKind) -> Self {
        self.kind = kind;
        self
    }

    /// Sprint 4: pin a specific [`ChannelId`] for `create` to use.
    /// The id is validated at the store layer (parsed via
    /// `ChannelId::parse`); an invalid wire form surfaces as
    /// [`ChannelError::Adapter`].
    pub fn with_id(mut self, id: ChannelId) -> Self {
        self.id = Some(id);
        self
    }
}

/// Storage tier. PR-1 supported [`Tier::Runtime`] only.
///
/// PR-3d adds [`Tier::Shared`]: a channel created with this tier (or
/// promoted via [`ChannelPort::pin_to_shared`]) lives under the
/// principal's shared root (`<shared_dir>/channels/<channel_id>/...`)
/// so other principals on the same runtime can discover it.
///
/// **DO NOT** introduce a 4th tier — channels live *in* an existing
/// tier, not as their own. The Phase B authority gate (`write_shared`)
/// is reused as-is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Tier {
    /// `<runtime_dir>/channels/<channel_id>/...` — ephemeral,
    /// session-scoped. Default.
    #[default]
    Runtime,
    /// `<shared_dir>/channels/<channel_id>/...` — visible across
    /// principals (PR-3d). Must be opted in via `pin_to_shared`; we
    /// do NOT default-create channels in Shared because that would
    /// silently leak session state to other principals.
    Shared,
}

/// Per-principal per-channel checkpoint — opaque cursor into the
/// channel's event log. Newtype (instead of bare `String`) so the type
/// guides callers to wrap sensibly in their own state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checkpoint(pub TaskId);

impl Checkpoint {
    /// Empty checkpoint that "starts from the beginning".
    pub fn zero() -> Self {
        Self(String::new())
    }
}

/// Opaque message identifier (string alias). For the JSONL-backed
/// [`crate::ChannelStore`], the id is the line number where the event
/// was appended.
///
/// The wire form (`peko_protocol::channel::ChannelEvent::Posted.parent`)
/// carries the same string — kept as a type alias here so consumers
/// don't need to import from `peko-protocol`.
pub type TaskId = String;

/// A backward-anchored page of the channel log, returned by
/// [`ChannelPort::peek_tail`]. `events` are oldest→newest, each with
/// its line-number [`TaskId`]; `has_more` is true when older events
/// exist beyond the first returned one.
#[derive(Debug, Clone)]
pub struct TailPage {
    pub events: Vec<(TaskId, ChannelEvent)>,
    pub has_more: bool,
}

/// Filter for [`ChannelPort::search`]. All set predicates must match
/// (AND). A query with no predicates matches every `Posted` event —
/// it degenerates to a bounded backward browse.
#[derive(Debug, Clone, Default)]
pub struct ChannelQuery {
    /// Case-insensitive substring match against `Posted.text`.
    pub text: Option<String>,
    /// Exact match against `Posted.author` (the wire form, e.g.
    /// `user:alice` or a bare principal id).
    pub author: Option<String>,
    /// Only consider lines strictly older than this line number.
    /// `None` starts from the tip of the log.
    pub before: Option<TaskId>,
    /// Max matches to return. The store clamps this to a hard cap.
    pub limit: usize,
}

/// A page of search results, returned by [`ChannelPort::search`].
/// `events` are oldest→newest. `has_more` is true when older history
/// exists but was not fully scanned (scan budget exhausted or the
/// log continues past the scanned range); `resume_before` is the
/// oldest line the scan examined — pass it back as
/// [`ChannelQuery::before`] to continue. `resume_before` is `None`
/// when the scan reached the top of the log.
#[derive(Debug, Clone)]
pub struct SearchPage {
    pub events: Vec<(TaskId, ChannelEvent)>,
    pub has_more: bool,
    pub resume_before: Option<TaskId>,
}

/// Shared match predicate: `ev` must be a `Posted` event whose `text`
/// contains `needle` (already lowercased; case-insensitive) and whose
/// `author` equals `author`. `None` predicates match everything.
pub(crate) fn query_matches(ev: &ChannelEvent, needle: Option<&str>, author: Option<&str>) -> bool {
    let ChannelEvent::Posted {
        text, author: a, ..
    } = ev
    else {
        return false;
    };
    if let Some(n) = needle {
        if !text.to_lowercase().contains(n) {
            return false;
        }
    }
    if let Some(want) = author {
        if a != want {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// RemoteMember (PR-B cross-runtime)
// ---------------------------------------------------------------------------

/// A principal that lives on a different runtime than the one that
/// owns the channel's source-of-truth JSONL log.
///
/// Introduced by peko-channel cross-runtime PR-B. The creator's
/// runtime writes events to its `events.jsonl` and pushes them to
/// every other runtime that hosts a remote member of the channel;
/// those runtimes maintain a local mirror and a member row of this
/// shape so they know which runtime to send their own writes
/// through.
///
/// The DID forms (`runtime_id` is `did:key:z...`, `principal_id` is
/// `did:peko:principal:<hash>`) are kept as `String`s for back-compat
/// with `MembersJson`'s pre-PR-B shape (string-only member rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMember {
    /// The runtime that hosts the member's principal. `did:key:z...`
    /// form (self-certifying — the public key is derivable).
    pub runtime_id: String,
    /// The principal's stable DID on the remote runtime. Stringified
    /// so the on-disk `MembersJson` shape matches the pre-PR-B
    /// `members: Vec<String>` convention.
    pub principal_id: String,
}

// `Display` is already provided by `String` via the orphan rule; no
// manual impl needed.

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Channel-layer errors. The `Io` and `Serde` variants wrap inner
/// errors via `#[from]` so callers can use `?` freely.
#[derive(Debug, Error)]
pub enum ChannelError {
    /// Channel id passed to a method that doesn't exist on disk.
    #[error("channel not found: {0}")]
    NotFound(ChannelId),

    /// Caller isn't a member of the channel they're trying to act in.
    #[error("caller is not a member of the channel")]
    NotMember,

    /// `cursors.json` read/write failure.
    #[error("cursor error: {0}")]
    Cursor(String),

    /// I/O fallback (mostly for ad-hoc paths inside adapters).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// JSON encode/decode error.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    /// Fan-out cap exceeded. PR-1: hard cap of 8 principal members per
    /// channel (ADR-049 Phase 4: user members are uncapped — they add
    /// no per-post fan-out cost; see `store::FAN_OUT_CAP`).
    #[error("fan-out cap exceeded ({current}); max 8 pekos per channel")]
    FanOutCap { current: usize },

    /// Generic adapter-level error. Avoid for new code; prefer a
    /// dedicated variant.
    #[error("channel adapter: {0}")]
    Adapter(String),
}

/// Crate-level result alias. Every fallible function in `peko-channel`
/// returns `Result<T, ChannelError>`.
pub type Result<T> = std::result::Result<T, ChannelError>;

// ---------------------------------------------------------------------------
// NoopChannelPort
// ---------------------------------------------------------------------------

/// A `ChannelPort` that returns `Adapter` for every method. Used by
/// tests + `ToolRuntime::register_builtins` callers that don't have a
/// real adapter wired up yet. The `ChannelRead` tool still works through
/// this — `peek` returns `Adapter`, which surfaces as a hard error to
/// the LLM (no silent zero events).
///
/// Lives here (next to `ChannelPort`) so the test-only registration
/// sites in `ToolRuntime` don't have to invent their own no-op type.
#[derive(Debug, Default, Clone)]
pub struct NoopChannelPort;

#[async_trait]
impl ChannelPort for NoopChannelPort {
    async fn create(&self, _creator: &PrincipalId, _opts: CreateOpts) -> Result<ChannelId> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn invite(
        &self,
        _channel: &ChannelId,
        _inviter: &PrincipalId,
        _invitee: &ActorRef,
    ) -> Result<()> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn post(
        &self,
        _channel: &ChannelId,
        _sender: &ActorRef,
        _msg: PostMsg,
    ) -> Result<TaskId> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn peek(&self, _channel: &ChannelId, _since: &Checkpoint) -> Result<Vec<ChannelEvent>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn peek_with_ids(
        &self,
        _channel: &ChannelId,
        _since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn leave(&self, _channel: &ChannelId, _principal: &PrincipalId) -> Result<()> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn list_members(&self, _channel: &ChannelId) -> Result<Vec<ActorRef>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn list_for_principal(&self, _principal: &PrincipalId) -> Result<Vec<ChannelId>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn pin_to_shared(&self, _channel: &ChannelId) -> Result<std::path::PathBuf> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Global registry (mirrors `peko_cron::tools::set_global_runtime`)
// ---------------------------------------------------------------------------

static GLOBAL_CHANNEL_PORT: std::sync::OnceLock<std::sync::Arc<dyn ChannelPort>> =
    std::sync::OnceLock::new();

/// Install the process-wide channel port. Called once by the daemon at
/// startup (right after it builds the real file-backed port); later
/// calls are silently ignored (same semantics as the cron runtime
/// port). Principal-side tool-bag installs
/// (`PrincipalContext::core()`) read the port back through
/// [`global_channel_port`] so a late re-registration of the global
/// `ChannelRead` / `ChannelSend` tools keeps the real adapter instead
/// of clobbering it with a [`NoopChannelPort`].
pub fn set_global_channel_port(port: std::sync::Arc<dyn ChannelPort>) {
    let _ = GLOBAL_CHANNEL_PORT.set(port);
}

/// The installed channel port, if the daemon has started.
#[must_use]
pub fn global_channel_port() -> Option<std::sync::Arc<dyn ChannelPort>> {
    GLOBAL_CHANNEL_PORT.get().cloned()
}

#[cfg(test)]
mod global_registry_tests {
    use super::*;

    /// Set-once registry: `set_global_channel_port` installs the port
    /// and `global_channel_port` hands the same `Arc` back. Single
    /// test by design — the `OnceLock` is process-global, so no other
    /// test in this crate binary may call `set_global_channel_port`.
    #[test]
    fn set_then_get_returns_installed_port() {
        let port: std::sync::Arc<dyn ChannelPort> = std::sync::Arc::new(NoopChannelPort);
        set_global_channel_port(std::sync::Arc::clone(&port));
        let got = global_channel_port().expect("port was just installed");
        assert!(std::sync::Arc::ptr_eq(&got, &port));
        // Second install is silently ignored (set-once semantics).
        let other: std::sync::Arc<dyn ChannelPort> = std::sync::Arc::new(NoopChannelPort);
        set_global_channel_port(other);
        let got = global_channel_port().expect("port remains installed");
        assert!(std::sync::Arc::ptr_eq(&got, &port));
    }
}
