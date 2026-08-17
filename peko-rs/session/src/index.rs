//! Unified Session Index (TDD-002)
//!
//! Two-file architecture:
//! - sessions.json: Session metadata keyed by `session_id` (`HashMap`<String, `SessionEntry`>)
//! - peers.json: Subject routing keyed by `peer_key` (`HashMap`<String, `PeerInfo`>)
//!
//! This provides:
//! - O(1) lookup by `session_id`
//! - O(1) lookup by `peer_key` (critical for message routing)
//! - No data duplication
//! - Clean separation of concerns

use crate::key::safe_filename_component;
use anyhow::{Context, Result};
use peko_fs_persistence::FileLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

/// Process-global registry of per-directory serialization locks.
///
/// Every [`SessionIndex`] is short-lived — `SessionManager` opens a fresh one
/// per turn (see `principal/agent_runner.rs`), so multiple instances for the
/// same principal point at the same `sessions.json` / `peers.json` while each
/// holds its own ~30s in-memory cache. Without a shared serialization point,
/// two instances read the index, mutate their own cache, and each writes the
/// *whole* map back — last writer wins and the other's freshly-added session
/// is silently lost (the production race behind the flaky
/// `concurrent_receives_are_isolated`; see issue #89).
///
/// The cross-process `FileLock` does not help here: it guards against other
/// OS processes, but the lost-update happens between two tasks in the *same*
/// process, each reading a stale cache. We need an in-process lock that
/// serializes the read-modify-write, keyed by the index directory so writers
/// to different principals never contend.
static INDEX_DIR_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    OnceLock::new();

/// Get (or create) the shared serialization lock for an index directory.
fn dir_lock(dir: &Path) -> Arc<tokio::sync::Mutex<()>> {
    let registry = INDEX_DIR_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = registry.lock().unwrap();
    map.entry(dir.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Default cache TTL (30 seconds)
pub const DEFAULT_CACHE_TTL_MS: u64 = 30_000;

/// Default maintenance settings
pub const DEFAULT_PRUNE_AFTER_DAYS: u64 = 30;
pub const DEFAULT_MAX_SESSIONS: usize = 500;

/// Complete session metadata
///
/// Persisted to `sessions.json`. New fields added here must
/// use `#[serde(default)]` so that older `sessions.json` files
/// continue to deserialize after a runtime upgrade — and when
/// renaming an existing field, keep the legacy name as an
/// `alias` so pre-rename JSON stays readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub turn_count: u32,
    /// `total_tokens` reported by the most recent assistant message.
    /// This is the model's count of *how many tokens the current turn
    /// used* — it is NOT the model's maximum context window.
    #[serde(alias = "context_window")]
    pub last_total_tokens: usize,
    /// Cumulative input tokens across all assistant messages
    pub total_input_tokens: usize,
    /// Cumulative output tokens across all assistant messages
    pub total_output_tokens: usize,
    /// The model's maximum context window size, in tokens, if known.
    /// `None` for legacy entries and sessions opened without a
    /// provider/model reference. Populated by the engine after the
    /// compaction orchestrator pins the registry-resolved model max.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_limit: Option<usize>,
    pub transcript_file: String,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub trigger: String,
    /// Subject type ("user" or "agent") - for session identity restoration
    pub peer_type: Option<String>,
    /// Subject ID - for session identity restoration
    pub peer_id: Option<String>,
    /// Archived sessions are hidden from default listings and refuse
    /// resume/compact until unarchived (agent-owned session management).
    #[serde(default)]
    pub archived: bool,
    /// Set when an agent requests compaction of this session. The
    /// compaction orchestrator ORs this flag into its `should_request`
    /// decision at the session's next run and clears it once compaction
    /// actually starts, so the request survives restarts.
    #[serde(default)]
    pub compact_requested: bool,
    /// Standing sessions are exempt from maintenance pruning — their
    /// transcripts are durable regardless of idle age.
    #[serde(default)]
    pub standing: bool,
    /// Privileged sessions give their caller whole-store reach in the
    /// ownership guards (like a base caller) while keeping their parent
    /// pointer and tree membership (sprint 2 peer-child provisioning —
    /// set only for the principal owner's peer child).
    #[serde(default)]
    pub privileged: bool,
    /// Per-parent-unique path segment for `/slug/...` addressing
    /// (see `crate::path`). `None` for legacy entries and root
    /// `root:*` sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

impl SessionEntry {
    /// Create a new session entry
    #[must_use]
    pub fn new(session_id: String, agent_name: String, transcript_file: String) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            session_id,
            agent_name,
            created_at: now,
            updated_at: now,
            message_count: 0,
            turn_count: 0,
            last_total_tokens: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            model_context_limit: None,
            transcript_file,
            title: None,
            parent_session_id: None,
            trigger: "user".to_string(),
            peer_type: None,
            peer_id: None,
            archived: false,
            compact_requested: false,
            standing: false,
            privileged: false,
            slug: None,
        }
    }

    /// Create a new session entry with peer information
    #[must_use]
    pub fn with_peer(
        session_id: String,
        agent_name: String,
        transcript_file: String,
        peer_type: impl Into<String>,
        peer_id: impl Into<String>,
    ) -> Self {
        let mut entry = Self::new(session_id, agent_name, transcript_file);
        entry.peer_type = Some(peer_type.into());
        entry.peer_id = Some(peer_id.into());
        entry
    }

    /// Update timestamp
    pub fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// Record token usage for the most recent assistant message.
    ///
    /// `last_total_tokens` is the `total_tokens` reported by the
    /// provider on the last assistant turn. `input` and `output` are
    /// the incremental tokens for this turn.
    pub fn record_tokens(&mut self, last_total_tokens: usize, input: usize, output: usize) {
        self.last_total_tokens = last_total_tokens;
        self.total_input_tokens += input;
        self.total_output_tokens += output;
        self.touch();
    }

    /// Set the model's maximum context window size (in tokens).
    ///
    /// Idempotent. Called by the engine after the orchestrator
    /// pins the registry-resolved model max for this session.
    pub fn set_model_context_limit(&mut self, limit: usize) {
        if self.model_context_limit != Some(limit) {
            self.model_context_limit = Some(limit);
            self.touch();
        }
    }

    /// Builder-style: same as [`set_model_context_limit`] but returns
    /// `self` for fluent construction in call sites that already use
    /// chained builders (e.g. `StatelessAgentService`).
    #[must_use]
    pub fn with_model_context_limit(mut self, limit: usize) -> Self {
        self.set_model_context_limit(limit);
        self
    }

    /// Increment message count
    pub fn increment_messages(&mut self) {
        self.message_count += 1;
        self.touch();
    }

    /// Increment turn count
    pub fn increment_turn(&mut self) {
        self.turn_count += 1;
        self.touch();
    }

    /// Convert to `SessionMetadata` for backward compatibility
    ///
    /// This is the preferred conversion method when passing to API boundaries.
    #[must_use]
    pub fn to_metadata(&self) -> crate::metadata::SessionMetadata {
        crate::metadata::SessionMetadata::from_entry(self.clone())
    }

    /// Convert to `SessionInfo` for service layer
    ///
    /// This is the preferred conversion method when passing to `SessionService`.
    #[must_use]
    pub fn to_info(&self) -> crate::session_info::SessionInfo {
        crate::session_info::SessionInfo::from(self.clone())
    }
}

