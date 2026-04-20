//! Hook Registry and Management System
//!
//! Implements Milestone 8: Outbound Hooks and System Events
//! - Cron hooks (already implemented via cron_tool.rs)
//! - Webhook hooks with token validation
//! - File watch hooks
//! - Event bus hooks
//!
//! This module manages hook registration, validation, and triggering.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::debug;

pub mod event_bus;
pub mod file_watch;
pub mod lifecycle;
pub mod registry;
pub mod trigger;

pub use event_bus::EventBusHookIntegration;
pub use file_watch::FileWatchHookManager;
pub use lifecycle::LifecycleEmitter;
pub use registry::HookRegistry;
pub use trigger::{HookTrigger, TriggerSource};

/// A registered hook instance
#[derive(Debug, Clone)]
pub struct RegisteredHook {
    /// Unique hook ID
    pub id: String,
    /// Instance ID that owns this hook
    pub instance_id: String,
    /// Hook type and configuration
    pub hook_type: HookType,
    /// Action to take when triggered
    pub action: HookAction,
    /// Session target (new or active)
    pub session_target: SessionTarget,
    /// Whether hook is enabled
    pub enabled: bool,
}

/// Hook types supported by the system
#[derive(Debug, Clone)]
pub enum HookType {
    /// Cron-based schedule (uses existing cron system)
    Cron { schedule: String },
    /// Webhook endpoint
    Webhook {
        /// Path suffix (under /`webhooks/{instance_id`}/)
        path: String,
        /// Optional token for validation
        token: Option<String>,
    },
    /// Event bus subscription
    Event {
        /// Topic pattern to subscribe to
        topic: String,
    },
    /// File system watcher
    FileWatch {
        /// Path to watch (relative to instance workspace)
        path: String,
        /// Optional glob pattern filter
        pattern: Option<String>,
    },
}

/// Action to take when hook fires
#[derive(Debug, Clone)]
pub enum HookAction {
    /// Run a new session or inject into active session
    Run {
        /// Message/task to send
        message: String,
    },
}

/// Session target for hook execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTarget {
    /// Create a new session
    New,
    /// Inject into active session
    Active,
}

impl SessionTarget {
    #[must_use]
    pub fn from_str(s: &str) -> Self {
        match s {
            "active" => SessionTarget::Active,
            _ => SessionTarget::New,
        }
    }
}

impl std::fmt::Display for SessionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionTarget::New => write!(f, "new"),
            SessionTarget::Active => write!(f, "active"),
        }
    }
}

/// Result of webhook token validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenValidationResult {
    Valid,
    Invalid,
    Missing,
    NotRequired,
}

/// System event types emitted to the event stream
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemEventType {
    // Instance lifecycle
    InstanceCreated {
        instance: InstanceSnapshot,
    },
    InstanceStarted {
        instance_id: String,
    },
    InstanceStatusChanged {
        instance_id: String,
        previous_status: String,
        new_status: String,
    },
    InstanceStopped {
        instance_id: String,
    },
    InstanceError {
        instance_id: String,
        error: String,
    },
    InstanceUpgraded {
        instance_id: String,
        previous_digest: String,
        new_digest: String,
    },
    InstanceRemoved {
        instance_id: String,
    },

    // Team lifecycle
    TeamCreated {
        team: TeamSnapshot,
    },
    TeamReady {
        team_id: String,
    },
    TeamScaled {
        team_id: String,
        agent_name: String,
        previous_count: u32,
        new_count: u32,
    },
    TeamStopped {
        team_id: String,
    },
    TeamRemoved {
        team_id: String,
    },

    // Image lifecycle
    ImagePulled {
        image: ImageSnapshot,
    },
    ImageBuilt {
        image: ImageSnapshot,
    },
    ImagePushed {
        image: ImageSnapshot,
        remote_ref: String,
    },
    ImageRemoved {
        image_id: String,
    },

    // Bus events (optional, high volume)
    BusMessage {
        team_id: String,
        message_type: String,
        from: String,
        to: Option<String>,
    },

    // Hook events
    HookTriggered {
        hook_id: String,
        instance_id: String,
        source: String,
        session_id: Option<String>,
    },
}

