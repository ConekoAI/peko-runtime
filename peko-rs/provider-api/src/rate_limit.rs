//! Rate-limit snapshot types and parser trait.
//!
//! Phase F40a / Phase 2A. When a provider responds with a 429 (or a
//! hint that one is imminent), the `HttpClient` asks the adapter's
//! `RateLimitParser` to extract a structured snapshot of the rate-limit
//! state. The snapshot is:
//!
//! 1. Captured into the error message as a parseable token block so
//!    string-based retry sites (`RetryableError::retry_after`) get the
//!    hint without coupling to reqwest types.
//! 2. Held on `HttpClient::last_snapshot()` for callers that want
//!    structured introspection (metrics, dashboards).
//!
//! The parser trait takes `&[(String, String)]` (header key/value
//! pairs) rather than `reqwest::HeaderMap` so `peko_provider_api`
//! stays HTTP-free. The provider-side adapters translate the
//! `reqwest::HeaderMap` into a thin Vec and hand it to the parser.
//!
//! Three concrete parsers ship out of the box:
//! - `OpenAiRateLimitParser` — `x-ratelimit-*` family.
//! - `AnthropicRateLimitParser` — `anthropic-ratelimit-*` + `Retry-After`.
//! - `StandardRateLimitParser` — union of the two (default), since
//!   most providers adopt at least one of the two header families.

use std::time::{Duration, SystemTime};

/// Vendor family of a rate-limit snapshot. Lets callers branch on
/// "OpenAI quota vs Anthropic overloaded" without inspecting the
/// underlying header set.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitKind {
    #[default]
    Unknown,
    OpenAi,
    Anthropic,
    Standard,
}

/// Captured rate-limit state at the moment a 429 (or warning) was
/// observed. Every field is `Option` because vendors disagree on
/// which header keys they emit and we never want to invent values.
///
/// `reset_at` is the upstream-reported absolute time; `retry_after`
/// is the relative delay they suggested (integer seconds or
/// HTTP-date). `requests_remaining` / `tokens_remaining` are the
/// remaining-budget counters — `None` when the vendor doesn't split
/// quota by `requests` vs `tokens`.
///
/// `body_delay` is the body-parsed delay (extracted via
/// `retry_after::extract_body_delay`) used as a last-resort hint when
/// headers are absent and the body mentions a delay inline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateLimitSnapshot {
    pub kind: RateLimitKind,
    pub requests_remaining: Option<u32>,
    pub tokens_remaining: Option<u32>,
    pub requests_limit: Option<u32>,
    pub tokens_limit: Option<u32>,
    pub reset_at: Option<SystemTime>,
    pub retry_after: Option<Duration>,
    pub body_delay: Option<Duration>,
}

impl RateLimitSnapshot {
    /// True when the snapshot suggests a non-zero wait. Used by the
    /// transport to prefer this snapshot's `retry_after` over a
    /// computed exponential-backoff hint.
    #[must_use]
    pub fn effective_retry_after(&self) -> Option<Duration> {
        self.retry_after
            .or(self.body_delay)
            .filter(|d| !d.is_zero())
    }
}

/// One row of a parsed `HeaderMap`-equivalent. The parser takes this
/// rather than `reqwest::HeaderMap` directly because `peko_provider_api`
/// is forbidden from depending on reqwest / hyper (boundary rule: the
/// API crate is HTTP-free).
#[derive(Debug, Clone)]
pub struct HeaderEntry {
    pub name: String,
    pub value: String,
}

impl HeaderEntry {
    /// Build from a `(&str, &str)` pair (the canonical shape returned
    /// by `reqwest::HeaderMap::iter`).
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Extract the value for a header name (case-insensitive). Returns
/// the first matching value when the header appears multiple times
/// (some providers emit duplicate `Set-Cookie` / `Retry-After` keys).
pub fn header_value<'a>(headers: &'a [HeaderEntry], name: &str) -> Option<&'a str> {
    let needle = name.to_ascii_lowercase();
    headers
        .iter()
        .find(|h| h.name.to_ascii_lowercase() == needle)
        .map(|h| h.value.as_str())
}

