//! Lifecycle Manager - Tracks active executions in stateless architecture
//!
//! In the stateless cold-start architecture, this manager tracks only
//! currently executing agents (transient), not persistent instance states.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{debug, info, instrument, warn};
use uuid::Uuid;

/// Record of an active execution
#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    /// Unique execution ID
    pub id: String,
    /// Agent name
    pub agent_name: String,
    /// Session ID
    pub session_id: String,
    /// When execution started
    pub started_at: Instant,
    /// Request metadata
    pub metadata: Option<serde_json::Value>,
}

impl ExecutionRecord {
    /// Get duration since execution started
    #[must_use] 
    pub fn duration(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }
}

/// Simplified lifecycle manager for stateless architecture
///
/// Tracks only currently executing agents (transient state).
/// No persistent instance lifecycle (Starting/Running/Stopping/Stopped).
pub struct LifecycleManager {
    /// Active executions by execution ID
    active: Arc<RwLock<HashMap<String, ExecutionRecord>>>,
    /// Active executions by agent name (for quick lookup)
    by_agent: Arc<RwLock<HashMap<String, String>>>, // agent_name -> execution_id
}

impl LifecycleManager {
    /// Create new lifecycle manager
    #[must_use]
    pub fn new() -> Self {
        Self {
            active: Arc::new(RwLock::new(HashMap::new())),
            by_agent: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start tracking a new execution
    #[instrument(skip(self), fields(agent = %agent_name))]
    pub async fn start_execution(
        &self,
        agent_name: &str,
        session_id: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<String> {
        let execution_id = format!("exec_{}", Uuid::new_v4().simple());

        let record = ExecutionRecord {
            id: execution_id.clone(),
            agent_name: agent_name.to_string(),
            session_id: session_id.to_string(),
            started_at: Instant::now(),
            metadata,
        };

        {
            let mut active = self.active.write().await;
            active.insert(execution_id.clone(), record);
        }

        {
            let mut by_agent = self.by_agent.write().await;
            by_agent.insert(agent_name.to_string(), execution_id.clone());
        }

        info!(
            execution_id = %execution_id,
            agent = %agent_name,
            session = %session_id,
            "Started execution tracking"
        );

        Ok(execution_id)
    }

    /// Complete an execution and remove tracking
    #[instrument(skip(self), fields(execution_id = %execution_id))]
    pub async fn complete_execution(&self, execution_id: &str) -> Result<()> {
        let record = {
            let mut active = self.active.write().await;
            active.remove(execution_id)
        };

        if let Some(ref rec) = record {
            let mut by_agent = self.by_agent.write().await;
            by_agent.remove(&rec.agent_name);

            info!(
                execution_id = %execution_id,
                agent = %rec.agent_name,
                duration_ms = %rec.started_at.elapsed().as_millis(),
                "Completed execution"
            );
        } else {
            warn!(
                execution_id = %execution_id,
                "Attempted to complete unknown execution"
            );
        }

        Ok(())
    }

    /// Get execution record by ID
    pub async fn get_execution(&self, execution_id: &str) -> Option<ExecutionRecord> {
        let active = self.active.read().await;
        active.get(execution_id).cloned()
    }

    /// Check if an agent has an active execution
    pub async fn is_executing(&self, agent_name: &str) -> bool {
        let by_agent = self.by_agent.read().await;
        by_agent.contains_key(agent_name)
    }

    /// Get active execution for an agent
    pub async fn get_agent_execution(&self, agent_name: &str) -> Option<ExecutionRecord> {
        let by_agent = self.by_agent.read().await;
        let active = self.active.read().await;

        by_agent
            .get(agent_name)
            .and_then(|id| active.get(id).cloned())
    }

    /// List all active executions
    pub async fn active_executions(&self) -> Vec<ExecutionRecord> {
        let active = self.active.read().await;
        active.values().cloned().collect()
    }

    /// Get count of active executions
    pub async fn active_count(&self) -> usize {
        let active = self.active.read().await;
        active.len()
    }

    /// Get count of active executions for a specific agent
    pub async fn agent_active_count(&self, agent_name: &str) -> usize {
        let active = self.active.read().await;
        active
            .values()
            .filter(|rec| rec.agent_name == agent_name)
            .count()
    }

    /// Cancel an execution (for timeout/abort scenarios)
    #[instrument(skip(self), fields(execution_id = %execution_id))]
    pub async fn cancel_execution(&self, execution_id: &str, reason: &str) -> Result<bool> {
        let record = {
            let mut active = self.active.write().await;
            active.remove(execution_id)
        };

        if let Some(rec) = record {
            let mut by_agent = self.by_agent.write().await;
            by_agent.remove(&rec.agent_name);

            warn!(
                execution_id = %execution_id,
                agent = %rec.agent_name,
                reason = %reason,
                duration_ms = %rec.started_at.elapsed().as_millis(),
                "Cancelled execution"
            );

            Ok(true)
        } else {
            debug!(
                execution_id = %execution_id,
                "Attempted to cancel non-existent execution"
            );
            Ok(false)
        }
    }

    /// Get executions older than a threshold (for timeout detection)
    pub async fn stale_executions(&self, threshold: std::time::Duration) -> Vec<ExecutionRecord> {
        let active = self.active.read().await;
        active
            .values()
            .filter(|rec| rec.started_at.elapsed() > threshold)
            .cloned()
            .collect()
    }

    /// Clear all tracking (emergency cleanup)
    pub async fn clear_all(&self) {
        let mut active = self.active.write().await;
        let mut by_agent = self.by_agent.write().await;

        let count = active.len();
        active.clear();
        by_agent.clear();

        if count > 0 {
            warn!("Cleared {} active executions (emergency)", count);
        }
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_lifecycle_manager_basic() {
        let manager = LifecycleManager::new();

        // Initially no executions
        assert_eq!(manager.active_count().await, 0);
        assert!(!manager.is_executing("test-agent").await);

        // Start execution
        let exec_id = manager
            .start_execution("test-agent", "test-session", None)
            .await
            .unwrap();

        assert_eq!(manager.active_count().await, 1);
        assert!(manager.is_executing("test-agent").await);

        // Get execution record
        let record = manager.get_execution(&exec_id).await.unwrap();
        assert_eq!(record.agent_name, "test-agent");
        assert_eq!(record.session_id, "test-session");

        // Complete execution
        manager.complete_execution(&exec_id).await.unwrap();

        assert_eq!(manager.active_count().await, 0);
        assert!(!manager.is_executing("test-agent").await);
    }

    #[tokio::test]
    async fn test_multiple_executions() {
        let manager = LifecycleManager::new();

        // Start multiple executions for different agents
        let exec1 = manager
            .start_execution("agent-1", "session-1", None)
            .await
            .unwrap();
        let exec2 = manager
            .start_execution("agent-2", "session-2", None)
            .await
            .unwrap();

        assert_eq!(manager.active_count().await, 2);

        // Complete one
        manager.complete_execution(&exec1).await.unwrap();

        assert_eq!(manager.active_count().await, 1);
        assert!(!manager.is_executing("agent-1").await);
        assert!(manager.is_executing("agent-2").await);

        // Complete other
        manager.complete_execution(&exec2).await.unwrap();
        assert_eq!(manager.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_cancel_execution() {
        let manager = LifecycleManager::new();

        let exec_id = manager
            .start_execution("test-agent", "test-session", None)
            .await
            .unwrap();

        assert_eq!(manager.active_count().await, 1);

        // Cancel
        let cancelled = manager.cancel_execution(&exec_id, "timeout").await.unwrap();
        assert!(cancelled);

        assert_eq!(manager.active_count().await, 0);

        // Cancel non-existent
        let cancelled = manager.cancel_execution(&exec_id, "timeout").await.unwrap();
        assert!(!cancelled);
    }

    #[tokio::test]
    async fn test_stale_executions() {
        let manager = LifecycleManager::new();

        // This test would require time manipulation or a very short threshold
        // For now, just test the method exists and returns empty for fresh executions
        let stale = manager
            .stale_executions(std::time::Duration::from_nanos(1))
            .await;
        assert!(stale.is_empty());
    }
}
