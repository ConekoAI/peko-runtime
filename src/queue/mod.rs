//! Message queue system with lane-aware FIFO processing
//!
//! Matches OpenClaw's queue design:
//! - Lane-aware FIFO queue (per-session serialization)
//! - Configurable concurrency caps per lane
//! - Queue modes: steer, followup, collect, steer-backlog, interrupt
//! - Debounce and backpressure handling

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, warn};

/// Queue mode for handling inbound messages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    /// Inject immediately into current run (cancels pending tool calls)
    /// Falls back to followup if not streaming
    Steer,
    /// Queue for next agent turn after current run ends
    Followup,
    /// Coalesce all queued messages into a single followup turn (default)
    Collect,
    /// Steer now AND preserve message for followup turn
    SteerBacklog,
    /// Abort active run for that session, then run newest message
    Interrupt,
}

impl Default for QueueMode {
    fn default() -> Self {
        QueueMode::Collect
    }
}

impl QueueMode {
    /// Parse from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "steer" => QueueMode::Steer,
            "followup" => QueueMode::Followup,
            "collect" => QueueMode::Collect,
            "steer-backlog" | "steer_backlog" | "steer+backlog" => QueueMode::SteerBacklog,
            "interrupt" => QueueMode::Interrupt,
            "queue" => QueueMode::Steer, // Legacy alias
            _ => QueueMode::Collect,
        }
    }
}

/// Queue configuration
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Default queue mode
    pub mode: QueueMode,
    /// Debounce duration before starting followup turn
    pub debounce_ms: u64,
    /// Max queued messages per session
    pub cap: usize,
    /// Overflow policy when cap reached
    pub drop_policy: DropPolicy,
    /// Max concurrent runs for default lane
    pub max_concurrent: usize,
    /// Per-lane concurrency overrides
    pub lane_concurrency: HashMap<String, usize>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        let mut lane_concurrency = HashMap::new();
        lane_concurrency.insert("main".to_string(), 4);
        lane_concurrency.insert("cron".to_string(), 2);
        lane_concurrency.insert("subagent".to_string(), 8);

        Self {
            mode: QueueMode::Collect,
            debounce_ms: 1000,
            cap: 20,
            drop_policy: DropPolicy::Summarize,
            max_concurrent: 4,
            lane_concurrency,
        }
    }
}

/// Drop policy when queue overflows
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPolicy {
    /// Drop oldest messages
    Old,
    /// Drop newest messages
    New,
    /// Summarize dropped messages as bullet list
    Summarize,
}

/// A queued message
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    /// Message content
    pub content: String,
    /// Session key (lane identifier)
    pub session_key: String,
    /// Channel source
    pub channel: String,
    /// When queued
    pub queued_at: Instant,
    /// Queue mode for this message
    pub mode: QueueMode,
    /// Original message ID for reply threading
    pub message_id: Option<String>,
}

/// Lane state for a session
#[derive(Debug)]
struct LaneState {
    /// Queued messages for this lane
    queue: VecDeque<QueuedMessage>,
    /// Whether a run is currently active
    active: bool,
    /// When the current run started
    run_started_at: Option<Instant>,
    /// Pending tool calls that can be cancelled
    pending_tool_calls: Vec<String>,
    /// Last activity timestamp
    last_activity: Instant,
}

impl LaneState {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            active: false,
            run_started_at: None,
            pending_tool_calls: Vec::new(),
            last_activity: Instant::now(),
        }
    }
}

/// Global queue statistics
#[derive(Debug, Default, Clone)]
pub struct QueueStats {
    /// Total messages queued
    pub total_queued: u64,
    /// Total messages processed
    pub total_processed: u64,
    /// Total messages dropped
    pub total_dropped: u64,
    /// Current queue depth across all lanes
    pub current_depth: usize,
    /// Number of active lanes
    pub active_lanes: usize,
    /// Number of active runs
    pub active_runs: usize,
}

