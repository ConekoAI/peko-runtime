//! `ChannelStore` — concrete [`ChannelPort`] impl backed by an
//! append-only JSONL event log + a member-set JSON file.
//!
//! ## On-disk layout
//!
//! ```text
//! <runtime_dir>/
//!   channels/
//!     <chan_id>/
//!       meta.json       # { creator, name, created_at }
//!       members.json    # { members: [String] }
//!       events.jsonl    # one ChannelEvent per line, append-only
//!       config.toml     # ConfigOnDisk
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

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use peko_fs_persistence::{append_bytes_durable, FileLock};
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use peko_subject::PrincipalId;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::config::ConfigOnDisk;
use crate::port::{
    ChannelError, ChannelPort, Checkpoint, CreateOpts, PostMsg, Result, TaskId, Tier,
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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MembersJson {
    pub members: Vec<String>,
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
}

impl ChannelStore {
    /// Construct a store rooted at the given [`ChannelConfig`].
    #[must_use]
    pub fn new(cfg: ChannelConfig) -> Self {
        Self { cfg }
    }

    /// Borrow the underlying config (useful for tests asserting on the
    /// runtime directory).
    #[must_use]
    pub fn config(&self) -> &ChannelConfig {
        &self.cfg
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
        };
        meta.save(&chan_dir).await?;

        let members = MembersJson {
            members: vec![creator.to_string()],
        };
        members.save(&chan_dir).await?;

        let event = ChannelEvent::Created {
            channel: channel.clone(),
            creator: creator.to_string(),
            name: opts.name,
            at: now.to_rfc3339(),
        };
        self.append_event(opts.tier, &channel, &event).await?;

        // Seed config.toml with defaults so the file exists for
        // `load_config` callers.
        ConfigOnDisk::default().save(&chan_dir).await?;

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
        Ok(line.to_string())
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

    async fn load_config(&self, channel: &ChannelId) -> Result<ConfigOnDisk> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        ConfigOnDisk::load(&chan_dir).await
    }

    async fn save_config(
        &self,
        channel: &ChannelId,
        config: &ConfigOnDisk,
    ) -> Result<()> {
        let tier = self.resolve_tier(channel).await?;
        let chan_dir = self.channel_dir_for_tier(tier, channel);
        config.save(&chan_dir).await
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

        // Copy the four Runtime-tier files: meta.json, members.json,
        // config.toml, events.jsonl. None are recursive; the layout
        // is flat.
        for filename in [
            META_FILE,
            MEMBERS_FILE,
            ConfigOnDisk::path_in(&runtime_chan_dir)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("config.toml"),
            EVENTS_FILE,
        ] {
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
}

// Silence the unused `ChannelMembership` import — the trait's default
// `membership` impl walks `peek`, so the wire type is only referenced
// in the trait surface.
#[allow(dead_code)]
fn _ensure_wire_imported(_: ChannelMembership) {}