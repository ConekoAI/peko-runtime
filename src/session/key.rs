//! Session key derivation
//!
//! Provides semantic session keys for multi-user, multi-channel isolation.
//! Keys follow OpenClaw's format: `agent:{agent}:{context}:{identifier}`

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
#[derive(Debug, Clone, Default)]
pub struct SessionContext {
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
/// A session key string in OpenClaw format
///
/// # Examples
/// ```
/// use pekobot::session::key::{derive_session_key, SessionScope, SessionContext, ChatType};
///
/// // CLI default session
/// let ctx = SessionContext::default();
/// let key = derive_session_key("myagent", SessionScope::CliDefault, &ctx);
/// assert_eq!(key, "agent:myagent:cli:default");
///
/// // Discord DM
/// let ctx = SessionContext {
///     channel: Some("discord".to_string()),
///     sender_id: Some("123456".to_string()),
///     chat_type: ChatType::Direct,
///     ..Default::default()
/// };
/// let key = derive_session_key("myagent", SessionScope::PerSender, &ctx);
/// assert_eq!(key, "agent:myagent:discord:123456");
/// ```
pub fn derive_session_key(agent: &str, scope: SessionScope, ctx: &SessionContext) -> String {
    match scope {
        SessionScope::Global => {
            format!("agent:{}:global", agent)
        }
        SessionScope::CliDefault => {
            format!("agent:{}:cli:default", agent)
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
pub fn parse_session_key(key: &str) -> SessionKeyParts {
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
    let start_idx = if parts[0] == "agent" { 1 } else { 0 };
    
    let agent = parts.get(start_idx).copied().unwrap_or("");
    let context = parts.get(start_idx + 1).copied().unwrap_or("");
    let identifier = parts.get(start_idx + 2..).map(|p| p.join(":")).unwrap_or_default();
    
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
fn sanitize_key_component(s: &str) -> String {
    s.chars()
        .map(|c| if c == ':' { '_' } else { c })
        .take(64) // Limit component length
        .collect()
}

/// Get the scope from a session key
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
            let ctx = SessionContext {
                channel: Some("discord".to_string()),
                sender_id: Some(user.to_string()),
                chat_type: ChatType::Direct,
                ..Default::default()
            };
            derive_session_key(agent, SessionScope::PerSender, &ctx)
        }
        // Thread in guild
        (_, Some(guild), Some(channel), Some(thread)) => {
            format!(
                "agent:{}:discord:guild:{}:channel:{}:thread:{}",
                agent,
                guild,
                channel,
                thread
            )
        }
        // Channel in guild
        (_, Some(guild), Some(channel), None) => {
            format!(
                "agent:{}:discord:guild:{}:channel:{}",
                agent, guild, channel
            )
        }
        // Fallback to global
        _ => format!("agent:{}:global", agent),
    }
}

/// Build a CLI session key
pub fn cli_session_key(agent: &str) -> String {
    format!("agent:{}:cli:default", agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_default_key() {
        let ctx = SessionContext::default();
        let key = derive_session_key("testagent", SessionScope::CliDefault, &ctx);
        assert_eq!(key, "agent:testagent:cli:default");
    }

    #[test]
    fn test_global_key() {
        let ctx = SessionContext::default();
        let key = derive_session_key("testagent", SessionScope::Global, &ctx);
        assert_eq!(key, "agent:testagent:global");
    }

    #[test]
    fn test_per_sender_key() {
        let ctx = SessionContext {
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
        let ctx = SessionContext {
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
        let ctx = SessionContext {
            channel: Some("discord".to_string()),
            channel_id: Some("987654".to_string()),
            thread_id: Some("thread123".to_string()),
            chat_type: ChatType::Thread,
            ..Default::default()
        };
        let key = derive_session_key("testagent", SessionScope::PerChannel, &ctx);
        assert_eq!(key, "agent:testagent:discord:channel:987654:thread:thread123");
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
        let key = discord_session_key("testagent", None, Some("guild456"), Some("channel789"), None);
        assert_eq!(key, "agent:testagent:discord:guild:guild456:channel:channel789");
    }

    #[test]
    fn test_sanitize_component() {
        assert_eq!(sanitize_key_component("hello:world"), "hello_world");
        assert_eq!(sanitize_key_component("a:b:c"), "a_b_c");
    }
}
