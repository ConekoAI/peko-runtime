//! Simple word-overlap scoring backend for the synthetic `__tool_search` tool.
//!
//! Lives in `extensions::framework::core::scoring` (not `tools::builtin`)
//! because the framework's `ExtensionCore::list_deferred_tool_definitions`
//! uses it directly, and module-boundary rule 3 forbids
//! `extensions/framework/core/` from importing `tools/builtin`.
//!
//! Peko has <30 built-in tools today; BM25
//! (`codex-rs/core/src/tools/handlers/tool_search.rs:74-94`) is overkill.
//! Word-overlap on a lowercased `name + " " + description` is sufficient
//! for v1. If the catalog ever crosses ~50 entries, swap this for BM25 —
//! the public surface is just [`score`].
//!
//! ## Algorithm
//!
//! 1. Lowercase both query and candidate (`name + " " + description`).
//! 2. Tokenize on whitespace + ASCII punctuation.
//! 3. Build a `HashSet<String>` of query tokens (deduped).
//! 4. Score = sum across unique query tokens of: 100 for a name
//!    substring hit, 1 for a description substring hit, 0 otherwise.
//!    Substring (not equality) catches word fragments ("log" matches
//!    "logging").
//!
//! Stdlib only — no new dependency.

use std::collections::HashSet;

/// Score a candidate tool against the query. Higher = better match.
///
/// `name` and `description` are matched independently so a name hit
/// outranks a description hit at the same token count.
///
/// # Returns
/// Score in `0..=u32::MAX`. Empty queries always return `0`. A token
/// that appears in neither the name nor the description also returns
/// `0`.
#[must_use]
pub fn score(query: &str, name: &str, description: &str) -> u32 {
    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return 0;
    }

    let name_lower = name.to_ascii_lowercase();
    let desc_lower = description.to_ascii_lowercase();
    let unique_query: HashSet<String> = query_tokens.into_iter().collect();

    let mut total = 0u32;
    for token in &unique_query {
        // Name substring hit is worth much more than a description hit
        // so a name match outranks prose match at equal token count.
        if name_lower.contains(token) {
            total += 100;
        } else if desc_lower.contains(token) {
            total += 1;
        }
    }
    total
}

/// Split `text` into lowercased tokens on whitespace + ASCII punctuation.
///
/// Whitespace stripping uses `char::is_whitespace` (Unicode-aware) so the
/// tokenizer handles non-ASCII spaces cleanly. Punctuation stripping uses
/// `char::is_ascii_punctuation` to keep hyphenated tool names like
/// `mcp__docs__search` tokenized as `mcp`, `docs`, `search` (the `__`
/// separators split).
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Sort candidates by descending score, then by name ascending (stable).
///
/// Wraps [`score`] so callers don't reimplement the scoring call. Returns
/// the same input slice on a `Vec::new()` so the caller can `take(limit)`.
#[must_use]
pub fn rank(query: &str, candidates: &[(String, String)]) -> Vec<(String, String, u32)> {
    let mut scored: Vec<_> = candidates
        .iter()
        .map(|(name, description)| {
            let s = score(query, name, description);
            (name.clone(), description.clone(), s)
        })
        .collect();
    // Stable sort: score desc, name asc for tie-break.
    scored.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_empty_query_returns_zero() {
        assert_eq!(score("", "Bash", "execute commands"), 0);
        assert_eq!(score("   ", "Bash", "execute commands"), 0);
        assert_eq!(score("...", "Bash", "execute commands"), 0);
    }

    #[test]
    fn score_exact_name_match_high() {
        // `bash` matches `Bash` in the name. 1 token × 100 = 100.
        let s = score("bash", "Bash", "execute shell commands");
        assert!(s >= 100, "name match should score >= 100, got {s}");
    }

    #[test]
    fn score_description_only_hit_medium() {
        // `execute` matches only in the description. 1 token × 1 = 1.
        let s = score("execute", "ReadFile", "execute shell commands");
        assert_eq!(s, 1, "single description hit should score 1");
    }

    #[test]
    fn score_tie_break_by_name_substring() {
        // Both have one description hit, but the name match wins.
        let name_hit = score("bash", "Bash", "other things");
        let desc_only = score("bash", "Foo", "a bash wrapper");
        assert!(
            name_hit > desc_only,
            "name hit ({name_hit}) must outrank description hit ({desc_only})"
        );
    }

    #[test]
    fn score_case_insensitive() {
        // Query is uppercase, candidate is lowercase — should still match.
        let s = score("BASH", "bash", "execute shell");
        assert_eq!(s, 100, "case-insensitive name match should score 100");
    }

    #[test]
    fn score_handles_unicode_whitespace() {
        // NBSP between tokens should not break tokenization.
        let s = score("bash\u{00a0}execute", "Bash", "execute shell");
        assert_eq!(
            s,
            100 + 1,
            "name hit (100) + description hit (1) = 101, got {s}"
        );
    }

    #[test]
    fn score_zero_for_no_overlap() {
        assert_eq!(score("python", "Bash", "execute shell commands"), 0);
    }

    #[test]
    fn rank_orders_descending_with_name_tiebreak() {
        let candidates = vec![
            ("Foo".to_string(), "a bash wrapper".to_string()),
            ("Bash".to_string(), "execute shell".to_string()),
            ("Bar".to_string(), "no match here".to_string()),
        ];
        let ranked = rank("bash", &candidates);
        assert_eq!(ranked[0].0, "Bash", "Bash (name hit) ranks first");
        assert_eq!(ranked[1].0, "Foo", "Foo (description hit) ranks second");
        assert_eq!(ranked[2].0, "Bar", "Bar (no hit) ranks last");
    }

    #[test]
    fn rank_tiebreak_alphabetical_for_equal_scores() {
        let candidates = vec![
            ("ZTool".to_string(), "executes".to_string()),
            ("ATool".to_string(), "executes".to_string()),
        ];
        let ranked = rank("execute", &candidates);
        assert_eq!(ranked[0].0, "ATool", "alphabetical tiebreak");
        assert_eq!(ranked[1].0, "ZTool");
    }

    #[test]
    fn tokenize_splits_on_underscores() {
        // MCP-style names use `__` separators that should split.
        let toks = tokenize("mcp__docs__search");
        assert_eq!(toks, vec!["mcp", "docs", "search"]);
    }

    #[test]
    fn tokenize_drops_empty_tokens() {
        let toks = tokenize("  ..  a  .. b  ");
        assert_eq!(toks, vec!["a", "b"]);
    }
}
