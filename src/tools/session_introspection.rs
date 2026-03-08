//! Session introspection tools
//!
//! Tools for listing sessions, viewing history, and checking session status.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::debug;

use crate::tools::traits::Tool;

/// Session list arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsListArgs {
    /// Filter by session kinds (e.g., "main", "spawned", "cron")
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    /// Maximum number of sessions to return
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Only show sessions active in the last N minutes
    #[serde(default)]
    pub active_minutes: Option<i64>,
}

fn default_limit() -> usize {
    50
}

/// Session info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_key: String,
    pub session_id: String,
    pub kind: String,
    pub agent_id: Option<String>,
    pub label: Option<String>,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
    pub is_active: bool,
}

/// Session list result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsListResult {
    pub sessions: Vec<SessionInfo>,
    pub total: usize,
}

/// Session history arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsHistoryArgs {
    /// Session key or ID
    pub session_key: String,
    /// Maximum number of messages to return
    #[serde(default = "default_history_limit")]
    pub limit: usize,
    /// Include tool calls/results
    #[serde(default = "default_include_tools")]
    pub include_tools: bool,
}

fn default_history_limit() -> usize {
    100
}

fn default_include_tools() -> bool {
    true
}

/// Message in session history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_results: Option<Vec<ToolResultInfo>>,
    pub timestamp: String,
}

/// Tool call info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Tool result info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultInfo {
    pub tool_call_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Session history result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsHistoryResult {
    pub session_key: String,
    pub messages: Vec<HistoryMessage>,
    pub total_messages: usize,
}

/// Session status arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatusArgs {
    /// Session key or ID (defaults to current session)
    #[serde(default)]
    pub session_key: Option<String>,
    /// Optional timezone for timestamp formatting (e.g., "America/New_York", "UTC")
    /// If not provided, uses machine's local timezone
    #[serde(default)]
    pub timezone: Option<String>,
}

/// Usage stats
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

/// Session status result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatusResult {
    pub session_key: String,
    pub session_id: String,
    pub agent_id: String,
    pub model: String,
    pub status: String,
    pub created_at: String,
    pub last_activity: String,
    /// Current timestamp in ISO 8601 format (UTC)
    pub timestamp_utc: String,
    /// Current timestamp formatted for display (respects timezone parameter)
    pub timestamp: String,
    pub message_count: usize,
    pub usage: UsageStats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
}

/// Registry for accessing session data
pub trait SessionRegistry: Send + Sync {
    /// List available sessions
    fn list_sessions(
        &self,
        kinds: Option<&[String]>,
        limit: usize,
        active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>>;

    /// Get session history
    fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>>;

    /// Get session status
    fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult>;

    /// Get current session key
    fn current_session_key(&self) -> String;
}

/// Sessions list tool
pub struct SessionsListTool {
    registry: Box<dyn SessionRegistry>,
}

impl SessionsListTool {
    #[must_use]
    pub fn new(registry: Box<dyn SessionRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SessionsListTool {
    fn name(&self) -> &'static str {
        "sessions_list"
    }

    fn description(&self) -> &'static str {
        "List active sessions with optional filtering"
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: SessionsListArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        debug!(
            "Listing sessions: kinds={:?}, limit={}, active_minutes={:?}",
            args.kinds, args.limit, args.active_minutes
        );

        let kinds_ref = args.kinds.as_deref();
        let sessions = self
            .registry
            .list_sessions(kinds_ref, args.limit, args.active_minutes)?;

        let total = sessions.len();

        Ok(serde_json::to_value(SessionsListResult {
            sessions,
            total,
        })?)
    }
}

/// Sessions history tool
pub struct SessionsHistoryTool {
    registry: Box<dyn SessionRegistry>,
}

impl SessionsHistoryTool {
    #[must_use]
    pub fn new(registry: Box<dyn SessionRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SessionsHistoryTool {
    fn name(&self) -> &'static str {
        "sessions_history"
    }

    fn description(&self) -> &'static str {
        "Get message history for a specific session"
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: SessionsHistoryArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        if args.session_key.is_empty() {
            return Err(anyhow::anyhow!("session_key is required"));
        }

        debug!(
            "Getting history for session: {}, limit={}, include_tools={}",
            args.session_key, args.limit, args.include_tools
        );

        let messages =
            self.registry
                .get_history(&args.session_key, args.limit, args.include_tools)?;

        let total_messages = messages.len();

        Ok(serde_json::to_value(SessionsHistoryResult {
            session_key: args.session_key,
            messages,
            total_messages,
        })?)
    }
}

/// Session status tool
pub struct SessionStatusTool {
    registry: Box<dyn SessionRegistry>,
}

impl SessionStatusTool {
    #[must_use]
    pub fn new(registry: Box<dyn SessionRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SessionStatusTool {
    fn name(&self) -> &'static str {
        "session_status"
    }

    fn description(&self) -> &'static str {
        "Returns current session status including timestamp, token usage, and model information. \
         Use this tool when you need to know the current date and time. \
         Optional timezone parameter allows formatting time for a specific timezone (e.g., 'America/New_York', 'Europe/London', 'UTC')."
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: SessionStatusArgs =
            serde_json::from_value(args).map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        // Use provided session key or default to current
        let session_key = args
            .session_key
            .unwrap_or_else(|| self.registry.current_session_key());

        debug!("Getting status for session: {}", session_key);

        let mut status = self.registry.get_status(&session_key)?;

        // Add current timestamps
        let now_utc = chrono::Utc::now();
        status.timestamp_utc = now_utc.to_rfc3339();

        // Format timestamp based on requested timezone or default to local
        status.timestamp = if let Some(tz_str) = args.timezone {
            match tz_str.parse::<chrono_tz::Tz>() {
                Ok(tz) => {
                    let now_local = now_utc.with_timezone(&tz);
                    now_local.format("%Y-%m-%d %H:%M:%S %Z").to_string()
                }
                Err(_) => {
                    // Invalid timezone, fall back to local
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string()
                }
            }
        } else {
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z").to_string()
        };

        Ok(serde_json::to_value(status)?)
    }
}

/// Simple in-memory implementation for testing
pub struct InMemorySessionRegistry {
    current_session: String,
    sessions: std::sync::Mutex<HashMap<String, SessionInfo>>,
    histories: std::sync::Mutex<HashMap<String, Vec<HistoryMessage>>>,
    statuses: std::sync::Mutex<HashMap<String, SessionStatusResult>>,
}

impl InMemorySessionRegistry {
    #[must_use]
    pub fn new(current_session: String) -> Self {
        Self {
            current_session,
            sessions: std::sync::Mutex::new(HashMap::new()),
            histories: std::sync::Mutex::new(HashMap::new()),
            statuses: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn add_session(
        &self,
        key: String,
        info: SessionInfo,
        history: Vec<HistoryMessage>,
        status: SessionStatusResult,
    ) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(key.clone(), info);
        }
        if let Ok(mut histories) = self.histories.lock() {
            histories.insert(key.clone(), history);
        }
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.insert(key, status);
        }
    }
}

