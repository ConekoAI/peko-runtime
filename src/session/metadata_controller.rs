//! Metadata Controller
//!
//! The `MetadataController` is the SOLE authority for session metadata operations.
//! All metadata reads and writes must go through this controller to ensure:
//! - Data consistency between index and JSONL
//! - Single point of truth for metadata
//! - Centralized caching and reconciliation

use crate::session::index::{MaintenanceConfig, MaintenanceReport, SessionEntry, SessionIndex};
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
/// No other component should directly access `SessionIndex`.
///
/// Internally uses `SessionEntry` for storage; `SessionMetadata` is used
/// at API boundaries for backward compatibility.
pub struct MetadataController {
    index: SessionIndex,
    storage: SessionStorage,
    sessions_dir: PathBuf,
    /// In-memory cache of metadata (`session_id` -> `SessionEntry`)
    /// Using `SessionEntry` internally for consistency with `SessionIndex`
    cache: Arc<RwLock<HashMap<String, SessionEntry>>>,
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

    /// Create new metadata entry from `SessionMetadata`
    ///
    /// This is the ONLY way to create session metadata.
    /// Accepts `SessionMetadata` for backward compatibility but stores as `SessionEntry` internally.
    pub async fn create_metadata(&mut self, metadata: SessionMetadata) -> Result<()> {
        let session_id = metadata.session_id.clone();
        debug!("Creating metadata for session {}", session_id);

        // Convert to entry for internal storage
        let entry = metadata.to_entry();

        // Insert into index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.clone(), entry);

        info!("Created metadata for session {}", session_id);
        Ok(())
    }

    /// Create new metadata entry from `SessionEntry` (internal use)
    ///
    /// This method accepts `SessionEntry` directly for internal operations.
    pub async fn create_entry(&mut self, entry: SessionEntry) -> Result<()> {
        let session_id = entry.session_id.clone();
        debug!("Creating entry for session {}", session_id);

        // Insert into index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.clone(), entry);

        info!("Created entry for session {}", session_id);
        Ok(())
    }

    /// Get session entry internally (source of truth)
    ///
    /// This is the internal method that returns `SessionEntry` directly.
    /// All internal operations should use this method.
    async fn get_entry(
        &mut self,
        session_id: &str,
        sync_from_jsonl: bool,
    ) -> Result<Option<SessionEntry>> {
        // Check cache first (only if not syncing)
        if !sync_from_jsonl {
            if let Some(cached) = self.cache.read().await.get(session_id).cloned() {
                debug!("Cache hit for session {}", session_id);
                return Ok(Some(cached));
            }
        }

        // Load from index
        let mut entry = match self.index.get(session_id).await? {
            Some(e) => e,
            None => return Ok(None),
        };

        // Sync message count and token metrics from JSONL if requested
        if sync_from_jsonl {
            let mut needs_update = false;

            // Sync message count
            match self.count_messages_from_jsonl(session_id).await {
                Ok(actual_count) => {
                    if entry.message_count != actual_count {
                        debug!(
                            "Session {} message count synced: {} -> {}",
                            session_id, entry.message_count, actual_count
                        );
                        entry.message_count = actual_count;
                        needs_update = true;
                    }
                }
                Err(e) => {
                    warn!(
                        "Failed to count messages from JSONL for {}: {}",
                        session_id, e
                    );
                }
            }

            // Sync token metrics
            match self
                .sync_token_metrics_to_entry(session_id, &mut entry)
                .await
            {
                Ok(changed) if changed => needs_update = true,
                Ok(_) => {}
                Err(e) => warn!(
                    "Failed to sync token metrics from JSONL for {}: {}",
                    session_id, e
                ),
            }

            // Update index if any changes were made
            if needs_update {
                self.index.insert(entry.clone()).await?;
                self.index.save().await?;
            }
        }

        // Update cache
        self.cache
            .write()
            .await
            .insert(session_id.to_string(), entry.clone());

        Ok(Some(entry))
    }

