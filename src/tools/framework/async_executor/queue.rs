//! Queue management for async result delivery

use super::event_bus::AsyncTaskCompletionEvent;
use super::types::AsyncResultDeliveryMode;
use crate::common::registry::SimpleRegistry;

/// Queue for managing async result delivery (inspired by OpenClaw)
#[derive(Debug)]
pub struct AsyncResultQueue {
    #[allow(dead_code)]
    session_key: String,
    mode: AsyncResultDeliveryMode,
    items: Vec<AsyncTaskCompletionEvent>,
    /// Whether the parent session is currently executing
    is_parent_busy: bool,
}

impl AsyncResultQueue {
    #[must_use]
    pub fn new(session_key: String, mode: AsyncResultDeliveryMode) -> Self {
        Self {
            session_key,
            mode,
            items: Vec::new(),
            is_parent_busy: false,
        }
    }

    pub fn enqueue(&mut self, event: AsyncTaskCompletionEvent) {
        self.items.push(event);
    }

    /// Process queue based on delivery mode
    pub fn process(&mut self) -> Vec<AsyncTaskCompletionEvent> {
        if self.items.is_empty() {
            return Vec::new();
        }

        match self.mode {
            AsyncResultDeliveryMode::QueueWhenBusy => {
                if self.is_parent_busy {
                    // Keep items queued
                    Vec::new()
                } else {
                    // Deliver all items
                    std::mem::take(&mut self.items)
                }
            }
            AsyncResultDeliveryMode::Collect => {
                // Collect mode: batch all items into a single composite event
                if self.items.len() >= 2 {
                    let batched = self.create_batch_event();
                    self.items.clear();
                    if let Some(event) = batched {
                        return vec![event];
                    }
                }
                std::mem::take(&mut self.items)
            }
            AsyncResultDeliveryMode::Interrupt | AsyncResultDeliveryMode::Steer => {
                // Deliver immediately
                std::mem::take(&mut self.items)
            }
        }
    }

    fn create_batch_event(&self) -> Option<AsyncTaskCompletionEvent> {
        if self.items.is_empty() {
            return None;
        }

        let first = self.items.first()?;
        let combined_results: Vec<String> = self
            .items
            .iter()
            .map(|e| format!("## {} ({}):\n{}", e.tool_name, e.task_id, e.result_message))
            .collect();

        Some(AsyncTaskCompletionEvent {
            task_id: "batched".to_string(),
            tool_name: "async_batch".to_string(),
            result_message: format!(
                "[Multiple async tasks completed]\n\n{}",
                combined_results.join("\n\n")
            ),
            parent_session_key: first.parent_session_key.clone(),
            label: Some("batched".to_string()),
        })
    }

    pub fn set_parent_busy(&mut self, busy: bool) {
        self.is_parent_busy = busy;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Manager for async result queues per session.
///
/// Uses [`SimpleRegistry`] for queue storage to avoid hand-rolled `HashMap` patterns.
#[derive(Debug, Default)]
pub struct AsyncResultQueueManager {
    queues: SimpleRegistry<String, AsyncResultQueue>,
}

impl AsyncResultQueueManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            queues: SimpleRegistry::new(),
        }
    }

    pub fn get_or_create(
        &mut self,
        session_key: &str,
        mode: AsyncResultDeliveryMode,
    ) -> &mut AsyncResultQueue {
        self.queues
            .entry(session_key.to_string())
            .or_insert_with(|| AsyncResultQueue::new(session_key.to_string(), mode))
    }

    pub fn set_parent_busy(&mut self, session_key: &str, busy: bool) {
        if let Some(queue) = self.queues.get_mut(&session_key.to_string()) {
            queue.set_parent_busy(busy);
        }
    }

    pub fn enqueue(&mut self, event: AsyncTaskCompletionEvent) {
        let session_key = event.parent_session_key.clone();
        let queue = self.queues.entry(session_key.clone()).or_insert_with(|| {
            AsyncResultQueue::new(session_key, AsyncResultDeliveryMode::default())
        });
        queue.enqueue(event);
    }

    pub fn process_queue(&mut self, session_key: &str) -> Vec<AsyncTaskCompletionEvent> {
        self.queues
            .get_mut(&session_key.to_string())
            .map(AsyncResultQueue::process)
            .unwrap_or_default()
    }

    pub fn cleanup_empty_queues(&mut self) {
        self.queues.retain(|_, q| !q.is_empty());
    }
}

pub type SharedAsyncResultQueueManager = std::sync::Arc<tokio::sync::RwLock<AsyncResultQueueManager>>;
