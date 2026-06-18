//! Session key derivation
//!
//! Provides semantic session keys for multi-user, multi-channel isolation.
//! Keys follow `OpenClaw`'s format: `agent:{agent}:{context}:{identifier}`

use serde::{Deserialize, Serialize};

/// Session scope determines how sessions are shared/isolated
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionScope {
    /// One session per sender (user) - for DMs, private conversations
    PerSender,
    /// One session per channel - for shared group conversations
    PerChannel,
    /// Global session shared by all - for broadcast, announcements
    Global,
    /// CLI default persistent session
    CliDefault,
    /// Web/API session with specific token
    WebToken,
}

/// Context for deriving a session key
///
/// This type is used for parsing and validating session key formats,
/// NOT for runtime execution context (see `session::context::SessionContext`).
#[derive(Debug, Clone, Default)]
pub struct SessionKeyContext {
    /// Channel type (discord, cli, web, etc.)
    pub channel: Option<String>,
    /// Sender/user ID
    pub sender_id: Option<String>,
    /// Channel/guild ID (for group contexts)
    pub channel_id: Option<String>,
    /// Account ID (for multi-account channels)
    pub account_id: Option<String>,
    /// Thread ID (for threaded conversations)
    pub thread_id: Option<String>,
    /// Web token (for API sessions)
    pub web_token: Option<String>,
    /// Chat type (direct, group, channel)
    pub chat_type: ChatType,
}

/// Type of chat
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatType {
    #[default]
    Direct,
    Group,
    Channel,
    Thread,
}

/// Derive a session key from context
///
/// # Arguments
/// * `agent` - The agent name
/// * `scope` - The session scope
/// * `ctx` - Context information
///
/// # Returns
/// A session key string in `OpenClaw` format
///
/// # Examples
/// ```
/// use pekobot::session::key::{derive_session_key, SessionScope, SessionKeyContext, ChatType};
///
/// // CLI default session
/// let ctx = SessionKeyContext::default();
/// let key = derive_session_key("myagent", SessionScope::CliDefault, &ctx);
/// assert_eq!(key, "agent:myagent:cli:default");
///
/// // Discord DM
/// let ctx = SessionKeyContext {
///     channel: Some("discord".to_string()),
///     sender_id: Some("123456".to_string()),
///     chat_type: ChatType::Direct,
///     ..Default::default()
/// };
/// let key = derive_session_key("myagent", SessionScope::PerSender, &ctx);
/// assert_eq!(key, "agent:myagent:discord:123456");
/// ```
#[must_use]
pub fn derive_session_key(agent: &str, scope: SessionScope, ctx: &SessionKeyContext) -> String {
    match scope {
        SessionScope::Global => {
            format!("agent:{agent}:global")
        }
        SessionScope::CliDefault => {
            format!("agent:{agent}:cli:default")
        }
        SessionScope::WebToken => {
            let token = ctx.web_token.as_deref().unwrap_or("anonymous");
            format!("agent:{}:web:{}", agent, sanitize_key_component(token))
        }
        SessionScope::PerSender => {
            let channel = ctx.channel.as_deref().unwrap_or("unknown");
            let sender = ctx.sender_id.as_deref().unwrap_or("anonymous");
            format!(
                "agent:{}:{}:{}",
                agent,
                channel,
                sanitize_key_component(sender)
            )
        }
        SessionScope::PerChannel => {
            let channel = ctx.channel.as_deref().unwrap_or("unknown");
            let channel_id = ctx.channel_id.as_deref().unwrap_or("default");

            // Include thread ID if present
            if let Some(thread_id) = &ctx.thread_id {
                format!(
                    "agent:{}:{}:channel:{}:thread:{}",
                    agent,
                    channel,
                    sanitize_key_component(channel_id),
                    sanitize_key_component(thread_id)
                )
            } else {
                format!(
                    "agent:{}:{}:channel:{}",
                    agent,
                    channel,
                    sanitize_key_component(channel_id)
                )
            }
        }
    }
}

/// Parse a session key into its components
///
/// # Examples
/// ```
/// use pekobot::session::key::parse_session_key;
///
/// let parts = parse_session_key("agent:myagent:discord:123456");
/// assert_eq!(parts.agent, "myagent");
/// assert_eq!(parts.context, "discord");
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

