//! OpenAI `prompt_cache_key` helper
//!
//! OpenAI caps `prompt_cache_key` at 64 characters, counted by UTF-32
//! codepoints. This module provides the canonical clamp that the
//! engine loop uses when wiring `ChatOptions::prompt_cache_key` for
//! the OpenAI Chat Completions adapter. Mirrors pi's
//! `packages/ai/src/api/openai-prompt-cache.ts::clampOpenAIPromptCacheKey`.

/// Truncate a session id to OpenAI's 64-UTF-32-char limit.
///
/// If the input is already at or below the limit, the slice is
/// returned unchanged. Otherwise the first `64` codepoints are kept,
/// using `char_indices` to land on a UTF-8 boundary (so a multi-byte
/// character is never split).
///
/// # Examples
///
/// ```
/// use crate::providers::openai_prompt_cache::clamp_openai_prompt_cache_key;
///
/// assert_eq!(clamp_openai_prompt_cache_key("abc"), "abc");
/// let long = "a".repeat(80);
/// assert_eq!(clamp_openai_prompt_cache_key(&long).len(), 64);
/// ```
pub fn clamp_openai_prompt_cache_key(session_id: &str) -> String {
    if session_id.chars().count() <= 64 {
        return session_id.to_string();
    }

    let mut out = String::with_capacity(64 * 4); // worst case: 4 bytes per codepoint
    let mut count = 0usize;
    for ch in session_id.chars() {
        if count == 64 {
            break;
        }
        out.push(ch);
        count += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clamp_returns_input_when_short_enough() {
        assert_eq!(clamp_openai_prompt_cache_key("abc-123"), "abc-123");
        assert_eq!(clamp_openai_prompt_cache_key(""), "");
    }

    #[test]
    fn test_clamp_returns_input_at_exactly_64_chars() {
        let exact = "a".repeat(64);
        assert_eq!(clamp_openai_prompt_cache_key(&exact), exact);
    }

    #[test]
    fn test_clamp_truncates_to_64_utf32_chars() {
        let long = "a".repeat(80);
        assert_eq!(clamp_openai_prompt_cache_key(&long).chars().count(), 64);
    }

    #[test]
    fn test_clamp_does_not_split_multi_byte_characters() {
        // 80 codepoints, all multi-byte (`é` = 2 UTF-8 bytes).
        let long: String = std::iter::repeat('é').take(80).collect();
        let clamped = clamp_openai_prompt_cache_key(&long);
        assert_eq!(clamped.chars().count(), 64);
        // Every retained codepoint is `é` — none were split mid-codepoint.
        assert!(clamped.chars().all(|c| c == 'é'));
    }

    #[test]
    fn test_clamp_preserves_beyond_64_byte_length() {
        // 80 ASCII chars (80 bytes).
        let long = "a".repeat(80);
        let clamped = clamp_openai_prompt_cache_key(&long);
        assert_eq!(clamped.len(), 64); // 64 bytes, 64 chars
        assert!(clamped.chars().all(|c| c == 'a'));
    }
}
