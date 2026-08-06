//! Cross-runtime channel-event audit. peko-channel cross-runtime PR-A.
//!
//! Mirrors [`crate::tunnel::a2a_audit`] in shape: structured
//! `tracing::info!` log lines carrying the fields an audit consumer
//! needs to join an outbound `forwarded_outbound` row on the source
//! runtime to each inbound `received_inbound` row on a remote
//! runtime. No persisted event struct yet (channels don't have an
//! `A2aSentEvent` analog) — when one lands, slot it in alongside the
//! `emit_*` functions without changing the call sites.
//!
//! The two events emitted today:
//!
//! - `tunnel_channel.forwarded_outbound` — source runtime side,
//!   emitted after the outbound `TunnelChannelEvent` has been queued
//!   for delivery to each remote member's runtime.
//! - `tunnel_channel.received_inbound` — receiver side, emitted after
//!   the inbound `TunnelChannelEvent` has been signature-verified and
//!   appended to the local mirror (`events.jsonl`).
//!
//! Both carry: `{ event_kind, source_runtime_id, source_principal_did,
//! channel_id, request_id }` plus a free-form `event_payload` preview
//! so audit consumers see the kind discriminant and a snippet of the
//! event body without having to walk the full `ChannelEvent` JSON.

use tracing::info;

/// Preview length for `event_payload`. Mirrors `a2a_audit::preview`'s
/// 200-char budget — kept consistent so audit consumers can use a
/// single truncation heuristic across event kinds.
const PREVIEW_MAX: usize = 200;

/// Truncate a free-form event JSON for the audit-log payload. Audit
/// logs are meant to be searchable, not verbatim-event
/// repositories; a 200-char preview keeps the log line scannable
/// while still capturing the first hint of the event kind + body.
#[must_use]
pub fn preview_event_payload(s: &str) -> String {
    if s.len() <= PREVIEW_MAX {
        s.to_string()
    } else {
        let mut end = PREVIEW_MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Emit the outbound audit log line. Called by the source runtime
/// after the outbound `TunnelChannelEvent` has been queued for
/// delivery to each remote member's runtime (one event per remote
/// member; each fan-out target shares the same `request_id` so audit
/// consumers can correlate).
///
/// `event_payload_preview` is a short string the caller has already
/// truncated from the full event JSON — typically via
/// [`preview_event_payload`].
pub fn emit_forwarded_outbound(
    request_id: &str,
    source_runtime_id: &str,
    source_principal_did: &str,
    channel_id: &str,
    event_kind: &str,
    event_payload_preview: &str,
) {
    info!(
        event_kind = "tunnel_channel.forwarded_outbound",
        request_id = %request_id,
        source_runtime_id = %source_runtime_id,
        source_principal_did = %source_principal_did,
        channel_id = %channel_id,
        payload_kind = %event_kind,
        payload_preview = %event_payload_preview,
        "channel event forwarded to remote member"
    );
}

/// Emit the inbound audit log line. Called by a remote runtime's
/// dispatcher arm after a `TunnelChannelEvent` has been
/// signature-verified and appended to the local mirror.
pub fn emit_received_inbound(
    request_id: &str,
    source_runtime_id: &str,
    source_principal_did: &str,
    channel_id: &str,
    event_kind: &str,
    event_payload_preview: &str,
) {
    info!(
        event_kind = "tunnel_channel.received_inbound",
        request_id = %request_id,
        source_runtime_id = %source_runtime_id,
        source_principal_did = %source_principal_did,
        channel_id = %channel_id,
        payload_kind = %event_kind,
        payload_preview = %event_payload_preview,
        "channel event received from remote runtime"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Short previews pass through unchanged. Mirrors the
    /// `a2a_audit::preview` invariant so audit consumers can use a
    /// single truncation heuristic across event kinds.
    #[test]
    fn test_preview_passes_short_through() {
        let s = r#"{"kind":"posted","text":"hello"}"#;
        assert_eq!(preview_event_payload(s), s);
    }

    /// Long previews truncate with an ellipsis on a UTF-8 char
    /// boundary. The 200-char limit is the same as `a2a_audit`'s
    /// `preview` so audit log heuristics stay uniform.
    #[test]
    fn test_preview_truncates_with_ellipsis() {
        let long = "x".repeat(500);
        let out = preview_event_payload(&long);
        assert!(out.ends_with('…'), "truncated preview must end with ellipsis");
        // 200 chars + ellipsis.
        assert_eq!(out.chars().count(), PREVIEW_MAX + 1);
        // Truncated body does NOT include the tail.
        assert!(!out.contains(&"x".repeat(300)), "tail must not appear in preview");
    }

    /// The emit functions don't panic on any of the supported
    /// shapes. Catches a regression where a `?` or a missing field
    /// sneaks into the construction.
    #[test]
    fn test_emit_functions_dont_panic() {
        emit_forwarded_outbound(
            "chan-evt-1",
            "did:key:zRuntimeA",
            "prin_alice",
            "chan_abcdefgh",
            "posted",
            r#"{"kind":"posted","text":"hello"}"#,
        );
        emit_received_inbound(
            "chan-evt-1",
            "did:key:zRuntimeA",
            "prin_alice",
            "chan_abcdefgh",
            "posted",
            r#"{"kind":"posted","text":"hello"}"#,
        );
    }
}