/// Get the scope from a session key
#[must_use]
pub fn scope_from_key(key: &str) -> Option<SessionScope> {
    let parts = parse_session_key(key);

    match parts.context {
        "global" => Some(SessionScope::Global),
        "cli" => Some(SessionScope::CliDefault),
        "web" => Some(SessionScope::WebToken),
        "discord" | "telegram" | "whatsapp" | "slack" | "signal" | "imessage" => {
            // Check if it's per-channel or per-sender based on structure
            if parts.identifier.contains(":channel:") {
                Some(SessionScope::PerChannel)
            } else {
                Some(SessionScope::PerSender)
            }
        }
        _ => None,
    }
}

/// Build a Discord-specific session key
#[must_use]
pub fn discord_session_key(
    agent: &str,
    user_id: Option<&str>,
    guild_id: Option<&str>,
    channel_id: Option<&str>,
    thread_id: Option<&str>,
) -> String {
    match (user_id, guild_id, channel_id, thread_id) {
        // DM conversation
        (Some(user), None, _, _) => {
            let ctx = SessionKeyContext {
                channel: Some("discord".to_string()),
                sender_id: Some(user.to_string()),
                chat_type: ChatType::Direct,
                ..Default::default()
            };
            derive_session_key(agent, SessionScope::PerSender, &ctx)
        }
        // Thread in guild
        (_, Some(guild), Some(channel), Some(thread)) => {
            format!("agent:{agent}:discord:guild:{guild}:channel:{channel}:thread:{thread}")
        }
        // Channel in guild
        (_, Some(guild), Some(channel), None) => {
            format!("agent:{agent}:discord:guild:{guild}:channel:{channel}")
        }
        // Fallback to global
        _ => format!("agent:{agent}:global"),
    }
}

/// Build a CLI session key
#[must_use]
pub fn cli_session_key(agent: &str) -> String {
    format!("agent:{agent}:cli:default")
}

