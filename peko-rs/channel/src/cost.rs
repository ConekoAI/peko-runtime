//! Per-event audit bridge for the channel subscription loop.
//!
//! `AuditChannelMeter` (PR-3c) emits one audit record per `ChannelEvent`
//! the subscriber observes — `peko audit list --type channel.` surfaces
//! the resulting `channel.<kind>` events (ADR-046). The meter is purely
//! for event-observation audit (who saw what, when); per-spawn LLM cost
//! rides on F39 `QuotaMeter` on `SubagentExecutor`, not here.
//!
//! The `ChannelMeter` trait is the seam quota consumers attach to
//! when a per-channel attribution pass is wired in (the F19
//! `peko_quota::QuotaScope` shape). Today `AuditChannelMeter` (PR-3c)
//! is the active impl — every `ChannelSubscriber` constructed in
//! production carries one via `Arc<dyn ChannelMeter>`. Quota work
//! would slot in as an additional impl, not a re-shape of the trait.
//!
//! Design: traits live where they are CONSUMED (here), not where they
//! are produced (`peko-quota`). Mirrors the trait-port discipline
//! from `workspace-phase9b-n5a-traits.md` and
//! `audit-gate-bypass-side-surface.md` (F37 funnel fix).

use std::sync::Arc;

use async_trait::async_trait;
use peko_observability::Observability;
use peko_protocol::channel::{ChannelEvent, ChannelId};
use serde_json::json;

use crate::Result;

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Per-event attribution hook for a channel subscription. The
/// production impl (`AuditChannelMeter`) emits a `channel.<kind>` audit
/// record per call; future quota work (F19 / PR #174) could attach a
/// `peko_quota::QuotaScope` consumer here without changing call sites.
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

// ---------------------------------------------------------------------------
// Audit impl (PR-3c)
// ---------------------------------------------------------------------------

/// Emits one audit event per `record_event` call. The audit event type
/// is `channel.<kind>` (e.g. `channel.created`, `channel.posted`,
/// `channel.member_joined`, `channel.member_left`). The `agent_did`
/// field carries the principal name so per-principal audit queries
/// (`peko audit list --principal alice`) surface channel observations.
///
/// Sink: the supplied [`Observability`] instance, which writes to both
/// the in-memory ring buffer (for `peko audit list`) AND the
/// JSONL-per-day file (for `peko audit tail`) — see
/// `peko_observability::Observability::audit`.
pub struct AuditChannelMeter {
    observability: Arc<Observability>,
}

impl std::fmt::Debug for AuditChannelMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditChannelMeter").finish_non_exhaustive()
    }
}

impl AuditChannelMeter {
    /// Wrap an existing `Observability` so a subscription loop can emit
    /// per-event audit records without taking ownership of the
    /// observability hub. `AppState` constructs this once at daemon
    /// start; the resulting `Arc<AuditChannelMeter>` is cloned into
    /// each subscriber task.
    pub fn new(observability: Arc<Observability>) -> Self {
        Self { observability }
    }
}

#[async_trait]
impl ChannelMeter for AuditChannelMeter {
    async fn record_event(
        &self,
        channel: &ChannelId,
        principal: &str,
        event: &ChannelEvent,
    ) -> Result<()> {
        let event_type = format!("channel.{}", event.kind());
        let details = json!({
            "channel": channel.to_string(),
            "kind": event.kind(),
        });
        // Best-effort: a transient audit failure must NOT break the
        // subscription loop. The meter is observability, not
        // correctness. Mirror `subscription.rs:142-144` — log warn,
        // swallow.
        if let Err(e) = self
            .observability
            .audit(&event_type, Some(principal), details)
            .await
        {
            tracing::warn!(
                ?e,
                channel = %channel,
                principal,
                "audit channel meter record_event failed"
            );
        }
        Ok(())
    }
}

/// Convenience: construct the typical `Arc<dyn ChannelMeter>` from a
/// shared `Observability`. Production uses this in `AppState`; tests
/// use `AuditChannelMeter::new` directly.
pub fn audit_meter(observability: Arc<Observability>) -> Arc<dyn ChannelMeter> {
    Arc::new(AuditChannelMeter::new(observability))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use peko_observability::Observability;
    use peko_subject::PrincipalId;

    fn event_posted(channel: &ChannelId, author: &str) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: channel.clone(),
            author: author.into(),
            parent: None,
            text: "hello".into(),
            at: "2026-08-05T12:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn audit_meter_records_event_with_channel_prefix() {
        let obs = Arc::new(Observability::new("test"));
        let meter = AuditChannelMeter::new(obs.clone());
        let ch = ChannelId::parse("chan_abcdefgh").expect("valid id");
        let principal_id = PrincipalId::generate();
        let principal = principal_id.to_string();

        meter
            .record_event(&ch, &principal, &event_posted(&ch, "prin_alice"))
            .await
            .expect("record_event");

        let events = obs.get_audit_log(64).await;
        assert_eq!(events.len(), 1, "expected 1 audit event, got {events:?}");
        let ev = &events[0];
        assert_eq!(
            ev.event_type, "channel.posted",
            "event_type must be channel.<kind>"
        );
        assert_eq!(ev.agent_did.as_deref(), Some(principal.as_str()));
        assert_eq!(
            ev.details.get("channel").and_then(|v| v.as_str()),
            Some("chan_abcdefgh"),
        );
    }

    #[tokio::test]
    async fn audit_meter_records_all_event_kinds() {
        let obs = Arc::new(Observability::new("test"));
        let meter = AuditChannelMeter::new(obs.clone());
        let ch = ChannelId::parse("chan_12345678").expect("valid id");
        let principal = "prin_alice";

        // Created
        meter
            .record_event(
                &ch,
                principal,
                &ChannelEvent::Created {
                    channel: ch.clone(),
                    creator: principal.into(),
                    name: "team".into(),
                    at: "2026-08-05T12:00:00Z".into(),
                },
            )
            .await
            .expect("created");
        // Posted
        meter
            .record_event(&ch, principal, &event_posted(&ch, principal))
            .await
            .expect("posted");
        // MemberJoined
        meter
            .record_event(
                &ch,
                principal,
                &ChannelEvent::MemberJoined {
                    channel: ch.clone(),
                    member: principal.into(),
                    at: "2026-08-05T12:00:01Z".into(),
                },
            )
            .await
            .expect("joined");
        // MemberLeft
        meter
            .record_event(
                &ch,
                principal,
                &ChannelEvent::MemberLeft {
                    channel: ch.clone(),
                    member: principal.into(),
                    at: "2026-08-05T12:00:02Z".into(),
                },
            )
            .await
            .expect("left");

        let events = obs.get_audit_log(64).await;
        assert_eq!(events.len(), 4, "expected 4 audit events");
        // The audit ring buffer returns newest-first; we just check
        // each `channel.<kind>` event_type appears at least once.
        let types: std::collections::HashSet<&str> =
            events.iter().map(|e| e.event_type.as_str()).collect();
        for expected in [
            "channel.created",
            "channel.posted",
            "channel.member_joined",
            "channel.member_left",
        ] {
            assert!(
                types.contains(expected),
                "missing {expected} in {types:?}"
            );
        }
    }
}