/// Subject routing information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Currently active session for this peer. `None` after the active
    /// session was deleted/rotated away — the peer entry (and its
    /// `session_ids`) survives so the remaining sessions stay
    /// routable/listable.
    #[serde(default)]
    pub active_session_id: Option<String>,
    /// All session IDs for this peer (for switching)
    pub session_ids: Vec<String>,
}

impl PeerInfo {
    /// Create new peer info with initial session
    #[must_use]
    pub fn new(active_session_id: String) -> Self {
        Self {
            session_ids: vec![active_session_id.clone()],
            active_session_id: Some(active_session_id),
        }
    }

    /// Add session and make it active
    pub fn add_session(&mut self, session_id: String) {
        if !self.session_ids.contains(&session_id) {
            self.session_ids.push(session_id.clone());
        }
        self.active_session_id = Some(session_id);
    }

    /// Switch to different session
    pub fn switch_to(&mut self, session_id: &str) -> Result<()> {
        if !self.session_ids.contains(&session_id.to_string()) {
            return Err(anyhow::anyhow!("Session {session_id} not found for peer"));
        }
        self.active_session_id = Some(session_id.to_string());
        Ok(())
    }

    /// Get active session ID
    #[must_use]
    pub fn active_session_id(&self) -> Option<&str> {
        self.active_session_id.as_deref()
    }
}

/// Subject index structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerIndex {
    /// `peer_key` → peer info
    pub peers: HashMap<String, PeerInfo>,
}

/// Maintenance configuration
#[derive(Debug, Clone)]
pub struct MaintenanceConfig {
    pub prune_after: Duration,
    pub max_sessions: usize,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            prune_after: Duration::from_secs(DEFAULT_PRUNE_AFTER_DAYS * 24 * 60 * 60),
            max_sessions: DEFAULT_MAX_SESSIONS,
        }
    }
}

/// Maintenance report
#[derive(Debug, Clone, Default)]
pub struct MaintenanceReport {
    pub pruned: usize,
    pub total: usize,
}

/// Unified session index manager
#[derive(Debug, Clone)]
pub struct SessionIndex {
    sessions_path: PathBuf,
    peers_path: PathBuf,
    dir: PathBuf,
    // In-memory caches
    sessions_cache: Option<HashMap<String, SessionEntry>>,
    peers_cache: Option<PeerIndex>,
    sessions_modified: bool,
    peers_modified: bool,
    cache_ttl: Duration,
    sessions_loaded_at: Option<SystemTime>,
    peers_loaded_at: Option<SystemTime>,
    // Pending changes made by *this* instance since the last save. The save
    // path re-reads the on-disk index under the shared lock and applies only
    // these deltas, so a concurrent writer's entries are never clobbered.
    dirty_session_ids: HashSet<String>,
    removed_session_ids: HashSet<String>,
    dirty_peer_keys: HashSet<String>,
    removed_peer_keys: HashSet<String>,
}

impl SessionIndex {
    /// Open index at a specific directory
    pub fn open(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let sessions_path = dir.join("sessions.json");
        let peers_path = dir.join("peers.json");

        Self {
            sessions_path,
            peers_path,
            dir,
            sessions_cache: None,
            peers_cache: None,
            sessions_modified: false,
            peers_modified: false,
            cache_ttl: Duration::from_millis(DEFAULT_CACHE_TTL_MS),
            sessions_loaded_at: None,
            peers_loaded_at: None,
            dirty_session_ids: HashSet::new(),
            removed_session_ids: HashSet::new(),
            dirty_peer_keys: HashSet::new(),
            removed_peer_keys: HashSet::new(),
        }
    }