/// Trait for parsing rate-limit metadata from response headers and
/// body. Adapters implement this to surface vendor-specific
/// conventions; the `ApiAdapter::rate_limit_parser` default returns a
/// `StandardRateLimitParser` so most callers don't need to override.
pub trait RateLimitParser: Send + Sync {
    /// Identifier for parser implementations (used by the snapshot
    /// kind field so callers can route on it).
    fn kind(&self) -> RateLimitKind;

    /// Parse the headers + body of a rate-limited response into a
    /// `RateLimitSnapshot`. Returns `None` if the headers look empty
    /// (e.g. body-only error). Implementations should be tolerant:
    /// missing or malformed fields drop the field, they do NOT
    /// surface a hard error.
    fn parse(&self, headers: &[HeaderEntry], body: &str, now: SystemTime) -> Option<RateLimitSnapshot>;
}

/// OpenAI parser — covers `x-ratelimit-*` family (requests / tokens,
/// remaining / limit, reset-as-seconds). The OpenAI Chat Completions
/// endpoint emits these for both 429s and successful calls, so we
/// can also drive dashboards from the success path.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiRateLimitParser;

impl RateLimitParser for OpenAiRateLimitParser {
    fn kind(&self) -> RateLimitKind {
        RateLimitKind::OpenAi
    }

    fn parse(
        &self,
        headers: &[HeaderEntry],
        body: &str,
        _now: SystemTime,
    ) -> Option<RateLimitSnapshot> {
        let mut snap = RateLimitSnapshot {
            kind: RateLimitKind::OpenAi,
            ..Default::default()
        };
        if let Some(s) = header_value(headers, "x-ratelimit-remaining-requests") {
            snap.requests_remaining = s.parse().ok();
        }
        if let Some(s) = header_value(headers, "x-ratelimit-limit-requests") {
            snap.requests_limit = s.parse().ok();
        }
        if let Some(s) = header_value(headers, "x-ratelimit-remaining-tokens") {
            snap.tokens_remaining = s.parse().ok();
        }
        if let Some(s) = header_value(headers, "x-ratelimit-limit-tokens") {
            snap.tokens_limit = s.parse().ok();
        }
        // `x-ratelimit-reset-*` is seconds-until-reset per OpenAI's
        // spec. Convert to absolute via `now` (callers pass it in).
        // We omit the system-time plumbing for the absolute `reset_at`
        // field — older snapshots still record the original seconds.
        snap.body_delay = super::retry_after::extract_body_delay(body);
        if snap.requests_remaining.is_none()
            && snap.tokens_remaining.is_none()
            && snap.body_delay.is_none()
        {
            return None;
        }
        Some(snap)
    }
}

/// Anthropic parser — covers `anthropic-ratelimit-*` family
/// (Anthropic uses `requests-reset` / `tokens-reset` for absolute
/// second-deltas) plus the standard `Retry-After`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicRateLimitParser;

impl RateLimitParser for AnthropicRateLimitParser {
    fn kind(&self) -> RateLimitKind {
        RateLimitKind::Anthropic
    }

    fn parse(
        &self,
        headers: &[HeaderEntry],
        body: &str,
        now: SystemTime,
    ) -> Option<RateLimitSnapshot> {
        let mut snap = RateLimitSnapshot {
            kind: RateLimitKind::Anthropic,
            ..Default::default()
        };
        if let Some(s) = header_value(headers, "anthropic-ratelimit-requests-remaining") {
            snap.requests_remaining = s.parse().ok();
        }
        if let Some(s) = header_value(headers, "anthropic-ratelimit-tokens-remaining") {
            snap.tokens_remaining = s.parse().ok();
        }
        // Anthropic's reset is a delta-seconds; convert to absolute.
        if let Some(s) = header_value(headers, "anthropic-ratelimit-requests-reset") {
            if let Ok(secs) = s.trim().parse::<u64>() {
                snap.reset_at = now.checked_add(Duration::from_secs(secs));
            }
        } else if let Some(s) = header_value(headers, "anthropic-ratelimit-tokens-reset") {
            if let Ok(secs) = s.trim().parse::<u64>() {
                snap.reset_at = now.checked_add(Duration::from_secs(secs));
            }
        }
        // `Retry-After` — integer seconds or HTTP-date. We only take
        // integer-seconds here to keep the API crate HTTP-free; the
        // integer form is what Anthropic actually emits.
        if let Some(s) = header_value(headers, "retry-after") {
            snap.retry_after = s.trim().parse::<u64>().ok().map(Duration::from_secs);
        }
        snap.body_delay = super::retry_after::extract_body_delay(body);
        if snap.requests_remaining.is_none()
            && snap.tokens_remaining.is_none()
            && snap.reset_at.is_none()
            && snap.retry_after.is_none()
            && snap.body_delay.is_none()
        {
            return None;
        }
        Some(snap)
    }
}

