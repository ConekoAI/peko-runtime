//! Session Management - Lifecycle, expiration, and reset policies
//!
//! Matches `OpenClaw`'s session management:
//! - Daily reset at configurable time (default 4:00 AM)
//! - Idle timeout (sliding window)
//! - Manual reset triggers (/reset, /new)
//! - Per-type overrides (direct, group, thread)

pub mod pruning;
pub mod transcript;

use chrono::{DateTime, Duration, Local, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// Session reset mode
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ResetMode {
    /// Reset daily at specific hour
    #[default]
    Daily,
    /// Reset after idle period
    Idle,
    /// Reset on whichever comes first
    First,
}


/// Reset policy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResetPolicy {
    /// Reset mode
    pub mode: ResetMode,
    /// Hour of day for daily reset (0-23, local time)
    pub at_hour: u8,
    /// Idle minutes before reset (when mode is Idle or First)
    pub idle_minutes: Option<u64>,
}

impl Default for ResetPolicy {
    fn default() -> Self {
        Self {
            mode: ResetMode::Daily,
            at_hour: 4, // 4:00 AM local time (like OpenClaw)
            idle_minutes: Some(120), // 2 hours default
        }
    }
}

impl ResetPolicy {
    /// Create daily reset policy
    #[must_use] 
    pub fn daily(at_hour: u8) -> Self {
        Self {
            mode: ResetMode::Daily,
            at_hour,
            idle_minutes: None,
        }
    }

    /// Create idle-only reset policy
    #[must_use] 
    pub fn idle(minutes: u64) -> Self {
        Self {
            mode: ResetMode::Idle,
            at_hour: 0,
            idle_minutes: Some(minutes),
        }
    }

    /// Create "first of daily or idle" policy
    #[must_use] 
    pub fn first(at_hour: u8, idle_minutes: u64) -> Self {
        Self {
            mode: ResetMode::First,
            at_hour,
            idle_minutes: Some(idle_minutes),
        }
    }
}

/// Session type for per-type overrides
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    /// Direct message (1:1 chat)
    Direct,
    /// Group chat
    Group,
    /// Thread (Discord/Slack thread, Telegram topic)
    Thread,
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionType::Direct => write!(f, "direct"),
            SessionType::Group => write!(f, "group"),
            SessionType::Thread => write!(f, "thread"),
        }
    }
}

/// DM scope for session isolation
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DMScope {
    /// All DMs share the main session (default, single-user)
    #[default]
    Main,
    /// Isolate by sender ID across channels
    PerPeer,
    /// Isolate by channel + sender (multi-user inboxes)
    PerChannelPeer,
    /// Isolate by account + channel + sender (multi-account)
    PerAccountChannelPeer,
}


/// Session configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Main session key (default: "main")
    pub main_key: String,
    /// DM scope for session isolation
    pub dm_scope: DMScope,
    /// Default reset policy
    pub reset: ResetPolicy,
    /// Per-type reset overrides
    pub reset_by_type: HashMap<SessionType, ResetPolicy>,
    /// Per-channel reset overrides (channel name -> policy)
    pub reset_by_channel: HashMap<String, ResetPolicy>,
    /// Reset trigger commands (e.g., "/reset", "/new")
    pub reset_triggers: Vec<String>,
    /// Store file path
    pub store_path: String,
}

impl Default for SessionConfig {
    fn default() -> Self {
        let mut reset_by_type = HashMap::new();
        reset_by_type.insert(
            SessionType::Thread,
            ResetPolicy::daily(4),
        );
        reset_by_type.insert(
            SessionType::Direct,
            ResetPolicy::first(4, 240), // Daily at 4am OR 4 hours idle
        );
        reset_by_type.insert(
            SessionType::Group,
            ResetPolicy::first(4, 120), // Daily at 4am OR 2 hours idle
        );

        Self {
            main_key: "main".to_string(),
            dm_scope: DMScope::Main,
            reset: ResetPolicy::default(),
            reset_by_type,
            reset_by_channel: HashMap::new(),
            reset_triggers: vec!["/reset".to_string(), "/new".to_string()],
            store_path: "sessions.json".to_string(),
        }
    }
}

