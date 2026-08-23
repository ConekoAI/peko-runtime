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
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership, MemberProvenance};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Port trait for the multi-principal chat primitive.
///
/// `peko-rs/core` and `peko-rs/cli` hold `Arc<dyn ChannelPort>` on
/// their respective contexts. The concrete impl is
/// [`crate::ChannelStore`] (file-backed, append-only JSONL event log +
/// member set). Future impls — in-memory for tests, network-backed
/// for distributed deployments — slot in without changing call sites.
#[async_trait]
pub trait ChannelPort: Send + Sync + 'static {
    /// Create a new channel. The creator is automatically added as the
    /// first member. Returns the generated [`ChannelId`].
    async fn create(&self, creator: &PrincipalId, opts: CreateOpts) -> Result<ChannelId>;

    /// Add an invitee to a channel. Returns [`ChannelError::NotMember`]
    /// if `inviter` isn't already a member; [`ChannelError::FanOutCap`]
    /// if the channel already has 8 members (PR-1 cap; PR-3 may lift).
    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &PrincipalId,
    ) -> Result<()>;

    /// Post a message. The adapter enforces the at-most-one-parent
    /// convention — `msg.parent` references the line number of the
    /// message being replied to. Returns the new message's [`TaskId`]
    /// (the line number it was assigned in the channel's event log).
    async fn post(&self, channel: &ChannelId, sender: &PrincipalId, msg: PostMsg)
        -> Result<TaskId>;

    /// Phase 11 (agent-session paradigm sprint): like [`Self::post`]
    /// but writes an explicit `author` string onto the event instead
    /// of deriving it from `sender`. Membership + parent validation
    /// are still enforced against `sender` — `author` is a display
    /// attribution, not an authority claim.
    ///
    /// Used by the per-peer DM channels: the inbound message is posted
    /// with `sender = principal.id` (the channel's creator/member —
    /// the human peer is deliberately not added to membership) and
    /// `author = peer.to_string()` (the Subject wire form, e.g.
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
        sender: &PrincipalId,
        author: &str,
        msg: PostMsg,
    ) -> Result<TaskId> {
        let _ = author;
        self.post(channel, sender, msg).await
    }

    /// Walk the channel's event log starting from `since`, returning
    /// every event keyed at a strictly later `TaskId`. An empty
    /// `Checkpoint` (default) returns the entire log.
    async fn peek(&self, channel: &ChannelId, since: &Checkpoint) -> Result<Vec<ChannelEvent>>;

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

    /// Remove `principal` from the channel membership set. Emits a
    /// `MemberLeft` event. PR-1: leaves are always permitted.
    async fn leave(&self, channel: &ChannelId, principal: &PrincipalId) -> Result<()>;

    /// All current members of the channel.
    async fn list_members(&self, channel: &ChannelId) -> Result<Vec<PrincipalId>>;

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

    /// Phase 4 (agent-session paradigm sprint): the channel's passive
    /// binding — a session id or `/path` declared at create time
    /// (`CreateOpts::passive_binding`), persisted in `meta.json`.
    /// `None` means the channel is unbound and behaves exactly as
    /// before (active polling via the `channel read` tool only). The
    /// default impl returns `None` so adapters without binding support
    /// (`NoopChannelPort`, in-memory tests) don't need to override.
    async fn passive_binding(&self, channel: &ChannelId) -> Result<Option<String>> {
        let _ = channel;
        Ok(None)
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
}

impl PostMsg {
    /// Construct a root post (no parent).
    pub fn root(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parent: None,
        }
    }

    /// Construct a reply post.
    pub fn reply(parent: TaskId, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parent: Some(parent),
        }
    }
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
    /// Phase 4 (agent-session paradigm sprint): optional **passive
    /// binding** — a session id or `/path` in the creator principal's
    /// session tree. When set, the daemon's `PassiveBindingResponder`
    /// wakes the bound session on every inbound `Posted` event from
    /// another member and posts the reply back (DM-tier semantics,
    /// paradigm §3.1 type 1). `None` keeps the channel purely active
    /// (group tier, paradigm §3.1 type 2). Persisted to `meta.json`;
    /// immutable after create.
    pub passive_binding: Option<String>,
}

impl CreateOpts {
    /// Construct a Runtime-tier `CreateOpts` (PR-1 default).
    pub fn runtime(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tier: Tier::Runtime,
            id: None,
            passive_binding: None,
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
            passive_binding: None,
        }
    }

    /// Attach a passive binding (session id or `/path`). See
    /// [`Self::passive_binding`].
    pub fn with_passive_binding(mut self, binding: impl Into<String>) -> Self {
        self.passive_binding = Some(binding.into());
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

    /// Fan-out cap exceeded. PR-1: hard cap of 8 members per channel.
    #[error("fan-out cap exceeded ({current}); max 8 members per channel")]
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
        _invitee: &PrincipalId,
    ) -> Result<()> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn post(
        &self,
        _channel: &ChannelId,
        _sender: &PrincipalId,
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

    async fn list_members(&self, _channel: &ChannelId) -> Result<Vec<PrincipalId>> {
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