impl SessionRegistry for InMemorySessionRegistry {
    fn list_sessions(
        &self,
        _kinds: Option<&[String]>,
        _limit: usize,
        _active_minutes: Option<i64>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {e}"))?;
        Ok(sessions.values().cloned().collect())
    }

    fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        _include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        let histories = self
            .histories
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {e}"))?;
        let history = histories.get(session_key).cloned().unwrap_or_default();
        Ok(history.into_iter().take(limit).collect())
    }

    fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult> {
        let statuses = self
            .statuses
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock error: {e}"))?;
        statuses
            .get(session_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))
    }

    fn current_session_key(&self) -> String {
        self.current_session.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_registry() -> InMemorySessionRegistry {
        let registry = InMemorySessionRegistry::new("main".to_string());

        let session = SessionInfo {
            session_key: "test-session".to_string(),
            session_id: "abc123".to_string(),
            kind: "spawned".to_string(),
            agent_id: Some("test-agent".to_string()),
            label: Some("Test Session".to_string()),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 10,
            is_active: true,
        };

        let history = vec![
            HistoryMessage {
                role: "user".to_string(),
                content: "Hello".to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            },
            HistoryMessage {
                role: "assistant".to_string(),
                content: "Hi there!".to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            },
        ];

        let status = SessionStatusResult {
            session_key: "test-session".to_string(),
            session_id: "abc123".to_string(),
            agent_id: "test-agent".to_string(),
            model: "kimi-k2.5".to_string(),
            status: "active".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 10,
            usage: UsageStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                estimated_cost_usd: Some(0.0015),
            },
            label: Some("Test Session".to_string()),
            parent_session: Some("main".to_string()),
        };

        registry.add_session("test-session".to_string(), session, history, status);
        registry
    }

    #[tokio::test]
    async fn test_sessions_list() {
        let registry = create_test_registry();
        let tool = SessionsListTool::new(Box::new(registry));

        let result = tool
            .execute(serde_json::json!({
                "limit": 10
            }))
            .await
            .unwrap();

        let list_result: SessionsListResult = serde_json::from_value(result).unwrap();
        assert_eq!(list_result.total, 1);
        assert_eq!(list_result.sessions[0].session_key, "test-session");
    }

    #[tokio::test]
    async fn test_sessions_history() {
        let registry = create_test_registry();
        let tool = SessionsHistoryTool::new(Box::new(registry));

        let result = tool
            .execute(serde_json::json!({
                "session_key": "test-session",
                "limit": 10
            }))
            .await
            .unwrap();

        let history_result: SessionsHistoryResult = serde_json::from_value(result).unwrap();
        assert_eq!(history_result.total_messages, 2);
        assert_eq!(history_result.messages[0].role, "user");
        assert_eq!(history_result.messages[0].content, "Hello");
    }

    #[tokio::test]
    async fn test_session_status() {
        let registry = create_test_registry();
        let tool = SessionStatusTool::new(Box::new(registry));

        let result = tool
            .execute(serde_json::json!({
                "session_key": "test-session"
            }))
            .await
            .unwrap();

        let status_result: SessionStatusResult = serde_json::from_value(result).unwrap();
        assert_eq!(status_result.session_key, "test-session");
        assert_eq!(status_result.model, "kimi-k2.5");
        assert_eq!(status_result.usage.total_tokens, 150);
    }

    #[tokio::test]
    async fn test_session_status_defaults_to_current() {
        let registry = InMemorySessionRegistry::new("current-session".to_string());

        // Add current session
        let status = SessionStatusResult {
            session_key: "current-session".to_string(),
            session_id: "current123".to_string(),
            agent_id: "main".to_string(),
            model: "kimi-k2.5".to_string(),
            status: "active".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            usage: UsageStats {
                prompt_tokens: 50,
                completion_tokens: 25,
                total_tokens: 75,
                estimated_cost_usd: None,
            },
            label: None,
            parent_session: None,
        };

        let session = SessionInfo {
            session_key: "current-session".to_string(),
            session_id: "current123".to_string(),
            kind: "main".to_string(),
            agent_id: Some("main".to_string()),
            label: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 5,
            is_active: true,
        };

        registry.add_session("current-session".to_string(), session, vec![], status);

        let tool = SessionStatusTool::new(Box::new(registry));

        // Call without session_key - should default to current
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        let status_result: SessionStatusResult = serde_json::from_value(result).unwrap();
        assert_eq!(status_result.session_key, "current-session");
    }
}
