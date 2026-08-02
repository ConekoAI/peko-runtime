//! Daemon-side bootstrap-code state (ADR-045 PR #2).
//!
//! The diceware code is generated once at daemon startup, written to
//! `~/.peko/run/auth-code` (mode 0600), and printed to stderr. The
//! CLI's first `peko auth submit` reads the file (or re-types the
//! code) and sends it via `RequestPacket::AuthSubmit`.
//!
//! ## Security model
//!
//! - **Single-use**: the code is consumed on first successful submit
//!   (the `consumed: AtomicBool` flips to true). A second submit
//!   from any SID returns `AuthCodeError::AlreadyConsumed`.
//! - **Short TTL**: `ttl` defaults to 10 minutes. After expiry the
//!   code is invalid even if it has never been used; the user must
//!   restart the daemon to get a new code.
//! - **Attempt budget**: `max_attempts` defaults to 5 globally. Each
//!   failed submit (wrong code, malformed input) increments
//!   `attempts`; exceeding the budget returns
//!   `AuthCodeError::Exhausted` and the daemon is effectively
//!   unreachable until restart.
//! - **Constant-time comparison**: `verify_and_consume` hashes both
//!   the stored and supplied codes with SHA-256 and compares the
//!   32-byte digests via the existing `ct_eq` primitive — no
//!   length-based or byte-by-byte early-exit short circuits.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use super::auth::{ct_eq, hash_token};
use super::auth_code::normalize_code;

/// Default TTL for the startup diceware code (10 minutes).
pub const DEFAULT_CODE_TTL: Duration = Duration::from_secs(10 * 60);

/// Default maximum failed-attempt budget. After this many wrong
/// submits (across all SIDs) the daemon refuses further submits until
/// restart.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 5;

/// Errors that `verify_and_consume` can return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthCodeError {
    /// Code has been used successfully once; further submits fail.
    AlreadyConsumed,
    /// TTL elapsed without a successful submit.
    Expired,
    /// Failed-attempt budget exceeded.
    Exhausted,
    /// Supplied code did not match the stored hash.
    Mismatch,
    /// Supplied input was empty after normalization.
    Empty,
}

/// Daemon-side state for the startup diceware code.
///
/// Stored as `Arc<AuthCodeState>` in `AppState`; the daemon holds
/// only the SHA-256 hash of the raw code (never the code itself in
/// long-lived memory — it does briefly live in `Daemon::run` until
/// the code is emitted and hashed, then dropped).
pub struct AuthCodeState {
    /// SHA-256 hash of the normalized startup code.
    hashed_code: [u8; 32],
    /// When the code was generated.
    created_at: Instant,
    /// How long the code stays valid.
    ttl: Duration,
    /// Maximum failed-attempt budget.
    max_attempts: u32,
    /// Current attempt count (incremented before each comparison).
    attempts: AtomicU32,
    /// Single-use flag — flips to true on successful submit.
    consumed: AtomicBool,
}

impl AuthCodeState {
    /// Build a new state holder from the raw startup code.
    ///
    /// Normalizes the code the same way the CLI's submitted code is
    /// normalized, then hashes it. The raw code is dropped after
    /// this function returns.
    pub fn from_raw_code(raw_code: &str, ttl: Duration, max_attempts: u32) -> Self {
        let normalized = normalize_code(raw_code);
        let hashed_code = hash_token(normalized.as_bytes());
        Self {
            hashed_code,
            created_at: Instant::now(),
            ttl,
            max_attempts,
            attempts: AtomicU32::new(0),
            consumed: AtomicBool::new(false),
        }
    }

    /// Verify a supplied code and (on success) consume it.
    ///
    /// Returns `Ok(())` on first successful match; any subsequent
    /// call returns `AuthCodeError::AlreadyConsumed`. Wrong codes
    /// increment the attempt counter and return `Mismatch` (or
    /// `Exhausted` once the budget is exceeded). Empty input is
    /// rejected with `Empty` before touching the attempt budget.
    pub fn verify_and_consume(&self, raw_supplied: &str) -> Result<(), AuthCodeError> {
        if self.consumed.load(Ordering::Acquire) {
            return Err(AuthCodeError::AlreadyConsumed);
        }
        if self.created_at.elapsed() > self.ttl {
            return Err(AuthCodeError::Expired);
        }

        let normalized = normalize_code(raw_supplied);
        if normalized.is_empty() {
            return Err(AuthCodeError::Empty);
        }

        let attempts_so_far = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if attempts_so_far > self.max_attempts {
            return Err(AuthCodeError::Exhausted);
        }

        let supplied_hash = hash_token(normalized.as_bytes());
        if !ct_eq(&supplied_hash, &self.hashed_code) {
            return Err(AuthCodeError::Mismatch);
        }

        // Success — mark consumed. A concurrent winner of the CAS
        // race still wins; the loser observes AlreadyConsumed on
        // the next call.
        let was_first = !self.consumed.swap(true, Ordering::AcqRel);
        if was_first {
            Ok(())
        } else {
            Err(AuthCodeError::AlreadyConsumed)
        }
    }

