//! Event Subscriber for internal cross-module communication
//!
//! Provides a broadcast-based event bus for internal system events.

use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, error, info, warn};

use crate::orchestration::events::SystemEvent;

/// Internal event bus for cross-module communication
pub struct EventSubscriber {
    /// Broadcast sender for internal events
    sender: broadcast::Sender<SystemEvent>,
    /// Event channel receiver (for integration with EventRouter)
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
}

impl EventSubscriber {
    /// Create a new event subscriber with broadcast channel
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(100);
        
        info!("EventSubscriber created with broadcast channel");
        
        Self { 
            sender,
            event_rx: None,
        }
    }
    
    /// Create with an mpsc channel for EventRouter integration
    pub fn with_event_channel(event_rx: mpsc::Receiver<SystemEvent>) -> Self {
        let (sender, _receiver) = broadcast::channel(100);
        
        info!("EventSubscriber created with EventRouter integration");
        
        Self { 
            sender,
            event_rx: Some(event_rx),
        }
    }
    
    /// Publish an internal event to all subscribers
    pub fn publish(&self, event: SystemEvent) -> anyhow::Result<usize> {
        let event_type = event.event_type().to_string();
        match self.sender.send(event) {
            Ok(receiver_count) => {
                debug!("Published {} event to {} receivers", event_type, receiver_count);
                Ok(receiver_count)
            }
            Err(e) => {
                warn!("Failed to publish event: no receivers: {}", e);
                Err(anyhow::anyhow!("No receivers for event"))
            }
        }
    }
    
    /// Subscribe to internal events
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.sender.subscribe()
    }
    
    /// Get the sender for external use
    pub fn sender(&self) -> broadcast::Sender<SystemEvent> {
        self.sender.clone()
    }
    
    /// Get the number of active subscribers
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
    
    /// Start forwarding events from the mpsc channel to broadcast
    /// This bridges EventRouter events to internal subscribers
    pub fn start_forwarding(&mut self) {
        if let Some(mut rx) = self.event_rx.take() {
            let sender = self.sender.clone();
            
            tokio::spawn(async move {
                info!("EventSubscriber forwarding task started");
                
                while let Some(event) = rx.recv().await {
                    let event_type = event.event_type().to_string();
                    
                    if let Err(e) = sender.send(event) {
                        warn!("Failed to forward {} event: no receivers", event_type);
                    } else {
                        debug!("Forwarded {} event to broadcast", event_type);
                    }
                }
                
                info!("EventSubscriber forwarding task stopped");
            });
        }
    }
    
    /// Start a task that forwards broadcast events to an mpsc sender
    /// This allows external components to receive events via channel
    pub fn start_receiver_forwarding(&self, event_tx: mpsc::Sender<SystemEvent>) {
        let mut rx = self.subscribe();
        
        tokio::spawn(async move {
            info!("EventSubscriber receiver forwarding task started");
            
            while let Ok(event) = rx.recv().await {
                if let Err(e) = event_tx.send(event).await {
                    warn!("Failed to forward event to channel: {}", e);
                    break;
                }
            }
            
            info!("EventSubscriber receiver forwarding task stopped");
        });
    }
}

impl Default for EventSubscriber {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for creating EventSubscriber with multiple sources
pub struct EventSubscriberBuilder {
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
    capacity: usize,
}

impl EventSubscriberBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            event_rx: None,
            capacity: 100,
        }
    }
    
    /// Set the event channel for EventRouter integration
    pub fn with_event_channel(mut self, rx: mpsc::Receiver<SystemEvent>) -> Self {
        self.event_rx = Some(rx);
        self
    }
    
    /// Set the broadcast channel capacity
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }
    
    /// Build the EventSubscriber
    pub fn build(self) -> EventSubscriber {
        let (sender, _receiver) = broadcast::channel(self.capacity);
        
        EventSubscriber {
            sender,
            event_rx: self.event_rx,
        }
    }
}