    /// Get metadata for a session
    ///
    /// If `sync_from_jsonl` is true, the message count will be synced from
    /// the actual JSONL content (source of truth).
    ///
    /// This method converts from internal `SessionEntry` to `SessionMetadata`
    /// at the API boundary for backward compatibility.
    pub async fn get_metadata(
        &mut self,
        session_id: &str,
        sync_from_jsonl: bool,
    ) -> Result<Option<SessionMetadata>> {
        let entry = self.get_entry(session_id, sync_from_jsonl).await?;
        Ok(entry.map(|e| e.to_metadata()))
    }

    /// Get metadata without consistency check (faster)
    pub async fn get_metadata_fast(&mut self, session_id: &str) -> Result<Option<SessionMetadata>> {
        self.get_metadata(session_id, false).await
    }

    /// Update metadata (full replacement)
    ///
    /// Accepts `SessionMetadata` for backward compatibility but stores as `SessionEntry` internally.
    pub async fn update_metadata(&mut self, metadata: SessionMetadata) -> Result<()> {
        let session_id = metadata.session_id.clone();
        debug!("Updating metadata for session {}", session_id);

        // Convert to entry for internal storage
        let entry = metadata.to_entry();

        // Update index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.clone(), entry);

        debug!("Updated metadata for session {}", session_id);
        Ok(())
    }

    /// Update entry (full replacement, internal use)
    ///
    /// This method accepts `SessionEntry` directly for internal operations.
    pub async fn update_entry(&mut self, entry: SessionEntry) -> Result<()> {
        let session_id = entry.session_id.clone();
        debug!("Updating entry for session {}", session_id);

        // Update index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.clone(), entry);

        debug!("Updated entry for session {}", session_id);
        Ok(())
    }

    /// Update message counts atomically
    pub async fn update_message_counts(
        &mut self,
        session_id: &str,
        message_count: usize,
        context_window: usize,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        debug!(
            "Updating counts for {}: messages={}, window={}, in={}, out={}",
            session_id, message_count, context_window, input_tokens, output_tokens
        );

        // Load current entry
        let mut entry = match self.get_entry(session_id, false).await? {
            Some(e) => e,
            None => {
                return Err(anyhow::anyhow!(
                    "Cannot update counts for non-existent session {session_id}"
                ));
            }
        };

        // Update fields directly on entry
        entry.message_count = message_count;
        entry.context_window = context_window;
        entry.total_input_tokens += input_tokens;
        entry.total_output_tokens += output_tokens;
        entry.touch();

        // Save
        self.update_entry(entry).await
    }

