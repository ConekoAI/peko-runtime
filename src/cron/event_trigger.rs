//! Event trigger service for scheduler
//!
//! Subscribes to system events and triggers scheduled jobs
//! when matching events occur.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::cron::{CronJob, CronScheduler};
use crate::orchestration::events::SystemEvent;

/// Service that listens for system events and triggers matching jobs
pub struct EventTriggerService {
    /// Reference to scheduler for job execution
    scheduler: Arc<CronScheduler>,
    /// Event receiver channel
    event_rx: mpsc::Receiver<SystemEvent>,
    /// Map of event types to job IDs
    event_jobs: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Job trigger sender
    trigger_tx: mpsc::Sender<String>,
}

impl EventTriggerService {
    /// Create a new event trigger service
    pub fn new(
        scheduler: Arc<CronScheduler>,
        event_rx: mpsc::Receiver<SystemEvent>,
        trigger_tx: mpsc::Sender<String>,
    ) -> Self {
        Self {
            scheduler,
            event_rx,
            event_jobs: Arc::new(RwLock::new(HashMap::new())),
            trigger_tx,
        }
    }

    /// Register event-triggered jobs from scheduler
    pub async fn load_event_jobs(&self) -> anyhow::Result<()> {
        let jobs = self.scheduler.list_jobs(false)?;
        let mut event_jobs = self.event_jobs.write().await;
        event_jobs.clear();

        for job in jobs {
            if let crate::cron::ScheduleKind::Event { event_type, .. } = &job.schedule {
                event_jobs
                    .entry(event_type.clone())
                    .or_default()
                    .push(job.id.clone());
                debug!(
                    "Registered event job: {} for event type: {}",
                    job.id, event_type
                );
            }
        }

        info!("Loaded {} event-triggered jobs", event_jobs.len());
        Ok(())
    }

    /// Start the event trigger service
    pub async fn run(mut self) {
        info!("EventTriggerService started");

        while let Some(event) = self.event_rx.recv().await {
            let event_type = event.event_type().to_string();
            debug!("Received event: {}", event_type);

            // Check if we have jobs for this event type
            let jobs_to_trigger = {
                let event_jobs = self.event_jobs.read().await;
                event_jobs.get(&event_type).cloned()
            };

            if let Some(job_ids) = jobs_to_trigger {
                for job_id in job_ids {
                    // Check filter if present
                    if let Ok(Some(job)) = self.scheduler.get_job(&job_id) {
                        if let crate::cron::ScheduleKind::Event { filter, once, .. } = &job.schedule {
                            // Check if filter matches
                            if let Some(filter) = filter {
                                if !Self::event_matches_filter(&event, filter) {
                                    debug!(
                                        "Job {} filter did not match event",
                                        job_id
                                    );
                                    continue;
                                }
                            }

                            // Trigger the job
                            if let Err(e) = self.trigger_tx.send(job_id.clone()).await {
                                error!("Failed to trigger job {}: {}", job_id, e);
                                continue;
                            }

                            // If once flag is set, disable the job
                            if *once {
                                if let Err(e) = self.scheduler.set_job_enabled(&job_id, false) {
                                    warn!("Failed to disable one-time job {}: {}", job_id, e);
                                } else {
                                    info!("Disabled one-time event job: {}", job_id);
                                }
                            }
                        }
                    }
                }
            }
        }

        info!("EventTriggerService stopped");
    }

    /// Check if an event matches a filter
    fn event_matches_filter(event: &SystemEvent, filter: &serde_json::Value) -> bool {
        // Simple filter matching - check if filter is subset of event JSON
        let event_json = match serde_json::to_value(event) {
            Ok(v) => v,
            Err(_) => return false,
        };

        Self::json_matches(&event_json, filter)
    }

