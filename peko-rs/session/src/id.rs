//! `SessionId` — opaque UUID newtype for session identifiers.
//!
//! Sprint 6 (2026-08-21) collapses the legacy session-id shape
//! (`"root:user:alice"`, `"root:self"`, `agent:{a}:peer:{type}:{id}`)
//! to opaque UUIDs. The `SessionId` newtype enforces the discipline at
//! the type level: session ids are not strings, they are 128-bit UUIDs,
//! and the LLM-facing surface never sees them — the model sees slug
//! paths via `peko_session::path`.
//!
//! ## Why a newtype
//!
//! Three reasons the legacy shape was wrong:
//!
//! 1. **Storage keys should be opaque.** Encoding peer routing into
//!    the id string (`root:user:alice`) conflates "where on disk"
//!    with "who originated this session." Peer routing is a channel
//!    concern; session ids are storage keys.
//! 2. **UUIDs are filesystem-safe.** Windows-illegal chars (`< > : " /
//!    \ | ? *`) don't appear in canonical UUIDs, so the
//!    `safe_filename_component` shim can stay as a Windows-compat
//!    no-op without behavior change.
//! 3. **Type-level separation.** `String` lets code accidentally
//!    compare a session id to a peer id or a slug; `SessionId` makes
//!    that impossible at compile time.
//!
//! ## Wire / JSON
//!
//! `SessionId` serializes transparently as a UUID string
//! (`"550e8400-e29b-41d4-a716-446655440000"`). On-disk `sessions.json`
//! reads/writes the string form via `#[serde(transparent)]`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque UUID session identifier.
///
/// Sprint 6: every engine-internal session id is a `SessionId`. The
/// LLM-facing surface never sees them — the model sees slug paths
/// (`/a/b/c`, `agent-c`) instead. Trunk session lookup is by
/// `parent_session_id == None` (see `crate::ownership::find_trunk_session`),
/// not by a magic string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl SessionId {
    /// Mint a fresh, v4 (random) session id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Borrow the inner ` Uuid`.
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }

    /// Render as a canonical hyphenated UUID string. Used for JSONL
    /// filenames (`<uuid>.jsonl`), audit-log fields, and IPC packets
    /// that carry session ids across the wire.
    #[must_use]
    pub fn as_str(&self) -> String {
        self.0.to_string()
    }

    /// Parse a UUID-shaped string. Returns `None` for any non-UUID
    /// input — including the legacy `root:*` shapes that pre-sprint-6
    /// fixtures may still carry.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Uuid::parse_str(s).ok().map(Self)
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.to_string())
    }
}

impl From<Uuid> for SessionId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl From<&str> for SessionId {
    /// Parses a canonical UUID string, or — for test-fixture convenience
    /// — derives a stable v5 UUID from any other string. Production
    /// code that needs to validate untrusted input should call
    /// [`SessionId::parse`] instead.
    fn from(s: &str) -> Self {
        Self::parse(s).unwrap_or_else(|| {
            const NAMESPACE: Uuid = Uuid::from_bytes([
                0x6e, 0xb4, 0x0b, 0xe6, 0x70, 0x88, 0x4a, 0x86,
                0xa5, 0x6b, 0x49, 0x68, 0x6f, 0x59, 0x4c, 0x99,
            ]);
            Self(Uuid::new_v5(&NAMESPACE, s.as_bytes()))
        })
    }
}

impl From<String> for SessionId {
    /// See [`From<&str>`] for the rationale (canonical UUID parse,
    /// v5 derivation fallback for test fixtures).
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&String> for SessionId {
    /// See [`From<&str>`] for the rationale (canonical UUID parse,
    /// v5 derivation fallback for test fixtures).
    fn from(s: &String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<SessionId> for Uuid {
    fn from(id: SessionId) -> Uuid {
        id.0
    }
}

impl From<SessionId> for String {
    fn from(id: SessionId) -> String {
        id.0.to_string()
    }
}

impl AsRef<Uuid> for SessionId {
    fn as_ref(&self) -> &Uuid {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_new_is_unique() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_round_trips_through_string() {
        let id = SessionId::new();
        let s = id.as_str();
        let parsed = SessionId::parse(&s).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn session_id_display_uses_canonical_hyphenated_form() {
        let id = SessionId(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap());
        assert_eq!(id.to_string(), "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(id.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn session_id_parse_rejects_non_uuid_strings() {
        assert!(SessionId::parse("root:self").is_none());
        assert!(SessionId::parse("root:user:alice").is_none());
        assert!(SessionId::parse("not-a-uuid").is_none());
        assert!(SessionId::parse("").is_none());
        assert!(SessionId::parse("550e8400").is_none()); // too short
    }

    #[test]
    fn session_id_serializes_transparently() {
        let id = SessionId(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"550e8400-e29b-41d4-a716-446655440000\"");
        let parsed: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn session_id_serializes_in_option() {
        let id = SessionId::new();
        let wrapped = Some(id);
        let json = serde_json::to_string(&wrapped).unwrap();
        let parsed: Option<SessionId> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Some(id));
    }
}