/// Union parser — accepts either family so adapters that proxy one
/// provider to another's wire format (e.g. OpenAI-compatible
/// proxies emitting `anthropic-ratelimit-*`) still get a snapshot.
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardRateLimitParser;

impl RateLimitParser for StandardRateLimitParser {
    fn kind(&self) -> RateLimitKind {
        RateLimitKind::Standard
    }

    fn parse(
        &self,
        headers: &[HeaderEntry],
        body: &str,
        now: SystemTime,
    ) -> Option<RateLimitSnapshot> {
        let openai = OpenAiRateLimitParser.parse(headers, body, now);
        let anthropic = AnthropicRateLimitParser.parse(headers, body, now);
        match (openai, anthropic) {
            (Some(o), Some(a)) => Some(merge_snapshots(o, a)),
            (Some(o), None) => Some(o),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
}

fn merge_snapshots(mut o: RateLimitSnapshot, a: RateLimitSnapshot) -> RateLimitSnapshot {
    // Anthropic usually carries the `Retry-After`; OpenAI usually
    // carries the `tokens_remaining` count. Take whichever is set,
    // preferring Anthropic's `reset_at` over OpenAI's empty one.
    if o.retry_after.is_none() {
        o.retry_after = a.retry_after;
    }
    if o.reset_at.is_none() {
        o.reset_at = a.reset_at;
    }
    if o.body_delay.is_none() {
        o.body_delay = a.body_delay;
    }
    if o.tokens_remaining.is_none() {
        o.tokens_remaining = a.tokens_remaining;
    }
    if o.requests_limit.is_none() {
        o.requests_limit = a.requests_limit;
    }
    o.kind = RateLimitKind::Standard;
    o
}

/// Format a snapshot as the canonical token block embedded in error
/// messages. Round-trips through `parse_snapshot`.
#[must_use]
pub fn format_snapshot(s: &RateLimitSnapshot) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("rate_limit_kind={:?}", s.kind));
    if let Some(n) = s.requests_remaining {
        parts.push(format!("rate_limit_requests_remaining={n}"));
    }
    if let Some(n) = s.tokens_remaining {
        parts.push(format!("rate_limit_tokens_remaining={n}"));
    }
    if let Some(n) = s.requests_limit {
        parts.push(format!("rate_limit_requests_limit={n}"));
    }
    if let Some(n) = s.tokens_limit {
        parts.push(format!("rate_limit_tokens_limit={n}"));
    }
    if let Some(d) = s.retry_after {
        parts.push(format!("rate_limit_retry_after={}s", d.as_secs()));
    }
    if let Some(d) = s.body_delay {
        parts.push(format!("rate_limit_body_delay={}s", d.as_secs()));
    }
    if let Some(t) = s.reset_at {
        if let Ok(dur) = t.duration_since(SystemTime::UNIX_EPOCH) {
            parts.push(format!("rate_limit_reset_at_unix={}", dur.as_secs()));
        }
    }
    parts.join("; ")
}

