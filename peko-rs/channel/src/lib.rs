//! `peko-channel` — multi-principal chat primitive.
//!
//! A `Channel` is a persistent chat container with N members, where each
//! member subscribes, polls for new messages, and decides-to-respond via
//! subagent dispatch. The polling semantic (vs. passive push) prevents
//! the multi-agent infinite-feedback loop the user's group-chat design
//! explicitly calls out.
//!
//! ## Crate boundary
//!
//! - [`port`] — `ChannelPort` trait (consumer-defined). Operations:
//!   `create`, `invite`, `post`, `peek`, `leave`, `list_members`,
//!   `list_for_principal`. Errors via [`port::ChannelError`].
//! - [`responder`] — `ChannelResponder` trait (also consumer-defined).
//!   One method: `consider_response(ctx)`. The shipped impl is the
//!   `NoopChannelResponder`; agents read channels actively via the
//!   `ChannelRead` tool (peko-core) rather than reacting via a daemon
//!   responder. See `peko-channel-pr4-shipped.md` for the rationale.
//! - [`plan_channel`] — `PlanChannelAdapter: ChannelPort` (storage impl
//!   backed by `peko_plan::PlanStorage`).
//! - [`subscription`] — `ChannelSubscriber` (per-member poll loop;
//!   `tick_once` is the test seam).
//! - [`cursors`] — `ChannelCursors` (per-channel runtime-tier
//!   "last_read_task_id" map).
//! - [`cost`] — metering bridge (PR-3c wires `AuditChannelMeter`).
//! - [`cli_handlers`] — handler bodies wired into `peko-rs/cli`.
//!
//! Wire types (ChannelId, ChannelEvent, ChannelMembership) live in
//! `peko-protocol`; re-exported here for ergonomic callers.
//!
//! ## Tier rule
//!
//! - Default: channels live in the **Runtime** tier (ephemeral;
//!   `<runtime_dir>/channels/<channel_id>/...`).
//! - PR-3 introduces a `PinToShared` op for opt-in promotion to the
//!   Shared tier; the Phase B authority gate is reused as-is.
//! - We do NOT introduce a 4th storage tier — channels live *in* an
//!   existing tier (per `phase-a-three-tier-storage.md`).
//!
//! ## Why no per-channel capability intersection
//!
//! Earlier drafts had a `ChannelResponder` impl wrapping subagent
//! dispatch plus a `caps::intersect_member_caps` helper to gate it.
//! That wiring was speculative — agents read actively (PR-4a
//! `ChannelRead` tool) so there is no daemon-side responder to gate.
//! Capability is a principal-level concept; channels are just a shared
//! append-only log. See `peko-channel-pr5-shipped.md` (PR-5a).
//!
//! See `multi-model-subagents-phase2-shipped.md` for the recent work
//! this composes against.
//!
//! ## Forbidden deps
//!
//! This crate must not depend on:
//! - `peko-engine` — would invert the dispatch/contract direction.
//! - `peko-core` — the root crate is a thin composition layer; leaf
//!   crates don't reach into it.
//! - `peko-cron` — orthogonal domain.
//! - `peko-protocol` for anything other than re-exports.
//! - Any `peko-extension-*` crate.
//!
//! Allowed deps: `peko-plan` (storage), `peko-protocol` (wire types).
//! `peko-subject` is reachable through `peko_plan::PrincipalId`.

#![allow(clippy::module_inception)]

pub mod cli_handlers;
pub mod config;
pub mod cost;
pub mod cursors;
pub mod plan_channel;
pub mod port;
pub mod responder;
pub mod subscription;

// Flat re-exports — channel callers should not need to know which
// submodule a type lives in. Mirrors `peko_cron::lib.rs:59-65` and
// `peko-rs/cron/src/lib.rs:53-67`.
pub use cli_handlers::ChannelCliRouter;
pub use config::ConfigOnDisk;
pub use cost::{audit_meter, AuditChannelMeter, ChannelMeter, NoopChannelMeter};
pub use cursors::ChannelCursors;
pub use plan_channel::{ChannelConfig, PlanChannelAdapter};
pub use port::{ChannelError, ChannelPort, Checkpoint, CreateOpts, NoopChannelPort, PostMsg, Result, Tier};
pub use responder::{ChannelResponder, NoopChannelResponder, RespondCtx};
pub use subscription::{ChannelSubscriber, SubscriptionConfig};

// Wire types live in `peko-protocol`; re-exported here for ergonomic
// callers so `peko_channel::ChannelId` reads naturally.
pub use peko_protocol::channel::{
    ChannelEvent, ChannelId, ChannelMembership,
};