    /// Returns true if the code has already been successfully
    /// submitted. Useful for the daemon to delete the
    /// `~/.peko/run/auth-code` file after first use.
    pub fn is_consumed(&self) -> bool {
        self.consumed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh(code: &str) -> AuthCodeState {
        AuthCodeState::from_raw_code(code, DEFAULT_CODE_TTL, DEFAULT_MAX_ATTEMPTS)
    }

    #[test]
    fn correct_code_consumes() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert!(s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest").is_ok());
        assert!(s.is_consumed());
    }

    #[test]
    fn whitespace_and_case_normalize() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert!(
            s.verify_and_consume("  ALPHA  bridge\tCLOUD drift EAGLE forest ")
                .is_ok()
        );
    }

    #[test]
    fn second_submit_returns_already_consumed() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert!(s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest").is_ok());
        assert_eq!(
            s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest"),
            Err(AuthCodeError::AlreadyConsumed)
        );
    }

    #[test]
    fn wrong_code_returns_mismatch_and_increments_attempts() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert_eq!(
            s.verify_and_consume("zebra-bridge-cloud-drift-eagle-forest"),
            Err(AuthCodeError::Mismatch)
        );
        // Two more wrong codes → attempt counter at 3, still within budget.
        let _ = s.verify_and_consume("alpha-bridge-cloud-drift-eagle-ZEBRA");
        let _ = s.verify_and_consume("alpha-bridge-cloud-drift-eagle-tiger");
        // The 4th attempt is the last allowed one (budget 5).
        // The 5th attempt succeeds if correct.
        assert!(s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest").is_ok());
    }

    #[test]
    fn exhausts_after_max_attempts() {
        let s = AuthCodeState::from_raw_code(
            "alpha-bridge-cloud-drift-eagle-forest",
            DEFAULT_CODE_TTL,
            3,
        );
        assert_eq!(
            s.verify_and_consume("wrong-1-bridge-cloud-drift-eagle"),
            Err(AuthCodeError::Mismatch)
        );
        assert_eq!(
            s.verify_and_consume("wrong-2-bridge-cloud-drift-eagle"),
            Err(AuthCodeError::Mismatch)
        );
        // 3rd attempt — counter goes to 3, which equals max_attempts.
        // The check is `attempts_so_far > max_attempts`, so this one
        // IS allowed but returns mismatch.
        assert_eq!(
            s.verify_and_consume("wrong-3-bridge-cloud-drift-eagle"),
            Err(AuthCodeError::Mismatch)
        );
        // 4th attempt → Exhausted.
        assert_eq!(
            s.verify_and_consume("wrong-4-bridge-cloud-drift-eagle"),
            Err(AuthCodeError::Exhausted)
        );
        // Even the correct code is refused now.
        assert_eq!(
            s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest"),
            Err(AuthCodeError::Exhausted)
        );
    }

    #[test]
    fn empty_input_returns_empty() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert_eq!(s.verify_and_consume(""), Err(AuthCodeError::Empty));
        assert_eq!(s.verify_and_consume("   \t  "), Err(AuthCodeError::Empty));
    }

    #[test]
    fn expired_returns_expired() {
        let s = AuthCodeState::from_raw_code(
            "alpha-bridge-cloud-drift-eagle-forest",
            Duration::from_millis(0),
            DEFAULT_MAX_ATTEMPTS,
        );
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(
            s.verify_and_consume("alpha-bridge-cloud-drift-eagle-forest"),
            Err(AuthCodeError::Expired)
        );
    }

    #[test]
    fn is_consumed_initially_false() {
        let s = fresh("alpha-bridge-cloud-drift-eagle-forest");
        assert!(!s.is_consumed());
    }
}