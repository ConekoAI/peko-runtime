//! Lifecycle Event Emitter
//!
//! Emits system events during instance and team lifecycle transitions.

use std::sync::Arc;
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::hooks::{
    EventBroadcaster, InstanceSnapshot, SystemEvent, SystemEventType, TeamSnapshot,
};

/// Lifecycle event emitter
pub struct LifecycleEmitter {
    broadcaster: Arc<EventBroadcaster>,
}

impl LifecycleEmitter {
    /// Create a new lifecycle emitter
    pub fn new(broadcaster: Arc<EventBroadcaster>) -> Self {
        Self { broadcaster }
    }

    /// Emit instance created event
    pub async fn emit_instance_created(&self, instance: InstanceSnapshot) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceCreated { instance },
        };

        self.emit(event).await;
    }

    /// Emit instance started event
    pub async fn emit_instance_started(&self, instance_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceStarted {
                instance_id: instance_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit instance status changed event
    pub async fn emit_instance_status_changed(
        &self,
        instance_id: &str,
        previous_status: &str,
        new_status: &str,
    ) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceStatusChanged {
                instance_id: instance_id.to_string(),
                previous_status: previous_status.to_string(),
                new_status: new_status.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit instance stopped event
    pub async fn emit_instance_stopped(&self, instance_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceStopped {
                instance_id: instance_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit instance error event
    pub async fn emit_instance_error(&self, instance_id: &str, error: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceError {
                instance_id: instance_id.to_string(),
                error: error.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit instance upgraded event
    pub async fn emit_instance_upgraded(
        &self,
        instance_id: &str,
        previous_digest: &str,
        new_digest: &str,
    ) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceUpgraded {
                instance_id: instance_id.to_string(),
                previous_digest: previous_digest.to_string(),
                new_digest: new_digest.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit instance removed event
    pub async fn emit_instance_removed(&self, instance_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::InstanceRemoved {
                instance_id: instance_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit team created event
    pub async fn emit_team_created(&self, team: TeamSnapshot) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::TeamCreated { team },
        };

        self.emit(event).await;
    }

    /// Emit team ready event
    pub async fn emit_team_ready(&self, team_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::TeamReady {
                team_id: team_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit team scaled event
    pub async fn emit_team_scaled(
        &self,
        team_id: &str,
        agent_name: &str,
        previous_count: u32,
        new_count: u32,
    ) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::TeamScaled {
                team_id: team_id.to_string(),
                agent_name: agent_name.to_string(),
                previous_count,
                new_count,
            },
        };

        self.emit(event).await;
    }

    /// Emit team stopped event
    pub async fn emit_team_stopped(&self, team_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::TeamStopped {
                team_id: team_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit team removed event
    pub async fn emit_team_removed(&self, team_id: &str) {
        let event = SystemEvent {
            id: format!("evt_{}", Uuid::new_v4().simple()),
            ts: chrono::Utc::now().to_rfc3339(),
            event_type: SystemEventType::TeamRemoved {
                team_id: team_id.to_string(),
            },
        };

        self.emit(event).await;
    }

    /// Emit an event
    async fn emit(&self, event: SystemEvent) {
        info!("Emitting lifecycle event: {:?}", event.event_type);
        self.broadcaster.broadcast(event).await;
    }
}

impl Clone for LifecycleEmitter {
    fn clone(&self) -> Self {
        Self {
            broadcaster: Arc::clone(&self.broadcaster),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_emit_instance_events() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let emitter = LifecycleEmitter::new(broadcaster.clone());

        // Subscribe to events
        let mut rx = broadcaster.subscribe().await;

        // Emit instance started
        emitter.emit_instance_started("inst_123").await;

        // Should receive event
        let event = rx.recv().await.unwrap();
        match event.event_type {
            SystemEventType::InstanceStarted { instance_id } => {
                assert_eq!(instance_id, "inst_123");
            }
            _ => panic!("Expected InstanceStarted event"),
        }
    }

    #[tokio::test]
    async fn test_emit_instance_status_changed() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let emitter = LifecycleEmitter::new(broadcaster.clone());

        let mut rx = broadcaster.subscribe().await;

        emitter
            .emit_instance_status_changed("inst_123", "starting", "running")
            .await;

        let event = rx.recv().await.unwrap();
        match event.event_type {
            SystemEventType::InstanceStatusChanged {
                instance_id,
                previous_status,
                new_status,
            } => {
                assert_eq!(instance_id, "inst_123");
                assert_eq!(previous_status, "starting");
                assert_eq!(new_status, "running");
            }
            _ => panic!("Expected InstanceStatusChanged event"),
        }
    }

    #[tokio::test]
    async fn test_emit_team_events() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let emitter = LifecycleEmitter::new(broadcaster.clone());

        let mut rx = broadcaster.subscribe().await;

        let team = TeamSnapshot {
            id: "team_456".to_string(),
            name: "test-team".to_string(),
            status: "starting".to_string(),
            agent_count: 3,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        emitter.emit_team_created(team).await;

        let event = rx.recv().await.unwrap();
        match event.event_type {
            SystemEventType::TeamCreated { team } => {
                assert_eq!(team.id, "team_456");
                assert_eq!(team.name, "test-team");
            }
            _ => panic!("Expected TeamCreated event"),
        }
    }

    #[tokio::test]
    async fn test_emit_team_scaled() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let emitter = LifecycleEmitter::new(broadcaster.clone());

        let mut rx = broadcaster.subscribe().await;

        emitter.emit_team_scaled("team_456", "worker", 3, 5).await;

        let event = rx.recv().await.unwrap();
        match event.event_type {
            SystemEventType::TeamScaled {
                team_id,
                agent_name,
                previous_count,
                new_count,
            } => {
                assert_eq!(team_id, "team_456");
                assert_eq!(agent_name, "worker");
                assert_eq!(previous_count, 3);
                assert_eq!(new_count, 5);
            }
            _ => panic!("Expected TeamScaled event"),
        }
    }

    #[tokio::test]
    async fn test_clone_emitter() {
        let broadcaster = Arc::new(EventBroadcaster::new());
        let emitter1 = LifecycleEmitter::new(broadcaster);
        let emitter2 = emitter1.clone();

        // Both should work independently
        let mut rx = emitter1.broadcaster.subscribe().await;

        emitter2.emit_instance_stopped("inst_789").await;

        let event = rx.recv().await.unwrap();
        match event.event_type {
            SystemEventType::InstanceStopped { instance_id } => {
                assert_eq!(instance_id, "inst_789");
            }
            _ => panic!("Expected InstanceStopped event"),
        }
    }
}