/// Instance snapshot for events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    pub id: String,
    pub name: String,
    pub image_ref: String,
    pub image_digest: String,
    pub status: String,
    pub team_id: Option<String>,
    pub created_at: String,
}

/// Team snapshot for events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSnapshot {
    pub id: String,
    pub name: String,
    pub status: String,
    pub agent_count: u32,
    pub created_at: String,
}

/// Image snapshot for events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSnapshot {
    pub id: String,
    pub name: String,
    pub version: String,
    pub digest: String,
    pub size_bytes: u64,
    pub created_at: String,
}

/// System event envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    /// Event ID
    pub id: String,
    /// Event timestamp (ISO 8601)
    pub ts: String,
    /// Event type and data
    #[serde(flatten)]
    pub event_type: SystemEventType,
}

/// Event filter for subscriptions
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Filter by resource types
    pub resource_types: Option<Vec<String>>,
    /// Filter by instance IDs
    pub instance_ids: Option<Vec<String>>,
    /// Filter by team IDs
    pub team_ids: Option<Vec<String>>,
    /// Filter by event types
    pub event_types: Option<Vec<String>>,
    /// Include bus messages (high volume)
    pub include_bus_messages: bool,
}

impl EventFilter {
    /// Check if an event matches this filter
    #[must_use]
    pub fn matches(&self, event: &SystemEvent) -> bool {
        // Check resource type filters
        if let Some(ref types) = self.resource_types {
            let resource_type = match &event.event_type {
                SystemEventType::InstanceCreated { .. }
                | SystemEventType::InstanceStarted { .. }
                | SystemEventType::InstanceStatusChanged { .. }
                | SystemEventType::InstanceStopped { .. }
                | SystemEventType::InstanceError { .. }
                | SystemEventType::InstanceUpgraded { .. }
                | SystemEventType::InstanceRemoved { .. } => "instance",
                SystemEventType::TeamCreated { .. }
                | SystemEventType::TeamReady { .. }
                | SystemEventType::TeamScaled { .. }
                | SystemEventType::TeamStopped { .. }
                | SystemEventType::TeamRemoved { .. } => "team",
                SystemEventType::ImagePulled { .. }
                | SystemEventType::ImageBuilt { .. }
                | SystemEventType::ImagePushed { .. }
                | SystemEventType::ImageRemoved { .. } => "image",
                SystemEventType::BusMessage { .. } => "bus",
                SystemEventType::HookTriggered { .. } => "hook",
            };
            if !types.contains(&resource_type.to_string()) {
                return false;
            }
        }

        // Check instance ID filters
        if let Some(ref ids) = self.instance_ids {
            let instance_id = match &event.event_type {
                SystemEventType::InstanceCreated { instance } => Some(&instance.id),
                SystemEventType::InstanceStarted { instance_id }
                | SystemEventType::InstanceStatusChanged { instance_id, .. }
                | SystemEventType::InstanceStopped { instance_id }
                | SystemEventType::InstanceError { instance_id, .. }
                | SystemEventType::InstanceUpgraded { instance_id, .. }
                | SystemEventType::InstanceRemoved { instance_id } => Some(instance_id),
                SystemEventType::HookTriggered { instance_id, .. } => Some(instance_id),
                _ => None,
            };
            if let Some(id) = instance_id {
                if !ids.contains(id) {
                    return false;
                }
            }
        }

        // Check team ID filters
        if let Some(ref ids) = self.team_ids {
            let team_id = match &event.event_type {
                SystemEventType::TeamCreated { team } => Some(&team.id),
                SystemEventType::TeamReady { team_id }
                | SystemEventType::TeamScaled { team_id, .. }
                | SystemEventType::TeamStopped { team_id }
                | SystemEventType::TeamRemoved { team_id } => Some(team_id),
                SystemEventType::InstanceCreated { instance } => instance.team_id.as_ref(),
                SystemEventType::BusMessage { team_id, .. } => Some(team_id),
                _ => None,
            };
            if let Some(id) = team_id {
                if !ids.contains(id) {
                    return false;
                }
            }
        }

        // Check event type filters
        if let Some(ref types) = self.event_types {
            let event_type_str = match &event.event_type {
                SystemEventType::InstanceCreated { .. } => "instance.created",
                SystemEventType::InstanceStarted { .. } => "instance.started",
                SystemEventType::InstanceStatusChanged { .. } => "instance.status_changed",
                SystemEventType::InstanceStopped { .. } => "instance.stopped",
                SystemEventType::InstanceError { .. } => "instance.error",
                SystemEventType::InstanceUpgraded { .. } => "instance.upgraded",
                SystemEventType::InstanceRemoved { .. } => "instance.removed",
                SystemEventType::TeamCreated { .. } => "team.created",
                SystemEventType::TeamReady { .. } => "team.ready",
                SystemEventType::TeamScaled { .. } => "team.scaled",
                SystemEventType::TeamStopped { .. } => "team.stopped",
                SystemEventType::TeamRemoved { .. } => "team.removed",
                SystemEventType::ImagePulled { .. } => "image.pulled",
                SystemEventType::ImageBuilt { .. } => "image.built",
                SystemEventType::ImagePushed { .. } => "image.pushed",
                SystemEventType::ImageRemoved { .. } => "image.removed",
                SystemEventType::BusMessage { .. } => "bus.message",
                SystemEventType::HookTriggered { .. } => "hook.triggered",
            };
            if !types.contains(&event_type_str.to_string()) {
                return false;
            }
        }

        // Check bus message filter
        if matches!(event.event_type, SystemEventType::BusMessage { .. })
            && !self.include_bus_messages
        {
            return false;
        }

        true
    }
}

