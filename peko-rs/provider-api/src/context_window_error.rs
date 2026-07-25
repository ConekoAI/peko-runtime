//! Classifier for `ContextWindowExceeded` HTTP errors.
//!
//! The transport layer stringifies provider errors as
//! `HTTP error <status>: <body>` (see `src/providers/transport/client.rs`
//! in the root crate). Anthropic surfaces 400 `"prompt is too long"` for
//! over-context requests and 413 `"request too large"` for some deployments;
//! OpenAI surfaces 400 `context_length_exceeded`. We match the well-known
//! substrings rather than parsing structured JSON because the body format
//! is provider-specific and unstable, and the loop's recovery (front-evict
//! + retry, F22) doesn't need the exact number — just the
//! "drop oldest and try again" signal.
//!
//! Lifted from `src/providers/transport/client.rs::is_context_window_exceeded`
//! in Phase 9b.N.5b.5 — this is a pure bool helper over `&anyhow::Error`,
//! so it lives in the API crate. The HTTP-status extraction is inlined
//! here (rather than depending on `crate::providers::transport::retry`'s
//! `RetryableError::http_status`) so the API crate stays a thin types
//! layer with no transport dependency.
//!
//! Mirrors the `is_auth_failure` pattern at
//! `src/providers/rotating_auth.rs:117-120`: a pure bool helper over
//! `&anyhow::Error` that reads the error string. No new error enum, no
//! `ProviderError` variant, no breakage of existing call sites.

/// Returns `true` if `e` represents an HTTP 400/413 context-window overflow.
///
/// Mirrors the well-known substring patterns the transport layer has
/// historically matched. When we lift `is_context_window_exceeded` here
/// the matching rules MUST stay byte-identical to the original
/// implementation; F22's eviction loop relies on these substrings to
/// trigger front-eviction + retry.
pub fn is_context_window_exceeded(e: &anyhow::Error) -> bool {
    let msg = e.to_string();

    // Extract the HTTP status (mirrors `RetryableError::http_status`'
    // substring strategy — the transport layer logs `HTTP error NNN: ...`,
    // ` status NNN`, or a bare ` NNN ` in the formatted error).
    let has_400 =
        msg.contains(" 400") || msg.contains("HTTP error 400") || msg.contains("status 400");
    let has_413 =
        msg.contains(" 413") || msg.contains("HTTP error 413") || msg.contains("status 413");

    // 413 is unambiguous (request body too large) — Anthropic's
    // payload-too-large on some deployments.
    if has_413 {
        return true;
    }

    // 400 needs a second-tier substring match — many 400 responses are
    // validation errors, not context overflow.
    if has_400 {
        return msg.contains("prompt is too long")
            || msg.contains("context_length_exceeded")
            || msg.contains("maximum context length")
            || msg.contains("context window");
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_context_window_exceeded_detects_anthropic() {
        let e = anyhow::anyhow!("HTTP error 400: prompt is too long");
        assert!(is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_detects_openai() {
        let e = anyhow::anyhow!("HTTP error 400: context_length_exceeded");
        assert!(is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_detects_anthropic_maximum_context_length() {
        let e = anyhow::anyhow!("HTTP error 400: maximum context length is 200000 tokens");
        assert!(is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_detects_anthropic_context_window_phrase() {
        let e = anyhow::anyhow!("HTTP error 400: request exceeds context window");
        assert!(is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_detects_413() {
        let e = anyhow::anyhow!("HTTP error 413: request body too large");
        assert!(is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_returns_false_for_other_4xx() {
        let e = anyhow::anyhow!("HTTP error 422: validation failed");
        assert!(!is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_returns_false_for_500() {
        let e = anyhow::anyhow!("HTTP error 500: internal server error");
        assert!(!is_context_window_exceeded(&e));
    }

    #[test]
    fn test_is_context_window_exceeded_returns_false_for_non_http_error() {
        let e = anyhow::anyhow!("connection reset");
        assert!(!is_context_window_exceeded(&e));
    }
}
