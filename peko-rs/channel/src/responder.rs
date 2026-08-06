//! `ChannelResponder` trait + a `Noop` impl.
//!
//! The trait is consumer-defined (lives here, NOT in `peko-engine`) so
//! the dep direction stays acyclic:
//!
//! - `peko-channel` declares the contract;
//! - `peko-channel::ChannelSubscriber` consumes it via
//!   `Arc<dyn ChannelResponder>`.
//!
//! Only the `Noop` impl ships. Agents read channels actively via the
//! `ChannelRead` tool (peko-core / PR-4a), so there is no daemon-side
//! responder to dispatch through. Future per-channel behaviour can
//! land as additional impls without touching the trait.

use async_trait::async_trait;
use peko_subject::PrincipalId;
use peko_protocol::channel::{ChannelEvent, ChannelId};

use crate::Result;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Callback the per-member subscription loop calls on every *new* event
/// (i.e. an event keyed strictly after the local cursor). The
/// implementation owns the "should I respond?" decision.
///
/// Naming choice: `consider_response` (vs. `respond`) — the polling
/// semantic is a "should I?" question, not a forced reply. Captures the
/// anti-loop inheritance from the multi-model subagent work
/// (`multi-model-subagents-phase2-shipped.md`): the responder may
/// decide to dispatch a subagent, drop the event, or post a
/// continuation, but it never forces an answer.
#[async_trait]
pub trait ChannelResponder: Send + Sync + 'static {
    /// Consider whether to respond to `ctx.event` posted in `ctx.channel`
    /// while acting as `ctx.principal`.
    async fn consider_response(&self, ctx: RespondCtx) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Per-call context passed to [`ChannelResponder::consider_response`].
///
/// `now` is the wall-clock instant at which the subscriber observed the
/// event (NOT the event's own `at` field — clocks can drift between
/// hosts and we want the responder to see its own time).
#[derive(Debug, Clone)]
pub struct RespondCtx {
    pub channel: ChannelId,
    pub principal: PrincipalId,
    pub event: ChannelEvent,
    pub now: std::time::SystemTime,
}

// ---------------------------------------------------------------------------
// Noop
// ---------------------------------------------------------------------------

/// `ChannelResponder` impl that does nothing. Production default
/// (audit observation runs alongside it via `ChannelMeter`); tests wrap
/// it in a counter to verify per-event fan-out.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopChannelResponder;

#[async_trait]
impl ChannelResponder for NoopChannelResponder {
    async fn consider_response(&self, _ctx: RespondCtx) -> Result<()> {
        Ok(())
    }
}