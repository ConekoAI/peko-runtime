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
//!   One method: `consider_response(ctx)`. PR-1 ships a `Noop` impl;
//!   PR-2 wires `peko-engine` subagent dispatch here.
//! - [`plan_channel`] — `PlanChannelAdapter: ChannelPort` (storage impl
//!   backed by `peko_plan::PlanStorage`).
//! - [`subscription`] — `ChannelSubscriber` (per-member poll loop;
//!   `tick_once` is the test seam).
//! - [`cursors`] — `ChannelCursors` (per-channel runtime-tier
//!   "last_read_task_id" map).
//! - [`caps`] — `intersect_member_caps` (concrete capability
//!   intersection, NOT a generic abstraction — see memory note on
//!   `prefer-concrete-over-speculative-abstraction.md`).
//! - [`cost`] — metering bridge (PR-1 stub; PR-3 wires
//!   `peko_quota::MeteredProvider`).
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
//! ## Anti-loop inheritance
//!
//! Each member's poll hands new events to `ChannelResponder`. The PR-2
//! impl will wrap `peko-engine` subagent dispatch, inheriting four
//! existing rails so `peko-channel` doesn't need its own loop control:
//! - `max_depth = 1` (F33 / PR #237).
//! - per-poll cost ceiling + typed retry (F40 / PR #243).
//! - `MeteredProvider` attribution (F19 / PR #174).
//! - per-spawn `model` / `model_list` (PR #346).
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

pub mod caps;
pub mod cli_handlers;
pub mod cost;
pub mod cursors;
pub mod plan_channel;
pub mod port;
pub mod responder;
pub mod subscription;

// Flat re-exports — channel callers should not need to know which
// submodule a type lives in. Mirrors `peko_cron::lib.rs:59-65` and
// `peko-rs/cron/src/lib.rs:53-67`.
pub use caps::intersect_member_caps;
pub use cli_handlers::ChannelCliRouter;
pub use cost::NoopChannelMeter;
pub use cursors::ChannelCursors;
pub use plan_channel::{ChannelConfig, PlanChannelAdapter};
pub use port::{ChannelError, ChannelPort, Checkpoint, CreateOpts, PostMsg, Result, Tier};
pub use responder::{ChannelResponder, NoopChannelResponder, RespondCtx};
pub use subscription::{ChannelSubscriber, SubscriptionConfig};

// Wire types live in `peko-protocol`; re-exported here for ergonomic
// callers so `peko_channel::ChannelId` reads naturally.
pub use peko_protocol::channel::{
    ChannelEvent, ChannelId, ChannelMembership,
};