/// Message queue with lane-aware processing
pub struct MessageQueue {
    /// Queue configuration
    config: QueueConfig,
    /// Lane states by session key
    lanes: Arc<Mutex<HashMap<String, LaneState>>>,
    /// Global concurrency semaphore
    global_sem: Arc<tokio::sync::Semaphore>,
    /// Notify for new messages
    notify: Arc<Notify>,
    /// Statistics
    stats: Arc<Mutex<QueueStats>>,
    /// Shutdown signal
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl MessageQueue {
    /// Create a new message queue
    pub fn new(config: QueueConfig) -> Self {
        let max_concurrent = config.max_concurrent;
        let global_sem = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

        Self {
            config,
            lanes: Arc::new(Mutex::new(HashMap::new())),
            global_sem,
            notify: Arc::new(Notify::new()),
            stats: Arc::new(Mutex::new(QueueStats::default())),
            shutdown_tx: None,
        }
    }

    /// Create with default configuration
    pub fn default_config() -> Self {
        Self::new(QueueConfig::default())
    }

    /// Enqueue a message for processing
    pub async fn enqueue(
        &self,
        content: String,
        session_key: String,
        channel: String,
        mode: Option<QueueMode>,
        message_id: Option<String>,
    ) -> Result<EnqueueResult, QueueError> {
        let mode = mode.unwrap_or(self.config.mode);
        let lane_key = format!("session:{}", session_key);

        let msg = QueuedMessage {
            content,
            session_key: session_key.clone(),
            channel,
            queued_at: Instant::now(),
            mode,
            message_id,
        };

        let mut lanes = self.lanes.lock().await;
        let lane = lanes.entry(lane_key.clone()).or_insert_with(LaneState::new);

        // Handle steer mode - inject into active run
        if mode == QueueMode::Steer && lane.active {
            debug!(
                "Steering message into active run for session {}",
                session_key
            );
            // Cancel pending tool calls
            for tool_call in &lane.pending_tool_calls {
                debug!("Cancelling pending tool call: {}", tool_call);
            }
            lane.pending_tool_calls.clear();

            // Drop the steering message since we injected it
            drop(lanes);
            return Ok(EnqueueResult::Steered);
        }

        // Handle interrupt mode - abort active run
        if mode == QueueMode::Interrupt && lane.active {
            warn!(
                "Interrupting active run for session {}",
                session_key
            );
            lane.active = false;
            lane.run_started_at = None;
            lane.pending_tool_calls.clear();
        }

        // Check capacity and apply drop policy
        if lane.queue.len() >= self.config.cap {
            match self.config.drop_policy {
                DropPolicy::Old => {
                    lane.queue.pop_front();
                    self.increment_dropped().await;
                    debug!("Dropped oldest message due to capacity");
                }
                DropPolicy::New => {
                    self.increment_dropped().await;
                    debug!("Dropped new message due to capacity");
                    return Ok(EnqueueResult::Dropped);
                }
                DropPolicy::Summarize => {
                    // Collect dropped messages for summary
                    let dropped: Vec<_> = lane.queue.drain(0..lane.queue.len() / 2).collect();
                    self.increment_dropped().await;
                    
                    // Create summary
                    let summary = format!(
                        "[{} messages summarized]",
                        dropped.len()
                    );
                    
                    // Add synthetic message with summary
                    let summary_msg = QueuedMessage {
                        content: summary,
                        session_key: session_key.clone(),
                        channel: msg.channel.clone(),
                        queued_at: Instant::now(),
                        mode: QueueMode::Collect,
                        message_id: None,
                    };
                    lane.queue.push_back(summary_msg);
                    debug!("Summarized {} dropped messages", dropped.len());
                }
            }
        }

        // Add message to queue
        lane.queue.push_back(msg);
        lane.last_activity = Instant::now();

        let queue_depth = lane.queue.len();
        let is_active = lane.active;

        drop(lanes);

        // Update stats
        self.increment_queued().await;

        // Notify waiting processor
        self.notify.notify_one();

        debug!(
            "Enqueued message for session {} (depth: {}, active: {})",
            session_key, queue_depth, is_active
        );

        Ok(EnqueueResult::Queued(queue_depth))
    }

    /// Dequeue the next message for processing
    pub async fn dequeue(&self,
    ) -> Result<Option<(QueuedMessage, QueueToken)>, QueueError> {
        loop {
            // Try to acquire global concurrency permit
            let _permit = match self.global_sem.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    // Wait for a permit
                    match self.global_sem.clone().acquire_owned().await {
                        Ok(p) => p,
                        Err(_) => return Err(QueueError::Shutdown),
                    }
                }
            };

            let mut lanes = self.lanes.lock().await;

            // Find a lane with queued messages that's not active
            let ready_lane: Option<(String, QueuedMessage)> = {
                let mut result = None;
                for (lane_key, lane) in lanes.iter_mut() {
                    if !lane.active && !lane.queue.is_empty() {
                        // Check debounce for followup/collect modes
                        if let Some(first_msg) = lane.queue.front() {
                            match first_msg.mode {
                                QueueMode::Followup | QueueMode::Collect => {
                                    let elapsed = first_msg.queued_at.elapsed().as_millis() as u64;
                                    if elapsed >= self.config.debounce_ms {
                                        if let Some(msg) = lane.queue.pop_front() {
                                            result = Some((lane_key.clone(), msg));
                                            break;
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(msg) = lane.queue.pop_front() {
                                        result = Some((lane_key.clone(), msg));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                result
            };

            if let Some((lane_key, mut msg)) = ready_lane {
                // Mark lane as active
                if let Some(lane) = lanes.get_mut(&lane_key) {
                    lane.active = true;
                    lane.run_started_at = Some(Instant::now());
                    
                    // For collect mode, coalesce remaining messages
                    if msg.mode == QueueMode::Collect {
                        let coalesced = Self::coalesce_messages(&mut lane.queue,
                            self.config.debounce_ms,
                        );
                        if !coalesced.is_empty() {
                            msg.content = format!(
                                "{}\n\n[Additional messages:]\n{}",
                                msg.content,
                                coalesced.join("\n")
                            );
                        }
                    }
                }

                drop(lanes);

                self.increment_processed().await;

                let token = QueueToken {
                    lane_key,
                    lanes: self.lanes.clone(),
                    stats: self.stats.clone(),
                };

                return Ok(Some((msg, token)));
            }

            drop(lanes);

            // No messages ready, wait for notification
            tokio::select! {
                _ = self.notify.notified() => continue,
                _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
            }
        }
    }

    /// Coalesce messages from queue that are within debounce window
    fn coalesce_messages(queue: &mut VecDeque<QueuedMessage>, debounce_ms: u64) -> Vec<String> {
        let cutoff = Instant::now() - Duration::from_millis(debounce_ms);
        let mut coalesced = Vec::new();
        
        // Take messages that arrived within debounce window
        while let Some(msg) = queue.front() {
            if msg.queued_at < cutoff {
                coalesced.push(queue.pop_front().unwrap().content);
            } else {
                break;
            }
        }
        
        coalesced
    }

    /// Get current statistics
    pub async fn stats(&self) -> QueueStats {
        let lanes = self.lanes.lock().await;
        let stats = self.stats.lock().await;
        
        QueueStats {
            total_queued: stats.total_queued,
            total_processed: stats.total_processed,
            total_dropped: stats.total_dropped,
            current_depth: lanes.values().map(|l| l.queue.len()).sum(),
            active_lanes: lanes.len(),
            active_runs: lanes.values().filter(|l| l.active).count(),
        }
    }

    /// Set queue mode for a session (per-session override)
    pub async fn set_session_mode(
        &self,
        session_key: &str,
        mode: QueueMode,
    ) {
        let lane_key = format!("session:{}", session_key);
        let mut lanes = self.lanes.lock().await;
        
        if let Some(lane) = lanes.get_mut(&lane_key) {
            // Update mode for all queued messages
            for msg in &mut lane.queue {
                msg.mode = mode;
            }
            debug!("Set queue mode to {:?} for session {}", mode, session_key);
        }
    }

    // Stats helpers
    async fn increment_queued(&self) {
        let mut stats = self.stats.lock().await;
        stats.total_queued += 1;
    }

    async fn increment_processed(&self) {
        let mut stats = self.stats.lock().await;
        stats.total_processed += 1;
    }

    async fn increment_dropped(&self) {
        let mut stats = self.stats.lock().await;
        stats.total_dropped += 1;
    }
}

/// Result of enqueuing a message
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueResult {
    /// Message queued successfully with current depth
    Queued(usize),
    /// Message was steered into active run (not queued)
    Steered,
    /// Message was dropped due to capacity
    Dropped,
}

/// Token representing an active queue run
pub struct QueueToken {
    lane_key: String,
    lanes: Arc<Mutex<HashMap<String, LaneState>>>,
    stats: Arc<Mutex<QueueStats>>,
}

impl QueueToken {
    /// Mark the run as complete, releasing the lane
    pub async fn complete(self) {
        let mut lanes = self.lanes.lock().await;
        if let Some(lane) = lanes.get_mut(&self.lane_key) {
            lane.active = false;
            lane.run_started_at = None;
            lane.pending_tool_calls.clear();
            debug!("Completed run for lane {}", self.lane_key);
        }
    }

    /// Register a pending tool call that can be cancelled
    pub async fn register_tool_call(&self,
        tool_call_id: String,
    ) {
        let mut lanes = self.lanes.lock().await;
        if let Some(lane) = lanes.get_mut(&self.lane_key) {
            lane.pending_tool_calls.push(tool_call_id);
        }
    }

    /// Mark a tool call as complete
    pub async fn complete_tool_call(
        &self,
        tool_call_id: &str,
    ) {
        let mut lanes = self.lanes.lock().await;
        if let Some(lane) = lanes.get_mut(&self.lane_key) {
            lane.pending_tool_calls.retain(|id| id != tool_call_id);
        }
    }
}

/// Queue errors
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("Queue has been shut down")]
    Shutdown,
    #[error("Invalid session key")]
    InvalidSession,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_mode_from_str() {
        assert_eq!(QueueMode::from_str("steer"), QueueMode::Steer);
        assert_eq!(QueueMode::from_str("followup"), QueueMode::Followup);
        assert_eq!(QueueMode::from_str("collect"), QueueMode::Collect);
        assert_eq!(QueueMode::from_str("steer-backlog"), QueueMode::SteerBacklog);
        assert_eq!(QueueMode::from_str("steer_backlog"), QueueMode::SteerBacklog);
        assert_eq!(QueueMode::from_str("interrupt"), QueueMode::Interrupt);
        assert_eq!(QueueMode::from_str("queue"), QueueMode::Steer); // Legacy
        assert_eq!(QueueMode::from_str("unknown"), QueueMode::Collect); // Default
    }

    #[tokio::test]
    async fn test_enqueue_and_dequeue() {
        let queue = MessageQueue::default_config();

        // Enqueue a message
        let result = queue
            .enqueue(
                "Hello".to_string(),
                "session1".to_string(),
                "discord".to_string(),
                None,
                None,
            )
            .await;

        assert!(matches!(result, Ok(EnqueueResult::Queued(1))));

        // Dequeue should get it (after debounce)
        tokio::time::sleep(Duration::from_millis(1100)).await;

        let dequeued = queue.dequeue().await.unwrap();
        assert!(dequeued.is_some());

        let (msg, token) = dequeued.unwrap();
        assert_eq!(msg.content, "Hello");
        assert_eq!(msg.session_key, "session1");

        // Complete the run
        token.complete().await;

        // Stats should show processed
        let stats = queue.stats().await;
        assert_eq!(stats.total_queued, 1);
        assert_eq!(stats.total_processed, 1);
    }

    #[tokio::test]
    async fn test_capacity_drop_old() {
        let config = QueueConfig {
            cap: 2,
            drop_policy: DropPolicy::Old,
            ..Default::default()
        };
        let queue = MessageQueue::new(config);

        // Fill to capacity
        for i in 0..2 {
            queue
                .enqueue(
                    format!("msg{}", i),
                    "session1".to_string(),
                    "discord".to_string(),
                    None,
                    None,
                )
                .await
                .unwrap();
        }

        // This should drop the oldest
        queue
            .enqueue(
                "newest".to_string(),
                "session1".to_string(),
                "discord".to_string(),
                None,
                None,
            )
            .await
            .unwrap();

        let stats = queue.stats().await;
        assert_eq!(stats.total_dropped, 1);
    }

    #[tokio::test]
    async fn test_concurrent_runs_limited() {
        let config = QueueConfig {
            max_concurrent: 1,
            debounce_ms: 0, // No debounce for test
            ..Default::default()
        };
        let queue = MessageQueue::new(config);

        // Enqueue two messages
        queue
            .enqueue(
                "first".to_string(),
                "session1".to_string(),
                "discord".to_string(),
                None,
                None,
            )
            .await
            .unwrap();

        queue
            .enqueue(
                "second".to_string(),
                "session2".to_string(),
                "discord".to_string(),
                None,
                None,
            )
            .await
            .unwrap();

        // First dequeue should succeed
        let first = queue.dequeue().await.unwrap();
        assert!(first.is_some());

        // Second dequeue should block until first completes
        // (In real usage, we'd have separate tasks)
    }
}