/// Parse the token block back out of an error message. Round-trips
/// with `format_snapshot`. Returns the empty `Default` snapshot
/// when no recognized fields are present (callers usually
/// short-circuit on that).
#[must_use]
pub fn parse_snapshot_metadata(msg: &str) -> RateLimitSnapshot {
    let mut snap = RateLimitSnapshot::default();
    for token in token_iter(msg) {
        if let Some((k, v)) = token.split_once('=') {
            match k {
                "rate_limit_kind" => {
                    snap.kind = match v {
                        "OpenAi" => RateLimitKind::OpenAi,
                        "Anthropic" => RateLimitKind::Anthropic,
                        "Standard" => RateLimitKind::Standard,
                        _ => RateLimitKind::Unknown,
                    };
                }
                "rate_limit_requests_remaining" => {
                    if let Ok(n) = v.parse() {
                        snap.requests_remaining = Some(n);
                    }
                }
                "rate_limit_tokens_remaining" => {
                    if let Ok(n) = v.parse() {
                        snap.tokens_remaining = Some(n);
                    }
                }
                "rate_limit_requests_limit" => {
                    if let Ok(n) = v.parse() {
                        snap.requests_limit = Some(n);
                    }
                }
                "rate_limit_tokens_limit" => {
                    if let Ok(n) = v.parse() {
                        snap.tokens_limit = Some(n);
                    }
                }
                "rate_limit_retry_after" => {
                    if let Some(secs) = parse_secs_token(v) {
                        snap.retry_after = Some(Duration::from_secs(secs));
                    }
                }
                "rate_limit_body_delay" => {
                    if let Some(secs) = parse_secs_token(v) {
                        snap.body_delay = Some(Duration::from_secs(secs));
                    }
                }
                "rate_limit_reset_at_unix" => {
                    if let Ok(secs) = v.parse::<u64>() {
                        snap.reset_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
                    }
                }
                _ => {}
            }
        }
    }
    snap
}

/// Split a message into key=value tokens. Used by the snapshot parser
/// to lift metadata out of an `anyhow::Error`'s message — the message
/// is shaped like `(rate_limit_kind=Anthropic; rate_limit_remaining=5;
/// ...)`, so the tokenizer splits on anything that isn't valid inside a
/// key or value run (alphanumerics, underscore, digits). This catches
/// `(`, `)`, `[`, `]`, `:`, etc. that legitimately bracket tokens in a
/// formatted message.
fn token_iter(msg: &str) -> impl Iterator<Item = &str> {
    msg.split(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '_' || c == '=' || c == '.' || c == '-')
    })
    .map(str::trim)
    .filter(|t| !t.is_empty() && t.contains('='))
}