/// Session metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Session ID
    pub id: String,
    /// Session key
    pub key: String,
    /// Session type
    pub session_type: SessionType,
    /// Channel (if applicable)
    pub channel: Option<String>,
    /// When session was created
    pub created_at: DateTime<Utc>,
    /// Last activity timestamp
    pub last_activity: DateTime<Utc>,
    /// Message count
    pub message_count: usize,
    /// Last reset timestamp
    pub last_reset_at: Option<DateTime<Utc>>,
    /// Display name for UI
    pub display_name: Option<String>,
    /// Origin info (where session came from)
    pub origin: Option<SessionOrigin>,
}

/// Session origin metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionOrigin {
    /// Human-readable label
    pub label: String,
    /// Provider/channel
    pub provider: String,
    /// From address/ID
    pub from: Option<String>,
    /// To address/ID
    pub to: Option<String>,
    /// Account ID (for multi-account)
    pub account_id: Option<String>,
    /// Thread/topic ID
    pub thread_id: Option<String>,
}

/// Session manager handles lifecycle and expiration
pub struct SessionManager {
    config: SessionConfig,
    sessions: HashMap<String, SessionMeta>,
}

impl SessionManager {
    /// Create a new session manager
    #[must_use] 
    pub fn new(config: SessionConfig) -> Self {
        Self {
            config,
            sessions: HashMap::new(),
        }
    }

    /// Create with default configuration
    #[must_use] 
    pub fn default_config() -> Self {
        Self::new(SessionConfig::default())
    }

