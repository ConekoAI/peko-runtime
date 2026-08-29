//! CLI handler surface for the `peko channel` subcommand.
//!
//! PR-1 ships the handler function bodies only — the `peko-rs/cli`
//! binary's arg-parser wiring lives in 4 files
//! (`peko-rs/cli/src/commands/channel.rs`,
//! `commands/mod.rs:17,160-162`, `main.rs:137`, `Cargo.toml`).
//! It's a separate PR-1 review item flagged in
//! `lexical-soaring-pretzel.md` §11.
//!
//! ## Why a separate handler module
//!
//! The handlers here operate on `Arc<dyn ChannelPort>` directly — no
//! `peko-protocol` IPC, no daemon dependency — so:
//!
//! 1. Tests can drive them against an in-memory adapter.
//! 2. The CLI binary can call them from a thin clap dispatch arm.
//! 3. PR-3 can add a daemon-side IPC variant without touching the
//!    handler bodies — same shapes, just over the wire.
//!
//! ## Why not split per-subcommand into 5 files
//!
//! The handlers are short (<30 lines each) and share request/response
//! types via this module. Splitting per command would multiply files
//! for negligible gain. PR-3 may split if handlers grow beyond ~80L
//! each.

use std::sync::Arc;

use peko_subject::{PrincipalId, Subject};
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use serde::{Deserialize, Serialize};

use crate::port::{ChannelPort, Checkpoint, CreateOpts, PostMsg, Result};

// ---------------------------------------------------------------------------
// ChannelCliRouter
// ---------------------------------------------------------------------------

/// Thin handle for the CLI binary. Holds the port impl; future
/// revisions may add a daemon-client option (PR-3 IPC variant).
#[derive(Clone)]
pub struct ChannelCliRouter {
    port: Arc<dyn ChannelPort>,
}

impl std::fmt::Debug for ChannelCliRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelCliRouter").finish_non_exhaustive()
    }
}

impl ChannelCliRouter {
    /// Construct a router over an in-process port. Used by the CLI
    /// binary's "no daemon" code path and by tests.
    #[must_use]
    pub fn new(port: Arc<dyn ChannelPort>) -> Self {
        Self { port }
    }

    /// Borrow the underlying port (for advanced use cases; PR-2 IPC).
    #[must_use]
    pub fn port(&self) -> &Arc<dyn ChannelPort> {
        &self.port
    }

    // -----------------------------------------------------------------
    // Subcommand handlers
    // -----------------------------------------------------------------

    /// `peko channel create <name>` — create a channel owned by
    /// `creator`, with the given name. Returns the new channel id.
    ///
    /// `passive_binding` (Phase 4, agent-session paradigm sprint) is the
    /// optional `--bind` value: a session id or `/path` in the creator's
    /// session tree that inbound `Posted` events wake. `None` creates a
    /// purely active (group-tier) channel — today's default.
    pub async fn handle_create(
        &self,
        creator: &PrincipalId,
        name: &str,
        passive_binding: Option<String>,
    ) -> Result<CreateResponse> {
        let mut opts = CreateOpts::runtime(name);
        opts.passive_binding = passive_binding;
        let channel = self.port.create(creator, opts).await?;
        Ok(CreateResponse { channel })
    }

    /// `peko channel invite <channel> <invitee>` — add `invitee` to
    /// `channel` as `inviter`. ADR-049 Phase 1: the invitee is
    /// [`Subject`]-typed (principals and users); the inviter stays a
    /// principal.
    pub async fn handle_invite(
        &self,
        channel: &ChannelId,
        inviter: &PrincipalId,
        invitee: &Subject,
    ) -> Result<InviteResponse> {
        self.port.invite(channel, inviter, invitee).await?;
        Ok(InviteResponse {
            channel: channel.clone(),
            invitee: invitee.clone(),
        })
    }

    /// `peko channel post <channel> [--parent <task_id>] <text>` —
    /// append a message. Returns the new task id. The sender is
    /// principal-typed at this layer (the user write path via CLI/IPC
    /// is ADR-049 Phase 2); the port call bridges to `Subject`.
    pub async fn handle_post(
        &self,
        channel: &ChannelId,
        sender: &PrincipalId,
        text: &str,
        parent: Option<String>,
    ) -> Result<PostResponse> {
        let msg = match parent {
            Some(p) => PostMsg::reply(p, text),
            None => PostMsg::root(text),
        };
        let task_id = self.port.post(channel, &Subject::from(sender), msg).await?;
        Ok(PostResponse {
            channel: channel.clone(),
            task_id,
        })
    }

