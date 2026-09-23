//! Session key parsing + derivation for multi-user, multi-channel isolation.
//!
//! What lives here:
//! - [`parse_session_key`] — split an `agent:{agent}:{context}:{identifier}`
//!   attribution-envelope key into its parts (the `ExecuteTool` handler
//!   resolves the owning principal from the `agent` segment).
//! - [`sanitize_key_component`] / [`safe_filename_component`] — component
//!   sanitizers for key segments and on-disk filenames.
//! - [`derive_base_session_key`] / [`derive_overlay_key`] /
//!   [`base_key_from_overlay`] / [`parse_session_key_v2`] — the v2
//!   peer/overlay session-key family (`agent:{agent}:peer:{type}:{id}`),
//!   used by peer-child provisioning and spawn cleanup.
//!
//! The legacy OpenClaw derivation machinery (`derive_session_key`,
//! `SessionScope`, `SessionKeyContext`, `ChatType`, `scope_from_key`)
//! was removed in the ADR-061 follow-up: it had no production callers
//! left — keys are minted by `derive_base_session_key` (peer routing)
//! and `AgenticLoop` session ids (UUIDs), not by scope templates.

/// Parse a session key into its components
///
/// # Examples
/// ```
/// use peko_session::key::parse_session_key;
///
/// let parts = parse_session_key("agent:myagent:cli:123456");
/// assert_eq!(parts.agent, "myagent");
/// assert_eq!(parts.context, "cli");
/// assert_eq!(parts.identifier, "123456");
/// ```
#[must_use]
pub fn parse_session_key(key: &str) -> SessionKeyParts<'_> {
    let parts: Vec<&str> = key.split(':').collect();

    if parts.len() < 2 {
        return SessionKeyParts {
            agent: "",
            context: "",
            identifier: String::new(),
            raw: key,
        };
    }

    // Skip "agent:" prefix if present
    let start_idx = usize::from(parts[0] == "agent");

    let agent = parts.get(start_idx).copied().unwrap_or("");
    let context = parts.get(start_idx + 1).copied().unwrap_or("");
    let identifier = parts
        .get(start_idx + 2..)
        .map(|p| p.join(":"))
        .unwrap_or_default();

    SessionKeyParts {
        agent,
        context,
        identifier,
        raw: key,
    }
}

/// Components of a parsed session key
#[derive(Debug, Clone)]
pub struct SessionKeyParts<'a> {
    pub agent: &'a str,
    pub context: &'a str,
    pub identifier: String,
    pub raw: &'a str,
}

/// Sanitize a component for use in a session key
/// Replaces colons with underscores, limits length
#[must_use]
pub fn sanitize_key_component(s: &str) -> String {
    s.chars()
        .map(|c| if c == ':' { '_' } else { c })
        .take(64) // Limit component length
        .collect()
}

