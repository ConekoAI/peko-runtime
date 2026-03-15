//! System event types for orchestration
//!
//! Defines the taxonomy of events that can trigger agent invocation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// System event types that can trigger agent invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// File system change event
    File {
        path: PathBuf,
        change_type: FileChangeType,
        timestamp: DateTime<Utc>,
    },

    /// Webhook received from external system
    Webhook {
        source: String,
        route: String,
        payload: serde_json::Value,
        headers: HashMap<String, String>,
        timestamp: DateTime<Utc>,
    },

    /// Internal system event
    Internal {
        event_type: String,
        source: String,
        payload: serde_json::Value,
        timestamp: DateTime<Utc>,
    },

    /// Timer/scheduled event (from scheduler)
    Timer {
        schedule_id: String,
        task_id: String,
        fired_at: DateTime<Utc>,
    },
}

/// Types of file changes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

impl SystemEvent {
    /// Get the event type as a string for routing
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            SystemEvent::File { .. } => "file",
            SystemEvent::Webhook { .. } => "webhook",
            SystemEvent::Internal { .. } => "internal",
            SystemEvent::Timer { .. } => "timer",
        }
    }

    /// Get event timestamp
    #[must_use]
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            SystemEvent::File { timestamp, .. } => *timestamp,
            SystemEvent::Webhook { timestamp, .. } => *timestamp,
            SystemEvent::Internal { timestamp, .. } => *timestamp,
            SystemEvent::Timer { fired_at, .. } => *fired_at,
        }
    }
}

impl std::fmt::Display for FileChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileChangeType::Created => write!(f, "created"),
            FileChangeType::Modified => write!(f, "modified"),
            FileChangeType::Deleted => write!(f, "deleted"),
            FileChangeType::Renamed { from } => write!(f, "renamed from {}", from.display()),
        }
    }
}

impl std::fmt::Display for SystemEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SystemEvent::File {
                path, change_type, ..
            } => {
                write!(f, "File {}: {}", change_type, path.display())
            }
            SystemEvent::Webhook { source, route, .. } => {
                write!(f, "Webhook from {source} on {route}")
            }
            SystemEvent::Internal {
                event_type, source, ..
            } => {
                write!(f, "Internal {event_type} from {source}")
            }
            SystemEvent::Timer { schedule_id, .. } => {
                write!(f, "Timer for schedule {schedule_id}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_types() {
        let file_event = SystemEvent::File {
            path: PathBuf::from("/test/file.txt"),
            change_type: FileChangeType::Created,
            timestamp: Utc::now(),
        };
        assert_eq!(file_event.event_type(), "file");

        let webhook_event = SystemEvent::Webhook {
            source: "github".to_string(),
            route: "/webhook".to_string(),
            payload: serde_json::json!({}),
            headers: HashMap::new(),
            timestamp: Utc::now(),
        };
        assert_eq!(webhook_event.event_type(), "webhook");

        let internal_event = SystemEvent::Internal {
            event_type: "test".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: Utc::now(),
        };
        assert_eq!(internal_event.event_type(), "internal");

        let timer_event = SystemEvent::Timer {
            schedule_id: "test".to_string(),
            task_id: "test".to_string(),
            fired_at: Utc::now(),
        };
        assert_eq!(timer_event.event_type(), "timer");
    }

    #[test]
    fn test_file_change_type_display() {
        assert_eq!(format!("{}", FileChangeType::Created), "created");
        assert_eq!(format!("{}", FileChangeType::Modified), "modified");
        assert_eq!(format!("{}", FileChangeType::Deleted), "deleted");

        let renamed = FileChangeType::Renamed {
            from: PathBuf::from("/old"),
        };
        assert_eq!(format!("{}", renamed), "renamed from /old");
    }

    #[test]
    fn test_serialization() {
        let event = SystemEvent::File {
            path: PathBuf::from("/test.txt"),
            change_type: FileChangeType::Modified,
            timestamp: Utc::now(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SystemEvent = serde_json::from_str(&json).unwrap();

        match deserialized {
            SystemEvent::File {
                path, change_type, ..
            } => {
                assert_eq!(path, PathBuf::from("/test.txt"));
                assert_eq!(change_type, FileChangeType::Modified);
            }
            _ => panic!("Wrong event type"),
        }
    }
}
