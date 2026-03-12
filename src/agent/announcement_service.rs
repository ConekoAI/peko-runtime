//! Announcement Service
//!
//! Processes completed subagent runs and announces their results to parent sessions.
//! Runs as a background task that polls for completed runs.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::agent::subagent_executor::{AnnouncementReceiver, SubagentExecutor};
use crate::agent::subagent_registry::SubagentStatus;
use crate::session::context::SessionContext;
use crate::session::manager::SessionManager;

/// Service for announcing subagent results to parent sessions
pub struct AnnouncementService {
    /// Executor to get completed runs from
    executor: Arc<SubagentExecutor>,
    /// Session manager for accessing parent sessions
    session_manager: Arc<RwLock<SessionManager>>,
    /// Polling interval in seconds
    poll_interval_secs: u64,
}

impl AnnouncementService {
    /// Create a new announcement service
    #[must_use]
    pub fn new(
        executor: Arc<SubagentExecutor>,
        session_manager: Arc<RwLock<SessionManager>>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            executor,
            session_manager,
            poll_interval_secs,
        }
    }

    /// Run the announcement service loop
    ///
    /// This continuously polls for completed runs and announces them.
    pub async fn run(&self) {
        let mut interval =
            tokio::time::interval(tokio::time::Duration::from_secs(self.poll_interval_secs));

        info!(
            "Starting announcement service (poll interval: {}s)",
            self.poll_interval_secs
        );

        loop {
            interval.tick().await;

            if let Err(e) = self.process_completed_runs().await {
                error!("Error processing completed runs: {}", e);
            }
        }
    }

    /// Process all completed runs that need announcement
    async fn process_completed_runs(&self) -> anyhow::Result<()> {
        // Get completed runs from executor
        let runs = self.executor.get_completed_for_announcement().await;

        for run in runs {
            info!(
                "Announcing subagent result: run_id={} parent={} status={}",
                run.run_id, run.parent_session_key, run.status
            );

            // Get parent session context
            let parent_ctx = self.get_parent_context(&run.parent_session_key).await;

            match parent_ctx {
                Some(ctx) => {
                    // Announce to parent
                    if let Err(e) =
                        crate::agent::subagent_announce::announce_to_parent(&ctx, &run).await
                    {
                        error!(
                            "Failed to announce to parent: run_id={} error={}",
                            run.run_id, e
                        );
                    } else {
                        info!("Successfully announced to parent: run_id={}", run.run_id);
                    }
                }
                None => {
                    warn!(
                        "Parent session not found for announcement: run_id={} parent={}",
                        run.run_id, run.parent_session_key
                    );
                }
            }

            // Handle cleanup if delete policy
            if matches!(
                run.cleanup,
                crate::session::types::SpawnCleanupPolicy::Delete
            ) {
                info!(
                    "Cleaning up subagent session: run_id={} session={}",
                    run.run_id, run.child_session_key
                );
                // TODO: Implement actual session deletion
                // For now we just log it
            }
        }

        Ok(())
    }

    /// Get parent session context by key
    async fn get_parent_context(&self, parent_key: &str) -> Option<SessionContext> {
        // Parse the parent key to get agent and peer info
        let parsed = crate::session::key::parse_session_key_v2(parent_key)?;

        let peer = match parsed.peer_type.as_str() {
            "user" => crate::session::types::Peer::User(parsed.peer_id.clone()),
            "agent" => crate::session::types::Peer::Agent(parsed.peer_id.clone()),
            _ => return None,
        };

        // Get session from manager
        let mut manager = self.session_manager.write().await;

        // Try to get existing base session
        let base = manager
            .get_or_create_base(&parsed.agent, &peer)
            .await
            .ok()?;

        // Create a hybrid session with no overlay (this is the parent)
        let hybrid = crate::session::manager::HybridSession {
            base,
            overlay: crate::session::manager::OverlayRef::None,
        };

        Some(SessionContext {
            hybrid,
            channel_type: None,
            is_subagent: false,
        })
    }

    /// Run announcement processing once (for testing)
    pub async fn process_once(&self) -> usize {
        match self.process_completed_runs().await {
            Ok(_) => {
                // Return count of processed runs
                self.executor.get_completed_for_announcement().await.len()
            }
            Err(e) => {
                error!("Error in process_once: {}", e);
                0
            }
        }
    }
}

/// Alternative: Channel-based announcement service
///
/// This version uses a channel to receive completed runs directly,
/// avoiding the need for polling.
pub struct ChannelAnnouncementService {
    /// Receiver for completed runs
    receiver: AnnouncementReceiver,
    /// Session manager for accessing parent sessions
    session_manager: Arc<RwLock<SessionManager>>,
}

impl ChannelAnnouncementService {
    /// Create a new channel-based announcement service
    #[must_use]
    pub fn new(
        receiver: AnnouncementReceiver,
        session_manager: Arc<RwLock<SessionManager>>,
    ) -> Self {
        Self {
            receiver,
            session_manager,
        }
    }

    /// Run the service loop
    pub async fn run(&mut self) {
        info!("Starting channel-based announcement service");

        while let Some(completed) = self.receiver.recv().await {
            info!(
                "Received completed run for announcement: run_id={}",
                completed.run.run_id
            );

            // Get parent context
            let parent_ctx = self.get_parent_context(&completed.parent_session_key).await;

            match parent_ctx {
                Some(ctx) => {
                    // Add the announcement message directly
                    if let Err(e) = ctx
                        .add_assistant_message(&completed.announcement, None)
                        .await
                    {
                        error!(
                            "Failed to add announcement: run_id={} error={}",
                            completed.run.run_id, e
                        );
                    } else {
                        info!("Successfully announced: run_id={}", completed.run.run_id);
                    }
                }
                None => {
                    warn!(
                        "Parent session not found: run_id={} parent={}",
                        completed.run.run_id, completed.parent_session_key
                    );
                }
            }

            // Handle cleanup
            if matches!(
                completed.run.cleanup,
                crate::session::types::SpawnCleanupPolicy::Delete
            ) {
                info!(
                    "Would delete session: run_id={} session={}",
                    completed.run.run_id, completed.run.child_session_key
                );
            }
        }

        info!("Channel-based announcement service stopped");
    }

    /// Get parent session context by key
    async fn get_parent_context(&self, parent_key: &str) -> Option<SessionContext> {
        let parsed = crate::session::key::parse_session_key_v2(parent_key)?;

        let peer = match parsed.peer_type.as_str() {
            "user" => crate::session::types::Peer::User(parsed.peer_id.clone()),
            "agent" => crate::session::types::Peer::Agent(parsed.peer_id.clone()),
            _ => return None,
        };

        let mut manager = self.session_manager.write().await;
        let base = manager
            .get_or_create_base(&parsed.agent, &peer)
            .await
            .ok()?;

        let hybrid = crate::session::manager::HybridSession {
            base,
            overlay: crate::session::manager::OverlayRef::None,
        };

        Some(SessionContext {
            hybrid,
            channel_type: None,
            is_subagent: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manager::SessionManager;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn test_announcement_service_creation() {
        let manager = Arc::new(RwLock::new(SessionManager::new()));
        let executor = Arc::new(crate::agent::subagent_executor::SubagentExecutor::new(
            crate::session::context::SessionRouter::new(manager.clone(), "test"),
            manager.clone(),
            "test",
            5,
        ));

        let service = AnnouncementService::new(executor, manager, 5);
        assert_eq!(service.poll_interval_secs, 5);
    }
}