    /// Ensure directory exists
    async fn ensure_dir(&self) -> Result<()> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir).await?;
        }
        Ok(())
    }

    /// Load sessions.json into cache
    ///
    /// NOTE: This method does NOT create the directory. Directory creation
    /// is deferred to write operations to avoid side effects during lookups.
    async fn load_sessions(&mut self) -> Result<&HashMap<String, SessionEntry>> {
        // Check cache validity
        if let Some(loaded_at) = self.sessions_loaded_at {
            if loaded_at.elapsed().unwrap_or(Duration::MAX) < self.cache_ttl {
                if let Some(ref cache) = self.sessions_cache {
                    return Ok(cache);
                }
            }
        }

        // Load from disk (directory may not exist yet - that's OK)
        let mut entries = if self.sessions_path.exists() {
            let content = fs::read_to_string(&self.sessions_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to read sessions index: {}",
                        self.sessions_path.display()
                    )
                })?;

            if content.trim().is_empty() {
                HashMap::new()
            } else {
                serde_json::from_str(&content).with_context(|| {
                    format!(
                        "Failed to parse sessions index: {}",
                        self.sessions_path.display()
                    )
                })?
            }
        } else {
            HashMap::new()
        };

        // Backfill peer attribution for legacy entries. Sessions
        // branched before `peer_type`/`peer_id` were plumbed through
        // serialize with both fields as `None`, so the peer's
        // routing/list view can't see them. Walk up the parent chain
        // (cycle-safe) and copy the first ancestor's attribution; if
        // no ancestor has it, leave the entry untouched. Sets
        // `sessions_modified` if any field was backfilled so the next
        // `save` persists the fix.
        let backfilled = backfill_peer_attribution(&mut entries);
        self.sessions_cache = Some(entries);
        self.sessions_loaded_at = Some(SystemTime::now());
        self.sessions_modified = backfilled;

        Ok(self.sessions_cache.as_ref().unwrap())
    }

    /// Load peers.json into cache
    ///
    /// NOTE: This method does NOT create the directory. Directory creation
    /// is deferred to write operations to avoid side effects during lookups.
    async fn load_peers(&mut self) -> Result<&PeerIndex> {
        // Check cache validity
        if let Some(loaded_at) = self.peers_loaded_at {
            if loaded_at.elapsed().unwrap_or(Duration::MAX) < self.cache_ttl {
                if let Some(ref cache) = self.peers_cache {
                    return Ok(cache);
                }
            }
        }

        // Load from disk (directory may not exist yet - that's OK)
        let index = if self.peers_path.exists() {
            let content = fs::read_to_string(&self.peers_path)
                .await
                .with_context(|| {
                    format!("Failed to read peers index: {}", self.peers_path.display())
                })?;

            if content.trim().is_empty() {
                PeerIndex::default()
            } else {
                serde_json::from_str(&content).with_context(|| {
                    format!("Failed to parse peers index: {}", self.peers_path.display())
                })?
            }
        } else {
            PeerIndex::default()
        };

        self.peers_cache = Some(index);
        self.peers_loaded_at = Some(SystemTime::now());
        self.peers_modified = false;

        Ok(self.peers_cache.as_ref().unwrap())
    }

    /// Get mutable sessions cache
    async fn load_sessions_mut(&mut self) -> Result<&mut HashMap<String, SessionEntry>> {
        self.load_sessions().await?;
        Ok(self.sessions_cache.as_mut().unwrap())
    }

    /// Get mutable peers cache
    async fn load_peers_mut(&mut self) -> Result<&mut PeerIndex> {
        self.load_peers().await?;
        Ok(self.peers_cache.as_mut().unwrap())
    }

    // =================================================================================
    // Core CRUD Operations
    // =================================================================================

    /// Get session by ID (O(1))
    pub async fn get(&mut self, session_id: &str) -> Result<Option<SessionEntry>> {
        let sessions = self.load_sessions().await?;
        Ok(sessions.get(session_id).cloned())
    }

    /// Get session by ID, reading `sessions.json` directly from disk
    /// (both caches bypassed).
    ///
    /// Used by the engine's per-iteration `compact_requested` peek:
    /// the flag may have been written moments ago by a *different*
    /// `MetadataController` (the session tool's adapter), so the 30s
    /// index cache and the controller cache would both hide it.
    pub async fn get_uncached(&self, session_id: &str) -> Result<Option<SessionEntry>> {
        let on_disk = Self::read_sessions_file(&self.sessions_path).await?;
        Ok(on_disk.get(session_id).cloned())
    }

    /// Insert or update session (O(1))
    pub async fn insert(&mut self, entry: SessionEntry) -> Result<()> {
        let session_id = entry.session_id.clone();
        let sessions = self.load_sessions_mut().await?;
        sessions.insert(session_id.clone(), entry);
        self.sessions_modified = true;
        self.removed_session_ids.remove(&session_id);
        self.dirty_session_ids.insert(session_id);
        Ok(())
    }

    /// Remove session (O(1))
    pub async fn remove(&mut self, session_id: &str) -> Result<Option<SessionEntry>> {
        let sessions = self.load_sessions_mut().await?;
        let removed = sessions.remove(session_id);
        if removed.is_some() {
            self.sessions_modified = true;
            self.dirty_session_ids.remove(session_id);
            self.removed_session_ids.insert(session_id.to_string());
        }
        Ok(removed)
    }

    // =================================================================================
    // Subject Routing Operations (O(1) for critical path)
    // =================================================================================

    /// Get active session for peer (O(1)) - CRITICAL for message routing
    pub async fn get_active_for_peer(&mut self, peer_key: &str) -> Result<Option<SessionEntry>> {
        let peers = self.load_peers().await?;
        let active_id = peers
            .peers
            .get(peer_key)
            .and_then(|p| p.active_session_id.clone());
        let Some(active_id) = active_id else {
            return Ok(None);
        };

        let sessions = self.load_sessions().await?;
        Ok(sessions.get(&active_id).cloned())
    }

    /// Get active session ID for peer (O(1))
    pub async fn get_active_session_id(&mut self, peer_key: &str) -> Result<Option<String>> {
        let peers = self.load_peers().await?;
        Ok(peers
            .peers
            .get(peer_key)
            .and_then(|p| p.active_session_id.clone()))
    }

    /// List all sessions for a peer (O(N) where N = sessions for peer, typically small)
    pub async fn list_for_peer(&mut self, peer_key: &str) -> Result<Vec<SessionEntry>> {
        let peers = self.load_peers().await?;
        let session_ids: Vec<String> = peers
            .peers
            .get(peer_key)
            .map(|p| p.session_ids.clone())
            .unwrap_or_default();
        let sessions = self.load_sessions().await?;
        Ok(session_ids
            .iter()
            .filter_map(|id| sessions.get(id).cloned())
            .collect())
    }

    /// Switch active session for peer (O(1))
    pub async fn set_active_for_peer(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        let peers = self.load_peers_mut().await?;

        let peer_info = peers
            .peers
            .get_mut(peer_key)
            .ok_or_else(|| anyhow::anyhow!("Subject {peer_key} not found"))?;

        peer_info.switch_to(session_id)?;
        self.peers_modified = true;
        self.removed_peer_keys.remove(peer_key);
        self.dirty_peer_keys.insert(peer_key.to_string());

        info!("Switched {} to session {}", peer_key, session_id);
        Ok(())
    }

    /// Clear the active-session pointer for a peer (used when the active
    /// session goes away). The peer entry itself — and its `session_ids`
    /// list — survives, so the peer's other sessions remain routable and
    /// listable.
    pub async fn clear_active_for_peer(&mut self, peer_key: &str) -> Result<()> {
        let peers = self.load_peers_mut().await?;

        if let Some(peer_info) = peers.peers.get_mut(peer_key) {
            if peer_info.active_session_id.is_some() {
                peer_info.active_session_id = None;
                self.peers_modified = true;
                self.removed_peer_keys.remove(peer_key);
                self.dirty_peer_keys.insert(peer_key.to_string());
                info!("Cleared active session pointer for {}", peer_key);
            }
        }

        Ok(())
    }

    /// Remove a session from a peer's routing entry (used when the
    /// session is deleted). Scrubs the id from `session_ids` and clears
    /// the active pointer if it pointed at the removed session. A peer
    /// left with no sessions at all is dropped entirely.
    pub async fn remove_session_from_peer(
        &mut self,
        peer_key: &str,
        session_id: &str,
    ) -> Result<()> {
        let peers = self.load_peers_mut().await?;

        let Some(peer_info) = peers.peers.get_mut(peer_key) else {
            return Ok(());
        };

        let had_session = peer_info.session_ids.iter().any(|id| id == session_id);
        let was_active = peer_info.active_session_id.as_deref() == Some(session_id);
        if !had_session && !was_active {
            return Ok(());
        }

        peer_info.session_ids.retain(|id| id != session_id);
        if was_active {
            peer_info.active_session_id = None;
        }
        let now_empty = peer_info.session_ids.is_empty();

        if now_empty {
            peers.peers.remove(peer_key);
        }
        self.peers_modified = true;
        if now_empty {
            self.dirty_peer_keys.remove(peer_key);
            self.removed_peer_keys.insert(peer_key.to_string());
            info!(
                "Removed session {} from peer {} and dropped the empty peer entry",
                session_id, peer_key
            );
        } else {
            self.removed_peer_keys.remove(peer_key);
            self.dirty_peer_keys.insert(peer_key.to_string());
            info!("Removed session {} from peer {}", session_id, peer_key);
        }

        Ok(())
    }

    /// All peer keys whose routing entry currently references
    /// `session_id` (in `session_ids` or as `active_session_id`). Used
    /// by `SessionManager::set_archived` so an archived session is
    /// scrubbed from every peer that knew about it.
    #[must_use]
    pub async fn peer_keys_with_session(&mut self, session_id: &str) -> Vec<String> {
        let Ok(peers) = self.load_peers().await else {
            return Vec::new();
        };
        peers
            .peers
            .iter()
            .filter_map(|(k, p)| {
                let in_list = p.session_ids.iter().any(|s| s == session_id);
                let is_active = p.active_session_id.as_deref() == Some(session_id);
                if in_list || is_active {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Create new session for peer (O(1))
    pub async fn create_for_peer(&mut self, entry: SessionEntry, peer_key: &str) -> Result<()> {
        let session_id = entry.session_id.clone();

        // Add to sessions.json
        let sessions = self.load_sessions_mut().await?;
        sessions.insert(session_id.clone(), entry);
        self.sessions_modified = true;
        self.removed_session_ids.remove(&session_id);
        self.dirty_session_ids.insert(session_id.clone());

        // Update peers.json
        let peers = self.load_peers_mut().await?;
        let peer_info = peers
            .peers
            .entry(peer_key.to_string())
            .or_insert_with(|| PeerInfo::new(session_id.clone()));

        peer_info.add_session(session_id.clone());
        let active_id = peer_info.active_session_id.clone();
        self.peers_modified = true;
        self.removed_peer_keys.remove(peer_key);
        self.dirty_peer_keys.insert(peer_key.to_string());

        info!(
            "Created session {} for peer {}, active_session_id={:?}",
            session_id, peer_key, active_id
        );
        Ok(())
    }

    /// Ensure a peer routing exists and set the given session as active.
    /// If the peer does not exist, it is created. If the session is not yet
    /// tracked for the peer, it is added.
    pub async fn ensure_peer_active(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        let peers = self.load_peers_mut().await?;

        let peer_info = peers
            .peers
            .entry(peer_key.to_string())
            .or_insert_with(|| PeerInfo::new(session_id.to_string()));

        if !peer_info.session_ids.contains(&session_id.to_string()) {
            peer_info.session_ids.push(session_id.to_string());
        }
        peer_info.active_session_id = Some(session_id.to_string());
        self.peers_modified = true;
        self.removed_peer_keys.remove(peer_key);
        self.dirty_peer_keys.insert(peer_key.to_string());

        info!("Ensured peer {} active session: {}", peer_key, session_id);
        Ok(())
    }

    // =================================================================================
    // Listing Operations
    // =================================================================================

    /// List all sessions (O(N))
    pub async fn list_all(&mut self) -> Result<Vec<SessionEntry>> {
        let sessions = self.load_sessions().await?;
        let mut entries: Vec<_> = sessions.values().cloned().collect();
        // Sort by updated_at descending (most recent first)
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        Ok(entries)
    }

    /// List sessions for agent (O(N))
    pub async fn list_for_agent(&mut self, agent_name: &str) -> Result<Vec<SessionEntry>> {
        let sessions = self.load_sessions().await?;
        let mut entries: Vec<_> = sessions
            .values()
            .filter(|e| e.agent_name == agent_name)
            .cloned()
            .collect();
        // Sort by updated_at descending
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        Ok(entries)
    }

    // =================================================================================
    // Persistence
    // =================================================================================

    /// Save sessions.json if modified
    pub async fn save_sessions(&mut self) -> Result<()> {
        if !self.sessions_modified {
            return Ok(());
        }
        let lock = dir_lock(&self.dir);
        let _guard = lock.lock().await;
        self.merge_save_sessions().await
    }

    /// Save peers.json if modified
    pub async fn save_peers(&mut self) -> Result<()> {
        if !self.peers_modified {
            return Ok(());
        }
        let lock = dir_lock(&self.dir);
        let _guard = lock.lock().await;
        self.merge_save_peers().await
    }

    /// Save both if modified.
    ///
    /// Acquires the per-directory serialization lock once and merges both
    /// files under it, so a concurrent saver on the same directory can't
    /// interleave a sessions write with our peers write.
    pub async fn save(&mut self) -> Result<()> {
        if !self.sessions_modified && !self.peers_modified {
            return Ok(());
        }
        let lock = dir_lock(&self.dir);
        let _guard = lock.lock().await;
        if self.sessions_modified {
            self.merge_save_sessions().await?;
        }
        if self.peers_modified {
            self.merge_save_peers().await?;
        }
        Ok(())
    }

    /// Merge this instance's session deltas onto the current on-disk index
    /// and write the result atomically.
    ///
    /// Caller MUST hold the per-directory lock from [`dir_lock`]. We re-read
    /// `sessions.json` fresh (ignoring our possibly-stale cache), apply only
    /// the entries this instance inserted/removed, then write. This is what
    /// prevents lost updates: a concurrent instance's entries that we never
    /// touched are read back from disk and preserved rather than clobbered by
    /// a whole-map overwrite of our stale cache.
    async fn merge_save_sessions(&mut self) -> Result<()> {
        self.ensure_dir().await?;

        // Cross-process guard (other OS processes); the dir lock guards
        // other tasks in this process.
        let _file_lock = FileLock::acquire(&self.sessions_path, 5000).await?;

        let mut on_disk = Self::read_sessions_file(&self.sessions_path).await?;

        if let Some(cache) = self.sessions_cache.as_ref() {
            for id in &self.dirty_session_ids {
                if let Some(entry) = cache.get(id) {
                    on_disk.insert(id.clone(), entry.clone());
                }
            }
        }
        for id in &self.removed_session_ids {
            on_disk.remove(id);
        }

        Self::write_json_atomic(&self.sessions_path, &on_disk).await?;

        let len = on_disk.len();
        // The merged map is now the authoritative state; adopt it as our
        // cache so subsequent reads within the TTL stay consistent.
        self.sessions_cache = Some(on_disk);
        self.sessions_loaded_at = Some(SystemTime::now());
        self.sessions_modified = false;
        self.dirty_session_ids.clear();
        self.removed_session_ids.clear();

        debug!("Saved sessions.json: {} entries", len);
        Ok(())
    }

    /// Merge this instance's peer-routing deltas onto the current on-disk
    /// index and write atomically. See [`Self::merge_save_sessions`].
    async fn merge_save_peers(&mut self) -> Result<()> {
        self.ensure_dir().await?;

        let _file_lock = FileLock::acquire(&self.peers_path, 5000).await?;

        let mut on_disk = Self::read_peers_file(&self.peers_path).await?;

        if let Some(cache) = self.peers_cache.as_ref() {
            for key in &self.dirty_peer_keys {
                if let Some(info) = cache.peers.get(key) {
                    on_disk.peers.insert(key.clone(), info.clone());
                }
            }
        }
        for key in &self.removed_peer_keys {
            on_disk.peers.remove(key);
        }

        Self::write_json_atomic(&self.peers_path, &on_disk).await?;

        let len = on_disk.peers.len();
        self.peers_cache = Some(on_disk);
        self.peers_loaded_at = Some(SystemTime::now());
        self.peers_modified = false;
        self.dirty_peer_keys.clear();
        self.removed_peer_keys.clear();

        debug!("Saved peers.json: {} peers", len);
        Ok(())
    }

    /// Read and parse sessions.json directly from disk (no cache).
    async fn read_sessions_file(path: &Path) -> Result<HashMap<String, SessionEntry>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read sessions index: {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(HashMap::new());
        }
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse sessions index: {}", path.display()))
    }

    /// Read and parse peers.json directly from disk (no cache).
    async fn read_peers_file(path: &Path) -> Result<PeerIndex> {
        if !path.exists() {
            return Ok(PeerIndex::default());
        }
        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read peers index: {}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(PeerIndex::default());
        }
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse peers index: {}", path.display()))
    }

    /// Serialize `value` and write it to `path` atomically: write a sibling
    /// temp file, fsync it, then `rename(2)` over the target. A reader sees
    /// either the old file or the new one, never a torn write. The temp name
    /// is per-process so a leftover from a crashed run can't collide with a
    /// live writer's rename (the `ENOENT` race in issue #89). Writers are
    /// serialized by the per-directory lock, so the fixed per-process suffix
    /// is safe.
    async fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        let json = serde_json::to_string_pretty(value)?;
        let temp_path = path.with_extension(format!("{}.tmp", std::process::id()));

        let mut file = fs::File::create(&temp_path).await.with_context(|| {
            format!("Failed to create temp index file: {}", temp_path.display())
        })?;
        file.write_all(json.as_bytes()).await?;
        file.sync_all().await?;
        drop(file);

        fs::rename(&temp_path, path).await.with_context(|| {
            format!("Failed to rename index file into place: {}", path.display())
        })?;
        Ok(())
    }

    // =================================================================================
    // Maintenance
    // =================================================================================

    /// Perform maintenance (prune old sessions)
    pub async fn maintenance(&mut self, config: &MaintenanceConfig) -> Result<MaintenanceReport> {
        // First, collect session IDs to prune without holding mutable borrows
        let to_prune: Vec<String> = {
            let sessions = self.load_sessions().await?;

            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            let cutoff = now - config.prune_after.as_millis() as u64;

            sessions
                .iter()
                // Prune exemptions: the principal's trunk session
                // (`root:self` — the only `root:*` id left after the
                // Phase 7 retirement of the per-peer root sessions) is
                // continuous and engine-managed, never idle-prunable;
                // and archived/standing sessions must never lose their
                // transcripts — archiving/standing is a durability
                // promise, not a deletion candidate.
                .filter(|(id, e)| {
                    e.updated_at < cutoff
                        && id.as_str() != "root:self"
                        && !e.archived
                        && !e.standing
                })
                .map(|(k, _)| k.clone())
                .collect()
        };

        let mut pruned = 0;

        for session_id in to_prune {
            // Remove from sessions
            let sessions = self.load_sessions_mut().await?;
            sessions.remove(&session_id);
            self.sessions_modified = true;
            self.dirty_session_ids.remove(&session_id);
            self.removed_session_ids.insert(session_id.clone());

            // Remove from peers. Capture the key set before/after so we can
            // record which peers were modified vs. dropped — the merge-save
            // path applies these deltas onto the fresh on-disk index.
            let peers = self.load_peers_mut().await?;
            let before: HashSet<String> = peers.peers.keys().cloned().collect();
            for peer_info in peers.peers.values_mut() {
                peer_info.session_ids.retain(|id| id != &session_id);
            }
            // Remove empty peers
            peers.peers.retain(|_, p| !p.session_ids.is_empty());
            let after: HashSet<String> = peers.peers.keys().cloned().collect();
            self.peers_modified = true;
            // Surviving peers had their session list edited → dirty.
            for key in &after {
                self.removed_peer_keys.remove(key);
                self.dirty_peer_keys.insert(key.clone());
            }
            // Peers that disappeared (no sessions left) → removed.
            for key in before.difference(&after) {
                self.dirty_peer_keys.remove(key);
                self.removed_peer_keys.insert(key.clone());
            }

            // Delete transcript file
            let transcript_path = self
                .dir
                .join(format!("{}.jsonl", safe_filename_component(&session_id)));
            if transcript_path.exists() {
                let _ = fs::remove_file(&transcript_path).await;
            }

            pruned += 1;
        }

        if pruned > 0 {
            self.save().await?;
            info!("Pruned {} old sessions", pruned);
        }

        // Get total count for report
        let sessions = self.load_sessions().await?;
        let total_sessions: usize = sessions.len();

        Ok(MaintenanceReport {
            pruned,
            total: total_sessions,
        })
    }
}