    /// `peko channel peek <channel> [--since <task_id>]` — list events.
    pub async fn handle_peek(
        &self,
        channel: &ChannelId,
        since: Option<String>,
    ) -> Result<PeekResponse> {
        let events = self
            .port
            .peek(channel, &Checkpoint(since.unwrap_or_default()))
            .await?;
        Ok(PeekResponse {
            channel: channel.clone(),
            events,
        })
    }

    /// `peko channel leave <channel>` — remove `principal` from
    /// `channel`.
    pub async fn handle_leave(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
    ) -> Result<LeaveResponse> {
        self.port.leave(channel, principal).await?;
        Ok(LeaveResponse {
            channel: channel.clone(),
            principal: principal.clone(),
        })
    }

    /// `peko channel members <channel>` — list current members.
    pub async fn handle_members(
        &self,
        channel: &ChannelId,
    ) -> Result<MembersResponse> {
        let members = self.port.list_members(channel).await?;
        // P1.2 attribution: pull per-member runtime provenance from
        // `members.json`. The IPC consumer (desktop) reads the flat
        // `members` for back-compat + `member_provenance` for the
        // local/remote distinction in `MemberList`.
        let member_provenance = self.port.members_with_attribution(channel).await?;
        Ok(MembersResponse {
            channel: channel.clone(),
            members,
            member_provenance,
        })
    }

    /// `peko channel ls` — list channels where `principal` is a member.
    pub async fn handle_list(
        &self,
        principal: &PrincipalId,
    ) -> Result<ListResponse> {
        let channels = self.port.list_for_principal(principal).await?;
        Ok(ListResponse {
            principal: principal.clone(),
            channels,
        })
    }

    /// `peko channel show <channel>` — membership snapshot for IPC.
    pub async fn handle_show(
        &self,
        channel: &ChannelId,
    ) -> Result<ChannelMembership> {
        self.port.membership(channel).await
    }

    /// `peko channel pin-to-shared <channel>` — copy a Runtime-tier
    /// channel into the adapter's Shared tier (PR-3d). Returns the
    /// absolute Shared path. COPY semantics — the Runtime source
    /// remains. The authority gate (`channel:write_shared`) is
    /// enforced upstream by the daemon IPC handler; the CLI does the
    /// same check via `RuntimeAuthority::write_shared_channels` on
    /// the daemon path. The in-process fallback path is gated the
    /// same way in the CLI dispatch arm.
    pub async fn handle_pin_to_shared(
        &self,
        channel: &ChannelId,
    ) -> Result<std::path::PathBuf> {
        self.port.pin_to_shared(channel).await
    }
}

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

/// `--json` output for `peko channel create`. Mirrors
/// `peko_rpc::ChannelCreateResponse` (PR-3 wire shape) — keep field
/// names stable across the PR boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateResponse {
    pub channel: ChannelId,
}

/// `--json` output for `peko channel invite`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteResponse {
    pub channel: ChannelId,
    /// ADR-049 Phase 1: the invitee is `Subject`-typed (a principal or
    /// a user).
    pub invitee: Subject,
}

/// `--json` output for `peko channel post`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostResponse {
    pub channel: ChannelId,
    pub task_id: String,
}

/// `--json` output for `peko channel peek`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeekResponse {
    pub channel: ChannelId,
    pub events: Vec<ChannelEvent>,
}

/// `--json` output for `peko channel leave`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaveResponse {
    pub channel: ChannelId,
    pub principal: PrincipalId,
}

/// `--json` output for `peko channel members`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembersResponse {
    pub channel: ChannelId,
    /// ADR-049 Phase 1: members are `Subject`-typed (principals and
    /// users).
    pub members: Vec<Subject>,
    /// P1.2 attribution: per-member runtime provenance. Empty for
    /// channels with no remote members; legacy pre-PR-3b
    /// implementations may omit the field entirely
    /// (`#[serde(default)]`).
    #[serde(default)]
    pub member_provenance: Vec<peko_protocol::channel::MemberProvenance>,
}

/// `--json` output for `peko channel ls`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub principal: PrincipalId,
    pub channels: Vec<ChannelId>,
}
