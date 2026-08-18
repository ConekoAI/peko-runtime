//! `ChannelStore` — concrete [`ChannelPort`] impl backed by an
//! append-only JSONL event log + a member-set JSON file.
//!
//! ## On-disk layout
//!
//! ```text
//! <runtime_dir>/
//!   channels/
//!     <chan_id>/
//!       meta.json       # { creator, name, created_at, tier, passive_binding? }
//!       members.json    # { members: [String] }
//!       events.jsonl    # one ChannelEvent per line, append-only
//! ```
//!
//! Symmetric with [`peko_chat_log::ChatLogStore`] — append-only JSONL
//! with [`FileLock`] + [`append_bytes_durable`] for crash safety. The
//! cursor is a count-based offset into `events.jsonl`.
//!
//! ## Why no DAG
//!
//! Reply chains are carried by the `parent` field on
//! [`ChannelEvent::Posted`] — the wire form references the parent
//! message's `TaskId` (the line number where the parent event lives).
//! There is no PlanNode DAG, and `peko-plan` is no longer a dependency
//! of this crate.
//!
//! ## TaskId shape
//!
//! Every event gets a `TaskId` equal to its line number in
//! `events.jsonl`. Line 0 is the channel's `Created` event; lines
//! after that are `MemberJoined`, `Posted`, and `MemberLeft` events in
//! causal (insertion) order. Line numbers are stable because the log
//! is append-only — no rotations, no deletions, no rewrites.
//!
//! [`FileLock`]: peko_fs_persistence::FileLock
//! [`append_bytes_durable`]: peko_fs_persistence::append_bytes_durable

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use peko_fs_persistence::{append_bytes_durable, FileLock};
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast;

use crate::port::{
    ChannelError, ChannelPort, Checkpoint, CreateOpts, PostMsg, RemoteMember, Result, TaskId,
    Tier,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Hard cap on members per channel. PR-1 default. PR-3 may make this
/// configurable on the channel.
pub const FAN_OUT_CAP: usize = 8;

/// Channel directory under the runtime tier root.
const CHANNELS_DIR: &str = "channels";

/// Per-channel metadata file (owner, name, created_at). Members live in
/// `MEMBERS_FILE` so a high-frequency membership change does not force
/// a rewrite of the channel's creation metadata.
const META_FILE: &str = "meta.json";

/// Per-channel member set. Authoritative "who is currently a member"
/// answer; `peek`'s `MemberJoined - MemberLeft` walk is for replay only.
const MEMBERS_FILE: &str = "members.json";

/// Per-channel append-only event log. One [`ChannelEvent`] per line,
/// JSON-serialized with a trailing `\n`.
const EVENTS_FILE: &str = "events.jsonl";

/// Per-shard lock timeout for `events.jsonl` writes. Mirrors
/// `peko_chat_log::store::CHAT_LOG_LOCK_TIMEOUT_MS`.
const CHANNEL_LOCK_TIMEOUT_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// ChannelConfig
// ---------------------------------------------------------------------------

/// Construction-time config for [`ChannelStore::new`]. Cheap to clone.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Runtime-tier root.
    pub runtime_dir: PathBuf,
    /// Shared-tier root (PR-3d). `None` means "no Shared support" —
    /// `pin_to_shared` returns [`ChannelError::Adapter`]. Production
    /// (the daemon) populates this from
    /// `PathResolver::shared_channels_dir` /
    /// `RuntimeAuthority::shared_channels_dir`.
    pub shared_dir: Option<PathBuf>,
}

impl ChannelConfig {
    /// `<runtime_dir>/channels` — where every Runtime-tier channel
    /// directory lives.
    fn channels_dir(&self) -> PathBuf {
        self.runtime_dir.join(CHANNELS_DIR)
    }

    /// `<shared_dir>/channels` — where every Shared-tier channel
    /// directory lives. Returns `None` if `shared_dir` is unset so
    /// callers can emit a clean [`ChannelError::Adapter`] rather than
    /// silently creating `<runtime_dir>/shared/channels`.
    fn shared_channels_dir(&self) -> Option<PathBuf> {
        self.shared_dir.as_ref().map(|d| d.join(CHANNELS_DIR))
    }

    /// Pick the channel dir for the given tier.
    fn channel_dir_for(&self, tier: Tier, channel: &ChannelId) -> Result<PathBuf> {
        match tier {
            Tier::Runtime => Ok(self.channel_dir(channel)),
            Tier::Shared => self
                .shared_channels_dir()
                .map(|d| d.join(channel.as_str()))
                .ok_or_else(|| {
                    ChannelError::Adapter(
                        "ChannelConfig::shared_dir is None; cannot resolve Shared tier"
                            .into(),
                    )
                }),
        }
    }

    /// `<runtime_dir>/channels/<chan_id>/` — the per-channel sandbox.
    fn channel_dir(&self, channel: &ChannelId) -> PathBuf {
        self.channels_dir().join(channel.as_str())
    }
}

// ---------------------------------------------------------------------------
// MetaJson
// ---------------------------------------------------------------------------

/// On-disk metadata for a single channel. Stored at `<chan_dir>/meta.json`.
///
/// Membership is *not* tracked here — it lives in `members.json`. This
/// split keeps `meta.json` write-light (only on create).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaJson {
    pub creator: String,
    pub name: String,
    pub created_at: chrono::DateTime<Utc>,
    /// Storage tier where this channel's `events.jsonl` lives.
    /// Snapshotted at create-time so subsequent ops don't have to
    /// probe both Runtime and Shared roots.
    pub tier: Tier,
    /// Phase 4 (agent-session paradigm sprint): optional passive
    /// binding — a session id or `/path` in the creator's session
    /// tree. `#[serde(default)]` so pre-Phase-4 `meta.json` files
    /// deserialize as `None` without a migration; skipped on write
    /// when `None` so unbound channels keep the legacy file shape
    /// byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passive_binding: Option<String>,
}

