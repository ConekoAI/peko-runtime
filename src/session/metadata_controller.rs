//! Metadata Controller
//!
//! The MetadataController is the SOLE authority for session metadata operations.
//! All metadata reads and writes must go through this controller to ensure:
//! - Data consistency between index and JSONL
//! - Single point of truth for metadata
//! - Centralized caching and reconciliation

use crate::session::index::SessionIndex;
use crate::session::jsonl::SessionStorage;
use crate::session::metadata::{ReconciliationResult, SessionMetadata};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Controller for all session metadata operations
///
/// This is the SINGLE POINT OF TRUTH for session metadata.
/// No other component should directly access SessionIndex.
pub struct MetadataController {
    index: SessionIndex,
    storage: SessionStorage,
    sessions_dir: PathBuf,
    /// In-memory cache of metadata (session_id -> metadata)
    cache: Arc<RwLock<HashMap<String, SessionMetadata>>>,
}

impl Clone for MetadataController {
    fn clone(&self) -> Self {
        // Create a fresh controller with the same directory
        // Note: We don't clone the cache or index state - this is intentional
        // to ensure consistency when the cloned controller is used independently
        Self::new(&self.sessions_dir)
    }
}

impl std::fmt::Debug for MetadataController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetadataController")
            .field("sessions_dir", &self.sessions_dir)
            .finish_non_exhaustive()
    }
}

