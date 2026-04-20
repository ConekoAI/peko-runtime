//! Hook Trigger
//!
//! Handles triggering hooks and routing them to agent sessions.

use super::{HookAction, HookType, RegisteredHook, SessionTarget};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Source of a hook trigger
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum TriggerSource {
    /// Cron schedule fired
    Cron { schedule: String },
    /// Webhook received
    Webhook {
        path: String,
        payload: serde_json::Value,
        headers: std::collections::HashMap<String, String>,
    },
    /// Event bus message
    Event {
        topic: String,
        payload: serde_json::Value,
    },
    /// File system change
    FileWatch { path: String, change_type: String },
}

impl TriggerSource {
    /// Get display name for the trigger source
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        match self {
            TriggerSource::Cron { .. } => "cron",
            TriggerSource::Webhook { .. } => "webhook",
            TriggerSource::Event { .. } => "event",
            TriggerSource::FileWatch { .. } => "file_watch",
        }
    }

    /// Get payload data for the trigger
    #[must_use]
    pub fn payload(&self) -> serde_json::Value {
        match self {
            TriggerSource::Cron { schedule } => {
                serde_json::json!({"schedule": schedule})
            }
            TriggerSource::Webhook {
                payload, headers, ..
            } => {
                serde_json::json!({
                    "payload": payload,
                    "headers": headers,
                })
            }
            TriggerSource::Event { topic, payload } => {
                serde_json::json!({
                    "topic": topic,
                    "payload": payload,
                })
            }
            TriggerSource::FileWatch { path, change_type } => {
                serde_json::json!({
                    "path": path,
                    "event": change_type,
                })
            }
        }
    }
}

/// Result of triggering a hook
#[derive(Debug, Clone)]
pub enum TriggerResult {
    /// Hook triggered successfully, session created/updated
    Success {
        hook_id: String,
        session_id: Option<String>,
    },
    /// Instance not running, queued for later
    Queued { hook_id: String },
    /// Hook is disabled
    Disabled { hook_id: String },
    /// Error triggering hook
    Error { hook_id: String, error: String },
}

/// Hook trigger handler
pub struct HookTrigger {
    /// Hook being triggered
    hook: RegisteredHook,
    /// Source of the trigger
    source: TriggerSource,
}

impl HookTrigger {
    /// Create a new hook trigger
    #[must_use]
    pub fn new(hook: RegisteredHook, source: TriggerSource) -> Self {
        Self { hook, source }
    }

    /// Build the message to send to the agent
    #[must_use]
    pub fn build_message(&self) -> String {
        match &self.hook.action {
            HookAction::Run { message } => {
                let source_info = match &self.source {
                    TriggerSource::Cron { schedule } => {
                        format!("Cron schedule '{schedule}' fired")
                    }
                    TriggerSource::Webhook { path, payload, .. } => {
                        format!(
                            "Webhook received on path '{}' with payload: {}",
                            path,
                            serde_json::to_string_pretty(payload).unwrap_or_default()
                        )
                    }
                    TriggerSource::Event { topic, payload } => {
                        format!(
                            "Event received on topic '{}' with payload: {}",
                            topic,
                            serde_json::to_string_pretty(payload).unwrap_or_default()
                        )
                    }
                    TriggerSource::FileWatch { path, change_type } => {
                        format!("File {change_type} detected at path: {path}")
                    }
                };

                format!(
                    "{}\n\nSource: {}\nTrigger: {:?}",
                    message, source_info, self.source
                )
            }
        }
    }

    /// Build a session event for the trigger
    #[must_use]
    pub fn build_session_event(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "hook.trigger",
            "hook_type": match &self.hook.hook_type {
                HookType::Cron { .. } => "cron",
                HookType::Webhook { .. } => "webhook",
                HookType::Event { .. } => "event",
                HookType::FileWatch { .. } => "file_watch",
            },
            "source": self.source,
            "payload": self.source.payload(),
        })
    }

    /// Get the target session type
    #[must_use]
    pub fn session_target(&self) -> SessionTarget {
        self.hook.session_target
    }

    /// Get hook ID
    #[must_use]
    pub fn hook_id(&self) -> &str {
        &self.hook.id
    }

    /// Get instance ID
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.hook.instance_id
    }

    /// Check if hook is enabled
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.hook.enabled
    }
}