    /// Check if target JSON contains all fields from filter with matching values
    fn json_matches(target: &serde_json::Value, filter: &serde_json::Value) -> bool {
        match (target, filter) {
            (serde_json::Value::Object(target_obj), serde_json::Value::Object(filter_obj)) => {
                for (key, filter_val) in filter_obj {
                    match target_obj.get(key) {
                        Some(target_val) => {
                            if !Self::json_matches(target_val, filter_val) {
                                return false;
                            }
                        }
                        None => return false,
                    }
                }
                true
            }
            (a, b) => a == b,
        }
    }

    /// Refresh event jobs (call when jobs are added/removed)
    pub async fn refresh(&self) {
        if let Err(e) = self.load_event_jobs().await {
            error!("Failed to refresh event jobs: {}", e);
        }
    }
}

/// Builder for EventTriggerService
pub struct EventTriggerServiceBuilder {
    scheduler: Option<Arc<CronScheduler>>,
    event_rx: Option<mpsc::Receiver<SystemEvent>>,
    trigger_tx: Option<mpsc::Sender<String>>,
}

impl EventTriggerServiceBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            scheduler: None,
            event_rx: None,
            trigger_tx: None,
        }
    }

    /// Set the scheduler
    pub fn with_scheduler(mut self, scheduler: Arc<CronScheduler>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Set the event receiver
    pub fn with_event_receiver(mut self, rx: mpsc::Receiver<SystemEvent>) -> Self {
        self.event_rx = Some(rx);
        self
    }

    /// Set the trigger sender
    pub fn with_trigger_sender(mut self, tx: mpsc::Sender<String>) -> Self {
        self.trigger_tx = Some(tx);
        self
    }

    /// Build the service
    pub fn build(self) -> anyhow::Result<EventTriggerService> {
        let scheduler = self.scheduler.ok_or_else(|| anyhow::anyhow!("Scheduler required"))?;
        let event_rx = self.event_rx.ok_or_else(|| anyhow::anyhow!("Event receiver required"))?;
        let trigger_tx = self.trigger_tx.ok_or_else(|| anyhow::anyhow!("Trigger sender required"))?;

        Ok(EventTriggerService::new(scheduler, event_rx, trigger_tx))
    }
}

impl Default for EventTriggerServiceBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::events::FileChangeType;
    use std::collections::HashMap;

    #[test]
    fn test_json_matches_simple() {
        let target = serde_json::json!({
            "source": "github",
            "event": "push"
        });
        let filter = serde_json::json!({
            "source": "github"
        });

        assert!(EventTriggerService::json_matches(&target, &filter));
    }

    #[test]
    fn test_json_matches_nested() {
        let target = serde_json::json!({
            "payload": {
                "action": "created",
                "user": "alice"
            }
        });
        let filter = serde_json::json!({
            "payload": {
                "action": "created"
            }
        });

        assert!(EventTriggerService::json_matches(&target, &filter));
    }

    #[test]
    fn test_json_matches_no_match() {
        let target = serde_json::json!({
            "source": "github",
            "event": "pull_request"
        });
        let filter = serde_json::json!({
            "source": "gitlab"
        });

        assert!(!EventTriggerService::json_matches(&target, &filter));
    }

    #[test]
    fn test_event_matches_filter() {
        let event = SystemEvent::Webhook {
            source: "github".to_string(),
            route: "/webhook/github".to_string(),
            payload: serde_json::json!({"action": "push"}),
            headers: HashMap::new(),
            timestamp: chrono::Utc::now(),
        };

        let filter = serde_json::json!({
            "Webhook": {
                "source": "github"
            }
        });

        assert!(EventTriggerService::event_matches_filter(&event, &filter));
    }

    #[test]
    fn test_event_matches_no_match() {
        let event = SystemEvent::File {
            path: std::path::PathBuf::from("/tmp/test.txt"),
            change_type: FileChangeType::Modified,
            timestamp: chrono::Utc::now(),
        };

        let filter = serde_json::json!({
            "Webhook": {}
        });

        assert!(!EventTriggerService::event_matches_filter(&event, &filter));
    }
}