impl Default for EventSubscriberBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_event_subscriber_creation() {
        let subscriber = EventSubscriber::new();
        assert_eq!(subscriber.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_publish_and_subscribe() {
        let subscriber = EventSubscriber::new();
        let mut rx = subscriber.subscribe();
        
        // Publish an event
        let event = SystemEvent::Internal {
            event_type: "test".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({"key": "value"}),
            timestamp: chrono::Utc::now(),
        };
        
        let count = subscriber.publish(event.clone()).unwrap();
        assert_eq!(count, 1); // One subscriber
        
        // Receive the event
        let received = rx.recv().await.unwrap();
        match received {
            SystemEvent::Internal { event_type, source, .. } => {
                assert_eq!(event_type, "test");
                assert_eq!(source, "test");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let subscriber = EventSubscriber::new();
        let mut rx1 = subscriber.subscribe();
        let mut rx2 = subscriber.subscribe();
        let mut rx3 = subscriber.subscribe();
        
        assert_eq!(subscriber.subscriber_count(), 3);
        
        // Publish event
        let event = SystemEvent::Internal {
            event_type: "broadcast".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };
        
        let count = subscriber.publish(event).unwrap();
        assert_eq!(count, 3);
        
        // All subscribers should receive
        let _ = rx1.recv().await.unwrap();
        let _ = rx2.recv().await.unwrap();
        let _ = rx3.recv().await.unwrap();
    }

    #[tokio::test]
    async fn test_forwarding_to_mpsc() {
        let (tx, mut rx) = mpsc::channel(10);
        
        let subscriber = EventSubscriber::new();
        subscriber.start_receiver_forwarding(tx);
        
        // Give task time to start
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        
        // Publish event
        let event = SystemEvent::Internal {
            event_type: "forwarded".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };
        
        subscriber.publish(event).unwrap();
        
        // Should receive via mpsc
        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            rx.recv()
        ).await.unwrap().unwrap();
        
        match received {
            SystemEvent::Internal { event_type, .. } => {
                assert_eq!(event_type, "forwarded");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_forwarding_from_mpsc() {
        let (event_tx, event_rx) = mpsc::channel(10);
        
        let mut subscriber = EventSubscriber::with_event_channel(event_rx);
        let mut broadcast_rx = subscriber.subscribe();
        
        // Start forwarding
        subscriber.start_forwarding();
        
        // Send via mpsc
        let event = SystemEvent::Internal {
            event_type: "mpsc-source".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };
        
        event_tx.send(event).await.unwrap();
        
        // Should receive via broadcast
        let received = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            broadcast_rx.recv()
        ).await.unwrap().unwrap();
        
        match received {
            SystemEvent::Internal { event_type, .. } => {
                assert_eq!(event_type, "mpsc-source");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_publish_no_subscribers() {
        let subscriber = EventSubscriber::new();
        // No subscribers
        
        let event = SystemEvent::Internal {
            event_type: "test".to_string(),
            source: "test".to_string(),
            payload: serde_json::json!({}),
            timestamp: chrono::Utc::now(),
        };
        
        // Should fail when no subscribers
        let result = subscriber.publish(event);
        assert!(result.is_err());
    }

    #[test]
    fn test_builder() {
        let subscriber = EventSubscriberBuilder::new()
            .with_capacity(200)
            .build();
        
        assert_eq!(subscriber.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_all_event_types() {
        let subscriber = EventSubscriber::new();
        let mut rx = subscriber.subscribe();
        
        // Test File event
        let file_event = SystemEvent::File {
            path: std::path::PathBuf::from("/tmp/test.txt"),
            change_type: crate::orchestration::events::FileChangeType::Modified,
            timestamp: chrono::Utc::now(),
        };
        subscriber.publish(file_event).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "file");
        
        // Test Webhook event
        let webhook_event = SystemEvent::Webhook {
            source: "github".to_string(),
            route: "/webhook/github".to_string(),
            payload: serde_json::json!({"action": "push"}),
            headers: HashMap::new(),
            timestamp: chrono::Utc::now(),
        };
        subscriber.publish(webhook_event).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "webhook");
        
        // Test Timer event
        let timer_event = SystemEvent::Timer {
            schedule_id: "schedule-1".to_string(),
            task_id: "task-1".to_string(),
            fired_at: chrono::Utc::now(),
        };
        subscriber.publish(timer_event).unwrap();
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "timer");
    }
}