/// Hook trigger processor
///
/// Processes hook triggers and routes them to the appropriate agent sessions.
pub struct HookTriggerProcessor {
    // In a real implementation, this would have references to:
    // - Session manager for creating/injecting sessions
    // - Agent manager for checking instance status
    // - Event broadcaster for emitting events
}

impl HookTriggerProcessor {
    /// Create a new trigger processor
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }

    /// Process a hook trigger
    ///
    /// This would normally:
    /// 1. Check if instance is running
    /// 2. Create a new session or inject into active session
    /// 3. Emit system event
    /// 4. Return result
    pub async fn process(&self, trigger: HookTrigger) -> TriggerResult {
        if !trigger.is_enabled() {
            return TriggerResult::Disabled {
                hook_id: trigger.hook_id().to_string(),
            };
        }

        let message = trigger.build_message();
        let _session_event = trigger.build_session_event();

        debug!(
            "Processing hook trigger for instance {}: {}",
            trigger.instance_id(),
            message
        );

        // Placeholder: In real implementation, this would:
        // 1. Get the instance
        // 2. Check if it's running
        // 3. Create new session or inject into active
        // 4. Return the session ID

        TriggerResult::Success {
            hook_id: trigger.hook_id().to_string(),
            session_id: Some(format!("sess_placeholder_{}", trigger.hook_id())),
        }
    }

    /// Check if an instance is running
    pub async fn is_instance_running(&self, _instance_id: &str) -> bool {
        // Placeholder: Check with agent manager
        true
    }

    /// Create a new session for a hook trigger
    pub async fn create_session(
        &self,
        instance_id: &str,
        _message: &str,
        trigger_source: &TriggerSource,
    ) -> anyhow::Result<String> {
        // Placeholder: Create session via session manager
        info!(
            "Creating session for instance {} with trigger from {:?}",
            instance_id, trigger_source
        );

        // Return a placeholder session ID
        Ok(format!(
            "sess_{}_{}",
            instance_id,
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Inject message into active session
    pub async fn inject_into_active_session(
        &self,
        instance_id: &str,
        message: &str,
    ) -> anyhow::Result<Option<String>> {
        // Placeholder: Get active session and inject message
        info!(
            "Injecting message into active session for instance {}: {}",
            instance_id, message
        );

        // Return the active session ID if available
        Ok(Some(format!("sess_active_{instance_id}")))
    }
}

impl Default for HookTriggerProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_source_display() {
        let cron = TriggerSource::Cron {
            schedule: "0 8 * * *".to_string(),
        };
        assert_eq!(cron.display_name(), "cron");

        let webhook = TriggerSource::Webhook {
            path: "/github".to_string(),
            payload: serde_json::json!({}),
            headers: std::collections::HashMap::new(),
        };
        assert_eq!(webhook.display_name(), "webhook");
    }

    #[test]
    fn test_build_message() {
        let hook = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Webhook {
                path: "/github".to_string(),
                token: None,
            },
            action: HookAction::Run {
                message: "GitHub webhook received".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        let source = TriggerSource::Webhook {
            path: "/github".to_string(),
            payload: serde_json::json!({"action": "push"}),
            headers: std::collections::HashMap::new(),
        };

        let trigger = HookTrigger::new(hook, source);
        let message = trigger.build_message();

        assert!(message.contains("GitHub webhook received"));
        assert!(message.contains("/github"));
    }

    #[test]
    fn test_build_session_event() {
        let hook = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Cron {
                schedule: "0 8 * * *".to_string(),
            },
            action: HookAction::Run {
                message: "Daily task".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        let source = TriggerSource::Cron {
            schedule: "0 8 * * *".to_string(),
        };

        let trigger = HookTrigger::new(hook, source);
        let event = trigger.build_session_event();

        assert_eq!(event["type"], "hook.trigger");
        assert_eq!(event["hook_type"], "cron");
    }
}
