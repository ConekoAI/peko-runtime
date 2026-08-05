//! `PlanChannelAdapter` — concrete [`ChannelPort`] impl backed by
//! `peko_plan::PlanStorage`.
//!
//! ## On-disk layout (Runtime tier — PR-1)
//!
//! ```text
//! <runtime_dir>/
//!   channels/
//!     <chan_id>/
//!       meta.json                  # { plan_id, creator, name, created_at, members }
//!       plan_<plan_id>.jsonl       # PlanRecord — event log lives in `nodes`
//!       cursors.json               # per-member last-observed TaskId
//! ```
//!
//! One channel = one `PlanRecord`; messages = nodes in `record.nodes`.
//! Conventions enforced at the adapter (NOT in `PlanNode` — we don't
//! extend plan-core types per `prefer-concrete-over-speculative-abstraction.md`):
//!
//! - Each `ChannelEvent` is stored as a `PlanNode` whose `step` field
//!   carries the JSON-serialized event payload. The adapter parses
//!   `step` JSON on `peek` and assembles a `Vec<ChannelEvent>` in causal
//!   (insertion) order. `node_id` becomes the wire-side `TaskId`.
//! - The at-most-one parent convention: for `Posted` events, the
//!   adapter enforces `depends_on.len() <= 1` (root posts have empty
//!   `depends_on`; replies carry exactly one parent id).
//! - The channel's owner — for `PlanStorage` purposes — is the channel
//!   creator. All `add_node` calls pass the creator's `principal_id`
//!   so the storage layer's `get_for_principal` check passes.
//!
//! ## PR-1 fan-out cap
//!
//! Hard cap of 8 members per channel. Enforced by `meta.json::members`
//! counting; the variant `ChannelError::FanOutCap { current }` carries
//! the count for callers that want to surface "8 already" UX.
//!
//! ## Why not extend `PlanPort` with a `list_since` method
//!
//! Per anti-pattern #7 in `lexical-soaring-pretzel.md`: client-side
//! filter on `get` is fine for PR-1's fan-out cap of 8; we add the
//! method in PR-3 if perf pressure shows up.
//!
//! ## Shared-tier path
//!
//! PR-3 will introduce `Tier::Shared` support. The adapter will
//! resolve the channel dir through the Phase B `RuntimeAuthority`
//! seal; the on-disk shape will mirror Runtime with a `.shared/`
//! directory and `RuntimeAuthority`-sealed access. No code in PR-1
//! touches that path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use peko_plan::schema::{NodeId, PlanNode, PlanNodeStatus, PlanRecord};
use peko_plan::{PlanError, PlanPort, PlanStorage};
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use serde::{Deserialize, Serialize};
use tokio::fs;

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

/// Per-channel metadata file (owner, members, plan_id binding).
const META_FILE: &str = "meta.json";

// ---------------------------------------------------------------------------
// ChannelConfig
// ---------------------------------------------------------------------------

/// Construction-time config for [`PlanChannelAdapter::new`]. Cheap to
/// clone (just two paths).
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// Runtime-tier root. PR-1 only; PR-3 adds a Shared-root field
    /// alongside (reuses Phase B RuntimeAuthority seal).
    pub runtime_dir: PathBuf,
}

impl ChannelConfig {
    /// `<runtime_dir>/channels` — where every channel directory lives.
    fn channels_dir(&self) -> PathBuf {
        self.runtime_dir.join(CHANNELS_DIR)
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
/// The `members` field is the authoritative "who is currently a member"
/// set; `peek`'s `MemberJoined - MemberLeft` walk is for replay only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaJson {
    pub plan_id: String,
    pub creator: String,
    pub name: String,
    pub created_at: chrono::DateTime<Utc>,
    pub members: Vec<String>,
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ChannelError::NotFound(ChannelId(
                    channel_dir
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string(),
                )))
            }
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
// PlanChannelAdapter
// ---------------------------------------------------------------------------

/// File-backed [`ChannelPort`] implementation. `Send + Sync` via the
/// inner `Arc<PlanStorage>` (one storage per operation; the storage
/// itself is `Clone`).
#[derive(Debug, Clone)]
pub struct PlanChannelAdapter {
    cfg: ChannelConfig,
}

impl PlanChannelAdapter {
    /// Construct an adapter rooted at `runtime_dir`.
    #[must_use]
    pub fn new(cfg: ChannelConfig) -> Self {
        Self { cfg }
    }

    /// Returns the underlying config (useful for tests asserting on the
    /// runtime directory).
    pub fn config(&self) -> &ChannelConfig {
        &self.cfg
    }

    // -----------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------