    /// Record token usage for a session
    ///
    /// `context_window` is the `total_tokens` from the current assistant message.
    /// `input_tokens` and `output_tokens` are the incremental tokens for this turn.
    pub async fn record_token_usage(
        &mut self,
        session_id: &str,
        context_window: usize,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        debug!(
            "Recording token usage for {}: window={}, in={}, out={}",
            session_id, context_window, input_tokens, output_tokens
        );

        let mut entry = match self.get_entry(session_id, false).await? {
            Some(e) => e,
            None => {
                return Err(anyhow::anyhow!(
                    "Cannot record tokens for non-existent session {session_id}"
                ));
            }
        };

        entry.record_tokens(context_window, input_tokens, output_tokens);
        self.update_entry(entry).await
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

    /// Get entry without consistency check (faster, internal use)
    async fn get_entry_fast(&mut self, session_id: &str) -> Result<Option<SessionEntry>> {
        self.get_entry(session_id, false).await
    }

    /// Sync token metrics from JSONL into entry (source of truth)
    ///
    /// Updates the entry's `context_window`, `total_input_tokens`, and `total_output_tokens`
    /// based on the actual token usage data stored in the JSONL file.
    ///
    /// Returns `true` if the entry was modified, `false` otherwise.
    async fn sync_token_metrics_to_entry(
        &self,
        session_id: &str,
        entry: &mut SessionEntry,
    ) -> Result<bool> {
        let (context_window, total_input, total_output) =
            self.get_token_metrics_from_jsonl(session_id).await?;

        let changed = entry.context_window != context_window
            || entry.total_input_tokens != total_input
            || entry.total_output_tokens != total_output;

        if changed {
            debug!(
                "Session {} token metrics synced: window={}, in={}, out={} -> window={}, in={}, out={}",
                session_id,
                entry.context_window,
                entry.total_input_tokens,
                entry.total_output_tokens,
                context_window,
                total_input,
                total_output
            );
            entry.context_window = context_window;
            entry.total_input_tokens = total_input;
            entry.total_output_tokens = total_output;
        }
        Ok(changed)
    }

    /// Delete session completely (metadata + JSONL file)
    ///
    /// This is the preferred way to delete a session. It ensures:
    /// - Metadata is removed from the index
    /// - JSONL file is deleted
    /// - Cache is updated
    /// - Subject routing is cleaned up (if this session is the active one for its peer)
    ///
    /// Returns Ok(true) if session existed and was deleted, Ok(false) if not found.
    pub async fn delete_session(&mut self, session_id: &str) -> Result<bool> {
        debug!("Deleting session {} (metadata + file)", session_id);

        // Check if session exists and capture metadata before deletion
        // Use get_entry (not get_entry_fast) to ensure we load from index if not cached
        let entry = self.get_entry(session_id, false).await?;
        let exists = entry.is_some();

        if !exists {
            // Still try to delete the file if it exists (cleanup)
            self.storage.delete_session(session_id).await.ok();
            return Ok(false);
        }

        // DERIVE peer key from session metadata using centralized method
        // and clear peer routing if this session is still the active one.
        // This prevents "Session not found" errors when sending without --new flag.
        if let Some(e) = entry {
            use crate::auth::Subject;
            use crate::session::key::derive_base_session_key;

            let peer = match e.peer_type.as_deref() {
                Some("user") => e.peer_id.as_ref().map(|id| Subject::User(id.clone())),
                Some("agent") => e.peer_id.as_ref().map(|id| Subject::Principal(id.clone())),
                _ => None,
            };

            if let Some(p) = peer {
                let peer_key = derive_base_session_key(&e.agent_name, &p);

                if self
                    .index
                    .get_active_session_id(&peer_key)
                    .await?
                    .as_deref()
                    == Some(session_id)
                {
                    self.index.clear_active_for_peer(&peer_key).await?;
                    self.index.save().await?;
                    info!(
                        "Cleared peer routing for {} after session deletion",
                        peer_key
                    );
                }
            }
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

    /// List all session entries internally
    ///
    /// If `sync_from_jsonl` is true, message counts and token usage will be synced from
    /// the actual JSONL content (source of truth).
    async fn list_entries(&mut self, sync_from_jsonl: bool) -> Result<Vec<SessionEntry>> {
        let mut entries = self.index.list_all().await?;

        if sync_from_jsonl {
            for entry in &mut entries {
                let session_id = entry.session_id.clone();

                // Sync message count
                match self.count_messages_from_jsonl(&session_id).await {
                    Ok(actual_count) => {
                        if entry.message_count != actual_count {
                            debug!(
                                "Session {} message count synced: {} -> {}",
                                session_id, entry.message_count, actual_count
                            );
                            entry.message_count = actual_count;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to count messages for {}: {}", session_id, e);
                    }
                }

                // Sync token usage
                if let Err(e) = self.sync_token_metrics_to_entry(&session_id, entry).await {
                    warn!("Failed to sync token metrics for {}: {}", session_id, e);
                }
            }
        }

        // Sort by updated_at descending (most recent first)
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(entries)
    }

    /// List all sessions with metadata
    ///
    /// If `sync_from_jsonl` is true, message counts will be synced from
    /// the actual JSONL content (source of truth).
    ///
    /// Converts from internal `SessionEntry` to `SessionMetadata` at API boundary.
    pub async fn list_metadata(&mut self, sync_from_jsonl: bool) -> Result<Vec<SessionMetadata>> {
        let entries = self.list_entries(sync_from_jsonl).await?;
        Ok(entries.into_iter().map(|e| e.to_metadata()).collect())
    }

    /// List sessions for a specific agent
    pub async fn list_for_agent(
        &mut self,
        agent_name: &str,
        verify_consistency: bool,
    ) -> Result<Vec<SessionMetadata>> {
        let entries = self.list_entries(verify_consistency).await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.agent_name == agent_name)
            .map(|e| e.to_metadata())
            .collect())
    }

    /// List sessions for a specific peer (internal - returns `SessionEntry`)
    async fn list_entries_for_peer(
        &mut self,
        peer_key: &str,
        sync_from_jsonl: bool,
    ) -> Result<Vec<SessionEntry>> {
        let mut entries = self.index.list_for_peer(peer_key).await?;

        if sync_from_jsonl {
            for entry in &mut entries {
                let session_id = entry.session_id.clone();
                match self.count_messages_from_jsonl(&session_id).await {
                    Ok(actual_count) => {
                        if entry.message_count != actual_count {
                            debug!(
                                "Session {} message count synced: {} -> {}",
                                session_id, entry.message_count, actual_count
                            );
                            entry.message_count = actual_count;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to count messages for {}: {}", session_id, e);
                    }
                }
            }
        }

        Ok(entries)
    }

    /// List sessions for a specific peer
    ///
    /// Converts from internal `SessionEntry` to `SessionMetadata` at API boundary.
    pub async fn list_for_peer(
        &mut self,
        peer_key: &str,
        sync_from_jsonl: bool,
    ) -> Result<Vec<SessionMetadata>> {
        let entries = self
            .list_entries_for_peer(peer_key, sync_from_jsonl)
            .await?;
        Ok(entries.into_iter().map(|e| e.to_metadata()).collect())
    }

    /// List sessions for a specific peer and return `SessionInfo` directly
    ///
    /// This is a convenience method for the service layer that avoids double conversion.
    pub async fn list_session_info_for_peer(
        &mut self,
        peer_key: &str,
        sync_from_jsonl: bool,
    ) -> Result<Vec<crate::common::services::session_service::SessionInfo>> {
        let entries = self
            .list_entries_for_peer(peer_key, sync_from_jsonl)
            .await?;
        Ok(entries.into_iter().map(|e| e.to_info()).collect())
    }

    // ====================================================================================
    // JSONL Sync (Source of Truth)
    // ====================================================================================

    /// Compute message count from JSONL (source of truth)
    pub async fn count_messages_from_jsonl(&self, session_id: &str) -> Result<usize> {
        let events = self
            .storage
            .load_events(session_id)
            .await
            .with_context(|| format!("Failed to load JSONL for session {session_id}"))?;

        Ok(events.iter().filter(|e| e.is_message()).count())
    }

    /// Get token usage metrics from JSONL (source of truth)
    ///
    /// Returns (`context_window`, `total_input_tokens`, `total_output_tokens)`:
    /// - `context_window`: `total_tokens` from the last assistant message
    /// - `total_input_tokens`: sum of `input_tokens` from all assistant messages
    /// - `total_output_tokens`: sum of `output_tokens` from all assistant messages
    pub async fn get_token_metrics_from_jsonl(
        &self,
        session_id: &str,
    ) -> Result<(usize, usize, usize)> {
        let events = self
            .storage
            .load_events(session_id)
            .await
            .with_context(|| format!("Failed to load JSONL for session {session_id}"))?;

        let mut total_input = 0usize;
        let mut total_output = 0usize;
        let mut context_window = 0usize;

        for event in &events {
            if let crate::session::events::SessionEvent::MessageV2(msg) = event {
                if let Some(usage) = msg.usage() {
                    total_input += usage.input as usize;
                    total_output += usage.output as usize;
                    // Last seen total becomes the context window
                    context_window = usage.total as usize;
                }
            }
        }

        Ok((context_window, total_input, total_output))
    }

    /// Sync metadata from JSONL (source of truth)
    ///
    /// This is the PRIMARY method for ensuring metadata matches JSONL.
    /// The JSONL file is the source of truth for message count and token usage.
    pub async fn sync_from_jsonl(&mut self, session_id: &str) -> Result<usize> {
        let actual_count = self.count_messages_from_jsonl(session_id).await?;
        let (context_window, total_input, total_output) =
            self.get_token_metrics_from_jsonl(session_id).await?;

        // Get current entry
        let mut entry = match self.get_entry_fast(session_id).await? {
            Some(e) => e,
            None => {
                return Err(anyhow::anyhow!(
                    "Cannot sync non-existent session {session_id}"
                ));
            }
        };

        // Always update to match JSONL (JSONL is source of truth)
        let needs_update = entry.message_count != actual_count
            || entry.context_window != context_window
            || entry.total_input_tokens != total_input
            || entry.total_output_tokens != total_output;

        if needs_update {
            debug!(
                "Syncing session {}: messages={}->{}, window={}->{}",
                session_id, entry.message_count, actual_count, entry.context_window, context_window
            );
            entry.message_count = actual_count;
            entry.context_window = context_window;
            entry.total_input_tokens = total_input;
            entry.total_output_tokens = total_output;
            entry.touch();
            self.update_entry(entry).await?;
        }

        Ok(actual_count)
    }

    /// Reconcile metadata with actual JSONL content (internal maintenance helper)
    #[doc(hidden)]
    pub async fn reconcile_metadata(
        &mut self,
        session_id: &str,
        metadata: &mut SessionMetadata,
    ) -> Result<ReconciliationResult> {
        // Use new SessionEvent format for counting (supports both new and legacy formats)
        let events = self
            .storage
            .load_events(session_id)
            .await
            .with_context(|| format!("Failed to load JSONL for session {session_id}"))?;

        // Count message events (message.v2 is the new format)
        let actual_count = events.iter().filter(|e| e.is_message()).count();

        let old_count = metadata.message_count;

        if actual_count == old_count {
            Ok(ReconciliationResult::new(session_id))
        } else {
            metadata.set_message_count(actual_count);
            let entry = metadata.clone().to_entry();
            self.index.insert(entry.clone()).await?;
            self.index.save().await?;
            self.cache
                .write()
                .await
                .insert(session_id.to_string(), entry);

            Ok(ReconciliationResult::new(session_id)
                .with_discrepancy("message_count", old_count, actual_count)
                .reconciled(old_count, actual_count))
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

        // Count JSONL messages using new SessionEvent format
        let jsonl_count = if self.storage.session_exists(session_id).await {
            let events = self.storage.load_events(session_id).await?;
            events.iter().filter(|e| e.is_message()).count()
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

    /// Sync all sessions from JSONL (for maintenance)
    ///
    /// This syncs the index with the actual JSONL content (source of truth).
    #[doc(hidden)]
    pub async fn reconcile_all(&mut self) -> Result<Vec<ReconciliationResult>> {
        info!("Starting sync of all sessions from JSONL");

        let entries = self.index.list_all().await?;
        let mut results = Vec::new();

        for entry in entries {
            let session_id = entry.session_id.clone();
            let old_count = entry.message_count;

            match self.sync_from_jsonl(&session_id).await {
                Ok(new_count) => {
                    if new_count == old_count {
                        results.push(ReconciliationResult::new(&session_id));
                    } else {
                        info!(
                            "Synced session {}: {} -> {}",
                            session_id, old_count, new_count
                        );
                        results.push(
                            ReconciliationResult::new(&session_id)
                                .with_discrepancy("message_count", old_count, new_count)
                                .reconciled(old_count, new_count),
                        );
                    }
                }
                Err(e) => {
                    warn!("Failed to sync session {}: {}", session_id, e);
                    results.push(ReconciliationResult::new(&session_id));
                }
            }
        }

        let synced_count = results.iter().filter(|r| r.was_reconciled).count();
        info!(
            "Sync complete: {}/{} sessions updated",
            synced_count,
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

    // ====================================================================================
    // Proxy Methods for SessionIndex (Phase 2b: Privatize SessionIndex)
    // ====================================================================================

    /// Get a session entry by ID (proxy to `SessionIndex`)
    ///
    /// This method provides direct access to the underlying `SessionIndex`
    /// for cases where you need the raw `SessionEntry`.
    pub async fn get_entry_from_index(&mut self, session_id: &str) -> Result<Option<SessionEntry>> {
        self.index.get(session_id).await
    }

    /// Set active session for peer (proxy to `SessionIndex`)
    ///
    /// Updates the peer routing to make the specified session active.
    pub async fn set_active_for_peer(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        self.index.set_active_for_peer(peer_key, session_id).await?;
        self.index.save().await
    }

    /// Ensure a peer routing exists and set the given session as active (proxy to `SessionIndex`)
    ///
    /// If the peer does not exist, it is created. If the session is not yet
    /// tracked for the peer, it is added.
    pub async fn ensure_peer_active(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        self.index.ensure_peer_active(peer_key, session_id).await?;
        self.index.save().await
    }

    /// Get active session for peer (proxy to `SessionIndex`)
    pub async fn get_active_for_peer(&mut self, peer_key: &str) -> Result<Option<SessionEntry>> {
        self.index.get_active_for_peer(peer_key).await
    }

    /// Run maintenance on sessions (proxy to `SessionIndex`)
    ///
    /// This prunes old sessions based on the maintenance configuration.
    pub async fn maintenance(&mut self, config: &MaintenanceConfig) -> Result<MaintenanceReport> {
        self.index.maintenance(config).await
    }

    /// List all sessions directly from index (proxy to `SessionIndex`)
    ///
    /// This bypasses the metadata cache and returns raw `SessionEntry` objects.
    pub async fn list_all_from_index(&mut self) -> Result<Vec<SessionEntry>> {
        self.index.list_all().await
    }

    /// List sessions for agent directly from index (proxy to `SessionIndex`)
    pub async fn list_for_agent_from_index(
        &mut self,
        agent_name: &str,
    ) -> Result<Vec<SessionEntry>> {
        self.index.list_for_agent(agent_name).await
    }

    /// List sessions for a specific peer directly from index (proxy to `SessionIndex`)
    ///
    /// This returns `SessionEntry` objects directly without conversion to `SessionMetadata`.
    pub async fn list_for_peer_from_index(&mut self, peer_key: &str) -> Result<Vec<SessionEntry>> {
        self.index.list_for_peer(peer_key).await
    }

    /// Get active session ID for peer (proxy to `SessionIndex`)
    pub async fn get_active_session_id(&mut self, peer_key: &str) -> Result<Option<String>> {
        self.index.get_active_session_id(peer_key).await
    }

    /// Create session for peer (proxy to `SessionIndex`)
    pub async fn create_for_peer(&mut self, entry: SessionEntry, peer_key: &str) -> Result<()> {
        self.index.create_for_peer(entry, peer_key).await
    }

    /// Save index changes (proxy to `SessionIndex`)
    pub async fn save_index(&mut self) -> Result<()> {
        self.index.save().await
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

        // Update with (session_id, message_count, context_window, input_tokens, output_tokens)
        controller
            .update_message_counts("sess_123", 10, 1000, 100, 50)
            .await
            .unwrap();

        let retrieved = controller
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.message_count, 10);
        assert_eq!(retrieved.context_window, 1000);
        assert_eq!(retrieved.total_input_tokens, 100);
        assert_eq!(retrieved.total_output_tokens, 50);
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