/// System event broadcaster
pub struct EventBroadcaster {
    subscribers: Arc<RwLock<Vec<tokio::sync::mpsc::UnboundedSender<SystemEvent>>>>,
}

impl EventBroadcaster {
    #[must_use]
    pub fn new() -> Self {
        Self {
            subscribers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Subscribe to system events
    pub async fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SystemEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut subscribers = self.subscribers.write().await;
        subscribers.push(tx);
        rx
    }

    /// Broadcast an event to all subscribers
    pub async fn broadcast(&self, event: SystemEvent) {
        let subscribers = self.subscribers.read().await;
        for tx in subscribers.iter() {
            if let Err(e) = tx.send(event.clone()) {
                debug!("Failed to send event to subscriber: {}", e);
            }
        }
    }

    /// Remove disconnected subscribers
    pub async fn cleanup(&self) {
        // This is a no-op with unbounded channels since we can't easily detect disconnects
        // In a production system, we'd use a more sophisticated approach
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_target_from_str() {
        assert_eq!(SessionTarget::from_str("new"), SessionTarget::New);
        assert_eq!(SessionTarget::from_str("active"), SessionTarget::Active);
        assert_eq!(SessionTarget::from_str("unknown"), SessionTarget::New);
    }

    #[test]
    fn test_event_filter_matches_instance() {
        let filter = EventFilter {
            resource_types: Some(vec!["instance".to_string()]),
            ..Default::default()
        };

        let event = SystemEvent {
            id: "evt_001".to_string(),
            ts: "2026-03-17T10:00:00.000Z".to_string(),
            event_type: SystemEventType::InstanceStarted {
                instance_id: "inst_123".to_string(),
            },
        };

        assert!(filter.matches(&event));

        let team_event = SystemEvent {
            id: "evt_002".to_string(),
            ts: "2026-03-17T10:00:00.000Z".to_string(),
            event_type: SystemEventType::TeamReady {
                team_id: "team_123".to_string(),
            },
        };

        assert!(!filter.matches(&team_event));
    }
}
