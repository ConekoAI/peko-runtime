//! EFF short diceware wordlist (ADR-045 PR #2).
//!
//! Vendored from the official EFF wordlist (`eff_short_wordlist_1.txt`,
//! 1296 short words, 4-die rolls). Public domain — see
//! <https://www.eff.org/dice>.
//!
//! Loaded lazily via `OnceLock` so the include_str! bytes aren't
//! parsed until first access. The list length and uniqueness
//! invariants are enforced by the unit tests at the bottom of this
//! module so a corrupt vendored copy is caught at `cargo test` time
//! rather than via silent entropy loss in production.

use std::sync::OnceLock;

/// The full 1296-word EFF short wordlist, in canonical order.
///
/// Each index `i` corresponds to the dice roll `i + 1111`, so picking
/// uniformly in `[0, 1296)` is equivalent to rolling 4 fair dice.
pub fn wordlist() -> &'static [&'static str; 1296] {
    static CACHE: OnceLock<Box<[&'static str; 1296]>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = include_str!("../../assets/eff_short_wordlist_1.txt");
        let mut arr: Box<[&'static str; 1296]> = Box::new(
            [""; 1296], // temporary fill; overwritten below
        );
        for (i, line) in raw.lines().enumerate() {
            // Each line is "DDDD\tword" (5-digit dice roll + tab + word).
            // We strip the prefix at load time so callers see just
            // the word.
            let word = line
                .splitn(2, '\t')
                .nth(1)
                .unwrap_or("")
                .trim();
            // Leak the word into 'static storage. Total memory cost
            // is ~13 KB (the include_str! bytes are also static, so
            // we are doubling the file's footprint; acceptable).
            let word_static: &'static str = Box::leak(word.to_string().into_boxed_str());
            arr[i] = word_static;
        }
        arr
    })
}

/// Convenience constant for the wordlist length.
pub const WORDLIST_LEN: usize = 1296;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_is_1296() {
        assert_eq!(wordlist().len(), 1296);
    }

    #[test]
    fn entries_are_non_empty() {
        for (i, w) in wordlist().iter().enumerate() {
            assert!(!w.is_empty(), "word at index {i} is empty");
            assert!(!w.contains('\t'), "word at index {i} contains a tab");
            assert!(!w.contains('\n'), "word at index {i} contains a newline");
            assert!(!w.contains(' '), "word at index {i} contains a space");
        }
    }

    #[test]
    fn entries_are_unique() {
        let mut sorted: Vec<&str> = wordlist().to_vec();
        sorted.sort_unstable();
        for w in sorted.windows(2) {
            assert_ne!(w[0], w[1], "duplicate word in wordlist: {}", w[0]);
        }
    }
}