impl MetadataController {
    /// Create a new metadata controller
    pub fn new(sessions_dir: impl Into<PathBuf>) -> Self {
        let sessions_dir = sessions_dir.into();
        let index = SessionIndex::open(&sessions_dir);
        let storage = SessionStorage::new(sessions_dir.clone());

        Self {
            index,
            storage,
            sessions_dir,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    // ====================================================================================
    // Core CRUD Operations
    // ====================================================================================

    /// Create new metadata entry
    ///
    /// This is the ONLY way to create session metadata.
    pub async fn create_metadata(&mut self, metadata: SessionMetadata) -> Result<()> {
        let session_id = metadata.session_id.clone();
        debug!("Creating metadata for session {}", session_id);

        // Clone for cache before converting to entry
        let metadata_for_cache = metadata.clone();

        // Insert into index
        let entry = metadata.to_entry();
        self.index.insert(entry).await?;
        self.index.save().await?;

        // Update cache
        self.cache
            .write()
            .await
            .insert(session_id.clone(), metadata_for_cache);

        info!("Created metadata for session {}", session_id);
        Ok(())
    }

    /// Get metadata for a session
    ///
    /// If `verify_consistency` is true, the metadata will be verified against
    /// the actual JSONL content and reconciled if necessary.
    pub async fn get_metadata(
        &mut self,
        session_id: &str,
        verify_consistency: bool,
    ) -> Result<Option<SessionMetadata>> {
        // Check cache first
        if let Some(cached) = self.cache.read().await.get(session_id).cloned() {
            debug!("Cache hit for session {}", session_id);
            if !verify_consistency {
                return Ok(Some(cached));
            }
        }

        // Load from index
        let entry = match self.index.get(session_id).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        let mut metadata = SessionMetadata::from_entry(entry);

        // Verify and reconcile if requested
        if verify_consistency {
            let result = self.reconcile_metadata(session_id, &mut metadata).await?;
            if result.was_reconciled {
                warn!(
                    "Session {} was reconciled: message count {} -> {}",
                    session_id, result.old_message_count, result.new_message_count
                );
            }
        }

        // Update cache
        self.cache
            .write()
            .await
            .insert(session_id.to_string(), metadata.clone());

        Ok(Some(metadata))
    }

    /// Get metadata without consistency check (faster)
    pub async fn get_metadata_fast(&mut self, session_id: &str) -> Result<Option<SessionMetadata>> {
        self.get_metadata(session_id, false).await
    }

    /// Update metadata (full replacement)
    pub async fn update_metadata(&mut self, metadata: SessionMetadata) -> Result<()> {
        let session_id = metadata.session_id.clone();
        debug!("Updating metadata for session {}", session_id);

        // Clone for cache before converting to entry
        let metadata_for_cache = metadata.clone();

        // Update index
        let entry = metadata.to_entry();
        self.index.insert(entry).await?;
        self.index.save().await?;

        // Update cache
        self.cache
            .write()
            .await
            .insert(session_id.clone(), metadata_for_cache);

        debug!("Updated metadata for session {}", session_id);
        Ok(())
    }

    /// Update message counts atomically
    pub async fn update_message_counts(
        &mut self,
        session_id: &str,
        message_count: usize,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        debug!(
            "Updating counts for {}: messages={}, tokens={}/{}",
            session_id, message_count, input_tokens, output_tokens
        );

        // Load current metadata
        let mut metadata = match self.get_metadata_fast(session_id).await? {
            Some(m) => m,
            None => {
                return Err(anyhow::anyhow!(
                    "Cannot update counts for non-existent session {}",
                    session_id
                ));
            }
        };

        // Update fields
        metadata.set_message_count(message_count);
        metadata.record_tokens(input_tokens, output_tokens);

        // Save
        self.update_metadata(metadata).await
    }

    /// Record token usage for a session
    pub async fn record_token_usage(
        &mut self,
        session_id: &str,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        debug!(
            "Recording token usage for {}: {}/{}",
            session_id, input_tokens, output_tokens
        );

        let mut metadata = match self.get_metadata_fast(session_id).await? {
            Some(m) => m,
            None => {
                return Err(anyhow::anyhow!(
                    "Cannot record tokens for non-existent session {}",
                    session_id
                ));
            }
        };

        metadata.record_tokens(input_tokens, output_tokens);
        self.update_metadata(metadata).await
    }

    /// Delete metadata
    pub async fn delete_metadata(&mut self, session_id: &str) -> Result<bool> {
        debug!("Deleting metadata for session {}", session_id);

        // Remove from index
        let removed = self.index.remove(session_id).await?.is_some();
        if removed {
            self.index.save().await?;
        }

        // Remove from cache
        self.cache.write().await.remove(session_id);

        if removed {
            info!("Deleted metadata for session {}", session_id);
        }

        Ok(removed)
    }

    /// Delete session completely (metadata + JSONL file)
    ///
    /// This is the preferred way to delete a session. It ensures:
    /// - Metadata is removed from the index
    /// - JSONL file is deleted
    /// - Cache is updated
    ///
    /// Returns Ok(true) if session existed and was deleted, Ok(false) if not found.
    pub async fn delete_session(&mut self, session_id: &str) -> Result<bool> {
        debug!("Deleting session {} (metadata + file)", session_id);

        // Check if session exists
        let exists = self.index.get(session_id).await?.is_some();
        if !exists {
            // Still try to delete the file if it exists (cleanup)
            self.storage.delete_session(session_id).await.ok();
            return Ok(false);
        }

        // Delete JSONL file first (idempotent - can retry if needed)
        self.storage.delete_session(session_id).await?;

        // Delete metadata
        let removed = self.delete_metadata(session_id).await?;

        info!("Deleted session {} (file + metadata)", session_id);
        Ok(removed)
    }

    // ====================================================================================
    // Listing Operations
    // ====================================================================================

    /// List all sessions with metadata
    ///
    /// Optionally verifies consistency for each session.
    pub async fn list_metadata(
        &mut self,
        verify_consistency: bool,
    ) -> Result<Vec<SessionMetadata>> {
        let entries = self.index.list_all().await?;
        let mut result = Vec::new();

        for entry in entries {
            let session_id = entry.session_id.clone();
            let mut metadata = SessionMetadata::from_entry(entry);

            if verify_consistency {
                if let Err(e) = self.reconcile_metadata(&session_id, &mut metadata).await {
                    warn!("Failed to reconcile session {}: {}", session_id, e);
                    // Continue with index values even if reconciliation fails
                }
            }

            result.push(metadata);
        }

        // Sort by updated_at descending (most recent first)
        result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(result)
    }

    /// List sessions for a specific agent
    pub async fn list_for_agent(
        &mut self,
        agent_name: &str,
        verify_consistency: bool,
    ) -> Result<Vec<SessionMetadata>> {
        let all = self.list_metadata(verify_consistency).await?;
        Ok(all
            .into_iter()
            .filter(|m| m.agent_name == agent_name)
            .collect())
    }

    /// List sessions for a specific peer
    pub async fn list_for_peer(
        &mut self,
        peer_key: &str,
        verify_consistency: bool,
    ) -> Result<Vec<SessionMetadata>> {
        let entries = self.index.list_for_peer(peer_key).await?;
        let mut result = Vec::new();

        for entry in entries {
            let session_id = entry.session_id.clone();
            let mut metadata = SessionMetadata::from_entry(entry);

            if verify_consistency {
                if let Err(e) = self.reconcile_metadata(&session_id, &mut metadata).await {
                    warn!("Failed to reconcile session {}: {}", session_id, e);
                }
            }

            result.push(metadata);
        }

        Ok(result)
    }

    // ====================================================================================
    // Consistency & Reconciliation
    // ====================================================================================

    /// Reconcile metadata with actual JSONL content
    ///
    /// The JSONL file is the SOURCE OF TRUTH for message count.
    /// If there's a discrepancy, the index will be updated to match.
    pub async fn reconcile_metadata(
        &mut self,
        session_id: &str,
        metadata: &mut SessionMetadata,
    ) -> Result<ReconciliationResult> {
        // Load JSONL entries
        let entries = self
            .storage
            .load_session(session_id)
            .await
            .with_context(|| format!("Failed to load JSONL for session {}", session_id))?;

        // Count actual messages in JSONL
        let actual_count = entries
            .iter()
            .filter(|e| matches!(e, crate::session::jsonl::SessionEntry::Message { .. }))
            .count();

        let old_count = metadata.message_count;

        if actual_count != old_count {
            warn!(
                "Session {} message count mismatch: index={}, jsonl={}",
                session_id, old_count, actual_count
            );

            // Update metadata
            metadata.set_message_count(actual_count);

            // Clone metadata for operations that consume it
            let metadata_for_index = metadata.clone();
            let metadata_for_cache = metadata.clone();

            // Update index
            let entry = metadata_for_index.to_entry();
            self.index.insert(entry).await?;
            self.index.save().await?;

            // Update cache
            self.cache
                .write()
                .await
                .insert(session_id.to_string(), metadata_for_cache);

            let result = ReconciliationResult::new(session_id)
                .with_discrepancy("message_count", old_count, actual_count)
                .reconciled(old_count, actual_count);

            info!(
                "Reconciled session {}: message count {} -> {}",
                session_id, old_count, actual_count
            );

            Ok(result)
        } else {
            Ok(ReconciliationResult::new(session_id))
        }
    }

    /// Check consistency without modifying
    pub async fn check_consistency(&mut self, session_id: &str) -> Result<ConsistencyStatus> {
        // Get index entry
        let index_count = match self.index.get(session_id).await? {
            Some(e) => e.message_count,
            None => {
                return Ok(ConsistencyStatus {
                    session_id: session_id.to_string(),
                    exists_in_index: false,
                    exists_in_jsonl: self.storage.session_exists(session_id).await,
                    index_message_count: 0,
                    jsonl_message_count: 0,
                    is_consistent: false,
                });
            }
        };

        // Count JSONL messages
        let jsonl_count = if self.storage.session_exists(session_id).await {
            let entries = self.storage.load_session(session_id).await?;
            entries
                .iter()
                .filter(|e| matches!(e, crate::session::jsonl::SessionEntry::Message { .. }))
                .count()
        } else {
            0
        };

        Ok(ConsistencyStatus {
            session_id: session_id.to_string(),
            exists_in_index: true,
            exists_in_jsonl: self.storage.session_exists(session_id).await,
            index_message_count: index_count,
            jsonl_message_count: jsonl_count,
            is_consistent: index_count == jsonl_count,
        })
    }

    /// Reconcile all sessions (for maintenance)
    pub async fn reconcile_all(&mut self) -> Result<Vec<ReconciliationResult>> {
        info!("Starting reconciliation of all sessions");

        let entries = self.index.list_all().await?;
        let mut results = Vec::new();

        for entry in entries {
            let session_id = entry.session_id.clone();
            let mut metadata = SessionMetadata::from_entry(entry);

            match self.reconcile_metadata(&session_id, &mut metadata).await {
                Ok(result) => {
                    if result.was_reconciled {
                        info!("Reconciled session {}", session_id);
                    }
                    results.push(result);
                }
                Err(e) => {
                    warn!("Failed to reconcile session {}: {}", session_id, e);
                    // Continue with other sessions
                }
            }
        }

        let reconciled_count = results.iter().filter(|r| r.was_reconciled).count();
        info!(
            "Reconciliation complete: {}/{} sessions reconciled",
            reconciled_count,
            results.len()
        );

        Ok(results)
    }

    /// Clear the metadata cache
    pub async fn clear_cache(&self) {
        self.cache.write().await.clear();
        debug!("Metadata cache cleared");
    }

    /// Get cache statistics
    pub async fn cache_stats(&self) -> (usize, usize) {
        let cache = self.cache.read().await;
        (cache.len(), cache.capacity())
    }
}

/// Consistency check result
#[derive(Debug, Clone)]
pub struct ConsistencyStatus {
    pub session_id: String,
    pub exists_in_index: bool,
    pub exists_in_jsonl: bool,
    pub index_message_count: usize,
    pub jsonl_message_count: usize,
    pub is_consistent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_controller() -> (MetadataController, TempDir) {
        let temp = TempDir::new().unwrap();
        let controller = MetadataController::new(temp.path());
        (controller, temp)
    }

    #[tokio::test]
    async fn test_create_and_get_metadata() {
        let (mut controller, _temp) = setup_controller().await;

        let metadata = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        controller.create_metadata(metadata.clone()).await.unwrap();

        let retrieved = controller.get_metadata_fast("sess_123").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().session_id, "sess_123");
    }

    #[tokio::test]
    async fn test_update_message_counts() {
        let (mut controller, _temp) = setup_controller().await;

        let metadata = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        controller.create_metadata(metadata).await.unwrap();

        controller
            .update_message_counts("sess_123", 10, 100, 50)
            .await
            .unwrap();

        let retrieved = controller
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.message_count, 10);
        assert_eq!(retrieved.input_tokens, 100);
        assert_eq!(retrieved.output_tokens, 50);
        assert_eq!(retrieved.total_tokens, 150);
    }

    #[tokio::test]
    async fn test_delete_metadata() {
        let (mut controller, _temp) = setup_controller().await;

        let metadata = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        controller.create_metadata(metadata).await.unwrap();

        assert!(controller.delete_metadata("sess_123").await.unwrap());
        assert!(controller
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .is_none());
    }
}