/// Sanitize a session identifier for use as an on-disk filename
/// component. Rewrites only characters Windows rejects (POSIX is
/// preserved bit-for-bit so existing on-disk files remain reachable on
/// Linux/macOS). The semantic session id stored in memory and
/// serialized into JSONL is unchanged — only the on-disk filename is
/// transformed.
#[must_use]
pub fn safe_filename_component(s: &str) -> String {
    if !cfg!(windows) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (ch as u32) < 0x20
        {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

// Sprint 9 Commit 2: `discord_session_key` retired. The chat-gateway
// adapter framework (the only production caller) was deleted in Commit 3;
// the platform-specific key shape no longer has a producer. Callers that
// need a peer-routed session key should build it via
// `derive_base_session_key` + `sanitize_key_component`.
// pub fn discord_session_key(
//     agent: &str,
//     user_id: Option<&str>,
//     guild_id: Option<&str>,
//     channel_id: Option<&str>,
//     thread_id: Option<&str>,
// ) -> String { ... }

/// Derive a base session key from agent and peer
/// Format: agent:{agent}:peer:{type}:{id}
///
/// After ADR-039, `Subject` is an alias for `Subject`. The key format
/// is **byte-stable** for `Subject::User`, `Subject::Principal`, and
/// `Subject::Visitor` (ADR-058 D5) — these are the valid session peers
/// (`Subject::is_session_peer`).
/// For `Subject::Public`, the function falls back to `peer:user:default`
/// and logs a warning, so a stray non-peer subject never produces an
/// orphan key. This is the documented behavior, not a bug.
#[must_use]
pub fn derive_base_session_key(agent: &str, peer: &peko_subject::Subject) -> String {
    use peko_subject::Subject;
    match peer {
        Subject::User(id) => {
            format!("agent:{}:peer:user:{}", agent, sanitize_key_component(id))
        }
        Subject::Principal(id) => {
            format!(
                "agent:{}:peer:agent:{}",
                agent,
                sanitize_key_component(id.as_str())
            )
        }
        Subject::Visitor(id) => {
            format!(
                "agent:{}:peer:visitor:{}",
                agent,
                sanitize_key_component(id)
            )
        }
        Subject::Public => {
            tracing::warn!(
                "derive_base_session_key called with non-peer Subject {peer}; \
                 falling back to peer:user:default (ADR-039)"
            );
            format!("agent:{agent}:peer:user:default")
        }
    }
}

/// Derive an overlay key from base key and overlay info
/// Format: {`base_key}:overlay:{type}:{overlay_id`}
#[must_use]
pub fn derive_overlay_key(base_key: &str, overlay_type: &str, overlay_id: &str) -> String {
    format!("{base_key}:overlay:{overlay_type}:{overlay_id}")
}

/// Parse a peer-based session key (v2 format)
#[derive(Debug, Clone)]
pub struct ParsedSessionKeyV2 {
    pub agent: String,
    pub peer_type: String,
    pub peer_id: String,
    pub overlay_type: Option<String>,
    pub overlay_id: Option<String>,
    pub is_overlay: bool,
    pub raw: String,
}

/// Parse a session key (supports both v1 and v2 formats)
#[must_use]
pub fn parse_session_key_v2(key: &str) -> Option<ParsedSessionKeyV2> {
    let parts: Vec<&str> = key.split(':').collect();

    if parts.len() < 2 {
        return None;
    }

    // Check for peer-based format (v2)
    // Format: agent:{agent}:peer:{type}:{id}[:overlay:{type}:{id}]
    if parts.len() >= 5 {
        if let Some(peer_idx) = parts.iter().position(|&p| p == "peer") {
            let agent = parts.get(1)?.to_string();
            let peer_type = parts.get(peer_idx + 1)?.to_string();
            let peer_id = parts
                .iter()
                .skip(peer_idx + 2)
                .take_while(|&&p| p != "overlay")
                .copied()
                .collect::<Vec<_>>()
                .join(":");

            // Check for overlay
            if let Some(overlay_idx) = parts.iter().position(|&p| p == "overlay") {
                let overlay_type = parts.get(overlay_idx + 1)?.to_string();
                let overlay_id = parts
                    .iter()
                    .skip(overlay_idx + 2)
                    .take_while(|&&p| p != "overlay")
                    .copied()
                    .collect::<Vec<_>>()
                    .join(":");

                return Some(ParsedSessionKeyV2 {
                    agent,
                    peer_type,
                    peer_id,
                    overlay_type: Some(overlay_type),
                    overlay_id: Some(overlay_id),
                    is_overlay: true,
                    raw: key.to_string(),
                });
            }

            return Some(ParsedSessionKeyV2 {
                agent,
                peer_type,
                peer_id,
                overlay_type: None,
                overlay_id: None,
                is_overlay: false,
                raw: key.to_string(),
            });
        }
    }

    // Legacy format (v1) - not parsed by this function
    None
}

/// Get the base key from an overlay key
#[must_use]
pub fn base_key_from_overlay(overlay_key: &str) -> Option<String> {
    // Format: agent:{agent}:peer:{type}:{id}:overlay:{type}:{overlay_id}
    overlay_key
        .find(":overlay:")
        .map(|pos| overlay_key[..pos].to_string())
}

#[cfg(test)]
mod tests {
    use crate::*;

    #[test]
    fn test_parse_session_key() {
        // Sprint 9 Commit 2: parse test uses "cli" instead of "discord".
        let parts = parse_session_key("agent:testagent:cli:123456");
        assert_eq!(parts.agent, "testagent");
        assert_eq!(parts.context, "cli");
        assert_eq!(parts.identifier, "123456");
    }

    #[test]
    fn test_parse_complex_key() {
        // Sprint 9 Commit 2: parse test uses "cli" instead of "discord".
        let parts = parse_session_key("agent:testagent:cli:guild:111:channel:222:thread:333");
        assert_eq!(parts.agent, "testagent");
        assert_eq!(parts.context, "cli");
        assert_eq!(parts.identifier, "guild:111:channel:222:thread:333");
    }

    #[test]
    fn test_sanitize_component() {
        assert_eq!(sanitize_key_component("hello:world"), "hello_world");
        assert_eq!(sanitize_key_component("a:b:c"), "a_b_c");
    }

    #[test]
    fn test_derive_base_session_key() {
        use peko_subject::Subject;

        let user_peer = Subject::User("alice".to_string());
        let key = derive_base_session_key("test_agent", &user_peer);
        assert_eq!(key, "agent:test_agent:peer:user:alice");

        let agent_peer = Subject::Principal("helper".into());
        let key = derive_base_session_key("test_agent", &agent_peer);
        assert_eq!(key, "agent:test_agent:peer:agent:helper");
    }

    #[test]
    fn test_derive_overlay_key() {
        // Sprint 9 Commit 2: overlay_id uses "cli" channel prefix.
        let base = "agent:test:peer:user:alice";
        let key = derive_overlay_key(base, "channel", "cli:guild123");
        assert_eq!(
            key,
            "agent:test:peer:user:alice:overlay:channel:cli:guild123"
        );
    }

    #[test]
    fn test_parse_session_key_v2_base() {
        let key = "agent:testagent:peer:user:alice";
        let parsed = parse_session_key_v2(key).unwrap();

        assert_eq!(parsed.agent, "testagent");
        assert_eq!(parsed.peer_type, "user");
        assert_eq!(parsed.peer_id, "alice");
        assert!(!parsed.is_overlay);
        assert_eq!(parsed.overlay_type, None);
    }

    #[test]
    fn test_parse_session_key_v2_overlay() {
        // Sprint 9 Commit 2: overlay_id uses "cli" channel prefix.
        let key = "agent:testagent:peer:user:alice:overlay:channel:cli:guild123";
        let parsed = parse_session_key_v2(key).unwrap();

        assert_eq!(parsed.agent, "testagent");
        assert_eq!(parsed.peer_type, "user");
        assert_eq!(parsed.peer_id, "alice");
        assert!(parsed.is_overlay);
        assert_eq!(parsed.overlay_type, Some("channel".to_string()));
        assert_eq!(parsed.overlay_id, Some("cli:guild123".to_string()));
    }

    #[test]
    fn test_parse_session_key_v2_agent_peer() {
        let key = "agent:testagent:peer:agent:helper";
        let parsed = parse_session_key_v2(key).unwrap();

        assert_eq!(parsed.agent, "testagent");
        assert_eq!(parsed.peer_type, "agent");
        assert_eq!(parsed.peer_id, "helper");
    }

    #[test]
    fn test_base_key_from_overlay() {
        // Sprint 9 Commit 2: overlay_id uses "cli" channel prefix.
        let overlay = "agent:test:peer:user:alice:overlay:channel:cli:guild123";
        let base = base_key_from_overlay(overlay).unwrap();
        assert_eq!(base, "agent:test:peer:user:alice");

        // Non-overlay key returns None
        assert_eq!(base_key_from_overlay("agent:test:peer:user:alice"), None);
    }

    #[test]
    fn test_legacy_key_returns_none() {
        // Legacy v1 channel prefix should not be parsed by v2 parser.
        // Sprint 9 Commit 2: "cli" replaced "discord" as the fixture
        // string — the v1 format is still unrecognized by v2.
        let legacy = "agent:testagent:cli:123456";
        assert!(parse_session_key_v2(legacy).is_none());
    }
}