/// Derive a base session key from agent and peer
/// Format: agent:{agent}:peer:{type}:{id}
///
/// After ADR-039, `Peer` is an alias for `Principal`. The key format
/// is **byte-stable** for `Principal::User` and `Principal::Agent` —
/// these are the only valid session peers (`Principal::is_session_peer`).
/// For `Principal::Team` and `Principal::Public`, the function falls
/// back to `peer:user:default` and logs a warning, so a stray non-peer
/// principal never produces an orphan key. This is the documented
/// behavior, not a bug.
#[must_use]
pub fn derive_base_session_key(agent: &str, peer: &super::Peer) -> String {
    use crate::auth::principal::Principal;
    match peer {
        Principal::User(id) => {
            format!("agent:{}:peer:user:{}", agent, sanitize_key_component(id))
        }
        Principal::Agent(id) => {
            format!("agent:{}:peer:agent:{}", agent, sanitize_key_component(id))
        }
        Principal::Team(_) | Principal::Public => {
            tracing::warn!(
                "derive_base_session_key called with non-peer Principal {peer}; \
                 falling back to peer:user:default (ADR-039)"
            );
            format!("agent:{}:peer:user:default", agent)
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

/// Get the base session key from an overlay key
#[must_use]
pub fn base_key_from_overlay(overlay_key: &str) -> Option<String> {
    // Format: agent:{agent}:peer:{type}:{id}:overlay:{overlay_type}:{overlay_id}
    overlay_key
        .find(":overlay:")
        .map(|pos| overlay_key[..pos].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_default_key() {
        let ctx = SessionKeyContext::default();
        let key = derive_session_key("testagent", SessionScope::CliDefault, &ctx);
        assert_eq!(key, "agent:testagent:cli:default");
    }

    #[test]
    fn test_global_key() {
        let ctx = SessionKeyContext::default();
        let key = derive_session_key("testagent", SessionScope::Global, &ctx);
        assert_eq!(key, "agent:testagent:global");
    }

    #[test]
    fn test_per_sender_key() {
        let ctx = SessionKeyContext {
            channel: Some("discord".to_string()),
            sender_id: Some("123456".to_string()),
            chat_type: ChatType::Direct,
            ..Default::default()
        };
        let key = derive_session_key("testagent", SessionScope::PerSender, &ctx);
        assert_eq!(key, "agent:testagent:discord:123456");
    }

    #[test]
    fn test_per_channel_key() {
        let ctx = SessionKeyContext {
            channel: Some("discord".to_string()),
            channel_id: Some("987654".to_string()),
            chat_type: ChatType::Channel,
            ..Default::default()
        };
        let key = derive_session_key("testagent", SessionScope::PerChannel, &ctx);
        assert_eq!(key, "agent:testagent:discord:channel:987654");
    }

    #[test]
    fn test_thread_key() {
        let ctx = SessionKeyContext {
            channel: Some("discord".to_string()),
            channel_id: Some("987654".to_string()),
            thread_id: Some("thread123".to_string()),
            chat_type: ChatType::Thread,
            ..Default::default()
        };
        let key = derive_session_key("testagent", SessionScope::PerChannel, &ctx);
        assert_eq!(
            key,
            "agent:testagent:discord:channel:987654:thread:thread123"
        );
    }

    #[test]
    fn test_parse_session_key() {
        let parts = parse_session_key("agent:testagent:discord:123456");
        assert_eq!(parts.agent, "testagent");
        assert_eq!(parts.context, "discord");
        assert_eq!(parts.identifier, "123456");
    }

    #[test]
    fn test_parse_complex_key() {
        let parts = parse_session_key("agent:testagent:discord:guild:111:channel:222:thread:333");
        assert_eq!(parts.agent, "testagent");
        assert_eq!(parts.context, "discord");
        assert_eq!(parts.identifier, "guild:111:channel:222:thread:333");
    }

    #[test]
    fn test_discord_dm_key() {
        let key = discord_session_key("testagent", Some("user123"), None, None, None);
        assert_eq!(key, "agent:testagent:discord:user123");
    }

    #[test]
    fn test_discord_guild_channel_key() {
        let key = discord_session_key(
            "testagent",
            None,
            Some("guild456"),
            Some("channel789"),
            None,
        );
        assert_eq!(
            key,
            "agent:testagent:discord:guild:guild456:channel:channel789"
        );
    }

    #[test]
    fn test_sanitize_component() {
        assert_eq!(sanitize_key_component("hello:world"), "hello_world");
        assert_eq!(sanitize_key_component("a:b:c"), "a_b_c");
    }

    #[test]
    fn test_derive_base_session_key() {
        use super::super::Peer;

        let user_peer = Peer::User("alice".to_string());
        let key = derive_base_session_key("testagent", &user_peer);
        assert_eq!(key, "agent:testagent:peer:user:alice");

        let agent_peer = Peer::Agent("helper".to_string());
        let key = derive_base_session_key("testagent", &agent_peer);
        assert_eq!(key, "agent:testagent:peer:agent:helper");
    }

    #[test]
    fn test_derive_overlay_key() {
        let base = "agent:test:peer:user:alice";
        let key = derive_overlay_key(base, "channel", "discord:guild123");
        assert_eq!(
            key,
            "agent:test:peer:user:alice:overlay:channel:discord:guild123"
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
        let key = "agent:testagent:peer:user:alice:overlay:channel:discord:guild123";
        let parsed = parse_session_key_v2(key).unwrap();

        assert_eq!(parsed.agent, "testagent");
        assert_eq!(parsed.peer_type, "user");
        assert_eq!(parsed.peer_id, "alice");
        assert!(parsed.is_overlay);
        assert_eq!(parsed.overlay_type, Some("channel".to_string()));
        assert_eq!(parsed.overlay_id, Some("discord:guild123".to_string()));
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
        let overlay = "agent:test:peer:user:alice:overlay:channel:discord:guild123";
        let base = base_key_from_overlay(overlay).unwrap();
        assert_eq!(base, "agent:test:peer:user:alice");

        // Non-overlay key returns None
        assert_eq!(base_key_from_overlay("agent:test:peer:user:alice"), None);
    }

    #[test]
    fn test_legacy_key_returns_none() {
        // Legacy format should not be parsed by v2 parser
        let legacy = "agent:testagent:discord:123456";
        assert!(parse_session_key_v2(legacy).is_none());
    }
}
