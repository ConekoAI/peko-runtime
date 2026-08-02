//! Session-group IPC auth (ADR-045 PR #1).
//!
//! The auth table maps Unix process session IDs to authorization entries.
//! When a CLI process connects to the daemon's Unix datagram socket, the
//! kernel exposes the connecting PID via `SO_PEERCRED`; we resolve its
//! session group via `getsid(pid)` and look it up here.
//!
//! ## Why session groups, not PIDs
//!
//! PIDs are recycled and ephemeral. Session IDs are set at `fork()` time
//! by the kernel and inherited by every descendant unless explicitly
//! changed via `setsid()`. All processes spawned from a user terminal
//! share one session ID; `tmux`/`screen` server + client panes share the
//! server's session; a principal spawned by the runtime inherits the
//! runtime's session, which is distinct from any user terminal's.
//!
//! This gives us the structural property the bash-launched escape
//! defense relies on: there is no way for code running inside bash to
//! change its own session ID without `CAP_SYS_ADMIN`.
//!
//! ## Strict-mode flag
//!
//! The `strict` flag (default false in PR #1) controls whether an SID
//! miss is treated as unauthorized. With `strict=false`, the table is
//! consulted opportunistically — a hit grants full caps, a miss falls
//! through to the existing `resolve_caller` path. PR #2 introduces
//! `peko auth` and flips the default to `strict=true`.
//!
//! ## Why this is unix-only
//!
//! Windows named pipes have kernel-enforced DACL peer identity
//! (ADR-038); the auth table is not needed there. UDP is the
//! remote-explicit transport and goes through JWT/API-key auth
//! (ADR-034), not session groups.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Unix process session ID (`getsid(0)` return value).
///
/// On Unix, this is `pid_t` (i32). On non-Unix platforms the type exists
/// for API symmetry but the auth table is never consulted (Windows uses
/// DACL; UDP uses JWT/ApiKey).
pub type SessionId = i32;

/// Default TTL for auth entries. Configurable via AppState.
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);

/// A single authorization entry. Keyed by session ID; values are opaque
/// session tokens compared in constant time at the IPC handshake.
#[derive(Debug, Clone)]
pub(crate) struct AuthEntry {
    /// SHA-256 hash of the session token (never store the raw token).
    pub token_hash: [u8; 32],
    /// When this entry expires.
    pub expires_at: Instant,
    /// Whether the strict-mode flag was set at insertion time
    /// (informational; the table-level flag is authoritative).
    pub source: AuthSource,
}

/// Where an auth entry came from. Used for observability and
/// cache-eviction policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthSource {
    /// Interactive session — `peko auth` from a TTY.
    Interactive,
    /// Service token — long-lived process (runtime, cron, persistent agent).
    Service,
}

/// Concurrent map of session ID → authorization entry.
///
/// Reads happen on every IPC accept (hot path); writes happen on auth
/// submission (cold path). `std::sync::RwLock` is appropriate here —
/// no async, no long-held critical sections.
#[derive(Debug)]
pub(crate) struct AuthTable {
    inner: RwLock<AuthTableInner>,
}

#[derive(Debug, Default)]
struct AuthTableInner {
    entries: HashMap<SessionId, AuthEntry>,
}

impl AuthTable {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(AuthTableInner::default()),
        })
    }

    /// Insert (or replace) an auth entry for the given session.
    pub(crate) fn insert(&self, sid: SessionId, entry: AuthEntry) {
        let mut g = self.inner.write().expect("auth table poisoned");
        g.entries.insert(sid, entry);
    }

    /// Look up an entry by session ID. Returns `None` if missing or
    /// expired (expired entries are evicted on access).
    pub(crate) fn lookup(&self, sid: SessionId) -> Option<AuthEntry> {
        let mut g = self.inner.write().expect("auth table poisoned");
        if let Some(entry) = g.entries.get(&sid) {
            if entry.expires_at > Instant::now() {
                return Some(entry.clone());
            }
            // Expired — evict and return None.
            g.entries.remove(&sid);
        }
        None
    }

    /// Remove an entry explicitly (revoke). Returns true if removed.
    pub(crate) fn revoke(&self, sid: SessionId) -> bool {
        self.inner
            .write()
            .expect("auth table poisoned")
            .entries
            .remove(&sid)
            .is_some()
    }

    /// Number of live (non-expired) entries. O(n) over the map.
    pub(crate) fn len_live(&self) -> usize {
        let now = Instant::now();
        let g = self.inner.read().expect("auth table poisoned");
        g.entries
            .values()
            .filter(|e| e.expires_at > now)
            .count()
    }

    /// Evict all expired entries. Called periodically to bound memory.
    pub(crate) fn evict_expired(&self) -> usize {
        let now = Instant::now();
        let mut g = self.inner.write().expect("auth table poisoned");
        let before = g.entries.len();
        g.entries.retain(|_, e| e.expires_at > now);
        before - g.entries.len()
    }
}

/// Hash a session token with SHA-256. Stored in the table; the raw
/// token is never persisted.
pub(crate) fn hash_token(token: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token);
    let out = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

/// Constant-time compare for two byte slices. Used to verify tokens
/// without leaking timing information.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ttl: Duration, source: AuthSource) -> AuthEntry {
        AuthEntry {
            token_hash: hash_token(b"test-token"),
            expires_at: Instant::now() + ttl,
            source,
        }
    }

    #[test]
    fn insert_and_lookup() {
        let t = AuthTable::new();
        t.insert(1234, entry(DEFAULT_SESSION_TTL, AuthSource::Interactive));
        assert!(t.lookup(1234).is_some());
        assert!(t.lookup(5678).is_none());
    }

    #[test]
    fn lookup_evicts_expired() {
        let t = AuthTable::new();
        t.insert(1234, entry(Duration::from_millis(0), AuthSource::Interactive));
        // Sleep just past expiry.
        std::thread::sleep(Duration::from_millis(5));
        assert!(t.lookup(1234).is_none());
    }

    #[test]
    fn revoke_removes() {
        let t = AuthTable::new();
        t.insert(1234, entry(DEFAULT_SESSION_TTL, AuthSource::Interactive));
        assert!(t.revoke(1234));
        assert!(!t.revoke(1234));
        assert!(t.lookup(1234).is_none());
    }

    #[test]
    fn evict_expired_returns_count() {
        let t = AuthTable::new();
        t.insert(1, entry(Duration::from_millis(0), AuthSource::Interactive));
        t.insert(2, entry(DEFAULT_SESSION_TTL, AuthSource::Interactive));
        std::thread::sleep(Duration::from_millis(5));
        let n = t.evict_expired();
        assert_eq!(n, 1);
        assert_eq!(t.len_live(), 1);
    }

    #[test]
    fn hash_token_is_deterministic_and_distinct() {
        let a = hash_token(b"alpha");
        let b = hash_token(b"alpha");
        let c = hash_token(b"beta");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn ct_eq_matches_and_distinguishes() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd")); // length mismatch
    }
}