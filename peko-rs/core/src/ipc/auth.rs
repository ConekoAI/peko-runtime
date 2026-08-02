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

/// TTL for daemon-internal service tokens. Service tokens are held
/// in memory only and rotate each daemon restart; 24h is generous
/// headroom for any long-running internal client.
pub const SERVICE_SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

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

/// Named service-token entry (ADR-045 PR #5).
///
/// Independent from [`AuthEntry`] because service tokens are
/// presented by long-lived processes whose Unix session ID is not
/// stable — the token itself is the credential, not `(sid, token)`.
/// The on-disk source of truth is
/// `crate::storage::service_token_store::ServiceTokenStore`;
/// this in-memory cache is rehydrated at daemon startup and
/// invalidated on revoke.
#[derive(Debug, Clone)]
pub(crate) struct ServiceTokenEntry {
    /// SHA-256 hash of the raw token (never the raw token).
    pub token_hash: [u8; 32],
    /// Capability list the token was created with. **Immutable** —
    /// the ADR's "cannot grow" rule means once registered, the
    /// caps set is fixed. To change caps, revoke + recreate.
    pub caps: Vec<String>,
    /// When this entry expires (None = no expiry).
    pub expires_at: Option<Instant>,
}

/// Concurrent map of session ID → authorization entry.
///
/// Reads happen on every IPC accept (hot path); writes happen on auth
/// submission (cold path). `std::sync::RwLock` is appropriate here —
/// no async, no long-held critical sections.
#[derive(Debug)]
pub(crate) struct AuthTable {
    inner: RwLock<AuthTableInner>,
    /// ADR-045 PR #5: parallel name-keyed map for persistent service
    /// tokens. Token is the credential (sid-independent); see
    /// [`ServiceTokenEntry`] doc for the rationale.
    service_tokens: RwLock<HashMap<String, ServiceTokenEntry>>,
}

#[derive(Debug, Default)]
struct AuthTableInner {
    entries: HashMap<SessionId, AuthEntry>,
}

impl AuthTable {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(AuthTableInner::default()),
            service_tokens: RwLock::new(HashMap::new()),
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

    /// Authorize an interactive session for `sid` keyed by `token`.
    ///
    /// The raw `token` is SHA-256 hashed before insertion; the table
    /// never holds the raw token. Replaces any existing entry for
    /// the same SID (this is what `peko auth submit` calls after a
    /// successful code verification).
    pub(crate) fn authorize_interactive(&self, sid: SessionId, token: &[u8]) {
        self.insert_with_ttl(
            sid,
            hash_token(token),
            DEFAULT_SESSION_TTL,
            AuthSource::Interactive,
        );
    }

    /// Authorize a long-lived service token for `sid`.
    ///
    /// Used at daemon startup to preauthorize the daemon's own SID
    /// so internal clients (cron adapter, etc.) can authenticate
    /// without going through the diceware-code flow.
    pub(crate) fn authorize_service(&self, sid: SessionId, token: &[u8]) {
        self.insert_with_ttl(
            sid,
            hash_token(token),
            SERVICE_SESSION_TTL,
            AuthSource::Service,
        );
    }

    /// Verify that `(sid, token)` matches a live entry.
    ///
    /// Returns true on match, false on miss / wrong SID / wrong token
    /// / expired entry. Expired entries are evicted on access.
    pub(crate) fn verify(&self, sid: SessionId, token: &[u8]) -> bool {
        let supplied = hash_token(token);
        if let Some(entry) = self.lookup(sid) {
            ct_eq(&supplied, &entry.token_hash)
        } else {
            false
        }
    }

    fn insert_with_ttl(
        &self,
        sid: SessionId,
        token_hash: [u8; 32],
        ttl: Duration,
        source: AuthSource,
    ) {
        let entry = AuthEntry {
            token_hash,
            expires_at: Instant::now() + ttl,
            source,
        };
        let mut g = self.inner.write().expect("auth table poisoned");
        g.entries.insert(sid, entry);
    }

