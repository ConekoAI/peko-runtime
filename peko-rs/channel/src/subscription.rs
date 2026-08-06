//! Per-member subscription loop.
//!
//! Each `ChannelSubscriber` polls a single channel for a single
//! principal on a tokio interval. New events flow to a
//! `ChannelResponder`; observed `TaskId`s are persisted to
//! `ChannelCursors` so a re-tick doesn't redeliver.
//!
//! ## Anti-loop inheritance (from `lexical-soaring-pretzel.md`)
//!
//! `tick_once` is a *read-only* poll: it observes the channel's event
//! log and hands new events to the responder + meter, but never posts
//! back as part of the same tick. Even if a future responder impl
//! dispatched a subagent, that subagent's subsequent post goes through
//! `ChannelPort::post`, NOT through this poll, so we can't
//! accidentally loop on our own writes.
//!
//! ## Test seam
//!
//! `tick_once` is `pub` and synchronous (returns the new events for
//! inspection). The integration test wraps `NoopChannelResponder` in a
//! counter and verifies (a) one `consider_response` call per new
//! event, (b) zero spurious calls when the cursor advances past the
//! last event.

use std::sync::Arc;
use std::time::Duration;

use peko_plan::PrincipalId;
use peko_protocol::channel::{ChannelEvent, ChannelId};

use crate::cost::ChannelMeter;
use crate::cursors::ChannelCursors;
use crate::port::{ChannelError, ChannelPort, Checkpoint, Result};
use crate::responder::{ChannelResponder, RespondCtx};

// ---------------------------------------------------------------------------
// SubscriptionConfig
// ---------------------------------------------------------------------------

/// Static configuration for the subscription loop.
#[derive(Debug, Clone)]
pub struct SubscriptionConfig {
    /// How long to sleep between ticks. Default 5s; configurable so
    /// tests can crank it down and host code can tune per environment.
    pub poll_interval: Duration,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelSubscriber
// ---------------------------------------------------------------------------

/// One principal's view of one channel. `Send + Sync` so it can live
/// on a tokio task spawned via [`Self::spawn`].
pub struct ChannelSubscriber {
    channel: ChannelId,
    principal: PrincipalId,
    port: Arc<dyn ChannelPort>,
    responder: Arc<dyn ChannelResponder>,
    meter: Arc<dyn ChannelMeter>,
    cursors: ChannelCursors,
    channel_dir: std::path::PathBuf,
    cfg: SubscriptionConfig,
}

impl std::fmt::Debug for ChannelSubscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSubscriber")
            .field("channel", &self.channel)
            .field("principal", &self.principal)
            .field("poll_interval", &self.cfg.poll_interval)
            .finish_non_exhaustive()
    }
}

impl ChannelSubscriber {
    /// Construct a subscriber. Caller supplies the channel dir for
    /// cursor persistence.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channel: ChannelId,
        principal: PrincipalId,
        channel_dir: std::path::PathBuf,
        port: Arc<dyn ChannelPort>,
        responder: Arc<dyn ChannelResponder>,
        meter: Arc<dyn ChannelMeter>,
        cursors: ChannelCursors,
        cfg: SubscriptionConfig,
    ) -> Self {
        Self {
            channel,
            principal,
            port,
            responder,
            meter,
            cursors,
            channel_dir,
            cfg,
        }
    }

    /// Borrow the configured poll interval (for tests asserting
    /// default-vs-custom).
    pub fn poll_interval(&self) -> Duration {
        self.cfg.poll_interval
    }

    /// Read the local cursor. Returns the empty `TaskId` if the
    /// principal has never observed any event.
    pub fn cursor(&self) -> String {
        self.cursors
            .get(&self.principal)
            .cloned()
            .unwrap_or_default()
    }

    /// One poll iteration. Returns the *new* events that triggered a
    /// `consider_response` call, in causal order. Cursor is advanced
    /// to the new total event count.
    ///
    /// Public so tests can drive ticks deterministically without
    /// needing a real timer. Production callers should use [`Self::spawn`].
    pub async fn tick_once(&mut self) -> Result<Vec<ChannelEvent>> {
        let since = Checkpoint(self.cursor());
        // `peek_with_ids` carries the underlying `peko_plan::NodeId`
        // alongside each event. PR-1's cursor semantics are
        // count-based (the adapter interprets the opaque checkpoint
        // string as a "drop first N events" offset).
        let items = self.port.peek_with_ids(&self.channel, &since).await?;

        let mut delivered: Vec<ChannelEvent> = Vec::with_capacity(items.len());
        let mut new_count: Option<usize> = None;
        for (task_id, ev) in items {
            // Meter (no-op in PR-1, real in PR-2).
            if let Err(e) = self.meter.record_event(&self.channel, &self.principal.to_string(), &ev).await {
                tracing::warn!(?e, "channel meter record_event failed");
            }

            // Hand to the responder.
            let ctx = RespondCtx {
                channel: self.channel.clone(),
                principal: self.principal.clone(),
                event: ev.clone(),
                now: std::time::SystemTime::now(),
            };
            self.responder.consider_response(ctx).await?;

            // task_id is currently unused — kept in the tuple for PR-3
            // when node-id-keyed cursors land.
            let _ = task_id;
            new_count = Some(delivered.len() + 1); // placeholder; fixed below
            delivered.push(ev);
        }

        // Compute the new cursor as the prior offset + the number of
        // events we just delivered. Avoids the non-monotonic lex-order
        // pitfall in `NodeId::generate()`.
        if !delivered.is_empty() {
            let prior: usize = since
                .0
                .parse()
                .unwrap_or(0);
            let high = (prior + delivered.len()).to_string();
            self.cursors.set(self.principal.clone(), high.clone());
            new_count = Some(prior + delivered.len());
            if let Err(e) = self.cursors.save(&self.channel_dir).await {
                tracing::warn!(?e, "channel cursor save failed");
            }
        }
        let _ = new_count; // suppress unused-assignment warning
        Ok(delivered)
    }

    /// Spawn the subscription loop on the current tokio runtime.
    /// Returns a `JoinHandle` for the background task; the task exits
    /// when `stop` (TODO PR-2 — not wired in PR-1) signals cancellation.
    ///
    /// PR-1: callers can ignore the `JoinHandle` and let the loop run
    /// for the process lifetime. The integration test invokes
    /// `tick_once` directly and never calls `spawn`.
    pub fn spawn(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = self.cfg.poll_interval;
            loop {
                match self.tick_once().await {
                    Ok(_n) => {
                        // success — sleep and poll again
                    }
                    Err(e) => match &e {
                        // Transient errors: keep looping. Catastrophic
                        // errors: log and break (a fresh spawn can
                        // recover the loop on restart).
                        ChannelError::NotFound(_) | ChannelError::NotMember => {
                            tracing::warn!(?e, "subscription ended (channel gone)");
                            return;
                        }
                        _ => {
                            tracing::error!(?e, "subscription tick failed");
                        }
                    },
                }
                tokio::time::sleep(interval).await;
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_poll_interval_is_5_seconds() {
        let cfg = SubscriptionConfig::default();
        assert_eq!(cfg.poll_interval, Duration::from_secs(5));
    }
}