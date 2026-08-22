//! Metadata Controller
//!
//! The `MetadataController` is the SOLE authority for session metadata operations.
//! All metadata reads and writes must go through this controller to ensure:
//! - Data consistency between index and JSONL
//! - Single point of truth for metadata
//! - Centralized caching and reconciliation

use crate::id::SessionId;
use crate::index::{MaintenanceConfig, MaintenanceReport, SessionEntry, SessionIndex};
use crate::jsonl::SessionStorage;
use crate::metadata::{ReconciliationResult, SessionMetadata};
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
        let session_id = metadata.session_id;
        debug!("Creating metadata for session {}", session_id);

        // Convert to entry for internal storage
        let entry = metadata.to_entry();

        // Insert into index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.to_string(), entry);

        info!("Created metadata for session {}", session_id);
        Ok(())
    }

    /// Create new metadata entry from `SessionEntry` (internal use)
    ///
    /// This method accepts `SessionEntry` directly for internal operations.
    pub async fn create_entry(&mut self, entry: SessionEntry) -> Result<()> {
        let session_id = entry.session_id;
        debug!("Creating entry for session {}", session_id);

        // Insert into index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.to_string(), entry);

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
        // Canonicalize the input to the SessionId form so test fixtures
        // can pass either the legacy literal ("sess_123") or a UUID
        // string and get the same entry. `SessionId::from` accepts both
        // — canonical UUIDs parse directly, anything else derives a
        // stable v5 UUID so the lookup is deterministic.
        let key = SessionId::from(session_id).to_string();
        // Check cache first (only if not syncing)
        if !sync_from_jsonl {
            if let Some(cached) = self.cache.read().await.get(&key).cloned() {
                debug!("Cache hit for session {}", session_id);
                return Ok(Some(cached));
            }
        }

        // Load from index
        let mut entry = match self.index.get(&key).await? {
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
        let session_id = metadata.session_id;
        debug!("Updating metadata for session {}", session_id);

        // Convert to entry for internal storage
        let entry = metadata.to_entry();

        // Update index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.to_string(), entry);

        debug!("Updated metadata for session {}", session_id);
        Ok(())
    }

    /// Update entry (full replacement, internal use)
    ///
    /// This method accepts `SessionEntry` directly for internal operations.
    pub async fn update_entry(&mut self, entry: SessionEntry) -> Result<()> {
        let session_id = entry.session_id;
        debug!("Updating entry for session {}", session_id);

        // Update index
        self.index.insert(entry.clone()).await?;
        self.index.save().await?;

        // Update cache with entry
        self.cache.write().await.insert(session_id.to_string(), entry);

        debug!("Updated entry for session {}", session_id);
        Ok(())
    }

    /// Set the archived flag on a session
    ///
    /// Same update pattern as [`Self::update_metadata`]: load the entry,
    /// mutate the flag, and write it back through the delta-merge-safe
    /// index save. Errors when the session does not exist.
    pub async fn set_archived(&mut self, session_id: &str, archived: bool) -> Result<()> {
        debug!("Setting archived={} for session {}", archived, session_id);

        let mut entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set archived for non-existent session {session_id}")
        })?;

        if entry.archived != archived {
            entry.archived = archived;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!("Set archived={} for session {}", archived, session_id);
        Ok(())
    }

    /// Set the parent session id on a session (reparent).
    ///
    /// Same update pattern as [`Self::set_archived`]: load the entry,
    /// mutate the field, and write it back through the delta-merge-safe
    /// index save. Errors when the session does not exist. This is a
    /// raw write — the ownership / cycle / live-run guards live in the
    /// caller (root's `SessionManagerRuntime::move_session`).
    pub async fn set_parent(
        &mut self,
        session_id: &str,
        new_parent: Option<SessionId>,
    ) -> Result<()> {
        debug!(
            "Setting parent_session_id={:?} for session {}",
            new_parent, session_id
        );

        let mut entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set parent for non-existent session {session_id}")
        })?;

        if entry.parent_session_id != new_parent {
            entry.parent_session_id = new_parent;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!("Set parent_session_id for session {}", session_id);
        Ok(())
    }

    /// Set the slug on a session (per-parent-unique path segment).
    ///
    /// Unlike [`Self::set_parent`] this is NOT a raw write: the slug
    /// format is validated (`crate::path::validate_slug`) and
    /// per-parent uniqueness is enforced by scanning the session's
    /// siblings (`crate::path::slug_conflict`) — a conflict is a
    /// structured error naming the conflicting session id. Passing
    /// `None` clears the slug without checks. Errors when the session
    /// does not exist.
    pub async fn set_slug(&mut self, session_id: &str, slug: Option<String>) -> Result<()> {
        debug!("Setting slug={:?} for session {}", slug, session_id);

        let entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set slug for non-existent session {session_id}")
        })?;

        if let Some(ref slug) = slug {
            crate::path::validate_slug(slug)?;
            let siblings = self.list_metadata(false).await?;
            if let Some(conflict) = crate::path::slug_conflict(
                &siblings,
                entry.parent_session_id,
                slug,
                SessionId::from(session_id),
            ) {
                return Err(crate::path::err_slug_conflict(
                    slug,
                    conflict,
                    entry.parent_session_id,
                ));
            }
        }

        if entry.slug != slug {
            let mut entry = entry;
            entry.slug = slug;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!("Set slug for session {}", session_id);
        Ok(())
    }

    /// Set the standing flag on a session.
    ///
    /// Same update pattern as [`Self::set_archived`]: load the entry,
    /// mutate the flag, and write it back through the delta-merge-safe
    /// index save. Standing sessions are exempt from maintenance
    /// pruning (`SessionIndex::maintenance`). Errors when the session
    /// does not exist.
    pub async fn set_standing(&mut self, session_id: &str, standing: bool) -> Result<()> {
        debug!("Setting standing={} for session {}", standing, session_id);

        let mut entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set standing for non-existent session {session_id}")
        })?;

        if entry.standing != standing {
            entry.standing = standing;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!("Set standing={} for session {}", standing, session_id);
        Ok(())
    }

    /// Set the privileged flag on a session.
    ///
    /// Same update pattern as [`Self::set_standing`]: load the entry,
    /// mutate the flag, and write it back through the delta-merge-safe
    /// index save. A privileged session's caller gets whole-store
    /// reach in the ownership guards (sprint 2 peer-child
    /// provisioning). Errors when the session does not exist.
    pub async fn set_privileged(&mut self, session_id: &str, privileged: bool) -> Result<()> {
        debug!(
            "Setting privileged={} for session {}",
            privileged, session_id
        );

        let mut entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set privileged for non-existent session {session_id}")
        })?;

        if entry.privileged != privileged {
            entry.privileged = privileged;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!("Set privileged={} for session {}", privileged, session_id);
        Ok(())
    }

    /// Set the compaction-request flag on a session
    ///
    /// The compaction orchestrator ORs this flag into its
    /// `should_request` decision at the session's next run and clears it
    /// once compaction actually starts. Errors when the session does not
    /// exist.
    pub async fn set_compact_requested(
        &mut self,
        session_id: &str,
        compact_requested: bool,
    ) -> Result<()> {
        debug!(
            "Setting compact_requested={} for session {}",
            compact_requested, session_id
        );

        let mut entry = self.get_entry(session_id, false).await?.ok_or_else(|| {
            anyhow::anyhow!("Cannot set compact_requested for non-existent session {session_id}")
        })?;

        if entry.compact_requested != compact_requested {
            entry.compact_requested = compact_requested;
            entry.touch();
            self.update_entry(entry).await?;
        }

        info!(
            "Set compact_requested={} for session {}",
            compact_requested, session_id
        );
        Ok(())
    }

    /// Read the compaction-request flag directly from the on-disk
    /// index, bypassing both the controller cache and the 30s index
    /// cache.
    ///
    /// The engine peeks this once per iteration; the flag may have
    /// been written moments earlier by a *different* controller (the
    /// session tool's adapter), so a cached read would hide it.
    pub async fn peek_compact_requested(&mut self, session_id: &str) -> Result<bool> {
        Ok(self
            .index
            .get_uncached(session_id)
            .await?
            .is_some_and(|e| e.compact_requested))
    }

    /// Update message counts atomically. `user_turn` bumps `turn_count`
    /// — pass true when the appended message has role `user`, so the
    /// index counts conversation turns rather than raw messages
    /// (2026-08-07 field test, Finding 7: the field was previously
    /// never maintained and always read 0).
    pub async fn update_message_counts(
        &mut self,
        session_id: &str,
        message_count: usize,
        last_total_tokens: usize,
        input_tokens: usize,
        output_tokens: usize,
        user_turn: bool,
    ) -> Result<()> {
        debug!(
            "Updating counts for {}: messages={}, last_total={}, in={}, out={}",
            session_id, message_count, last_total_tokens, input_tokens, output_tokens
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
        entry.last_total_tokens = last_total_tokens;
        entry.total_input_tokens += input_tokens;
        entry.total_output_tokens += output_tokens;
        if user_turn {
            entry.turn_count += 1;
        }
        entry.touch();

        // Save
        self.update_entry(entry).await
    }

    /// Delete metadata
    pub async fn delete_metadata(&mut self, session_id: &str) -> Result<bool> {
        debug!("Deleting metadata for session {}", session_id);

        // Canonicalize so test fixtures can pass either a literal or
        // a UUID — both go through the same key derivation.
        let key = SessionId::from(session_id).to_string();
        // Remove from index
        let removed = self.index.remove(&key).await?.is_some();
        if removed {
            self.index.save().await?;
        }

        // Remove from cache
        self.cache.write().await.remove(&key);

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
    /// Updates the entry's `last_total_tokens`, `total_input_tokens`,
    /// and `total_output_tokens` based on the actual token usage data
    /// stored in the JSONL file.
    ///
    /// Returns `true` if the entry was modified, `false` otherwise.
    async fn sync_token_metrics_to_entry(
        &self,
        session_id: &str,
        entry: &mut SessionEntry,
    ) -> Result<bool> {
        let (last_total, total_input, total_output) =
            self.get_token_metrics_from_jsonl(session_id).await?;

        let changed = entry.last_total_tokens != last_total
            || entry.total_input_tokens != total_input
            || entry.total_output_tokens != total_output;

        if changed {
            debug!(
                "Session {} token metrics synced: last_total={}, in={}, out={} -> last_total={}, in={}, out={}",
                session_id,
                entry.last_total_tokens,
                entry.total_input_tokens,
                entry.total_output_tokens,
                last_total,
                total_input,
                total_output
            );
            entry.last_total_tokens = last_total;
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
        // and scrub the session from the peer's routing entry: the id is
        // removed from `PeerInfo.session_ids`, and the active pointer is
        // cleared if it pointed at the deleted session. The peer entry
        // itself survives as long as it has other sessions, so those
        // stay routable/listable. This prevents "Session not found"
        // errors when sending without --new flag.
        if let Some(e) = entry {
            use crate::key::derive_base_session_key;
            use peko_subject::Subject;

            let peer = match e.peer_type.as_deref() {
                Some("user") => e.peer_id.as_ref().map(|id| Subject::User(id.clone())),
                Some("agent") | Some("principal") => e
                    .peer_id
                    .as_ref()
                    .map(|id| Subject::Principal(id.clone().into())),
                _ => None,
            };

            if let Some(p) = peer {
                let peer_key = derive_base_session_key(&e.agent_name, &p);

                self.index
                    .remove_session_from_peer(&peer_key, session_id)
                    .await?;
                self.index.save().await?;
                info!(
                    "Scrubbed session {} from peer routing {} after deletion",
                    session_id, peer_key
                );
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
                let session_id = entry.session_id;

                // Sync message count
                match self.count_messages_from_jsonl(&session_id.to_string()).await {
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
                if let Err(e) = self.sync_token_metrics_to_entry(&session_id.to_string(), entry).await {
                    warn!("Failed to sync token metrics for {}: {}", session_id, e);
                }
            }
        }

        // Sort by updated_at descending (most recent first)
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
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
                let session_id = entry.session_id;
                match self.count_messages_from_jsonl(&session_id.to_string()).await {
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
    ) -> Result<Vec<crate::session_info::SessionInfo>> {
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
    /// Returns (`last_total_tokens`, `total_input_tokens`, `total_output_tokens`):
    /// - `last_total_tokens`: `total_tokens` from the last assistant message
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
        let mut last_total = 0usize;

        for event in &events {
            if let crate::events::SessionEvent::MessageV2(msg) = event {
                if let Some(usage) = msg.usage() {
                    total_input += usage.input as usize;
                    total_output += usage.output as usize;
                    // Last seen total becomes the last_total_tokens
                    last_total = usage.total as usize;
                }
            }
        }

        Ok((last_total, total_input, total_output))
    }

    /// Sync metadata from JSONL (source of truth)
    ///
    /// This is the PRIMARY method for ensuring metadata matches JSONL.
    /// The JSONL file is the source of truth for message count and token usage.
    pub async fn sync_from_jsonl(&mut self, session_id: &str) -> Result<usize> {
        let actual_count = self.count_messages_from_jsonl(session_id).await?;
        let (last_total, total_input, total_output) =
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
            || entry.last_total_tokens != last_total
            || entry.total_input_tokens != total_input
            || entry.total_output_tokens != total_output;

        if needs_update {
            debug!(
                "Syncing session {}: messages={}->{}, last_total={}->{}",
                session_id, entry.message_count, actual_count, entry.last_total_tokens, last_total
            );
            entry.message_count = actual_count;
            entry.last_total_tokens = last_total;
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
            let session_id = entry.session_id;
            let old_count = entry.message_count;

            match self.sync_from_jsonl(&session_id.to_string()).await {
                Ok(new_count) => {
                    if new_count == old_count {
                        results.push(ReconciliationResult::new(session_id.to_string()));
                    } else {
                        info!(
                            "Synced session {}: {} -> {}",
                            session_id, old_count, new_count
                        );
                        results.push(
                            ReconciliationResult::new(session_id.to_string())
                                .with_discrepancy("message_count", old_count, new_count)
                                .reconciled(old_count, new_count),
                        );
                    }
                }
                Err(e) => {
                    warn!("Failed to sync session {}: {}", session_id, e);
                    results.push(ReconciliationResult::new(session_id.to_string()));
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
    use crate::*;
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
        assert_eq!(retrieved.unwrap().session_id, SessionId::from("sess_123"));
    }

    #[tokio::test]
    async fn test_update_message_counts() {
        let (mut controller, _temp) = setup_controller().await;

        let metadata = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        controller.create_metadata(metadata).await.unwrap();

        // Update with (session_id, message_count, last_total_tokens, input_tokens, output_tokens)
        controller
            .update_message_counts("sess_123", 10, 1000, 100, 50, true)
            .await
            .unwrap();

        let retrieved = controller
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.message_count, 10);
        assert_eq!(retrieved.last_total_tokens, 1000);
        assert_eq!(retrieved.total_input_tokens, 100);
        assert_eq!(retrieved.total_output_tokens, 50);
        assert_eq!(retrieved.model_context_limit, None);
        assert_eq!(retrieved.turn_count, 1, "user_turn=true bumps turn_count");

        // Non-user messages (assistant/tool) must not bump turn_count.
        controller
            .update_message_counts("sess_123", 11, 1000, 100, 50, false)
            .await
            .unwrap();
        let retrieved = controller
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retrieved.turn_count, 1);
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

    #[tokio::test]
    async fn test_set_archived_and_compact_requested_roundtrip() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();

        let mut controller = MetadataController::new(&dir);
        let metadata = SessionMetadata::new("sess_123", "test_agent", "sess_123.jsonl");
        controller.create_metadata(metadata).await.unwrap();

        controller.set_archived("sess_123", true).await.unwrap();
        controller
            .set_compact_requested("sess_123", true)
            .await
            .unwrap();

        // Reload through a fresh controller (fresh index + cache) to
        // prove the flags survived the save/reload round trip.
        let mut reloaded = MetadataController::new(&dir);
        let meta = reloaded
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert!(meta.archived);
        assert!(meta.compact_requested);

        // Clearing a flag persists too.
        reloaded.set_archived("sess_123", false).await.unwrap();
        let mut third = MetadataController::new(&dir);
        let meta = third.get_metadata_fast("sess_123").await.unwrap().unwrap();
        assert!(!meta.archived);
        assert!(meta.compact_requested);

        // Both setters error on a non-existent session.
        assert!(third.set_archived("sess_nope", true).await.is_err());
        assert!(third
            .set_compact_requested("sess_nope", true)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_set_parent_roundtrip() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();

        let mut controller = MetadataController::new(&dir);
        let metadata = SessionMetadata::new(
            SessionId::from("sess_123"),
            "test_agent",
            "sess_123.jsonl",
        );
        controller.create_metadata(metadata).await.unwrap();

        controller
            .set_parent(
                &SessionId::from("sess_123").to_string(),
                Some(SessionId::from("sess_parent")),
            )
            .await
            .unwrap();

        // Reload through a fresh controller (fresh index + cache) to
        // prove the reparent survived the save/reload round trip.
        let mut reloaded = MetadataController::new(&dir);
        let meta = reloaded
            .get_metadata_fast("sess_123")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            meta.parent_session_id,
            Some(SessionId::from("sess_parent"))
        );

        // Clearing the parent persists too.
        reloaded
            .set_parent(&SessionId::from("sess_123").to_string(), None)
            .await
            .unwrap();
        let mut third = MetadataController::new(&dir);
        let meta = third.get_metadata_fast("sess_123").await.unwrap().unwrap();
        assert_eq!(meta.parent_session_id, None);

        // Errors on a non-existent session.
        assert!(third
            .set_parent(
                &SessionId::from("sess_nope").to_string(),
                Some(SessionId::from("p")),
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_set_slug_validation_uniqueness_and_persistence() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();

        let mut controller = MetadataController::new(&dir);
        // root ── a (slug "task")
        //     └── b
        //         └── c (slug "task" — same slug, different parent: OK)
        let mut a = SessionMetadata::new(SessionId::from("sess_a"), "test_agent", "sess_a.jsonl");
        a.parent_session_id = Some(SessionId::from("root"));
        let mut b = SessionMetadata::new(SessionId::from("sess_b"), "test_agent", "sess_b.jsonl");
        b.parent_session_id = Some(SessionId::from("root"));
        let mut c = SessionMetadata::new(SessionId::from("sess_c"), "test_agent", "sess_c.jsonl");
        c.parent_session_id = Some(SessionId::from("sess_b"));
        for m in [a, b, c] {
            controller.create_metadata(m).await.unwrap();
        }

        // Format validation: structured error.
        for bad in ["", "a/b", " lead", "trail "] {
            let err = controller
                .set_slug("sess_a", Some(bad.to_string()))
                .await
                .unwrap_err();
            assert!(err.to_string().contains("invalid slug"), "{err}");
        }

        // Set + persist (reload through a fresh controller).
        controller
            .set_slug("sess_a", Some("task".to_string()))
            .await
            .unwrap();
        controller
            .set_slug("sess_c", Some("task".to_string()))
            .await
            .unwrap();
        let mut reloaded = MetadataController::new(&dir);
        assert_eq!(
            reloaded
                .get_metadata_fast("sess_a")
                .await
                .unwrap()
                .unwrap()
                .slug
                .as_deref(),
            Some("task")
        );

        // Sibling conflict: names the conflicting session id.
        let err = controller
            .set_slug("sess_b", Some("task".to_string()))
            .await
            .unwrap_err();
        let sess_a_uuid = SessionId::from("sess_a").to_string();
        assert!(err.to_string().contains(&sess_a_uuid), "{err}");
        assert!(err.to_string().contains("unique per parent"), "{err}");

        // Keeping your own slug is not a conflict (self excluded).
        controller
            .set_slug("sess_a", Some("task".to_string()))
            .await
            .unwrap();

        // Clearing is always allowed; non-existent sessions error.
        controller.set_slug("sess_a", None).await.unwrap();
        assert!(controller
            .set_slug("sess_nope", Some("x".to_string()))
            .await
            .is_err());
    }

    /// `set_standing` round-trips the flag through the index and
    /// errors on a non-existent session (same contract as
    /// `set_archived`).
    #[tokio::test]
    async fn test_set_standing() {
        let (mut controller, _temp) = setup_controller().await;
        let peer = Subject::User("alice".to_string());
        let peer_key = derive_base_session_key("test_agent", &peer);
        let entry = SessionEntry::with_peer(
            "sess_a".to_string(),
            "test_agent".to_string(),
            "sess_a.jsonl".to_string(),
            "user",
            "alice",
        );
        controller.create_for_peer(entry, &peer_key).await.unwrap();
        controller.save_index().await.unwrap();

        assert!(
            !controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .standing
        );
        controller.set_standing("sess_a", true).await.unwrap();
        assert!(
            controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .standing
        );
        // Idempotent + reversible.
        controller.set_standing("sess_a", true).await.unwrap();
        controller.set_standing("sess_a", false).await.unwrap();
        assert!(
            !controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .standing
        );
        // Non-existent session errors.
        assert!(controller.set_standing("sess_nope", true).await.is_err());
    }

    /// `set_privileged` round-trips the flag through the index and
    /// errors on a non-existent session (same contract as
    /// `set_standing`).
    #[tokio::test]
    async fn test_set_privileged() {
        let (mut controller, _temp) = setup_controller().await;
        let peer = Subject::User("alice".to_string());
        let peer_key = derive_base_session_key("test_agent", &peer);
        let entry = SessionEntry::with_peer(
            "sess_a".to_string(),
            "test_agent".to_string(),
            "sess_a.jsonl".to_string(),
            "user",
            "alice",
        );
        controller.create_for_peer(entry, &peer_key).await.unwrap();
        controller.save_index().await.unwrap();

        assert!(
            !controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .privileged
        );
        controller.set_privileged("sess_a", true).await.unwrap();
        assert!(
            controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .privileged
        );
        // Idempotent + reversible.
        controller.set_privileged("sess_a", true).await.unwrap();
        controller.set_privileged("sess_a", false).await.unwrap();
        assert!(
            !controller
                .get_entry("sess_a", false)
                .await
                .unwrap()
                .unwrap()
                .privileged
        );
        // Non-existent session errors.
        assert!(controller.set_privileged("sess_nope", true).await.is_err());
    }

    /// Deleting a session scrubs its id from the peer's `session_ids`
    /// (not just the active pointer); the peer's other sessions stay
    /// listable/routable, and the peer entry only disappears once its
    /// last session is gone.
    #[tokio::test]
    async fn test_delete_session_scrubs_peer_session_ids() {
        let (mut controller, _temp) = setup_controller().await;
        let peer = Subject::User("alice".to_string());
        let peer_key = derive_base_session_key("test_agent", &peer);

        for id in ["sess_a", "sess_b"] {
            let entry = SessionEntry::with_peer(
                SessionId::from(id),
                "test_agent".to_string(),
                format!("{id}.jsonl"),
                "user",
                "alice",
            );
            controller.create_for_peer(entry, &peer_key).await.unwrap();
        }
        controller.save_index().await.unwrap();

        // Sanity: two sessions routed, the second one active.
        assert_eq!(
            controller.get_active_session_id(&peer_key).await.unwrap(),
            Some(SessionId::from("sess_b").to_string())
        );

        // Delete the ACTIVE session: the active pointer is cleared but
        // sess_a remains listable/routable under the same peer.
        controller.delete_session(&SessionId::from("sess_b").to_string()).await.unwrap();
        assert_eq!(
            controller.get_active_session_id(&peer_key).await.unwrap(),
            None
        );
        let remaining = controller
            .list_for_peer_from_index(&peer_key)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].session_id, SessionId::from("sess_a"));

        // Delete the last session: the peer entry disappears entirely.
        controller.delete_session(&SessionId::from("sess_a").to_string()).await.unwrap();
        assert!(controller
            .list_for_peer_from_index(&peer_key)
            .await
            .unwrap()
            .is_empty());
    }
}