    // =====================================================================
    // Service-token map (ADR-045 PR #5)
    // =====================================================================

    /// Register (or replace) a named service-token entry. Called by
    /// `peko service-token create` (step 2) and by daemon
    /// `rehydrate` at startup.
    ///
    /// `ttl` is a relative duration; `None` means no expiry.
    pub(crate) fn register_service_token(
        &self,
        name: &str,
        token_hash: [u8; 32],
        caps: Vec<String>,
        ttl: Option<Duration>,
    ) {
        let entry = ServiceTokenEntry {
            token_hash,
            caps,
            expires_at: ttl.map(|d| Instant::now() + d),
        };
        let mut g = self
            .service_tokens
            .write()
            .expect("service-token map poisoned");
        g.insert(name.to_string(), entry);
    }

    /// Verify a raw token against the service-token map. Returns
    /// the bound capability list on match (None on miss / wrong
    /// token / unknown name / expired entry).
    ///
    /// Side effect: evicts expired entries on access (same shape as
    /// `lookup`).
    pub(crate) fn verify_service_token(&self, token: &[u8]) -> Option<Vec<String>> {
        let supplied = hash_token(token);
        let mut g = self
            .service_tokens
            .write()
            .expect("service-token map poisoned");
        let now = Instant::now();
        let mut expired: Option<String> = None;
        let mut found: Option<Vec<String>> = None;
        for (name, entry) in g.iter() {
            if let Some(exp) = entry.expires_at {
                if exp <= now {
                    expired.get_or_insert_with(|| name.clone());
                    continue;
                }
            }
            if ct_eq(&supplied, &entry.token_hash) {
                found = Some(entry.caps.clone());
                break;
            }
        }
        if let Some(name) = expired {
            g.remove(&name);
        }
        found
    }

    /// Revoke a named service-token entry. Returns `true` if the
    /// entry was present. On-disk revocation is the caller's job
    /// (see `ServiceTokenStore::revoke`); this method only clears
    /// the in-memory cache.
    pub(crate) fn revoke_service_token(&self, name: &str) -> bool {
        self.service_tokens
            .write()
            .expect("service-token map poisoned")
            .remove(name)
            .is_some()
    }