    /// Build a `PlanStorage` scoped to one channel directory. The
    /// `plans_dir` is `<chan_dir>/` so `plan_<id>.jsonl` files live
    /// alongside `meta.json` and `cursors.json`.
    fn storage_for(&self, channel_dir: &Path) -> PlanStorage {
        PlanStorage::new(channel_dir.to_path_buf())
    }

    /// Read the channel's `PlanRecord` (authoritative event log).
    async fn read_plan(&self, channel: &ChannelId) -> Result<PlanRecord> {
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        let storage = self.storage_for(&self.cfg.channel_dir(channel));
        match storage.get(&meta.plan_id).await? {
            Some(r) => Ok(r),
            None => Err(ChannelError::Adapter(format!(
                "plan {} for channel {} not found",
                meta.plan_id,
                channel.as_str()
            ))),
        }
    }

    /// Append one `PlanNode` to the channel's plan as the creator.
    async fn append_node(
        &self,
        channel: &ChannelId,
        creator: &peko_plan::PrincipalId,
        node: PlanNode,
    ) -> Result<()> {
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        let storage = self.storage_for(&self.cfg.channel_dir(channel));
        storage
            .add_node(&meta.plan_id, creator, node)
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    /// Convert a `PostMsg` into the canonical `ChannelEvent::Posted`
    /// JSON payload. The wire form carries the channel + author + parent
    /// (string) + text + at, mirroring `peko_protocol::channel::ChannelEvent::Posted`.
    fn posted_event(
        &self,
        channel: &ChannelId,
        author: &peko_plan::PrincipalId,
        msg: &PostMsg,
        parent_node_id: Option<&NodeId>,
        at: chrono::DateTime<Utc>,
    ) -> Result<ChannelEvent> {
        let parent = match (msg.parent.as_ref(), parent_node_id) {
            (Some(p), Some(id)) => {
                if p != id.as_str() {
                    return Err(ChannelError::Adapter(format!(
                        "parent TaskId mismatch: caller-supplied {p} != NodeId {}",
                        id.as_str()
                    )));
                }
                Some(id.as_str().to_string())
            }
            (None, None) => None,
            (Some(p), None) => {
                return Err(ChannelError::Adapter(format!(
                    "caller supplied parent {p} but parent_node_id was None"
                )));
            }
            (None, Some(id)) => {
                return Err(ChannelError::Adapter(format!(
                    "no parent in PostMsg but plan adapter assigned parent NodeId {}",
                    id.as_str()
                )));
            }
        };
        Ok(ChannelEvent::Posted {
            channel: channel.clone(),
            author: author.to_string(),
            parent,
            text: msg.text.clone(),
            at: at.to_rfc3339(),
        })
    }

    /// Wrap a `ChannelEvent` into a `PlanNode`. The event's JSON lives
    /// in `step`; `node_id` is provided by the caller (either fresh for
    /// a new event, or matched from the existing record for replay).
    /// `depends_on` carries the parent node id when present (root
    /// posts get `Vec::new()`).
    fn wrap_event_node(
        &self,
        node_id: NodeId,
        ev: &ChannelEvent,
        depends_on: Vec<NodeId>,
    ) -> Result<PlanNode> {
        let step = serde_json::to_string(ev).map_err(|e| {
            ChannelError::Adapter(format!("serialize ChannelEvent: {e}"))
        })?;
        Ok(PlanNode {
            node_id,
            step,
            status: PlanNodeStatus::Completed {
                completed_at: Utc::now(),
            },
            depends_on,
            evidence: None,
            blocked_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
    }

    /// Parse a `step` JSON string back into a `ChannelEvent`. Used by
    /// `peek`.
    fn parse_event_node(node: &PlanNode) -> Result<ChannelEvent> {
        serde_json::from_str(&node.step).map_err(|e| {
            ChannelError::Adapter(format!(
                "decode ChannelEvent from node {}: {e}",
                node.node_id.as_str()
            ))
        })
    }

    /// Filter events strictly after `since` (count-based cursor).
    /// See [`Self::peek_with_ids`] for the rationale.
    fn filter_since(
        &self,
        record: &PlanRecord,
        since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>> {
        let start: usize = if since.0.is_empty() {
            0
        } else {
            since.0.parse().map_err(|_| {
                ChannelError::Adapter(format!(
                    "checkpoint {} is not a numeric count",
                    since.0
                ))
            })?
        };
        let mut out = Vec::with_capacity(record.nodes.len().saturating_sub(start));
        for (idx, n) in record.nodes.iter().enumerate() {
            if idx < start {
                continue;
            }
            let ev = Self::parse_event_node(n)?;
            out.push(ev);
        }
        Ok(out)
    }

    /// Walk `<runtime_dir>/channels/` and return every `ChannelId` for
    /// which `principal` is in `meta.json::members`. Cheap with the
    /// PR-1 fan-out cap; PR-3 may add an index file if needed.
    async fn list_channels_for_principal(
        &self,
        principal: &peko_plan::PrincipalId,
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
            let meta = match MetaJson::load(&chan_dir).await {
                Ok(m) => m,
                Err(_) => continue, // skip corrupt / incomplete dirs
            };
            if meta.members.iter().any(|m| m == &principal.to_string()) {
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
impl ChannelPort for PlanChannelAdapter {
    async fn create(
        &self,
        creator: &peko_plan::PrincipalId,
        opts: CreateOpts,
    ) -> Result<ChannelId> {
        if !matches!(opts.tier, Tier::Runtime) {
            return Err(ChannelError::Adapter(
                "PR-1 only supports Tier::Runtime; Shared opt-in lands in PR-3".into(),
            ));
        }
        let channel = ChannelId::generate();
        let chan_dir = self.cfg.channel_dir(&channel);
        fs::create_dir_all(&chan_dir).await?;

        let now = Utc::now();
        let event = ChannelEvent::Created {
            channel: channel.clone(),
            creator: creator.to_string(),
            name: opts.name.clone(),
            at: now.to_rfc3339(),
        };
        let node = self.wrap_event_node(NodeId::generate(), &event, Vec::new())?;

        // Build a one-node plan owned by the creator. peko-plan generates
        // the plan_id; we capture it from the returned record so meta.json
        // points at the right file.
        let storage = self.storage_for(&chan_dir);
        let record = storage
            .create(
                creator.clone(),
                format!("channel:{}", channel.as_str()),
                vec![node],
            )
            .await?;
        let plan_id = record.plan_id.clone();

        let meta = MetaJson {
            plan_id,
            creator: creator.to_string(),
            name: opts.name,
            created_at: now,
            members: vec![creator.to_string()],
        };
        meta.save(&chan_dir).await?;

        // PR-2: seed config.toml with defaults so the file exists for
        // the responder to read. PR-3 may add a `pin` op that re-writes
        // config to a non-default state for Shared-tier channels.
        ConfigOnDisk::default().save(&chan_dir).await?;

        Ok(channel)
    }

    async fn invite(
        &self,
        channel: &ChannelId,
        inviter: &peko_plan::PrincipalId,
        invitee: &peko_plan::PrincipalId,
    ) -> Result<()> {
        let chan_dir = self.cfg.channel_dir(channel);
        let mut meta = MetaJson::load(&chan_dir).await?;
        if !meta.members.iter().any(|m| m == &inviter.to_string()) {
            return Err(ChannelError::NotMember);
        }
        if meta.members.iter().any(|m| m == &invitee.to_string()) {
            // Idempotent: invitee already a member. PR-1 silent; PR-3
            // may surface this to the responder.
            return Ok(());
        }
        if meta.members.len() >= FAN_OUT_CAP {
            return Err(ChannelError::FanOutCap {
                current: meta.members.len(),
            });
        }
        meta.members.push(invitee.to_string());
        meta.save(&chan_dir).await?;

        let ev = ChannelEvent::MemberJoined {
            channel: channel.clone(),
            member: invitee.to_string(),
            at: Utc::now().to_rfc3339(),
        };
        let node = self.wrap_event_node(NodeId::generate(), &ev, Vec::new())?;
        // Use the creator as the PlanStorage principal (the plan's owner).
        let creator = peko_plan::PrincipalId(meta.creator.clone());
        self.append_node(channel, &creator, node).await?;
        Ok(())
    }

    async fn post(
        &self,
        channel: &ChannelId,
        sender: &peko_plan::PrincipalId,
        msg: PostMsg,
    ) -> Result<TaskId> {
        let chan_dir = self.cfg.channel_dir(channel);
        let meta = MetaJson::load(&chan_dir).await?;
        if !meta.members.iter().any(|m| m == &sender.to_string()) {
            return Err(ChannelError::NotMember);
        }
        // Validate parent (if any) exists in the plan.
        let record = self.read_plan(channel).await?;
        let parent_node = match &msg.parent {
            Some(p) => {
                let nid = NodeId::parse(p).map_err(|e: PlanError| {
                    ChannelError::Adapter(format!("invalid parent TaskId {p}: {e}"))
                })?;
                if !record.nodes.iter().any(|n| n.node_id == nid) {
                    return Err(ChannelError::Adapter(format!(
                        "parent node {p} not found in channel log"
                    )));
                }
                Some(nid)
            }
            None => None,
        };

        let now = Utc::now();
        let ev = self.posted_event(channel, sender, &msg, parent_node.as_ref(), now)?;
        let new_id = NodeId::generate();
        let deps = parent_node.iter().cloned().collect::<Vec<_>>();
        let node = self.wrap_event_node(new_id.clone(), &ev, deps)?;
        let creator = peko_plan::PrincipalId(meta.creator.clone());
        self.append_node(channel, &creator, node).await?;
        Ok(new_id.as_str().to_string())
    }

    async fn peek(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<ChannelEvent>> {
        // Membership check (defense in depth: caller must be a member).
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        let record = self.read_plan(channel).await?;
        let _ = meta; // presence check; peek does not need the member list
        self.filter_since(&record, since)
    }

    async fn peek_with_ids(
        &self,
        channel: &ChannelId,
        since: &Checkpoint,
    ) -> Result<Vec<(TaskId, ChannelEvent)>> {
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        let record = self.read_plan(channel).await?;
        let _ = meta;
        // PR-1 cursor semantics: opaque string carrying the count of
        // events already delivered to this principal. New posts are
        // appended in causal order, so dropping the first N nodes is
        // equivalent to "drop everything up to position N".
        //
        // PR-3 may swap to a node-id-keyed cursor for finer-grained
        // compaction (e.g. tombstoning) — but for a polling channel,
        // count-based cursors are sufficient and avoid the
        // non-monotonic lex-order landmine from `NodeId::generate()`.
        let start: usize = if since.0.is_empty() {
            0
        } else {
            since.0.parse().map_err(|_| {
                ChannelError::Adapter(format!(
                    "checkpoint {} is not a numeric count",
                    since.0
                ))
            })?
        };
        let mut out = Vec::with_capacity(record.nodes.len().saturating_sub(start));
        for (idx, n) in record.nodes.iter().enumerate() {
            if idx < start {
                continue;
            }
            let ev = Self::parse_event_node(n)?;
            out.push((n.node_id.as_str().to_string(), ev));
        }
        Ok(out)
    }

    async fn leave(
        &self,
        channel: &ChannelId,
        principal: &peko_plan::PrincipalId,
    ) -> Result<()> {
        let chan_dir = self.cfg.channel_dir(channel);
        let mut meta = MetaJson::load(&chan_dir).await?;
        let before: HashSet<String> = meta.members.iter().cloned().collect();
        if !before.contains(&principal.to_string()) {
            // Idempotent leave for non-members.
            return Ok(());
        }
        meta.members.retain(|m| m != &principal.to_string());
        meta.save(&chan_dir).await?;

        let ev = ChannelEvent::MemberLeft {
            channel: channel.clone(),
            member: principal.to_string(),
            at: Utc::now().to_rfc3339(),
        };
        let node = self.wrap_event_node(NodeId::generate(), &ev, Vec::new())?;
        let creator = peko_plan::PrincipalId(meta.creator.clone());
        self.append_node(channel, &creator, node).await?;
        Ok(())
    }

    async fn list_members(
        &self,
        channel: &ChannelId,
    ) -> Result<Vec<peko_plan::PrincipalId>> {
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        Ok(meta
            .members
            .into_iter()
            .map(peko_plan::PrincipalId)
            .collect())
    }

    async fn list_for_principal(
        &self,
        principal: &peko_plan::PrincipalId,
    ) -> Result<Vec<ChannelId>> {
        self.list_channels_for_principal(principal).await
    }

    async fn membership(&self, channel: &ChannelId) -> Result<ChannelMembership> {
        let meta = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        // Pull the last Member* event timestamp from the plan to populate
        // `last_membership_change`. Walks `peek` from the default (start)
        // checkpoint and finds the most recent.
        let events = ChannelPort::peek(self, channel, &Checkpoint::default()).await?;
        let last_change = events
            .iter()
            .rev()
            .find_map(|ev| match ev {
                ChannelEvent::MemberJoined { at, .. } | ChannelEvent::MemberLeft { at, .. } => {
                    Some(at.clone())
                }
                _ => None,
            });
        Ok(ChannelMembership {
            channel: channel.clone(),
            name: meta.name,
            creator: meta.creator,
            members: meta.members,
            created_at: meta.created_at.to_rfc3339(),
            last_membership_change: last_change,
        })
    }

    async fn load_config(&self, channel: &ChannelId) -> Result<ConfigOnDisk> {
        // Defense-in-depth: confirm the channel exists before reading
        // its config (MetaJson::load returns NotFound for missing dirs).
        let _ = MetaJson::load(&self.cfg.channel_dir(channel)).await?;
        ConfigOnDisk::load(&self.cfg.channel_dir(channel)).await
    }
}

