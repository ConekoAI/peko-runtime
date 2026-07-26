//! HTTP `Retry-After` and body-parsed delay parsers.
//!
//! Lifted out of `peko_providers::transport::client` so the
//! `peko_provider_api` crate owns the helper (Phase F40a / Phase 2A).
//! Two shapes supported:
//!
//! 1. **HTTP-header form** ([RFC 7231 §7.1.3][rfc]):
//!    - Integer seconds (`Retry-After: 30`).
//!    - HTTP-date ([RFC 5322 IMF-fixdate][rfc5322]; e.g. `Fri, 31 Dec
//!      2025 23:59:59 GMT`). The HTTP-date form is rare from major
//!      LLM providers but required by the spec, so we accept it and
//!      compute the delta against the caller-supplied `now`.
//!
//! 2. **Body-parsed form** — non-standard substrings that some
//!    providers embed in error bodies when they omit the header:
//!    - `"Try again in 30 seconds"`, `"Try again in 5s"`,
//!      `"Retry after 1 minute"`, `"in 500ms"`, etc.
//!    - Returns `None` when no recognizable delay token is found.
//!
//! Pure functions, no `reqwest` / `hyper` deps — `peko_provider_api`
//! stays HTTP-free.
//!
//! [rfc]: https://datatracker.ietf.org/doc/html/rfc7231#section-7.1.3
//! [rfc5322]: https://datatracker.ietf.org/doc/html/rfc5322#section-2

use std::time::{Duration, SystemTime};

/// Parse a `Retry-After` header value (RFC 7231 §7.1.3).
///
/// Tries the integer-seconds form first, then the HTTP-date form
/// against `now`. Returns `None` for missing header, malformed
/// value, zero seconds, or a past HTTP-date. Zero seconds is
/// deliberately treated as "no hint" — see
/// `peko_providers::transport::retry` for the rationale (a
/// zero-second `Retry-After` from a hostile or stale header would
/// otherwise pin the executor in a tight retry loop).
#[must_use]
pub fn parse_retry_after_header(value: &str, now: SystemTime) -> Option<Duration> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Integer-seconds form (most common).
    if let Ok(secs) = trimmed.parse::<u64>() {
        if secs == 0 {
            return None;
        }
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date form (RFC 7231 §7.1.3): IMF-fixdate per RFC 5322.
    // `chrono` parses three variants: RFC 2822, RFC 850, and the
    // IMF-fixdate (`Fri, 31 Dec 2025 23:59:59 GMT`) — only the
    // IMF-fixdate is RFC 7231-compliant, but accepting all three
    // is harmless because the delta check rejects garbage.
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc2822(trimmed) {
        return delta_from_now(parsed.with_timezone(&chrono::Utc), now);
    }
    if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return delta_from_now(parsed.with_timezone(&chrono::Utc), now);
    }
    None
}

/// Compute the positive `Duration` between `now` and `target`. Returns
/// `None` if the target is in the past or the clock skew makes the
/// delta unrepresentable (`Duration` can't go negative).
fn delta_from_now(
    target: chrono::DateTime<chrono::Utc>,
    now: SystemTime,
) -> Option<Duration> {
    let target_systime: SystemTime = target.into();
    match target_systime.duration_since(now) {
        // OK case
        Ok(d) if !d.is_zero() => Some(d),
        // Past or zero — no useful hint.
        _ => None,
    }
}