/// Accept `Ns` or a bare integer `N` for sec-suffixed tokens. Both
/// `5s` and `5` round-trip here.
fn parse_secs_token(v: &str) -> Option<u64> {
    v.strip_suffix('s')
        .and_then(|s| s.parse().ok())
        .or_else(|| v.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn header(name: &str, value: &str) -> HeaderEntry {
        HeaderEntry::new(name, value)
    }

    #[test]
    fn openai_parser_reads_request_and_token_counters() {
        let h = vec![
            header("x-ratelimit-remaining-requests", "3"),
            header("x-ratelimit-limit-requests", "60"),
            header("x-ratelimit-remaining-tokens", "12000"),
            header("x-ratelimit-limit-tokens", "90000"),
        ];
        let s = OpenAiRateLimitParser
            .parse(&h, "rate limit hit", now())
            .unwrap();
        assert_eq!(s.kind, RateLimitKind::OpenAi);
        assert_eq!(s.requests_remaining, Some(3));
        assert_eq!(s.requests_limit, Some(60));
        assert_eq!(s.tokens_remaining, Some(12000));
        assert_eq!(s.tokens_limit, Some(90000));
    }

    #[test]
    fn openai_parser_extracts_body_delay_when_no_headers() {
        let h: Vec<HeaderEntry> = vec![];
        let s = OpenAiRateLimitParser
            .parse(&h, "Try again in 7 seconds.", now())
            .unwrap();
        assert_eq!(s.kind, RateLimitKind::OpenAi);
        assert_eq!(s.body_delay, Some(Duration::from_secs(7)));
        assert!(s.requests_remaining.is_none());
    }

    #[test]
    fn anthropic_parser_reads_requests_remaining_and_reset_delta() {
        let h = vec![
            header("anthropic-ratelimit-requests-remaining", "5"),
            header("anthropic-ratelimit-requests-reset", "30"),
            header("retry-after", "30"),
        ];
        let s = AnthropicRateLimitParser
            .parse(&h, "rate limit", now())
            .unwrap();
        assert_eq!(s.kind, RateLimitKind::Anthropic);
        assert_eq!(s.requests_remaining, Some(5));
        assert_eq!(s.retry_after, Some(Duration::from_secs(30)));
        // reset delta was 30s from now; assert > now.
        let reset = s.reset_at.unwrap();
        assert!(reset > now());
    }

    #[test]
    fn standard_parser_unions_both_families() {
        // Proxy that emits OpenAI request counter AND anthropic reset.
        let h = vec![
            header("x-ratelimit-remaining-requests", "0"),
            header("anthropic-ratelimit-requests-reset", "20"),
            header("retry-after", "20"),
        ];
        let s = StandardRateLimitParser
            .parse(&h, "rate limit", now())
            .unwrap();
        assert_eq!(s.kind, RateLimitKind::Standard);
        assert_eq!(s.requests_remaining, Some(0));
        assert_eq!(s.retry_after, Some(Duration::from_secs(20)));
        assert!(s.reset_at.unwrap() > now());
    }

    #[test]
    fn standard_parser_returns_none_when_no_signal() {
        let h: Vec<HeaderEntry> = vec![];
        let s = StandardRateLimitParser.parse(&h, "internal error", now());
        assert!(s.is_none());
    }

    #[test]
    fn header_value_is_case_insensitive() {
        let h = vec![header("X-RateLimit-Remaining-Requests", "5")];
        assert_eq!(header_value(&h, "x-ratelimit-remaining-requests"), Some("5"));
        assert_eq!(
            header_value(&h, "X-RATELIMIT-REMAINING-REQUESTS"),
            Some("5")
        );
        assert_eq!(header_value(&h, "x-ratelimit-foo"), None);
    }

    #[test]
    fn effective_retry_after_prefers_header() {
        let mut s = RateLimitSnapshot::default();
        s.retry_after = Some(Duration::from_secs(30));
        s.body_delay = Some(Duration::from_secs(5));
        assert_eq!(s.effective_retry_after(), Some(Duration::from_secs(30)));
        let mut s = RateLimitSnapshot::default();
        s.body_delay = Some(Duration::from_secs(5));
        assert_eq!(s.effective_retry_after(), Some(Duration::from_secs(5)));
        assert_eq!(RateLimitSnapshot::default().effective_retry_after(), None);
        let mut s = RateLimitSnapshot::default();
        s.retry_after = Some(Duration::ZERO);
        assert_eq!(s.effective_retry_after(), None);
    }

    #[test]
    fn snapshot_round_trip_through_format_then_parse() {
        let mut original = RateLimitSnapshot::default();
        original.kind = RateLimitKind::Anthropic;
        original.requests_remaining = Some(7);
        original.tokens_remaining = Some(100);
        original.retry_after = Some(Duration::from_secs(45));
        original.body_delay = Some(Duration::from_secs(2));
        let formatted = format_snapshot(&original);
        let msg = format!("HTTP error 429 ({formatted}): rate limit");
        let parsed = parse_snapshot_metadata(&msg);
        assert_eq!(parsed.kind, original.kind);
        assert_eq!(parsed.requests_remaining, original.requests_remaining);
        assert_eq!(parsed.tokens_remaining, original.tokens_remaining);
        assert_eq!(parsed.retry_after, original.retry_after);
        assert_eq!(parsed.body_delay, original.body_delay);
    }

    #[test]
    fn parse_snapshot_returns_default_when_message_has_no_metadata() {
        let parsed = parse_snapshot_metadata("HTTP error 503: backend down");
        assert_eq!(parsed, RateLimitSnapshot::default());
    }
}
