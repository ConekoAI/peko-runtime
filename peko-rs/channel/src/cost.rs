//! Metering bridge (PR-1 stub).
//!
//! PR-2 wires this to `peko_quota::QuotaMeter` + `MeteredProvider` (per
//! F19 / PR #174). PR-1 ships only the trait shape + a no-op impl so
//! the subscription loop has something to hold via `Arc<dyn>` without
//! paying the dep cost.
//!
//! Design: traits live where they are CONSUMED (here), not where they
//! are produced (`peko-quota`). Mirrors the trait-port discipline
//! from `workspace-phase9b-n5a-traits.md` and
//! `audit-gate-bypass-side-surface.md` (F37 funnel fix).

use std::sync::Arc;

use async_trait::async_trait;
use peko_protocol::channel::{ChannelEvent, ChannelId};

use crate::Result;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Records per-event cost attribution for a single member of a single
/// channel. PR-1: no-op. PR-2: wraps `peko_quota::QuotaScope` and emits
/// `TokenUsage` records keyed by `(channel, principal, event_kind)`.
#[async_trait]
pub trait ChannelMeter: Send + Sync + 'static {
    /// Account for one event the responder observed.
    async fn record_event(
        &self,
        channel: &ChannelId,
        principal: &str,
        event: &ChannelEvent,
    ) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Noop impl
// ---------------------------------------------------------------------------

/// Discards all calls. Used as the PR-1 default when nothing else is
/// configured.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopChannelMeter;

#[async_trait]
impl ChannelMeter for NoopChannelMeter {
    async fn record_event(
        &self,
        _channel: &ChannelId,
        _principal: &str,
        _event: &ChannelEvent,
    ) -> Result<()> {
        Ok(())
    }
}

/// Convenience: the typical subscription loop just needs an
/// `Arc<dyn ChannelMeter>` to pass to `ChannelSubscriber`. This helper
/// exists so callers don't have to write `Arc::new(NoopChannelMeter)`
/// inline; future PRs may swap it for a config-driven resolver.
pub fn noop_meter() -> Arc<dyn ChannelMeter> {
    Arc::new(NoopChannelMeter)
}