impl MetaJson {
    fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join(META_FILE)
    }

    async fn load(channel_dir: &Path) -> Result<Self> {
        let p = Self::path_in(channel_dir);
        match fs::read(&p).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                ChannelError::Adapter(format!("decode {}: {e}", p.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ChannelError::NotFound(
                ChannelId(
                    channel_dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string(),
                ),
            )),
            Err(e) => Err(ChannelError::Adapter(format!("read {}: {e}", p.display()))),
        }
    }

    async fn save(&self, channel_dir: &Path) -> Result<()> {
        let p = Self::path_in(channel_dir);
        fs::create_dir_all(channel_dir).await?;
        let pid = std::process::id();
        let tmp = channel_dir.join(format!(".meta.json.{pid}.tmp"));
        let bytes = serde_json::to_vec_pretty(self)?;
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| ChannelError::Adapter(format!("write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &p)
            .await
            .map_err(|e| ChannelError::Adapter(format!("rename {}: {e}", p.display())))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MembersJson
// ---------------------------------------------------------------------------

/// Authoritative member set for a channel. Stored at
/// `<chan_dir>/members.json`.
///
/// Membership changes are infrequent (≤ FAN_OUT_CAP), so atomic
/// tmp+rename is fine — see [`Self::save`].
///
/// ## On-disk back-compat (PR-B cross-runtime)
///
/// Pre-PR-B `members.json` files contained just `Vec<String>` of local
/// principal ids. PR-B adds `remote_members: Vec<RemoteMember>` for
/// principals that live on other runtimes; the field defaults to `[]`
/// when absent so legacy files deserialize cleanly without a
/// migration. New writes always serialize both fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MembersJson {
    pub members: Vec<String>,
    /// Members that live on other runtimes. PR-B cross-runtime.
    /// `#[serde(default)]` so files written before PR-B deserialize
    /// as `vec![]` without a migration pass.
    #[serde(default)]
    pub remote_members: Vec<RemoteMember>,
}

impl MembersJson {
    fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join(MEMBERS_FILE)
    }

    async fn load(channel_dir: &Path) -> Result<Self> {
        let p = Self::path_in(channel_dir);
        match fs::read(&p).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                ChannelError::Adapter(format!("decode {}: {e}", p.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ChannelError::Adapter(format!("read {}: {e}", p.display()))),
        }
    }

    async fn save(&self, channel_dir: &Path) -> Result<()> {
        let p = Self::path_in(channel_dir);
        let pid = std::process::id();
        let tmp = channel_dir.join(format!(".members.json.{pid}.tmp"));
        let bytes = serde_json::to_vec_pretty(self)?;
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|e| ChannelError::Adapter(format!("write {}: {e}", tmp.display())))?;
        fs::rename(&tmp, &p)
            .await
            .map_err(|e| ChannelError::Adapter(format!("rename {}: {e}", p.display())))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ChannelStore
// ---------------------------------------------------------------------------

/// File-backed [`ChannelPort`] implementation. `Send + Sync` via
/// `Clone` (every field is cheap to clone; the lock map lives per-event
/// inside helper methods, not on the struct).
#[derive(Debug, Clone)]
pub struct ChannelStore {
    cfg: ChannelConfig,
    /// PR-2b: per-channel event broadcast registry. Each entry holds
    /// a `broadcast::Sender` whose `Receiver`s are leased by
    /// [`ChannelPort::subscribe_events`] callers. The capacity is
    /// generous (256) so a slow consumer doesn't drop events during
    /// bursty posts; in practice the desktop Tauri command forwards
    /// each event within microseconds of receipt.
    ///
    /// Wrapped in `Arc` so the `Clone` impl stays cheap (the
    /// `broadcast::Sender` itself isn't `Clone` — receivers are
    /// leased from it). Lazy: entries are created on first
    /// subscribe. Dead entries (no receivers) are GC'd by the next
    /// `subscribe_events` call.
    notifiers: Arc<Mutex<HashMap<ChannelId, broadcast::Sender<ChannelEvent>>>>,
}

impl ChannelStore {
    /// Construct a store rooted at the given [`ChannelConfig`].
    #[must_use]
    pub fn new(cfg: ChannelConfig) -> Self {
        Self {
            cfg,
            notifiers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// PR-2b: subscribe to live events for `channel`. Returns a
    /// receiver that yields every event appended to the channel
    /// after this call. The sender is lazily created on first
    /// subscribe; subsequent calls return a fresh receiver from the
    /// same sender so all subscribers see the same event stream.
    pub async fn subscribe_events_broadcast(
        &self,
        channel: &ChannelId,
    ) -> broadcast::Receiver<ChannelEvent> {
        let mut guard = self.notifiers.lock().expect("notifier mutex");
        let sender = guard
            .entry(channel.clone())
            .or_insert_with(|| broadcast::channel(256).0);
        sender.subscribe()
    }

    /// PR-2b: fire `event` to every subscriber of `channel`. No-op
    /// when nobody has subscribed. Called by [`Self::append_event`]
    /// after every successful durable append so BOTH live consumers
    /// pick events up in real time: the desktop's `ChannelEventsWatch`
    /// stream and (sprint 3 Phase 10) the `ChannelSubscriber` poll
    /// loop, whose `select!` wakes on this broadcast instead of
    /// waiting out its backstop tick.
    fn notify_event(&self, channel: &ChannelId, event: &ChannelEvent) {
        let guard = self.notifiers.lock().expect("notifier mutex");
        if let Some(sender) = guard.get(channel) {
            // A `SendError` here just means there are no receivers
            // (or every receiver hit the buffer cap). Both are
            // acceptable for a best-effort notification.
            let _ = sender.send(event.clone());
        }
    }

    /// Borrow the underlying config (useful for tests asserting on the
    /// runtime directory).
    #[must_use]
    pub fn config(&self) -> &ChannelConfig {
        &self.cfg
    }

    /// Resolve the on-disk directory for `channel` (runtime-tier).
    /// Used by [`crate::tunnel::TunnelChannelPort::fanout_invite`]
    /// (peko-channel cross-runtime PR-3a commit 3) to read the
    /// freshly-written `meta.json` after a local `invite` so the
    /// outbound `TunnelChannelInvite` envelope carries the same
    /// `creator` + `name` the receiver will bootstrap from.
    ///
    /// Public accessor on top of [`ChannelConfig::channel_dir`] —
    /// the latter is private to the crate because the `Tier` /
    /// `Shared`-vs-`Runtime` resolution lives on `ChannelConfig`,
    /// but the runtime-tier path is the cross-runtime fan-out path
    /// and exposing it via this thin accessor is the smallest API
    /// surface for callers that already hold a [`ChannelStore`].
    #[must_use]
    pub fn channel_dir(&self, channel: &ChannelId) -> PathBuf {
        self.cfg.channel_dir(channel)
    }

    // -----------------------------------------------------------------
    // Tier resolution
    // -----------------------------------------------------------------

    /// Resolve the per-channel directory honoring the channel's tier
    /// (snapshotted in `meta.json` at create-time). Defaults to
    /// [`Tier::Runtime`] when meta.json is unreadable so callers get
    /// a clear `NotFound` instead of silently falling through.
    fn channel_dir_for_tier(&self, tier: Tier, channel: &ChannelId) -> PathBuf {
        self.cfg.channel_dir_for(tier, channel).unwrap_or_else(|_| {
            self.cfg
                .channel_dir(channel)
                .join("..")
                .join(channel.as_str())
        })
    }

    fn events_path_for(&self, tier: Tier, channel: &ChannelId) -> PathBuf {
        self.channel_dir_for_tier(tier, channel).join(EVENTS_FILE)
    }

    /// Read meta.json and return its tier. Falls back to
    /// `Tier::Runtime` if meta.json is missing (lets `peek`/`list_members`
    /// surface `NotFound` cleanly via the directory load).
    async fn resolve_tier(&self, channel: &ChannelId) -> Result<Tier> {
        let chan_dir = self.cfg.channel_dir(channel);
        let meta = MetaJson::load(&chan_dir).await?;
        Ok(meta.tier)
    }

    // -----------------------------------------------------------------
    // Event log helpers
    // -----------------------------------------------------------------

    /// Append one [`ChannelEvent`] as a single JSONL line. Acquires a
    /// per-shard [`FileLock`] + uses [`append_bytes_durable`] for
    /// crash safety. Returns the line number (0-indexed) the event
    /// was assigned.
    ///
    /// This is the SINGLE disk-append chokepoint for the store —
    /// `create`, `post_with_event`, `append_remote_event` (the
    /// cross-runtime mirror path), `invite`, `leave`, and
    /// `join_remote` all funnel through here — so the live-event
    /// broadcast (`notify_event`) fires from this one spot and every
    /// append wakes `subscribe_events` receivers regardless of which
    /// `ChannelPort` face was used. Best-effort: a missed
    /// notification just means a consumer polls for the event; the
    /// on-disk log is the source of truth.
    async fn append_event(
        &self,
        tier: Tier,
        channel: &ChannelId,
        ev: &ChannelEvent,
    ) -> Result<u64> {
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        fs::create_dir_all(&chan_dir).await?;
        let path = self.events_path_for(tier, channel);
        let _lock = FileLock::acquire(&path, CHANNEL_LOCK_TIMEOUT_MS)
            .await
            .map_err(|e| {
                ChannelError::Adapter(format!("lock {}: {e}", path.display()))
            })?;

        // Determine the next line number from the current file length
        // (line count = newline count for properly-terminated JSONL).
        let line_number = match fs::metadata(&path).await {
            Ok(_md) => {
                let bytes = fs::read(&path).await.map_err(|e| {
                    ChannelError::Adapter(format!("read {}: {e}", path.display()))
                })?;
                u64::try_from(bytes.iter().filter(|b| **b == b'\n').count())
                    .map_err(|e| ChannelError::Adapter(format!("line count: {e}")))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => {
                return Err(ChannelError::Adapter(format!(
                    "stat {}: {e}",
                    path.display()
                )));
            }
        };

        let mut bytes = serde_json::to_vec(ev).map_err(|e| {
            ChannelError::Adapter(format!("serialize ChannelEvent: {e}"))
        })?;
        bytes.push(b'\n');
        append_bytes_durable(&path, &bytes).await.map_err(|e| {
            ChannelError::Adapter(format!("append {}: {e}", path.display()))
        })?;
        // Notify live subscribers only after the durable append
        // succeeds (see the method docs — single chokepoint).
        self.notify_event(channel, ev);
        Ok(line_number)
    }

    /// Read `events.jsonl` from offset `since` (count-based) onward.
    /// Empty `Checkpoint` reads from the beginning. Returns the parsed
    /// events plus the line numbers used as [`TaskId`]s.
    async fn read_events(
        &self,
        tier: Tier,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        let path = self.events_path_for(tier, channel);
        let file = match fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(ChannelError::Adapter(format!(
                    "open {}: {e}",
                    path.display()
                )));
            }
        };
        let start: u64 = if since.0.is_empty() {
            0
        } else {
            since.0.parse().map_err(|_| {
                ChannelError::Adapter(format!(
                    "checkpoint {} is not a numeric line offset",
                    since.0
                ))
            })?
        };
        let reader = BufReader::new(file);
        let mut out = Vec::new();
        let mut lines = reader.lines();
        let mut idx: u64 = 0;
        while let Some(line) = lines.next_line().await.map_err(|e| {
            ChannelError::Adapter(format!("read {}: {e}", path.display()))
        })? {
            if idx >= start {
                let ev: ChannelEvent = serde_json::from_str(&line).map_err(|e| {
                    ChannelError::Adapter(format!(
                        "decode event at line {idx}: {e}"
                    ))
                })?;
                out.push((idx.to_string(), ev));
            }
            idx += 1;
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Membership helpers
    // -----------------------------------------------------------------

    /// Check whether `principal` is a current member. Loads + parses
    /// `members.json` directly (no event-log walk — the JSON file is
    /// the authoritative answer).
    async fn check_membership(
        &self,
        tier: Tier,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<bool> {
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let members = MembersJson::load(&chan_dir).await?;
        Ok(members.members.iter().any(|m| m == &principal.to_string()))
    }

    /// Walk `<runtime_dir>/channels/` and return every `ChannelId` for
    /// which `principal` is in `members.json`.
    async fn list_channels_for_principal(
        &self,
        principal: &PrincipalId,
    ) -> Result<Vec<ChannelId>> {
        let root = self.cfg.channels_dir();
        let mut out = Vec::new();
        let mut rd = match fs::read_dir(&root).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => {
                return Err(ChannelError::Adapter(format!(
                    "read {}: {e}",
                    root.display()
                )));
            }
        };
        while let Some(entry) = rd.next_entry().await.map_err(|e| {
            ChannelError::Adapter(format!("walk {}: {e}", root.display()))
        })? {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else { continue };
            if ChannelId::parse(name_str).is_none() {
                continue;
            }
            let chan_dir = entry.path();
            let members = match MembersJson::load(&chan_dir).await {
                Ok(m) => m,
                Err(_) => continue, // skip corrupt / incomplete dirs
            };
            if members.members.iter().any(|m| m == &principal.to_string()) {
                out.push(ChannelId(name_str.to_string()));
            }
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // Remote-mirror helpers (PR-B cross-runtime)
    // -----------------------------------------------------------------

    /// Append a [`ChannelEvent`] that originated from a different
    /// runtime (peko-hub relayed it via
    /// `TunnelMessage::TunnelChannelEvent`). The event is appended to
    /// the local mirror `events.jsonl` **without** the membership /
    /// parent / sender checks that the local `post` path enforces —
    /// those guarantees were already provided by the source runtime,
    /// and the inbound dispatcher verified the signature.
    ///
    /// Returns the line number (0-indexed) the event was assigned.
    /// Line numbers are assigned by the *source* runtime; the
    /// receiver appends at the tail of its own mirror, accepting that
    /// the receiver-side line numbers diverge from the source-side
    /// ones. (PR-1's `peek` is read on the local mirror only — see
    /// the PR-B tests for the invariant.)
    ///
    /// The `source_runtime_id` is currently recorded via the audit
    /// emitter; it is **not** persisted in `events.jsonl` because the
    /// `peko_protocol::channel::ChannelEvent` schema doesn't carry it
    /// (and adding the field would break the wire protocol). Source
    /// tracking is audit-only.
    pub async fn append_remote_event(
        &self,
        channel: &ChannelId,
        ev: &ChannelEvent,
    ) -> Result<TaskId> {
        let tier = self.resolve_tier(channel).await?;
        let line = self.append_event(tier, channel, ev).await?;
        Ok(line.to_string())
    }

    /// List the `RemoteMember` rows currently registered in this
    /// channel's `members.json`. Returns an empty Vec if the channel
    /// has no remote members, or if `members.json` is absent.
    pub async fn list_remote_members(
        &self,
        channel: &ChannelId,
    ) -> Result<Vec<RemoteMember>> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let members = MembersJson::load(&chan_dir).await?;
        Ok(members.remote_members)
    }

    /// P1.2 attribution: read the authoritative `members.json` for
    /// `channel` and return each member paired with their runtime
    /// provenance. Local rows (`members: Vec<String>`) get
    /// `runtime_id = None`; remote rows (`remote_members:
    /// Vec<RemoteMember>`) get `runtime_id = Some(...)`.
    ///
    /// Returns an empty Vec if the channel has no `members.json` yet
    /// (e.g. just-bootstrapped remote mirror) — see
    /// `members_with_attribution_returns_empty_for_unbootstrapped_channel`
    /// for the test that pins this fallback.
    pub async fn members_with_attribution(
        &self,
        channel: &ChannelId,
    ) -> Result<Vec<peko_protocol::channel::MemberProvenance>> {
        use peko_protocol::channel::MemberProvenance;
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let members = match MembersJson::load(&chan_dir).await {
            Ok(m) => m,
            // Channel has no members.json yet (e.g. pre-PR-3a channel
            // or freshly bootstrapped remote mirror). Treat as empty
            // membership rather than an error.
            Err(_) => return Ok(Vec::new()),
        };
        let mut out: Vec<MemberProvenance> = members
            .members
            .into_iter()
            .map(|p| MemberProvenance {
                principal: p,
                runtime_id: None,
            })
            .collect();
        out.extend(members.remote_members.into_iter().map(|rm| MemberProvenance {
            principal: rm.principal_id,
            runtime_id: Some(rm.runtime_id),
        }));
        Ok(out)
    }

    /// Predicate: is the `(runtime_id, principal_id)` pair registered
    /// as a remote member of `channel`? Reads `members.json` once.
    pub async fn is_remote_member(
        &self,
        channel: &ChannelId,
        runtime_id: &str,
        principal_id: &str,
    ) -> Result<bool> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let members = MembersJson::load(&chan_dir).await?;
        Ok(members.remote_members.iter().any(|rm| {
            rm.runtime_id == runtime_id && rm.principal_id == principal_id
        }))
    }

    /// Add a [`RemoteMember`] row to this channel's `members.json`,
    /// idempotent on the `(runtime_id, principal_id)` pair. Used by
    /// the cross-runtime `invite` path (`TunnelChannelPort`) to
    /// record the recipient before fan-out so the receiver's local
    /// mirror has a matching row.
    pub async fn add_remote_member(
        &self,
        channel: &ChannelId,
        runtime_id: &str,
        principal_id: &str,
    ) -> Result<()> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let mut members = MembersJson::load(&chan_dir).await?;
        let row = RemoteMember {
            runtime_id: runtime_id.to_string(),
            principal_id: principal_id.to_string(),
        };
        if !members.remote_members.contains(&row) {
            members.remote_members.push(row);
            members.save(&chan_dir).await?;
        }
        Ok(())
    }

    /// Bootstrap a local mirror of a cross-runtime channel the
    /// receiver was invited to. Called by the dispatcher on inbound
    /// `TunnelChannelInvite` envelopes (peko-channel cross-runtime
    /// PR-3a commit 2) **after** the signature verifies — the
    /// caller is responsible for that gate.
    ///
    /// Creates the channel directory under the runtime tier and
    /// writes:
    ///
    /// - `meta.json` — `{ creator, name, created_at: now, tier: Runtime }`
    /// - `members.json` — partitioned from `initial_members` (the
    ///   creator is always local; rows with `runtime_id: None` are
    ///   local; rows with `runtime_id: Some(id)` become
    ///   `RemoteMember` rows)
    /// - `events.jsonl` — pre-seeded with a synthetic
    ///   [`ChannelEvent::Created`] so the receiver's `peko-stream`
    ///   listener (PR-2b) fires on the desktop the same way it does
    ///   for a local channel create.
    ///
    /// **Idempotent.** If `meta.json` already exists, returns `Ok(())`
    /// without touching disk. This is the contract that makes the
    /// dispatcher safe to retry on a duplicate envelope (the hub
    /// could re-deliver after a partial write, and the source could
    /// re-emit after a network blip).
    ///
    /// Always writes to the **runtime tier** — cross-runtime invites
    /// are a runtime-scoped concern (peko-channel doesn't have
    /// shared-tier cross-runtime fan-out today; PR-3d's Shared tier
    /// is local-only).
    ///
    /// # Errors
    ///
    /// Returns the underlying filesystem / serialization error if
    /// any of the three writes fail. The meta.json-first check
    /// prevents partial writes: if `meta.json` write fails, no
    /// `members.json` or `events.jsonl` is written (the synthetic
    /// event append goes through `append_event`, which also fails
    /// fast on missing meta).
    pub async fn join_remote(
        &self,
        channel: &ChannelId,
        creator: &str,
        name: &str,
        initial_members: &[peko_protocol::channel::InitialMember],
    ) -> Result<()> {
        let chan_dir = self.cfg.channel_dir(channel);
        let meta_path = chan_dir.join(META_FILE);

        // Idempotency check: a bootstrapped channel has a meta.json.
        // If it exists, the previous `join_remote` (or a local
        // `create`) already completed; do nothing.
        if meta_path.exists() {
            return Ok(());
        }

        fs::create_dir_all(&chan_dir).await.map_err(|e| {
            ChannelError::Adapter(format!("mkdir {}: {e}", chan_dir.display()))
        })?;

        // Partition `initial_members` into local + remote. The creator
        // is always local; de-dup so we don't list the same principal
        // twice if the source runtime included them in the snapshot.
        let mut local_members: Vec<String> = vec![creator.to_string()];
        let mut remote_members: Vec<RemoteMember> = Vec::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        seen.insert(creator);
        for m in initial_members {
            if !seen.insert(m.principal_did.as_str()) {
                continue;
            }
            match &m.runtime_id {
                None => local_members.push(m.principal_did.clone()),
                Some(rid) => remote_members.push(RemoteMember {
                    runtime_id: rid.clone(),
                    principal_id: m.principal_did.clone(),
                }),
            }
        }

        let members = MembersJson {
            members: local_members,
            remote_members,
        };
        members.save(&chan_dir).await?;

        // Capture `now` once so meta.json's `created_at` and the
        // synthetic Created event's `at` agree byte-for-byte. Drift
        // here would be cosmetic but still observable: a user
        // comparing the two values would see a 1-nanosecond gap.
        let now = Utc::now();
        let now_rfc3339 = now.to_rfc3339();
        let meta = MetaJson {
            creator: creator.to_string(),
            name: name.to_string(),
            created_at: now,
            tier: Tier::Runtime,
            // Cross-runtime invites carry no binding on the wire
            // (`TunnelChannelInvite` has no such field); mirrors always
            // bootstrap unbound.
            passive_binding: None,
        };
        meta.save(&chan_dir).await?;

        // Append the synthetic Created event. `append_event` uses the
        // durable JSONL append + per-shard FileLock, so a second
        // `join_remote` (after the idempotency check above returned
        // early) would not have appended this — the listener only
        // fires once per channel join.
        let ev = ChannelEvent::Created {
            channel: channel.clone(),
            creator: creator.to_string(),
            name: name.to_string(),
            at: now_rfc3339,
        };
        self.append_event(Tier::Runtime, channel, &ev).await?;

        Ok(())
    }

    /// Like [`ChannelPort::post`] but returns the [`ChannelEvent`]
    /// that was appended so the caller can fan it out to remote
    /// subscribers without re-deriving its fields (which would risk
    /// timestamp / serialization divergence between the local
    /// on-disk copy and the cross-runtime envelope).
    ///
    /// Introduced by peko-channel cross-runtime PR-B commit 3 so the
    /// `TunnelChannelPort` outbound `post` can sign the **same**
    /// bytes it just wrote to `events.jsonl`. The trait method
    /// `ChannelPort::post` delegates here; non-trait call sites
    /// that need the event for outbound fan-out use this directly.
    ///
    /// Membership + parent validation are the same as the trait
    /// `post`.
    pub async fn post_with_event(
        &self,
        channel: &ChannelId,
        sender: &PrincipalId,
        msg: PostMsg,
    ) -> Result<(TaskId, ChannelEvent)> {
        let tier = self.resolve_tier(channel).await?;
        if !self.check_membership(tier, channel, sender).await? {
            return Err(ChannelError::NotMember);
        }

        // Validate parent (if any) references an existing line in the
        // log. The log is append-only, so existing line numbers are
        // stable until truncation (which we don't do).
        if let Some(parent_line) = msg.parent.as_ref() {
            let parent_idx: u64 = parent_line.parse().map_err(|_| {
                ChannelError::Adapter(format!(
                    "invalid parent TaskId {parent_line}: not a numeric line offset"
                ))
            })?;
            let events =
                self.read_events(tier, channel, &Checkpoint::default()).await?;
            if events.is_empty() || parent_idx >= events.len() as u64 {
                return Err(ChannelError::Adapter(format!(
                    "parent line {parent_line} not found in channel log"
                )));
            }
        }

        let ev = ChannelEvent::Posted {
            channel: channel.clone(),
            author: sender.to_string(),
            parent: msg.parent.clone(),
            text: msg.text,
            at: Utc::now().to_rfc3339(),
        };
        let line = self.append_event(tier, channel, &ev).await?;
        Ok((line.to_string(), ev))
    }
}

// ---------------------------------------------------------------------------
// ChannelPort impl
// ---------------------------------------------------------------------------

#[async_trait]
impl ChannelPort for ChannelStore {
    async fn create(
        &self,
        creator: &PrincipalId,
        opts: CreateOpts,
    ) -> Result<ChannelId> {
        let channel = ChannelId::generate();
        let chan_dir = self.cfg.channel_dir_for(opts.tier, &channel)?;
        fs::create_dir_all(&chan_dir).await?;

        // Write the meta + members files BEFORE appending the event so
        // a crash leaves the channel in a state where a subsequent
        // `peek` doesn't NPE on a missing file.
        let now = Utc::now();
        let meta = MetaJson {
            creator: creator.to_string(),
            name: opts.name.clone(),
            created_at: now,
            tier: opts.tier,
            passive_binding: opts.passive_binding.clone(),
        };
        meta.save(&chan_dir).await?;

        let members = MembersJson {
            members: vec![creator.to_string()],
            remote_members: Vec::new(),
        };
        members.save(&chan_dir).await?;

        let event = ChannelEvent::Created {
            channel: channel.clone(),
            creator: creator.to_string(),
            name: opts.name,
            at: now.to_rfc3339(),
        };
        self.append_event(opts.tier, &channel, &event).await?;

        Ok(channel)
    }

    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &PrincipalId,
    ) -> Result<()> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let mut members = MembersJson::load(&chan_dir).await?;
        if !members.members.iter().any(|m| m == &inviter.to_string()) {
            return Err(ChannelError::NotMember);
        }
        if members.members.iter().any(|m| m == &invitee.to_string()) {
            // Idempotent: invitee already a member.
            return Ok(());
        }
        if members.members.len() >= FAN_OUT_CAP {
            return Err(ChannelError::FanOutCap {
                current: members.members.len(),
            });
        }
        members.members.push(invitee.to_string());
        members.save(&chan_dir).await?;

        let ev = ChannelEvent::MemberJoined {
            channel: channel.clone(),
            member: invitee.to_string(),
            at: Utc::now().to_rfc3339(),
        };
        self.append_event(tier, channel, &ev).await?;
        Ok(())
    }

    async fn post(
        &self,
        channel: &ChannelId,
        sender: &PrincipalId,
        msg: PostMsg,
    ) -> Result<TaskId> {
        let (_line, _ev) = self.post_with_event(channel, sender, msg).await?;
        Ok(_line)
    }

    async fn peek(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>> {
        let tier = self.resolve_tier(channel).await?;
        let items = self.read_events(tier, channel, since).await?;
        Ok(items.into_iter().map(|(_, ev)| ev).collect())
    }

    async fn peek_with_ids(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        let tier = self.resolve_tier(channel).await?;
        self.read_events(tier, channel, since).await
    }

    /// Phase 4: read `meta.json`'s `passive_binding`. Probes the
    /// Runtime-tier dir first (the common case), then the Shared dir
    /// when configured — mirroring `resolve_tier`'s fallback so a
    /// pinned-to-shared channel still reports its binding.
    async fn passive_binding(&self, channel: &ChannelId) -> Result<Option<String>> {
        let runtime_dir = self.cfg.channel_dir(channel);
        match MetaJson::load(&runtime_dir).await {
            Ok(meta) => Ok(meta.passive_binding),
            Err(ChannelError::NotFound(_)) => match self.cfg.shared_channels_dir() {
                Some(shared) => {
                    let meta = MetaJson::load(&shared.join(channel.as_str())).await?;
                    Ok(meta.passive_binding)
                }
                None => Err(ChannelError::NotFound(channel.clone())),
            },
            Err(e) => Err(e),
        }
    }

    async fn leave(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<()> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let mut members = MembersJson::load(&chan_dir).await?;
        let before_len = members.members.len();
        members.members.retain(|m| m != &principal.to_string());
        if members.members.len() == before_len {
            // Idempotent leave for non-members.
            return Ok(());
        }
        members.save(&chan_dir).await?;

        let ev = ChannelEvent::MemberLeft {
            channel: channel.clone(),
            member: principal.to_string(),
            at: Utc::now().to_rfc3339(),
        };
        self.append_event(tier, channel, &ev).await?;
        Ok(())
    }

    async fn list_members(
        &self,
        channel: &ChannelId,
    ) -> Result<Vec<PrincipalId>> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        let members = MembersJson::load(&chan_dir).await?;
        Ok(members
            .members
            .into_iter()
            .map(PrincipalId)
            .collect())
    }

    async fn list_for_principal(
        &self,
        principal: &PrincipalId,
    ) -> Result<Vec<ChannelId>> {
        self.list_channels_for_principal(principal).await
    }

    async fn pin_to_shared(
        &self,
        channel: &ChannelId,
    ) -> Result<std::path::PathBuf> {
        // Resolve the Shared destination first so we fail fast if
        // `shared_dir` is unset.
        let shared_chan_dir = self.cfg.channel_dir_for(Tier::Shared, channel)?;
        let runtime_chan_dir = self.cfg.channel_dir(channel);

        // Defense-in-depth: source must exist.
        let _ = MetaJson::load(&runtime_chan_dir).await?;

        if let Some(parent) = shared_chan_dir.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::create_dir_all(&shared_chan_dir).await?;

        // Copy the three Runtime-tier files: meta.json, members.json,
        // events.jsonl. None are recursive; the layout is flat.
        for filename in [META_FILE, MEMBERS_FILE, EVENTS_FILE] {
            let src = runtime_chan_dir.join(filename);
            if !src.exists() {
                continue;
            }
            let dst = shared_chan_dir.join(filename);
            fs::copy(&src, &dst).await.map_err(|e| {
                ChannelError::Adapter(format!(
                    "copy {} -> {}: {e}",
                    src.display(),
                    dst.display()
                ))
            })?;
        }

        Ok(shared_chan_dir)
    }

    async fn subscribe_events(
        &self,
        channel: &ChannelId,
    ) -> broadcast::Receiver<ChannelEvent> {
        self.subscribe_events_broadcast(channel).await
    }
}

// Silence the unused `ChannelMembership` import — the trait's default
// `membership` impl walks `peek`, so the wire type is only referenced
// in the trait surface.
#[allow(dead_code)]
fn _ensure_wire_imported(_: ChannelMembership) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peko_protocol::channel::ChannelEvent;

    fn tmp_cfg(label: &str) -> ChannelConfig {
        let dir = std::env::temp_dir().join(format!("peko-channel-tests-{label}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        ChannelConfig {
            runtime_dir: dir,
            shared_dir: None,
        }
    }

    /// Helper: a `PrincipalId` is a newtype around `String`; using a
    /// test-prefix keeps the test outputs readable.
    fn pid(s: &str) -> PrincipalId {
        PrincipalId(s.to_string())
    }

    fn fake_remote(runtime: &str, principal: &str) -> RemoteMember {
        RemoteMember {
            runtime_id: runtime.to_string(),
            principal_id: principal.to_string(),
        }
    }

    // -- back-compat -----------------------------------------------------

    /// A pre-PR-B `members.json` (only `members: Vec<String>`,
    /// `remote_members` absent) deserializes to an empty
    /// `remote_members` Vec. This is the migration-free on-disk
    /// invariant that lets existing channel directories keep
    /// working after PR-B lands.
    #[test]
    fn members_json_deserializes_legacy_files_without_migration() {
        let legacy = r#"{"members":["prin_alice","prin_bob"]}"#;
        let parsed: MembersJson = serde_json::from_str(legacy).expect("legacy must parse");
        assert_eq!(parsed.members, vec!["prin_alice", "prin_bob"]);
        assert!(
            parsed.remote_members.is_empty(),
            "missing field must default to empty vec; got {:?}",
            parsed.remote_members
        );
    }

    /// New writes always include `remote_members` (possibly empty),
    /// so the on-disk shape is consistent going forward.
    #[test]
    fn members_json_round_trips_with_remote_members() {
        let m = MembersJson {
            members: vec!["prin_alice".into()],
            remote_members: vec![fake_remote("did:key:zRuntimeB", "prin_bob")],
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let parsed: MembersJson = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, m);
    }

    // -- passive_binding (Phase 4, agent-session paradigm sprint) ------

    /// `CreateOpts::passive_binding` persists into `meta.json` and is
    /// readable back through the `ChannelPort::passive_binding`
    /// accessor. This is the DM-tier binding the daemon's
    /// `PassiveBindingResponder` reads at subscriber-spawn time.
    #[tokio::test]
    async fn passive_binding_persists_through_create() {
        let cfg = tmp_cfg("passive-binding");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let opts = CreateOpts::runtime("dm-user-a").with_passive_binding("/user-a");
        let channel = store.create(&creator, opts).await.unwrap();

        assert_eq!(
            store.passive_binding(&channel).await.unwrap(),
            Some("/user-a".to_string())
        );
    }

    /// A channel created WITHOUT a binding reports `None` and its
    /// `meta.json` omits the field entirely (`skip_serializing_if`),
    /// so unbound channels keep the pre-Phase-4 file shape
    /// byte-for-byte.
    #[tokio::test]
    async fn no_binding_leaves_meta_unchanged() {
        let cfg = tmp_cfg("no-binding");
        let store = ChannelStore::new(cfg.clone());
        let creator = pid("prin_alice");
        let channel = store
            .create(&creator, CreateOpts::runtime("group"))
            .await
            .unwrap();

        assert_eq!(store.passive_binding(&channel).await.unwrap(), None);
        let raw = std::fs::read_to_string(cfg.channel_dir(&channel).join("meta.json")).unwrap();
        assert!(
            !raw.contains("passive_binding"),
            "unbound meta.json must not carry the field; got {raw}"
        );
    }

    /// A pre-Phase-4 `meta.json` (no `passive_binding` key) loads as
    /// `None` — the `#[serde(default)]` back-compat invariant.
    #[test]
    fn meta_json_deserializes_legacy_files_without_binding() {
        let legacy = r#"{"creator":"prin_alice","name":"team","created_at":"2026-08-05T12:00:00Z","tier":"Runtime"}"#;
        let parsed: MetaJson = serde_json::from_str(legacy).expect("legacy must parse");
        assert_eq!(parsed.passive_binding, None);
    }

    /// Full `MetaJson` round-trip with a binding set.
    #[test]
    fn meta_json_round_trips_with_binding() {
        let m = MetaJson {
            creator: "prin_alice".into(),
            name: "dm".into(),
            created_at: Utc::now(),
            tier: Tier::Runtime,
            passive_binding: Some("550e8400-e29b-41d4-a716-446655440000".into()),
        };
        let bytes = serde_json::to_vec(&m).unwrap();
        let parsed: MetaJson = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            parsed.passive_binding.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    // -- list_remote_members / is_remote_member -------------------------

    /// `list_remote_members` returns the rows persisted in
    /// `members.json`. The integration with the on-disk layout is
    /// what matters here, not the in-memory struct.
    #[tokio::test]
    async fn list_remote_members_round_trips_via_disk() {
        let cfg = tmp_cfg("list-remote");
        let store = ChannelStore::new(cfg.clone());
        let creator = pid("prin_alice");
        let channel = store.create(&creator, CreateOpts::runtime("team")).await.unwrap();

        // No remote members yet.
        assert!(store.list_remote_members(&channel).await.unwrap().is_empty());

        // Manually inject a remote member row (the API that adds
        // remote members lives in commit 2 / 3 of PR-B; here we
        // verify the read path).
        let chan_dir = cfg.channel_dir(&channel);
        let mut members = MembersJson::load(&chan_dir).await.unwrap();
        members.remote_members.push(fake_remote(
            "did:key:zRuntimeB",
            "prin_bob",
        ));
        members.save(&chan_dir).await.unwrap();

        let listed = store.list_remote_members(&channel).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], fake_remote("did:key:zRuntimeB", "prin_bob"));
    }

    /// `is_remote_member` returns true for a registered
    /// (runtime_id, principal_id) pair and false otherwise.
    #[tokio::test]
    async fn is_remote_member_matches_by_runtime_and_principal() {
        let cfg = tmp_cfg("is-remote");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store.create(&creator, CreateOpts::runtime("team")).await.unwrap();

        let chan_dir = store.config().channel_dir(&channel);
        let mut members = MembersJson::load(&chan_dir).await.unwrap();
        members.remote_members.push(fake_remote(
            "did:key:zRuntimeB",
            "prin_bob",
        ));
        members.save(&chan_dir).await.unwrap();

        assert!(store
            .is_remote_member(&channel, "did:key:zRuntimeB", "prin_bob")
            .await
            .unwrap());
        // Wrong runtime id for the same principal id.
        assert!(!store
            .is_remote_member(&channel, "did:key:zRuntimeC", "prin_bob")
            .await
            .unwrap());
        // Wrong principal id for the same runtime id.
        assert!(!store
            .is_remote_member(&channel, "did:key:zRuntimeB", "prin_carol")
            .await
            .unwrap());
    }

    // -- members_with_attribution ---------------------------------------

    /// P1.2 attribution: `members_with_attribution` returns local rows
    /// with `runtime_id = None` and remote rows with their
    /// `runtime_id`. Local-first ordering matches what `MemberList.tsx`
    /// expects so it can section the list visually without resorting.
    #[tokio::test]
    async fn members_with_attribution_partitions_local_and_remote() {
        use peko_protocol::channel::MemberProvenance;

        let cfg = tmp_cfg("attribution");
        let store = ChannelStore::new(cfg.clone());
        let creator = pid("prin_alice");
        let channel = store
            .create(&creator, CreateOpts::runtime("team"))
            .await
            .unwrap();

        // Inject a remote member row alongside the local creator.
        let chan_dir = cfg.channel_dir(&channel);
        let mut members = MembersJson::load(&chan_dir).await.unwrap();
        members
            .remote_members
            .push(fake_remote("did:key:zRuntimeB", "prin_bob"));
        members.save(&chan_dir).await.unwrap();

        let attributed = store.members_with_attribution(&channel).await.unwrap();

        // Local creator comes first with no runtime_id.
        assert_eq!(
            attributed[0],
            MemberProvenance {
                principal: "prin_alice".into(),
                runtime_id: None,
            }
        );
        // Remote member follows with the persisted runtime_id.
        assert_eq!(
            attributed[1],
            MemberProvenance {
                principal: "prin_bob".into(),
                runtime_id: Some("did:key:zRuntimeB".into()),
            }
        );
    }

    /// `members_with_attribution` returns an empty Vec for a channel
    /// with no `members.json` (e.g. just-bootstrapped remote mirror).
    /// Matches the `Err(_) -> Ok(vec![])` fallback so callers don't
    /// have to special-case the bootstrap window.
    #[tokio::test]
    async fn members_with_attribution_returns_empty_for_unbootstrapped_channel() {
        let cfg = tmp_cfg("attribution-empty");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store
            .create(&creator, CreateOpts::runtime("team"))
            .await
            .unwrap();

        // Sanity: members.json exists post-create, so explicitly nuke
        // it to simulate a pre-bootstrap state.
        let chan_dir = store.config().channel_dir(&channel);
        let members_path = chan_dir.join("members.json");
        if members_path.exists() {
            std::fs::remove_file(&members_path).unwrap();
        }

        let attributed = store.members_with_attribution(&channel).await.unwrap();
        assert!(attributed.is_empty(), "got {attributed:?}");
    }

    // -- append_remote_event --------------------------------------------

    /// `append_remote_event` writes the event into the local mirror's
    /// `events.jsonl` without invoking the membership / parent /
    /// sender checks that the local `post` path enforces. This is
    /// the test for the "remote events land in the mirror" invariant
    /// that PR-B's cross-runtime fan-out relies on.
    #[tokio::test]
    async fn append_remote_event_writes_into_local_mirror() {
        let cfg = tmp_cfg("append-remote");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store.create(&creator, CreateOpts::runtime("team")).await.unwrap();

        // Simulate an event relayed from runtime B for a remote principal.
        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".to_string(),
            parent: None,
            text: "hello from B".to_string(),
            at: "2026-08-06T00:00:00Z".to_string(),
        };
        let line = store
            .append_remote_event(&channel, &remote_event)
            .await
            .expect("append_remote_event must succeed on a created channel");

        // `peek` reads from the same `events.jsonl`, so the remote
        // event should be visible immediately.
        let events = store
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap();
        let posted: Vec<_> = events
            .iter()
            .filter(|ev| matches!(ev, ChannelEvent::Posted { .. }))
            .collect();
        assert_eq!(posted.len(), 1, "remote event must show up in peek");
        assert_eq!(
            line, "1",
            "Created is line 0, so the first remote event lands at line 1"
        );
    }

    /// `append_remote_event` does NOT enforce sender membership. A
    /// sender that isn't in the local `members.json` is accepted
    /// (the membership check was already done by the source
    /// runtime; we trust the verified event here). This is the
    /// property that lets a remote principal post to a channel
    /// without ever being a row in the local mirror's member set.
    #[tokio::test]
    async fn append_remote_event_skips_local_membership_check() {
        let cfg = tmp_cfg("skip-membership");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store.create(&creator, CreateOpts::runtime("team")).await.unwrap();

        // "prin_bob@runtime-B" is NOT in `members.json`.
        assert!(!store
            .is_remote_member(&channel, "did:key:zRuntimeB", "prin_bob@runtime-B")
            .await
            .unwrap());

        // `post` from a non-member would error — assert that first so
        // the contrast with append_remote_event is clear.
        let post_err = store
            .post(
                &channel,
                &pid("prin_bob@runtime-B"),
                PostMsg::root("hello"),
            )
            .await;
        assert!(
            matches!(post_err, Err(ChannelError::NotMember)),
            "local post must reject non-members; got: {post_err:?}"
        );

        // `append_remote_event` for the same principal succeeds.
        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".to_string(),
            parent: None,
            text: "hello from B".to_string(),
            at: "2026-08-06T00:00:00Z".to_string(),
        };
        store
            .append_remote_event(&channel, &remote_event)
            .await
            .expect("append_remote_event must accept a non-local-member sender");
    }

    /// `append_remote_event` on a non-existent channel surfaces
    /// `NotFound` rather than silently creating an empty channel dir.
    /// The dispatcher's signature-verify step happens first, so a
    /// bogus channel id never reaches `append_remote_event` in
    /// practice — but defense-in-depth: a bad id should still fail
    /// cleanly.
    #[tokio::test]
    async fn append_remote_event_on_unknown_channel_errors_not_found() {
        let cfg = tmp_cfg("not-found");
        let store = ChannelStore::new(cfg);
        let bogus = ChannelId::generate();
        let ev = ChannelEvent::Posted {
            channel: bogus.clone(),
            author: "prin_bob".to_string(),
            parent: None,
            text: "should not land".to_string(),
            at: "2026-08-06T00:00:00Z".to_string(),
        };
        let result = store.append_remote_event(&bogus, &ev).await;
        assert!(
            matches!(result, Err(ChannelError::NotFound(_))),
            "unknown channel must surface NotFound; got: {result:?}"
        );
    }

    // -- live-event wake (sprint 3 Phase 10 push-wake) -----------------

    /// Every append path fires the per-channel broadcast from the
    /// single `append_event` chokepoint. A `subscribe_events` receiver
    /// must observe a local `post` promptly — well under the
    /// subscriber's backstop tick (30s default; the pre-Phase-10 loop
    /// polled every 5s).
    #[tokio::test]
    async fn post_notifies_live_subscribers_promptly() {
        let cfg = tmp_cfg("wake-post");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store
            .create(&creator, CreateOpts::runtime("team"))
            .await
            .unwrap();

        let mut rx = store.subscribe_events(&channel).await;
        store
            .post(&channel, &creator, PostMsg::root("wake up"))
            .await
            .unwrap();

        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("post must wake a live subscriber well under the backstop tick")
            .expect("broadcast must be open");
        assert!(
            matches!(observed, ChannelEvent::Posted { ref text, .. } if text == "wake up"),
            "expected the Posted event, got {observed:?}"
        );
    }

    /// The cross-runtime mirror append (`append_remote_event`) wakes
    /// subscribers through the same chokepoint — the DM-tier responder
    /// depends on this to react to relayed remote posts in real time.
    #[tokio::test]
    async fn remote_mirror_append_notifies_live_subscribers() {
        let cfg = tmp_cfg("wake-remote");
        let store = ChannelStore::new(cfg);
        let creator = pid("prin_alice");
        let channel = store
            .create(&creator, CreateOpts::runtime("team"))
            .await
            .unwrap();

        let mut rx = store.subscribe_events(&channel).await;
        let remote_event = ChannelEvent::Posted {
            channel: channel.clone(),
            author: "prin_bob@runtime-B".to_string(),
            parent: None,
            text: "hello from B".to_string(),
            at: "2026-08-18T00:00:00Z".to_string(),
        };
        store
            .append_remote_event(&channel, &remote_event)
            .await
            .unwrap();

        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("mirror append must wake a live subscriber")
            .expect("broadcast must be open");
        assert!(
            matches!(observed, ChannelEvent::Posted { ref text, .. } if text == "hello from B"),
            "expected the mirrored Posted event, got {observed:?}"
        );
    }

    // -- join_remote (PR-3a commit 2) -----------------------------------

    /// `join_remote` on an unknown channel id creates the directory,
    /// writes `meta.json` + `members.json`, and seeds `events.jsonl`
    /// with a synthetic `ChannelEvent::Created`. Pins the contract
    /// the dispatcher relies on for the inbound `TunnelChannelInvite`
    /// bootstrap path: after a successful `join_remote`, `peek` and
    /// `list_members` work as if the channel had been `create`-d
    /// locally.
    #[tokio::test]
    async fn join_remote_creates_files_for_new_channel() {
        use peko_protocol::channel::InitialMember;
        let cfg = tmp_cfg("join-remote-new");
        let store = ChannelStore::new(cfg.clone());
        let channel = ChannelId::generate();
        let initial_members = vec![
            // Creator is local to the source runtime.
            InitialMember {
                principal_did: "prin_alice".to_string(),
                runtime_id: None,
            },
            // Bob lives on runtime B — must land in remote_members.
            InitialMember {
                principal_did: "prin_bob".to_string(),
                runtime_id: Some("did:key:zRuntimeB".to_string()),
            },
        ];
        store
            .join_remote(&channel, "prin_alice", "team-chat", &initial_members)
            .await
            .expect("join_remote must succeed on a new channel");

        // Filesystem layout: meta.json, members.json, events.jsonl.
        let chan_dir = cfg.channel_dir(&channel);
        assert!(chan_dir.join(META_FILE).exists(), "meta.json must exist");
        assert!(chan_dir.join(MEMBERS_FILE).exists(), "members.json must exist");
        assert!(
            chan_dir.join(EVENTS_FILE).exists(),
            "events.jsonl must exist"
        );

        // meta.json: creator + name + tier = Runtime.
        let meta = MetaJson::load(&chan_dir).await.unwrap();
        assert_eq!(meta.creator, "prin_alice");
        assert_eq!(meta.name, "team-chat");
        assert_eq!(meta.tier, Tier::Runtime);

        // members.json: alice local (creator), bob remote on B.
        let members = MembersJson::load(&chan_dir).await.unwrap();
        assert_eq!(members.members, vec!["prin_alice".to_string()]);
        assert_eq!(members.remote_members.len(), 1);
        assert_eq!(members.remote_members[0].runtime_id, "did:key:zRuntimeB");
        assert_eq!(members.remote_members[0].principal_id, "prin_bob");

        // events.jsonl: synthetic Created at line 0 — exactly the
        // shape PR-2b's peko-stream listener expects.
        let events = store.peek(&channel, &Checkpoint::default()).await.unwrap();
        assert_eq!(events.len(), 1, "synthetic Created event lands at line 0");
        match &events[0] {
            ChannelEvent::Created { creator, name, .. } => {
                assert_eq!(creator, "prin_alice");
                assert_eq!(name, "team-chat");
            }
            other => panic!("expected Created, got {other:?}"),
        }
    }

    /// `join_remote` on a channel that already exists is a no-op:
    /// the `meta.json`-existence check short-circuits before any
    /// write. This is the dispatcher-retry contract — a duplicate
    /// envelope must not corrupt the existing mirror or append a
    /// second `Created` event.
    #[tokio::test]
    async fn join_remote_is_idempotent_when_channel_exists() {
        use peko_protocol::channel::InitialMember;
        let cfg = tmp_cfg("join-remote-idem");
        let store = ChannelStore::new(cfg.clone());
        let channel = ChannelId::generate();
        let initial_members = vec![InitialMember {
            principal_did: "prin_alice".to_string(),
            runtime_id: None,
        }];
        // First call creates the mirror.
        store
            .join_remote(&channel, "prin_alice", "team-chat", &initial_members)
            .await
            .expect("first join_remote must succeed");
        let first_meta_mtime = std::fs::metadata(cfg.channel_dir(&channel).join(META_FILE))
            .unwrap()
            .modified()
            .unwrap();
        let first_events_len = store
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap()
            .len();
        assert_eq!(first_events_len, 1, "Created at line 0 after first join");

        // Sleep one millisecond so mtime can differ (some
        // filesystems have second-level mtime resolution; the test
        // is portable because we just need any elapsed time).
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Second call with a different name must NOT clobber the
        // first meta.json or append a second Created event.
        store
            .join_remote(&channel, "prin_alice", "different-name", &initial_members)
            .await
            .expect("second join_remote must succeed (no-op)");
        let second_meta_mtime = std::fs::metadata(cfg.channel_dir(&channel).join(META_FILE))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            first_meta_mtime, second_meta_mtime,
            "meta.json must not be rewritten on idempotent join"
        );
        let second_events_len = store
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap()
            .len();
        assert_eq!(
            second_events_len, 1,
            "no synthetic Created event on idempotent join"
        );

        // The original name survives (the second call did not
        // overwrite meta.json).
        let meta = MetaJson::load(&cfg.channel_dir(&channel)).await.unwrap();
        assert_eq!(meta.name, "team-chat", "original name must survive");
    }
}