    /// Generate session key based on DM scope
    #[must_use] 
    pub fn generate_session_key(
        &self,
        agent_id: &str,
        session_type: SessionType,
        channel: Option<&str>,
        peer_id: Option<&str>,
        account_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> String {
        match session_type {
            SessionType::Direct => {
                match self.config.dm_scope {
                    DMScope::Main => {
                        format!("agent:{}:{}", agent_id, self.config.main_key)
                    }
                    DMScope::PerPeer => {
                        let peer = peer_id.unwrap_or("unknown");
                        format!("agent:{agent_id}:dm:{peer}")
                    }
                    DMScope::PerChannelPeer => {
                        let ch = channel.unwrap_or("default");
                        let peer = peer_id.unwrap_or("unknown");
                        format!("agent:{agent_id}:{ch}:dm:{peer}")
                    }
                    DMScope::PerAccountChannelPeer => {
                        let acc = account_id.unwrap_or("default");
                        let ch = channel.unwrap_or("default");
                        let peer = peer_id.unwrap_or("unknown");
                        format!("agent:{agent_id}:{ch}:{acc}:dm:{peer}")
                    }
                }
            }
            SessionType::Group => {
                let ch = channel.unwrap_or("default");
                // Use thread_id as group ID for forums/topics
                let group_id = thread_id.map_or_else(|| "default".to_string(), |t| format!("group:{t}"));
                format!("agent:{agent_id}:{ch}:{group_id}")
            }
            SessionType::Thread => {
                let ch = channel.unwrap_or("default");
                let th = thread_id.unwrap_or("default");
                format!("agent:{agent_id}:{ch}:thread:{th}")
            }
        }
    }

    /// Check if a message is a reset trigger
    #[must_use] 
    pub fn is_reset_trigger(&self, message: &str) -> Option<&String> {
        let trimmed = message.trim();
        self.config.reset_triggers.iter()
            .find(|trigger| trimmed.starts_with(*trigger))
    }

    /// Check if session should be reset based on policy
    #[must_use] 
    pub fn should_reset(&self, session: &SessionMeta) -> bool {
        let policy = self.get_policy_for_session(session);
        
        match policy.mode {
            ResetMode::Daily => self.should_reset_daily(session, policy),
            ResetMode::Idle => self.should_reset_idle(session, policy),
            ResetMode::First => {
                self.should_reset_daily(session, policy) || 
                self.should_reset_idle(session, policy)
            }
        }
    }

    /// Check daily reset condition
    fn should_reset_daily(&self, session: &SessionMeta, policy: &ResetPolicy) -> bool {
        
        
        let now: DateTime<Local> = Local::now();
        
        // Calculate today's reset time by constructing a new datetime
        let today_reset: DateTime<Local> = match now.date_naive().and_hms_opt(
            u32::from(policy.at_hour),
            0,
            0
        ) {
            Some(naive) => match naive.and_local_timezone(Local) {
                chrono::LocalResult::Single(dt) => dt,
                _ => return false,
            },
            None => return false,
        };

        // Get last activity in local time
        let last_activity = session.last_activity.with_timezone(&Local);

        // Reset if last activity was before today's reset time
        // and current time is after reset time
        if now >= today_reset && last_activity < today_reset {
            debug!(
                "Session {} needs daily reset (last activity before {}:00)",
                session.key, policy.at_hour
            );
            return true;
        }

        false
    }

    /// Check idle reset condition
    fn should_reset_idle(&self, session: &SessionMeta, policy: &ResetPolicy) -> bool {
        let idle_minutes = match policy.idle_minutes {
            Some(m) => m,
            None => return false, // No idle timeout configured
        };

        let idle_duration = Utc::now() - session.last_activity;
        let idle_threshold = Duration::minutes(idle_minutes as i64);

        if idle_duration >= idle_threshold {
            debug!(
                "Session {} needs idle reset (idle for {} minutes)",
                session.key,
                idle_duration.num_minutes()
            );
            return true;
        }

        false
    }

    /// Get the reset policy for a session
    fn get_policy_for_session(&self, session: &SessionMeta) -> &ResetPolicy {
        // Check per-channel override first
        if let Some(channel) = &session.channel {
            if let Some(policy) = self.config.reset_by_channel.get(channel) {
                return policy;
            }
        }

        // Check per-type override
        if let Some(policy) = self.config.reset_by_type.get(&session.session_type) {
            return policy;
        }

        // Fall back to default
        &self.config.reset
    }

    /// Record activity for a session
    pub fn record_activity(&mut self, session_key: &str) {
        if let Some(session) = self.sessions.get_mut(session_key) {
            session.last_activity = Utc::now();
            session.message_count += 1;
            debug!("Recorded activity for session: {}", session_key);
        }
    }

    /// Create or get a session
    pub fn get_or_create_session(
        &mut self,
        key: String,
        session_type: SessionType,
        channel: Option<String>,
    ) -> &SessionMeta {
        let now = Utc::now();
        
        self.sessions.entry(key.clone()).or_insert_with(|| {
            info!("Creating new session: {}", key);
            SessionMeta {
                id: uuid::Uuid::new_v4().to_string(),
                key: key.clone(),
                session_type,
                channel,
                created_at: now,
                last_activity: now,
                message_count: 0,
                last_reset_at: None,
                display_name: None,
                origin: None,
            }
        })
    }

    /// Reset a session (creates new session ID)
    pub fn reset_session(&mut self, session_key: &str) -> Option<String> {
        if let Some(session) = self.sessions.get_mut(session_key) {
            let old_id = session.id.clone();
            session.id = uuid::Uuid::new_v4().to_string();
            session.created_at = Utc::now();
            session.last_activity = Utc::now();
            session.message_count = 0;
            session.last_reset_at = Some(Utc::now());
            
            info!(
                "Reset session {}: {} -> {}",
                session_key, old_id, session.id
            );
            
            return Some(session.id.clone());
        }
        None
    }

    /// Get session by key
    #[must_use] 
    pub fn get_session(&self, key: &str) -> Option<&SessionMeta> {
        self.sessions.get(key)
    }

    /// List all sessions
    #[must_use] 
    pub fn list_sessions(&self) -> Vec<&SessionMeta> {
        self.sessions.values().collect()
    }

    /// Check all sessions and return those needing reset
    #[must_use] 
    pub fn check_expirations(&self) -> Vec<&SessionMeta> {
        self.sessions
            .values()
            .filter(|s| self.should_reset(s))
            .collect()
    }

    /// Get status summary
    #[must_use] 
    pub fn status(&self) -> String {
        let total = self.sessions.len();
        let expired = self.check_expirations().len();
        
        format!(
            "📋 Sessions: {total} total, {expired} need reset"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_key_generation_main() {
        let manager = SessionManager::default_config();
        let key = manager.generate_session_key(
            "my-agent",
            SessionType::Direct,
            None,
            None,
            None,
            None,
        );
        assert_eq!(key, "agent:my-agent:main");
    }

    #[test]
    fn test_session_key_generation_per_peer() {
        let mut config = SessionConfig::default();
        config.dm_scope = DMScope::PerPeer;
        let manager = SessionManager::new(config);
        
        let key = manager.generate_session_key(
            "my-agent",
            SessionType::Direct,
            None,
            Some("user123"),
            None,
            None,
        );
        assert_eq!(key, "agent:my-agent:dm:user123");
    }

    #[test]
    fn test_session_key_generation_group() {
        let manager = SessionManager::default_config();
        let key = manager.generate_session_key(
            "my-agent",
            SessionType::Group,
            Some("discord"),
            None,
            None,
            Some("channel-456"),
        );
        assert_eq!(key, "agent:my-agent:discord:group:channel-456");
    }

    #[test]
    fn test_reset_trigger_detection() {
        let manager = SessionManager::default_config();
        
        assert!(manager.is_reset_trigger("/reset").is_some());
        assert!(manager.is_reset_trigger("/new").is_some());
        assert!(manager.is_reset_trigger("/reset and start fresh").is_some());
        assert!(manager.is_reset_trigger("  /new  ").is_some());
        assert!(manager.is_reset_trigger("hello").is_none());
    }

    #[test]
    fn test_idle_reset() {
        let config = SessionConfig {
            reset: ResetPolicy::idle(30), // 30 min idle
            ..Default::default()
        };
        let manager = SessionManager::new(config);
        
        let mut session = SessionMeta {
            id: "test".to_string(),
            key: "test".to_string(),
            session_type: SessionType::Direct,
            channel: None,
            created_at: Utc::now(),
            last_activity: Utc::now() - Duration::minutes(35),
            message_count: 10,
            last_reset_at: None,
            display_name: None,
            origin: None,
        };
        
        assert!(manager.should_reset_idle(&session, &manager.config.reset));
        
        // Update activity
        session.last_activity = Utc::now();
        assert!(!manager.should_reset_idle(&session, &manager.config.reset));
    }

    #[test]
    fn test_session_creation() {
        let mut manager = SessionManager::default_config();
        
        let session = manager.get_or_create_session(
            "test-key".to_string(),
            SessionType::Direct,
            None,
        );
        
        assert_eq!(session.key, "test-key");
        assert_eq!(session.session_type, SessionType::Direct);
        assert_eq!(session.message_count, 0);
    }

    #[test]
    fn test_session_reset() {
        let mut manager = SessionManager::default_config();
        
        // Create session
        manager.get_or_create_session(
            "test-key".to_string(),
            SessionType::Direct,
            None,
        );
        
        // Record some activity
        manager.record_activity("test-key");
        manager.record_activity("test-key");
        
        let old_id = manager.get_session("test-key").unwrap().id.clone();
        
        // Reset
        let new_id = manager.reset_session("test-key");
        
        assert!(new_id.is_some());
        assert_ne!(old_id, new_id.unwrap());
        assert_eq!(manager.get_session("test-key").unwrap().message_count, 0);
    }

    #[test]
    fn test_per_type_override() {
        let mut config = SessionConfig::default();
        config.reset_by_type.insert(
            SessionType::Thread,
            ResetPolicy::idle(60), // Threads have 1 hour idle
        );
        
        let manager = SessionManager::new(config);
        
        let thread_session = SessionMeta {
            id: "t1".to_string(),
            key: "thread-key".to_string(),
            session_type: SessionType::Thread,
            channel: None,
            created_at: Utc::now(),
            last_activity: Utc::now() - Duration::minutes(65),
            message_count: 5,
            last_reset_at: None,
            display_name: None,
            origin: None,
        };
        
        // Should use thread policy (60 min idle)
        assert!(manager.should_reset(&thread_session));
        
        let direct_session = SessionMeta {
            id: "d1".to_string(),
            key: "direct-key".to_string(),
            session_type: SessionType::Direct,
            channel: None,
            created_at: Utc::now(),
            last_activity: Utc::now() - Duration::minutes(65),
            message_count: 5,
            last_reset_at: None,
            display_name: None,
            origin: None,
        };
        
        // Direct uses default (2 hours idle in default config)
        // So 65 minutes should NOT trigger reset
        assert!(!manager.should_reset_idle(&direct_session, &manager.config.reset));
    }
}