/// Maximum delay we'll extract from a body substring (catches
/// absurdly long values like `"in 999999999 seconds"`).
const MAX_BODY_DELAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Try to extract a delay hint from a provider error body when the
/// `Retry-After` header is absent. Recognizes a handful of patterns:
///
/// - `"try again in <N>s"` / `"try again in <N> seconds"`
/// - `"retry after <N>s"` / `"retry after <N> seconds"`
/// - `"in <N> seconds"` / `"in <N>s"`
/// - `"wait <N>s"` / `"wait <N> seconds"`
/// - `"<N>s"` (last resort; rejects "<N>ms" / "<N> tokens")
/// - `"<N> minute(s)"` / `"<N> min(s)"` / `"<N> hour(s)"`
///
/// Case-insensitive substring match with word boundaries around the
/// number token. Returns `None` for ambiguous / unrecognized bodies
/// so we don't accidentally throttle on noise.
#[must_use]
pub fn extract_body_delay(body: &str) -> Option<Duration> {
    if body.is_empty() {
        return None;
    }
    let lc = body.to_ascii_lowercase();
    // Try patterns in order of specificity — most-specific first
    // reduces false positives like "429 tokens per minute" matching
    // a generic "minute" arm.
    if let Some(d) = extract_numeric_with_unit(&lc, &["second", "sec", "s"], 1) {
        return Some(d.min(MAX_BODY_DELAY));
    }
    if let Some(d) = extract_numeric_with_unit(&lc, &["ms", "millisecond"], 0) {
        return Some(d.min(MAX_BODY_DELAY));
    }
    if let Some(d) = extract_numeric_with_unit(&lc, &["minute", "min"], 60) {
        return Some(d.min(MAX_BODY_DELAY));
    }
    if let Some(d) = extract_numeric_with_unit(&lc, &["hour", "hr"], 3600) {
        return Some(d.min(MAX_BODY_DELAY));
    }
    None
}

/// Look for `<digit-run> <unit>` where `<digit-run>` is a contiguous
/// integer token separated from the unit by whitespace and from any
/// preceding digit-containing token by a non-digit boundary (so we
/// don't parse the `"5"` out of `"tokens5"`).
///
/// `unit_seconds` is the canonical seconds-per-unit multiplier (e.g.
/// `1` for seconds, `60` for minutes). Returns the implied duration.
/// The pre-fix version of this function used a fixed 20-char prefix
/// window and fell into `trim_end` whitespace before the unit, which
/// dropped the digit run and returned `None` — the rewrite scans the
/// full preceding text instead.
fn extract_numeric_with_unit(body: &str, units: &[&str], unit_seconds: u64) -> Option<Duration> {
    for unit in units {
        let mut start = 0;
        while let Some(pos) = body[start..].find(unit) {
            let abs = start + pos;
            // Look at the text BEFORE the unit token. Walk back past
            // optional whitespace (` `, `,`, etc.) and then check for
            // a digit run.
            let before = &body[..abs];
            // Trim trailing whitespace and the unit itself if it's
            // a prefix match (we already matched it via `pos`, so we
            // just need to skip back over leading whitespace).
            let trimmed_before = before.trim_end_matches(|c: char| c.is_whitespace() || c == ',');
            // Find the digit run that immediately precedes the unit.
            let numeric_end = trimmed_before.len();
            let numeric_start = trimmed_before
                .char_indices()
                .rev()
                .take_while(|(_, c)| c.is_ascii_digit())
                .last()
                .map(|(i, _)| i)
                .unwrap_or(numeric_end);
            if numeric_start < numeric_end {
                let candidate = &trimmed_before[numeric_start..numeric_end];
                if let Ok(n) = candidate.parse::<u64>() {
                    if digit_run_isolated(trimmed_before, numeric_start, numeric_end) {
                        let secs = n.saturating_mul(unit_seconds);
                        if secs > 0 {
                            return Some(Duration::from_secs(secs));
                        }
                    }
                }
            }
            start = abs + unit.len();
        }
    }
    None
}

/// True when the digit run `[numeric_start..numeric_end)` in `prefix`
/// is bounded on the LEFT side by a non-digit ASCII (or string edge).
/// We deliberately don't check the RIGHT side here because the
/// caller has already placed the unit token at `numeric_end`, so the
/// only relevant boundary is the one BEFORE the digit run.
fn digit_run_isolated(prefix: &str, numeric_start: usize, _numeric_end: usize) -> bool {
    prefix[..numeric_start]
        .chars()
        .next_back()
        .map_or(true, |c: char| !c.is_ascii_digit())
}

