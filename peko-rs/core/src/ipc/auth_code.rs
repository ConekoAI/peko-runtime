//! Diceware code + session-token generation (ADR-045 PR #2).
//!
//! Two distinct secrets with different lifetimes and purposes:
//!
//! - **diceware code**: 6 words from the EFF short list (~62 bits).
//!   Generated once per daemon startup, single-use, short TTL. Lives
//!   in `~/.peko/run/auth-code` (mode 0600) for the CLI to read.
//! - **session token**: 32 bytes from a CSPRNG, base64url-no-pad
//!   encoded (~43 chars). Generated fresh on every successful
//!   `peko auth submit`. Persisted by the CLI at
//!   `~/.peko/run/auth-token-<sid>` (mode 0600); daemon stores only
//!   the SHA-256 hash in memory.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;

use super::auth_wordlist::wordlist;

/// Number of words in a startup diceware code.
pub const DICEWARE_WORD_COUNT: usize = 6;

/// Separator between words in the diceware code.
pub const DICEWARE_SEPARATOR: char = '-';

/// Number of random bytes in a session token.
pub const SESSION_TOKEN_BYTES: usize = 32;

/// Generate a diceware code by sampling `words` entries uniformly
/// from the EFF short wordlist.
///
/// `rng` must be a CSPRNG (`OsRng` in production). The returned code
/// is lower-case, single-space-free (uses `-` separators), and
/// `~62 bits * words / 6` of entropy for the default word count.
pub fn generate_auth_code<R: RngCore>(rng: &mut R, words: usize) -> String {
    debug_assert!(words > 0, "code must contain at least one word");
    debug_assert!(
        words <= 16,
        "code word count >16 would exceed typical UX budgets"
    );

    let mut indices = [0u32; 16];
    for slot in indices.iter_mut() {
        *slot = rng.next_u32();
    }

    indices
        .iter()
        .take(words)
        .map(|raw| {
            // Bias-free reduction modulo 1296. `u32::MAX % 1296` is
            // small enough that the bias is negligible for our
            // purposes (we are not in an adversarial cryptographic
            // setting; the code is single-use and short-TTL).
            let idx = (*raw as usize) % wordlist().len();
            wordlist()[idx]
        })
        .collect::<Vec<&str>>()
        .join(&DICEWARE_SEPARATOR.to_string())
}

/// Generate a fresh session token: 32 bytes from `rng`, encoded as
/// URL-safe base64 without padding (matches the format used by
/// `peko-rs/auth/src/api_key.rs`).
pub fn generate_session_token<R: RngCore>(rng: &mut R) -> String {
    let mut bytes = [0u8; SESSION_TOKEN_BYTES];
    rng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Normalize a user-entered diceware code for comparison.
///
/// - Trims leading/trailing ASCII whitespace.
/// - Lowercases.
/// - Collapses internal whitespace (tabs, multiple spaces) into a
///   single `-` separator.
///
/// We deliberately do NOT do fuzzy matching (no Levenshtein, no
/// prefix matching, no autocorrect). The user types the exact words;
/// if they mistype, the daemon returns `[invalid_auth_code]` and the
/// user retries with a fresh code (the daemon invalidates the code
/// after several wrong attempts).
pub fn normalize_code(raw: &str) -> String {
    raw.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(&DICEWARE_SEPARATOR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn generate_auth_code_uses_separator_and_word_count() {
        let mut rng = StdRng::seed_from_u64(0xC0FFEE);
        let code = generate_auth_code(&mut rng, 6);
        let parts: Vec<&str> = code.split(DICEWARE_SEPARATOR).collect();
        assert_eq!(parts.len(), 6);
        for w in &parts {
            assert!(
                wordlist().contains(w),
                "{w} is not in the EFF short wordlist"
            );
        }
    }

    #[test]
    fn generate_auth_code_is_deterministic_for_same_seed() {
        let mut rng1 = StdRng::seed_from_u64(42);
        let mut rng2 = StdRng::seed_from_u64(42);
        assert_eq!(generate_auth_code(&mut rng1, 6), generate_auth_code(&mut rng2, 6));
    }

    #[test]
    fn generate_auth_code_varies_across_seeds() {
        let mut rng1 = StdRng::seed_from_u64(1);
        let mut rng2 = StdRng::seed_from_u64(2);
        // With 6 words from a 1296-word list, collision probability
        // is ~10^-19; safe to assert inequality.
        assert_ne!(generate_auth_code(&mut rng1, 6), generate_auth_code(&mut rng2, 6));
    }

    #[test]
    fn generate_session_token_decodes_to_32_bytes() {
        let mut rng = StdRng::seed_from_u64(7);
        let token = generate_session_token(&mut rng);
        let decoded = URL_SAFE_NO_PAD.decode(&token).expect("base64 decode");
        assert_eq!(decoded.len(), SESSION_TOKEN_BYTES);
    }

    #[test]
    fn generate_session_token_varies_across_calls() {
        let mut rng = StdRng::seed_from_u64(0);
        let t1 = generate_session_token(&mut rng);
        let t2 = generate_session_token(&mut rng);
        assert_ne!(t1, t2);
    }

    #[test]
    fn normalize_code_strips_whitespace_and_lowercases() {
        assert_eq!(normalize_code("  Alpha Bridge CLOUD "), "alpha-bridge-cloud");
        assert_eq!(
            normalize_code("alpha\tbridge\tcloud"),
            "alpha-bridge-cloud"
        );
        assert_eq!(
            normalize_code("ALPHA  bridge   cloud"),
            "alpha-bridge-cloud"
        );
    }

    #[test]
    fn normalize_code_empty_input_returns_empty() {
        assert_eq!(normalize_code(""), "");
        assert_eq!(normalize_code("   \t  "), "");
    }
}