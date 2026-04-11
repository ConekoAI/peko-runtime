//! System event types for cron and event-triggered jobs
//!
//! Defines the taxonomy of events that can trigger scheduled jobs.

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
            SystemEvent::Timer {
                schedule_id, task_id, ..
            } => {
                write!(f, "Timer {task_id} for {schedule_id}")
            }
        }
    }
}