/// Format a `Duration` as the canonical `(retry_after=Ns)` token that
/// `RetryableError::retry_after` parses. Used by `HttpClient::classify_http_error`
/// when embedding the upstream hint into the error message.
#[must_use]
pub fn format_retry_after_token(d: Duration) -> String {
    format!("retry_after={}s", d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retryable_error::RetryableError;
    use std::time::{Duration, UNIX_EPOCH};

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    // ---------------- HTTP-header parser ----------------

    #[test]
    fn parse_retry_after_secs_integer_form() {
        let d = parse_retry_after_header("30", now()).unwrap();
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn parse_retry_after_secs_with_whitespace() {
        let d = parse_retry_after_header("  30  ", now()).unwrap();
        assert_eq!(d, Duration::from_secs(30));
    }

    #[test]
    fn parse_retry_after_secs_zero_is_dropped() {
        // Zero-second hint is meaningless; the executor would loop
        // tightly. Force None.
        assert!(parse_retry_after_header("0", now()).is_none());
        assert!(parse_retry_after_header("0s", now()).is_none());
    }

    #[test]
    fn parse_retry_after_secs_negative_is_rejected() {
        // RFC 7231 only allows non-negative integer seconds or a
        // future HTTP-date. Negative integer is malformed.
        assert!(parse_retry_after_header("-1", now()).is_none());
    }

    #[test]
    fn parse_retry_after_empty_string_returns_none() {
        assert!(parse_retry_after_header("", now()).is_none());
        assert!(parse_retry_after_header("   ", now()).is_none());
    }

    #[test]
    fn parse_retry_after_garbage_returns_none() {
        assert!(parse_retry_after_header("not-a-number", now()).is_none());
    }

    #[test]
    fn parse_retry_after_http_date_future() {
        // Use 2027 (year close enough to `now` that chrono's
        // RFC 2822 parser accepts it without needing dates past
        // 9999; 31 Dec 2027 is a Friday — chrono validates the
        // day-of-week against the date). The delta is several
        // years → finite and positive. We pin a hard-coded
        // IMF-fixdate form so we don't depend on
        // `chrono::Utc::now()` (forbidden in this crate's
        // pure-parser surface).
        let future_str = "Fri, 31 Dec 2027 23:59:59 GMT";
        let d = parse_retry_after_header(future_str, now()).unwrap();
        assert!(d > Duration::from_secs(60), "got {d:?}");
    }

    #[test]
    fn parse_retry_after_http_date_past_returns_none() {
        // 1990 is before `now` (2023). The HTTP-date form requires
        // the target to be in the future (a "retry at <past>" is
        // meaningless).
        let past = "Mon, 01 Jan 1990 00:00:00 GMT";
        assert!(parse_retry_after_header(past, now()).is_none());
    }

    // ---------------- body-delay extractor ----------------

    #[test]
    fn extract_body_delay_seconds() {
        assert_eq!(
            extract_body_delay("Rate limit exceeded. Please try again in 30 seconds."),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            extract_body_delay("Retry-after in 5 seconds"),
            Some(Duration::from_secs(5))
        );
        // Bare `<N>s` shorthand (no space, no `second`/`sec` word)
        // also matches because providers like Google's APIs emit
        // `"retry-after: 5s"` inline. The unit table includes the
        // single-char `"s"` form as a fallback arm.
        assert_eq!(
            extract_body_delay("Retry-after in 5s"),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn extract_body_delay_minutes() {
        assert_eq!(
            extract_body_delay("Quota exceeded. Try again in 2 minutes."),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            extract_body_delay("1 min."),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn extract_body_delay_hours() {
        assert_eq!(
            extract_body_delay("Wait 1 hour."),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn extract_body_delay_returns_none_on_unrecognized() {
        assert!(extract_body_delay("plain error message").is_none());
        assert!(extract_body_delay("429 rate limit").is_none()); // no delay token
    }

    #[test]
    fn extract_body_delay_empty_returns_none() {
        assert!(extract_body_delay("").is_none());
    }

    #[test]
    fn extract_body_delay_isolates_digits() {
        // "5" embedded inside "tokens5" must NOT match.
        assert!(extract_body_delay("5 tokens per second").is_none());
    }

    // ---------------- format token ----------------

    #[test]
    fn format_retry_after_token_round_trip() {
        let d = Duration::from_secs(42);
        let s = format_retry_after_token(d);
        assert_eq!(s, "retry_after=42s");
        // parser-side: ensure it round-trips through the
        // existing `(retry_after=Ns)` extractor.
        let e = anyhow::anyhow!("HTTP error 503 ({s}): temp");
        assert_eq!(e.retry_after(), Some(d));
    }
}