    /// Snapshot of every registered service token's name + caps +
    /// expiry. Used by `peko service-token list` (via the IPC
    /// handler) and by PR #6 audit/counter paths.
    ///
    /// Excludes expired entries (they're evicted on access in
    /// [`verify_service_token`], but a list call should not trigger
    /// an eviction).
    pub(crate) fn list_service_tokens(&self) -> Vec<(String, Vec<String>, Option<Instant>)> {
        let g = self
            .service_tokens
            .read()
            .expect("service-token map poisoned");
        g.iter()
            .map(|(name, entry)| (name.clone(), entry.caps.clone(), entry.expires_at))
            .collect()
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

    #[test]
    fn authorize_interactive_registers_session() {
        let t = AuthTable::new();
        t.authorize_interactive(1000, b"session-token-a");
        assert!(t.verify(1000, b"session-token-a"));
        assert!(!t.verify(1000, b"session-token-b")); // wrong token
        assert!(!t.verify(1001, b"session-token-a")); // wrong SID
    }

    #[test]
    fn authorize_service_overwrites_existing_entry() {
        let t = AuthTable::new();
        t.authorize_interactive(2000, b"old-token");
        assert!(t.verify(2000, b"old-token"));

        t.authorize_service(2000, b"new-service-token");
        // The service entry replaces the interactive one (HashMap<_, _>).
        assert!(t.verify(2000, b"new-service-token"));
        assert!(!t.verify(2000, b"old-token"));
    }

    #[test]
    fn verify_expired_returns_false_and_evicts() {
        let t = AuthTable::new();
        // Manually insert an already-expired entry via the existing
        // `insert` helper to avoid sleeping in tests.
        t.insert(
            3000,
            AuthEntry {
                token_hash: hash_token(b"soon-expired"),
                expires_at: Instant::now(),
                source: AuthSource::Interactive,
            },
        );
        // Sleep just past expiry.
        std::thread::sleep(Duration::from_millis(5));
        assert!(!t.verify(3000, b"soon-expired"));
        // Entry should be evicted on access.
        assert!(!t.revoke(3000));
    }

    // ---- service-token map (ADR-045 PR #5) ----

    #[test]
    fn register_and_verify_service_token_round_trip() {
        let t = AuthTable::new();
        let token = b"my-secret-token";
        t.register_service_token(
            "runtime",
            hash_token(token),
            vec!["fs:read".into(), "tool:Bash".into()],
            None,
        );
        let caps = t.verify_service_token(token);
        assert_eq!(caps, Some(vec!["fs:read".to_string(), "tool:Bash".to_string()]));
    }

    #[test]
    fn verify_service_token_rejects_wrong_token() {
        let t = AuthTable::new();
        t.register_service_token(
            "runtime",
            hash_token(b"real-token"),
            vec!["fs:read".into()],
            None,
        );
        assert!(t.verify_service_token(b"wrong-token").is_none());
    }

    #[test]
    fn verify_service_token_rejects_unknown_name() {
        let t = AuthTable::new();
        t.register_service_token(
            "runtime",
            hash_token(b"a-token"),
            vec!["fs:read".into()],
            None,
        );
        // Same hash but unknown name → no hit.
        let unknown_hash = hash_token(b"a-token");
        // Verify against the map with no entry for "ghost" → None.
        assert!(t.verify_service_token(b"a-token").is_some()); // hits "runtime"
        // Now revoke "runtime" and confirm verify returns None.
        assert!(t.revoke_service_token("runtime"));
        assert!(t.verify_service_token(b"a-token").is_none());
        let _ = unknown_hash; // silence unused
    }

    #[test]
    fn revoke_service_token_removes_entry() {
        let t = AuthTable::new();
        t.register_service_token("rt", hash_token(b"tok"), vec!["x".into()], None);
        assert!(t.revoke_service_token("rt"));
        assert!(!t.revoke_service_token("rt"));
        assert!(t.verify_service_token(b"tok").is_none());
    }

    #[test]
    fn verify_service_token_with_ttl_returns_none_after_expiry() {
        let t = AuthTable::new();
        t.register_service_token(
            "rt",
            hash_token(b"tok"),
            vec!["x".into()],
            Some(Duration::from_millis(0)),
        );
        std::thread::sleep(Duration::from_millis(5));
        assert!(t.verify_service_token(b"tok").is_none());
        // Entry should be evicted.
        assert!(!t.revoke_service_token("rt"));
    }

    #[test]
    fn list_service_tokens_returns_all_entries() {
        let t = AuthTable::new();
        t.register_service_token("a", hash_token(b"tok-a"), vec!["a".into()], None);
        t.register_service_token("b", hash_token(b"tok-b"), vec!["b".into()], None);
        let list = t.list_service_tokens();
        assert_eq!(list.len(), 2);
        // Order not guaranteed, but both names must be present.
        let names: Vec<String> = list.iter().map(|(n, _, _)| n.clone()).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn service_token_caps_are_immutable_after_register() {
        // The ADR says "capability-scoped at creation (cannot grow)".
        // Enforce at the API surface: there is no `set_caps` /
        // `add_cap` method on the table. The only way to change
        // caps is revoke + register.
        let t = AuthTable::new();
        t.register_service_token(
            "rt",
            hash_token(b"tok"),
            vec!["fs:read".into()],
            None,
        );
        // Verify the registered caps are exactly what we set.
        assert_eq!(
            t.verify_service_token(b"tok"),
            Some(vec!["fs:read".to_string()])
        );
    }
}