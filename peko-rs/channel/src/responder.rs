//! `ChannelResponder` trait + a `Noop` impl.
//!
//! The trait is consumer-defined (lives here, NOT in `peko-engine`) so
//! the dep direction stays acyclic:
//!
//! - `peko-channel` declares the contract;
//! - `peko-channel::ChannelSubscriber` consumes it via
//!   `Arc<dyn ChannelResponder>`.
//!
//! Only the `Noop` impl ships in this crate. The production responder
//! (`GroupWakeResponder`) lives in the root crate
//! (`peko-rs/core/src/daemon/channel_binding.rs`) because driving a
//! turn needs the subagent executor — a root-only dependency this leaf
//! crate must not take.

use async_trait::async_trait;
use peko_protocol::channel::{ChannelEvent, ChannelId};
use peko_subject::PrincipalId;

use crate::port::TaskId;
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
///
/// `event_id` is the triggering event's [`TaskId`] (its line number in
/// the LOCAL `events.jsonl`). Responders that post a reply thread it
/// via `PostMsg::reply(event_id, …)` so the anti-loop parent rule
/// (`parent.is_none()` on the trigger side) can tell replies from root
/// posts. Line numbers are runtime-local: a mirrored channel's line
/// numbers diverge from the source runtime's, so a `parent` value is
/// only meaningful in the log where the reply was written.
#[derive(Debug, Clone)]
pub struct RespondCtx {
    pub channel: ChannelId,
    pub principal: PrincipalId,
    pub event: ChannelEvent,
    pub event_id: TaskId,
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

/// Whether `text` mentions `@<name>`: case-insensitive and not part of a
/// longer word (`@ada` is not mentioned by `@adam`; `@ada,` and `@ada.`
/// are mentions).
pub fn mentions(text: &str, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    if name.is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let needle = format!("@{name}");
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    text.match_indices(&needle).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        before.is_none_or(|c| !word(c) && c != '@') && after.is_none_or(|c| !word(c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_match_whole_names_case_insensitively() {
        for text in [
            "@self hi",
            "hi @Self",
            "ping @SELF, please",
            "(@self)",
            "@self.",
        ] {
            assert!(mentions(text, "self"), "{text}");
        }
        for text in ["self hi", "@selfish", "me@self", "@@self", "@sel"] {
            assert!(!mentions(text, "self"), "{text}");
        }
        assert!(
            !mentions("@ anything", ""),
            "an empty name is never mentioned"
        );
        assert!(mentions("cc @ada-lab", "ada-lab"));
    }
}
