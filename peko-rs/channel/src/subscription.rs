//! Per-member subscription loop.
//!
//! Each `ChannelSubscriber` watches a single channel for a single
//! principal. The loop is **push-woken** (sprint 3 Phase 10): it
//! `select!`s on the port's live-event broadcast
//! ([`ChannelPort::subscribe_events`], fired by `ChannelStore`'s
//! single append chokepoint on every durable append — local posts,
//! membership events, AND cross-runtime mirror appends) and on a
//! backstop interval tick. New events flow to a `ChannelResponder`;
//! observed `TaskId`s are persisted to `ChannelCursors` so a re-tick
//! doesn't redeliver. The broadcast is at-most-once wake-up only —
//! the cursor walk is authoritative, so a missed or lagged
//! notification is repaired by the next tick.
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

use peko_subject::PrincipalId;
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
    /// Backstop tick interval. Since sprint 3 Phase 10 the loop is
    /// push-woken by the port's live-event broadcast, so this only
    /// repairs missed notifications (lagged/closed broadcast,
    /// adapters without a registry). Default 30s (raised from 5s
    /// when push-wake landed — every tick re-reads the event log
    /// tail); configurable so tests can crank it down and host code
    /// can tune per environment.
    pub poll_interval: Duration,
}

impl Default for SubscriptionConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
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
        // `peek_with_ids` carries the source line number alongside
        // each event. The cursor semantics are count-based (the
        // adapter interprets the opaque checkpoint string as a
        // "drop first N events" offset).
        let items = self.port.peek_with_ids(&self.channel, &since).await?;

        let mut delivered: Vec<ChannelEvent> = Vec::with_capacity(items.len());
        for (task_id, ev) in items {
            // Meter.
            if let Err(e) = self.meter.record_event(&self.channel, &self.principal.to_string(), &ev).await {
                tracing::warn!(?e, "channel meter record_event failed");
            }

            // Hand to the responder. `event_id` is the source line
            // number so a responder reply can thread onto the
            // triggering event (`PostMsg::reply`).
            let ctx = RespondCtx {
                channel: self.channel.clone(),
                principal: self.principal.clone(),
                event: ev.clone(),
                event_id: task_id,
                now: std::time::SystemTime::now(),
            };
            self.responder.consider_response(ctx).await?;

            delivered.push(ev);
        }

        // Compute the new cursor as the prior offset + the number of
        // events we just delivered. The JSONL log is append-only, so
        // line numbers are stable — count-based cursors avoid the
        // non-monotonic lex-order pitfall of opaque-string ids.
        if !delivered.is_empty() {
            let prior: usize = since.0.parse().unwrap_or(0);
            let high = (prior + delivered.len()).to_string();
            self.cursors.set(self.principal.clone(), high);
            if let Err(e) = self.cursors.save(&self.channel_dir).await {
                tracing::warn!(?e, "channel cursor save failed");
            }
        }
        Ok(delivered)
    }

    /// Spawn the subscription loop on the current tokio runtime.
    /// Returns a `JoinHandle` for the background task; the task runs
    /// until the channel is closed (the `port.subscribe_events`
    /// broadcast returns `Closed`).
    ///
    /// The loop alternates between a tick and a `select!` wait on (a)
    /// the port's live-event broadcast — any append wakes an
    /// immediate tick — and (b) the backstop interval. A `Closed`
    /// broadcast (adapters using the trait's default no-op
    /// `subscribe_events`) permanently degrades the loop to
    /// interval-only ticking; `Lagged` just triggers a tick (the
    /// cursor walk picks up everything since the last one).
    ///
    /// Callers can ignore the `JoinHandle` and let the loop run for
    /// the process lifetime. The integration test invokes `tick_once`
    /// directly and never calls `spawn`.
    pub fn spawn(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let interval = self.cfg.poll_interval;
            // Subscribe BEFORE the first tick: events appended after
            // this point wake the loop, and anything earlier is
            // covered by the first tick's from-cursor walk.
            let mut wake = self.port.subscribe_events(&self.channel).await;
            let mut wake_open = true;
            loop {
                match self.tick_once().await {
                    Ok(_n) => {
                        // success — wait for the next wake
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
                if !wake_open {
                    tokio::time::sleep(interval).await;
                    continue;
                }
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    ev = wake.recv() => {
                        if matches!(ev, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                            // The port has no live registry (default
                            // trait impl) — fall back to pure ticking.
                            wake_open = false;
                        }
                        // Ok(_) or Lagged(_): an append landed — tick
                        // immediately; the cursor walk is authoritative.
                    }
                }
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
    fn default_poll_interval_is_30_seconds() {
        let cfg = SubscriptionConfig::default();
        assert_eq!(cfg.poll_interval, Duration::from_secs(30));
    }
}