/// One-shot backfill for legacy entries that predate `peer_type` /
/// `peer_id`. For each entry where either field is `None`, walk up
/// the parent chain (cycle-safe) and copy any ancestor field that is
/// set. Returns `true` if any field was backfilled (caller should
/// mark the index modified so the next save persists the fix).
fn backfill_peer_attribution(entries: &mut HashMap<String, SessionEntry>) -> bool {
    let mut changed = false;
    // Snapshot ids so we can mutate entries in place without borrowing
    // the map immutably while we walk the chain.
    let ids: Vec<String> = entries.keys().cloned().collect();
    for id in ids {
        let Some(entry) = entries.get(&id) else {
            continue;
        };
        if entry.peer_type.is_some() && entry.peer_id.is_some() {
            continue;
        }
        // Walk up the parent chain (cycle-safe). Cap at the entry
        // count so a corrupted chain can't loop forever.
        let cap = entries.len();
        let mut cursor = id.clone();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(cursor.clone());
        let mut peer_type: Option<String> = None;
        let mut peer_id: Option<String> = None;
        for _ in 0..cap {
            let Some(entry) = entries.get(&cursor) else {
                break;
            };
            if peer_type.is_none() {
                peer_type = entry.peer_type.clone();
            }
            if peer_id.is_none() {
                peer_id = entry.peer_id.clone();
            }
            if peer_type.is_some() && peer_id.is_some() {
                break;
            }
            let Some(parent) = entry.parent_session_id.clone() else {
                break;
            };
            if !visited.insert(parent.clone()) {
                break;
            }
            cursor = parent;
        }
        if let Some(entry) = entries.get_mut(&id) {
            if entry.peer_type.is_none() {
                if let Some(t) = peer_type.take() {
                    entry.peer_type = Some(t);
                    changed = true;
                }
            }
            if entry.peer_id.is_none() {
                if let Some(i) = peer_id.take() {
                    entry.peer_id = Some(i);
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use crate::index::backfill_peer_attribution;
    use crate::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Regression test for issue #89: concurrent `SessionIndex` instances on
    /// the same directory must not lose each other's writes.
    ///
    /// Before the per-directory serialization lock + delta-merge save, each
    /// instance read its own (possibly empty) cache, mutated it, and wrote the
    /// *whole* map back — so N peers racing to create sessions left only the
    /// last writer's entry. Here we spawn N tasks, each opening its own index
    /// (mirroring the per-turn instances in `agent_runner.rs`) and inserting a
    /// distinct session, then assert all N survive on disk.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_inserts_do_not_lose_updates() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().to_path_buf();
        let n = 25;

        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            let dir = dir.clone();
            handles.push(tokio::spawn(async move {
                // Fresh instance per task — independent caches, same files.
                let mut index = SessionIndex::open(&dir);
                let id = format!("sess_{i}");
                let entry = SessionEntry::with_peer(
                    id.clone(),
                    "testagent".to_string(),
                    format!("{id}.jsonl"),
                    "user",
                    format!("peer-{i}"),
                );
                index
                    .create_for_peer(entry, &format!("user:peer-{i}"))
                    .await
                    .unwrap();
                index.save().await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // A fresh reader must see every session and every peer routing.
        let mut reader = SessionIndex::open(&dir);
        let all = reader.list_all().await.unwrap();
        assert_eq!(all.len(), n, "all concurrent sessions should survive");
        for i in 0..n {
            assert!(
                reader.get(&format!("sess_{i}")).await.unwrap().is_some(),
                "session sess_{i} was lost"
            );
            assert!(
                reader
                    .get_active_for_peer(&format!("user:peer-{i}"))
                    .await
                    .unwrap()
                    .is_some(),
                "peer routing user:peer-{i} was lost"
            );
        }
    }

    #[tokio::test]
    async fn test_session_entry_crud() {
        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());

        // Create
        let entry = SessionEntry::new(
            "sess_123".to_string(),
            "testagent".to_string(),
            "sess_123.jsonl".to_string(),
        );
        index.insert(entry.clone()).await.unwrap();

        // Read
        let found = index.get("sess_123").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().session_id, "sess_123");

        // Update
        let mut updated = entry.clone();
        updated.title = Some("New Title".to_string());
        index.insert(updated).await.unwrap();

        let found = index.get("sess_123").await.unwrap();
        assert_eq!(found.unwrap().title, Some("New Title".to_string()));

        // Delete
        let removed = index.remove("sess_123").await.unwrap();
        assert!(removed.is_some());

        let not_found = index.get("sess_123").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_peer_routing() {
        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());

        let peer_key = "agent:test:peer:user:alice";

        // Create session for peer
        let entry = SessionEntry::new(
            "sess_abc".to_string(),
            "testagent".to_string(),
            "sess_abc.jsonl".to_string(),
        );
        index.create_for_peer(entry, peer_key).await.unwrap();

        // Get active
        let active = index.get_active_for_peer(peer_key).await.unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().session_id, "sess_abc");

        // Create another session
        let entry2 = SessionEntry::new(
            "sess_def".to_string(),
            "testagent".to_string(),
            "sess_def.jsonl".to_string(),
        );
        index.create_for_peer(entry2, peer_key).await.unwrap();

        // Active should be the new one
        let active = index.get_active_for_peer(peer_key).await.unwrap();
        assert_eq!(active.unwrap().session_id, "sess_def");

        // Switch back
        index
            .set_active_for_peer(peer_key, "sess_abc")
            .await
            .unwrap();

        let active = index.get_active_for_peer(peer_key).await.unwrap();
        assert_eq!(active.unwrap().session_id, "sess_abc");

        // List all
        let all = index.list_for_peer(peer_key).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_persistence() {
        let temp = TempDir::new().unwrap();

        // Create and save
        {
            let mut index = SessionIndex::open(temp.path());
            let entry = SessionEntry::new(
                "sess_123".to_string(),
                "testagent".to_string(),
                "sess_123.jsonl".to_string(),
            );
            index.insert(entry).await.unwrap();
            index.save().await.unwrap();
        }

        // Load and verify
        {
            let mut index = SessionIndex::open(temp.path());
            let found = index.get("sess_123").await.unwrap();
            assert!(found.is_some());
        }
    }

    /// Backward compatibility: a `sessions.json` entry written before
    /// the `archived` / `compact_requested` / `standing` / `privileged`
    /// fields existed must deserialize with all flags = false
    /// (`#[serde(default)]`).
    #[test]
    fn test_legacy_entry_without_archive_flags_defaults_false() {
        let legacy = serde_json::json!({
            "session_id": "sess_legacy",
            "agent_name": "testagent",
            "created_at": 1,
            "updated_at": 2,
            "message_count": 3,
            "turn_count": 1,
            "last_total_tokens": 100,
            "total_input_tokens": 80,
            "total_output_tokens": 20,
            "transcript_file": "sess_legacy.jsonl",
            "title": null,
            "parent_session_id": null,
            "trigger": "user",
            "peer_type": "user",
            "peer_id": "alice"
        });

        let entry: SessionEntry = serde_json::from_value(legacy).unwrap();
        assert!(!entry.archived);
        assert!(!entry.compact_requested);
        assert!(!entry.standing);
        assert!(!entry.privileged);

        // And the flags round-trip through serialization once set.
        let mut entry = entry;
        entry.archived = true;
        entry.compact_requested = true;
        entry.standing = true;
        entry.privileged = true;
        let json = serde_json::to_value(&entry).unwrap();
        let reloaded: SessionEntry = serde_json::from_value(json).unwrap();
        assert!(reloaded.archived);
        assert!(reloaded.compact_requested);
        assert!(reloaded.standing);
        assert!(reloaded.privileged);
    }

    /// Maintenance must retain sessions exempt from pruning: the
    /// principal's trunk `root:self` (the only `root:*` id left after
    /// Phase 7 retired the per-peer root sessions — a legacy
    /// `root:user:*` id has NO exemption and prunes like any plain
    /// session), archived sessions, and standing sessions — while
    /// still pruning plain idle sessions past the cutoff.
    #[tokio::test]
    async fn maintenance_prune_exemptions() {
        use crate::index::{MaintenanceConfig, DEFAULT_MAX_SESSIONS};
        use std::time::{Duration, SystemTime};
        use tokio::fs;

        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let two_days_ms: u64 = 2 * 24 * 60 * 60 * 1000;

        async fn insert_old(
            index: &mut SessionIndex,
            id: &str,
            updated_at: u64,
            mutate: impl FnOnce(&mut SessionEntry),
        ) {
            let mut entry = SessionEntry::new(
                id.to_string(),
                "testagent".to_string(),
                format!("{}.jsonl", safe_filename_component(id)),
            );
            entry.updated_at = updated_at;
            mutate(&mut entry);
            index.insert(entry).await.unwrap();
        }

        // One per exemption class, plus two plain idle sessions (one
        // carrying a retired `root:user:*`-shaped id — no longer
        // exempt).
        insert_old(&mut index, "root:self", now - two_days_ms, |_| {}).await;
        insert_old(&mut index, "sess_archived", now - two_days_ms, |e| {
            e.archived = true;
        })
        .await;
        insert_old(&mut index, "sess_standing", now - two_days_ms, |e| {
            e.standing = true;
        })
        .await;
        insert_old(&mut index, "sess_plain", now - two_days_ms, |_| {}).await;
        insert_old(&mut index, "root:user:alice", now - two_days_ms, |_| {}).await;
        index.save().await.unwrap();

        // Transcripts exist on disk for the exempt + plain sessions.
        for id in [
            "root:self",
            "sess_archived",
            "sess_standing",
            "sess_plain",
            "root:user:alice",
        ] {
            fs::write(
                temp.path()
                    .join(format!("{}.jsonl", safe_filename_component(id))),
                "{}\n",
            )
            .await
            .unwrap();
        }

        let config = MaintenanceConfig {
            prune_after: Duration::from_secs(24 * 60 * 60),
            max_sessions: DEFAULT_MAX_SESSIONS,
        };
        let report = index.maintenance(&config).await.unwrap();
        assert_eq!(
            report.pruned, 2,
            "the plain idle session AND the retired root:user:* id prune"
        );
        assert_eq!(report.total, 3);

        for id in ["root:self", "sess_archived", "sess_standing"] {
            assert!(index.get(id).await.unwrap().is_some(), "{id} retained");
            assert!(
                temp.path()
                    .join(format!("{}.jsonl", safe_filename_component(id)))
                    .exists(),
                "{id} transcript retained"
            );
        }
        for id in ["sess_plain", "root:user:alice"] {
            assert!(index.get(id).await.unwrap().is_none(), "{id} pruned");
            assert!(
                !temp
                    .path()
                    .join(format!("{}.jsonl", safe_filename_component(id)))
                    .exists(),
                "{id} transcript pruned"
            );
        }
    }

    /// `clear_active_for_peer` drops only the active-session pointer;
    /// the peer entry and its remaining sessions stay listable.
    #[tokio::test]
    async fn test_clear_active_for_peer_keeps_peer_entry() {
        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());
        let peer_key = "user:alice";

        for id in ["sess_a", "sess_b"] {
            let entry = SessionEntry::new(
                id.to_string(),
                "testagent".to_string(),
                format!("{id}.jsonl"),
            );
            index.create_for_peer(entry, peer_key).await.unwrap();
        }
        assert_eq!(
            index.get_active_session_id(peer_key).await.unwrap(),
            Some("sess_b".to_string())
        );

        index.clear_active_for_peer(peer_key).await.unwrap();

        // Active pointer cleared, but the peer entry survives with both
        // sessions still listable.
        assert_eq!(index.get_active_session_id(peer_key).await.unwrap(), None);
        let all = index.list_for_peer(peer_key).await.unwrap();
        assert_eq!(all.len(), 2);
    }

    /// `remove_session_from_peer` scrubs the id from `session_ids`,
    /// clears the active pointer when it matched, and drops the peer
    /// entry only when no sessions remain.
    #[tokio::test]
    async fn test_remove_session_from_peer() {
        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());
        let peer_key = "user:alice";

        for id in ["sess_a", "sess_b"] {
            let entry = SessionEntry::new(
                id.to_string(),
                "testagent".to_string(),
                format!("{id}.jsonl"),
            );
            index.create_for_peer(entry, peer_key).await.unwrap();
        }

        // Remove the active session: pointer cleared, other session kept.
        index
            .remove_session_from_peer(peer_key, "sess_b")
            .await
            .unwrap();
        assert_eq!(index.get_active_session_id(peer_key).await.unwrap(), None);
        let remaining = index.list_for_peer(peer_key).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].session_id, "sess_a");

        // Remove the last session: the peer entry disappears entirely.
        index
            .remove_session_from_peer(peer_key, "sess_a")
            .await
            .unwrap();
        assert!(index.list_for_peer(peer_key).await.unwrap().is_empty());
        assert_eq!(index.get_active_session_id(peer_key).await.unwrap(), None);

        // Unknown session / unknown peer: no-op, no error.
        index
            .remove_session_from_peer(peer_key, "sess_nope")
            .await
            .unwrap();
        index
            .remove_session_from_peer("user:nobody", "sess_a")
            .await
            .unwrap();
    }

    #[test]
    fn backfill_peer_attribution_copies_from_parent_chain() {
        let mut entries: HashMap<String, SessionEntry> = HashMap::new();
        // Ancestor with full attribution.
        entries.insert(
            "root:user:alice".to_string(),
            SessionEntry::with_peer(
                "root:user:alice".to_string(),
                "testagent".to_string(),
                "root:user:alice.jsonl".to_string(),
                "user",
                "alice",
            ),
        );
        // Legacy branch: peer fields None, parent has them.
        let mut legacy_branch = SessionEntry::new(
            "branch1".to_string(),
            "testagent".to_string(),
            "branch1.jsonl".to_string(),
        );
        legacy_branch.parent_session_id = Some("root:user:alice".to_string());
        entries.insert("branch1".to_string(), legacy_branch);
        // Legacy branch-of-branch: parent also has None — chain
        // continues upward and finds the root's attribution.
        let mut legacy_grandchild = SessionEntry::new(
            "grand1".to_string(),
            "testagent".to_string(),
            "grand1.jsonl".to_string(),
        );
        legacy_grandchild.parent_session_id = Some("branch1".to_string());
        entries.insert("grand1".to_string(), legacy_grandchild);

        let changed = backfill_peer_attribution(&mut entries);
        assert!(changed);
        assert_eq!(
            entries.get("branch1").unwrap().peer_type.as_deref(),
            Some("user")
        );
        assert_eq!(
            entries.get("branch1").unwrap().peer_id.as_deref(),
            Some("alice")
        );
        assert_eq!(
            entries.get("grand1").unwrap().peer_type.as_deref(),
            Some("user")
        );
        assert_eq!(
            entries.get("grand1").unwrap().peer_id.as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn backfill_peer_attribution_handles_orphans() {
        // No ancestor with attribution ⇒ left untouched, no panic.
        let mut entries: HashMap<String, SessionEntry> = HashMap::new();
        let mut orphan = SessionEntry::new(
            "orphan".to_string(),
            "testagent".to_string(),
            "orphan.jsonl".to_string(),
        );
        orphan.parent_session_id = Some("ghost".to_string()); // parent absent
        entries.insert("orphan".to_string(), orphan);

        let changed = backfill_peer_attribution(&mut entries);
        assert!(!changed);
        assert!(entries.get("orphan").unwrap().peer_type.is_none());
        assert!(entries.get("orphan").unwrap().peer_id.is_none());
    }

    #[test]
    fn backfill_peer_attribution_handles_partial_parent() {
        // Parent has `peer_type` but no `peer_id` — only fill in the
        // missing fields on the child, do not overwrite existing ones.
        let mut entries: HashMap<String, SessionEntry> = HashMap::new();
        let mut parent = SessionEntry::new(
            "parent".to_string(),
            "testagent".to_string(),
            "parent.jsonl".to_string(),
        );
        parent.peer_type = Some("user".to_string());
        parent.peer_id = None; // incomplete
        entries.insert("parent".to_string(), parent);
        let mut child = SessionEntry::new(
            "child".to_string(),
            "testagent".to_string(),
            "child.jsonl".to_string(),
        );
        child.parent_session_id = Some("parent".to_string());
        entries.insert("child".to_string(), child);

        let changed = backfill_peer_attribution(&mut entries);
        assert!(changed, "peer_type was copied down");
        let child = entries.get("child").unwrap();
        // peer_type came from the partial parent; peer_id stayed None
        // because no ancestor in the chain has it.
        assert_eq!(child.peer_type.as_deref(), Some("user"));
        assert!(child.peer_id.is_none(), "no ancestor had peer_id");
    }

    #[test]
    fn backfill_peer_attribution_survives_cycles() {
        // Cycle: a → b → a. The walk must terminate.
        let mut entries: HashMap<String, SessionEntry> = HashMap::new();
        let mut a = SessionEntry::new(
            "a".to_string(),
            "testagent".to_string(),
            "a.jsonl".to_string(),
        );
        a.parent_session_id = Some("b".to_string());
        a.peer_type = Some("user".to_string());
        a.peer_id = Some("alice".to_string());
        let mut b = SessionEntry::new(
            "b".to_string(),
            "testagent".to_string(),
            "b.jsonl".to_string(),
        );
        b.parent_session_id = Some("a".to_string());
        entries.insert("a".to_string(), a);
        entries.insert("b".to_string(), b);

        // Should terminate without infinite loop and pick up the
        // attribution from `a` (first reachable ancestor with both
        // fields).
        let changed = backfill_peer_attribution(&mut entries);
        assert!(changed);
        assert_eq!(entries.get("b").unwrap().peer_type.as_deref(), Some("user"));
        assert_eq!(entries.get("b").unwrap().peer_id.as_deref(), Some("alice"));
    }
}
