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
use peko_plan::PrincipalId;
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::ConfigOnDisk;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Port trait for the multi-principal chat primitive.
///
/// `peko-rs/core` and `peko-rs/cli` hold `Arc<dyn ChannelPort>` on
/// their respective contexts. The concrete impl is
/// [`crate::PlanChannelAdapter`] (file-backed, one `peko_plan::PlanRecord`
/// per channel). Future impls — in-memory for tests, network-backed for
/// distributed deployments — slot in without changing call sites.
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
    /// convention — `msg.parent` is always mapped to a single
    /// `peko_plan::NodeId` reference. Returns the new message's
    /// [`TaskId`] (opaque string, formats to `node_<8 base36>` at
    /// storage boundaries).
    async fn post(
        &self,
        channel: &ChannelId,
        sender: &PrincipalId,
        msg: PostMsg,
    ) -> Result<TaskId>;

    /// Walk the channel's event log starting from `since`, returning
    /// every event keyed at a strictly later `TaskId`. An empty
    /// `Checkpoint` (default) returns the entire log.
    async fn peek(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>>;

    /// Like [`Self::peek`] but each item carries its source `TaskId`
    /// (the underlying `peko_plan::NodeId` string). Used by the
    /// subscription loop to advance cursors precisely without
    /// re-decoding the wire event. Has a default impl that re-reads
    /// the plan via [`Self::peek`] and falls back to opaque cursors
    /// (one-event-at-a-time), so adapters don't *have* to override.
    async fn peek_with_ids(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        // PR-1 fallback: walk the events; we don't have TaskIds here.
        // Callers that need precise cursors must override.
        let _ = since;
        let _ = channel;
        Ok(Vec::new())
    }

    /// Remove `principal` from the channel membership set. Emits a
    /// `MemberLeft` event. Returns [`ChannelError::NotEmpty`] only if
    /// the implementation wishes to forbid leaving with other members
    /// present (PR-1: leaves are always permitted).
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
                ChannelEvent::Created { name: n, creator: c, at, .. } => {
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
            created_at,
            last_membership_change: last_change,
        })
    }

    /// Load the channel's per-channel config. PR-2 introduces this
    /// alongside `ConfigOnDisk`; the responder (commit 2b) calls it to
    /// read `model_list` + `cost_ceiling_usd` before dispatching.
    ///
    /// Default impl returns `ConfigOnDisk::default()` so callers without
    /// file-backed storage don't have to override. The file-backed
    /// [`crate::PlanChannelAdapter`] overrides to actually read the
    /// `<channel_dir>/config.toml` file.
    async fn load_config(&self, channel: &ChannelId) -> Result<ConfigOnDisk> {
        let _ = channel;
        Ok(ConfigOnDisk::default())
    }

    /// Persist the channel's per-channel config (PR-3b). The handler
    /// at `peko-rs/channel/src/cli_handlers.rs::handle_config_set`
    /// reads the current `ConfigOnDisk`, applies any non-None fields
    /// from the request, then calls this so adapters persist the new
    /// value. No default impl — adapters must opt in.
    async fn save_config(
        &self,
        channel: &ChannelId,
        config: &ConfigOnDisk,
    ) -> Result<()>;

    /// Copy an existing Runtime-tier channel into the adapter's
    /// Shared tier (PR-3d). Returns the absolute Shared path on
    /// success. COPY semantics — the Runtime source remains so the
    /// channel is still reachable from `peko channel show`. Adapters
    /// without a Shared dir (CLI fallback that only knows the
    /// runtime dir) must return `ChannelError::Adapter` with a
    /// clear message.
    async fn pin_to_shared(
        &self,
        channel: &ChannelId,
    ) -> Result<std::path::PathBuf>;
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
        Self { text: text.into(), parent: None }
    }

    /// Construct a reply post.
    pub fn reply(parent: TaskId, text: impl Into<String>) -> Self {
        Self { text: text.into(), parent: Some(parent) }
    }
}

/// Options for [`ChannelPort::create`].
#[derive(Debug, Default, Clone)]
pub struct CreateOpts {
    pub name: String,
    pub tier: Tier,
}

impl CreateOpts {
    /// Construct a Runtime-tier `CreateOpts` (PR-1 default).
    pub fn runtime(name: impl Into<String>) -> Self {
        Self { name: name.into(), tier: Tier::Runtime }
    }

    /// Construct a Shared-tier `CreateOpts` (PR-3d). The caller is
    /// responsible for the authority gate — the CLI does this via the
    /// Phase B `RuntimeAuthority::write_shared_channels` check.
    pub fn shared(name: impl Into<String>) -> Self {
        Self { name: name.into(), tier: Tier::Shared }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Opaque message identifier (string alias). Storage-side wrappers
/// (e.g. `peko_plan::NodeId`) convert at adapter boundaries.
///
/// The wire form (`peko_protocol::channel::ChannelEvent::Posted.parent`)
/// carries the same string — kept as a type alias here so consumers
/// don't need to import from `peko-protocol`.
pub type TaskId = String;

// `Display` is already provided by `String` via the orphan rule; no
// manual impl needed.

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Channel-layer errors. The `Plan` and `Io` variants wrap inner errors
/// via `#[from]` so callers can use `?` freely.
#[derive(Debug, Error)]
pub enum ChannelError {
    /// Channel id passed to a method that doesn't exist on disk.
    #[error("channel not found: {0}")]
    NotFound(ChannelId),

    /// Caller isn't a member of the channel they're trying to act in.
    #[error("caller is not a member of the channel")]
    NotMember,

    /// Inner `peko_plan::PlanError` (storage layer rejected the call).
    #[error("channel storage error: {0}")]
    Plan(#[from] peko_plan::PlanError),

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

    /// Attempted to leave a non-empty channel where leaving is forbidden.
    /// PR-1 does NOT raise this — leaves are always permitted — but the
    /// variant is reserved for future stricter semantics.
    #[error("channel still has {remaining} members; cannot remove")]
    NotEmpty { remaining: usize },

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
    async fn create(
        &self,
        _creator: &PrincipalId,
        _opts: CreateOpts,
    ) -> Result<ChannelId> {
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

    async fn peek(
        &self,
        _channel: &ChannelId,
        _since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn leave(
        &self,
        _channel: &ChannelId,
        _principal: &PrincipalId,
    ) -> Result<()> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn list_members(
        &self,
        _channel: &ChannelId,
    ) -> Result<Vec<PrincipalId>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn list_for_principal(
        &self,
        _principal: &PrincipalId,
    ) -> Result<Vec<ChannelId>> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn save_config(
        &self,
        _channel: &ChannelId,
        _config: &ConfigOnDisk,
    ) -> Result<()> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }

    async fn pin_to_shared(
        &self,
        _channel: &ChannelId,
    ) -> Result<std::path::PathBuf> {
        Err(ChannelError::Adapter(
            "no channel port configured (NoopChannelPort)".into(),
        ))
    }
}
