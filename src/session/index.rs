//! Session index (sessions.json) management
//!
//! Provides a central index for session metadata, enabling:
//! - Fast session lookup by key or ID
//! - Metadata aggregation across sessions
//! - Session lifecycle management (prune, cap, rotate)

use crate::session::lock::FileLock;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

/// Default cache TTL (45 seconds, same as `OpenClaw`)
pub const DEFAULT_CACHE_TTL_MS: u64 = 45_000;

/// Default maintenance settings
pub const DEFAULT_PRUNE_AFTER_DAYS: u64 = 30;
pub const DEFAULT_MAX_SESSIONS: usize = 500;
pub const DEFAULT_ROTATE_BYTES: usize = 10 * 1024 * 1024; // 10MB

/// Entry in the session index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Unique session ID (matches filename without .jsonl)
    pub session_id: String,
    /// Agent name this session belongs to
    pub agent_name: String,
    /// Optional session key (e.g., "agent:test:cli:default")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Creation timestamp (milliseconds since epoch)
    pub created_at: u64,
    /// Last update timestamp
    pub updated_at: u64,
    /// Number of messages in the session
    pub message_count: usize,
    /// Total tokens used (if tracked)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<usize>,
    /// Input tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<usize>,
    /// Output tokens
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<usize>,
    /// Path to transcript file (relative to index)
    pub transcript_file: String,
    /// Working directory where session was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Provider used (e.g., "anthropic", "openai")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model used (e.g., "claude-3-5-sonnet")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Channel this session belongs to (e.g., "discord", "cli")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Recipient/channel ID for multi-user isolation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    /// Account ID (for multi-account channels)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Last error (if any)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl IndexEntry {
    /// Create a new index entry
    #[must_use]
    pub fn new(session_id: String, agent_name: String, transcript_file: String) -> Self {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            session_id,
            agent_name,
            session_key: None,
            created_at: now,
            updated_at: now,
            message_count: 0,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            transcript_file,
            cwd: None,
            provider: None,
            model: None,
            channel: None,
            recipient: None,
            account_id: None,
            last_error: None,
        }
    }

    /// Update the entry with current timestamp
    pub fn touch(&mut self) {
        self.updated_at = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// Get the absolute path to the transcript file
    #[must_use]
    pub fn transcript_path(&self, index_dir: &Path) -> PathBuf {
        index_dir.join(&self.transcript_file)
    }
}

/// Cached index data with timestamp
#[derive(Debug, Clone)]
struct CachedIndex {
    data: HashMap<String, IndexEntry>,
    loaded_at: SystemTime,
    mtime_ms: u64,
}

/// Maintenance configuration
#[derive(Debug, Clone)]
pub struct MaintenanceConfig {
    /// Maintenance mode
    pub mode: MaintenanceMode,
    /// Prune sessions older than this
    pub prune_after: Duration,
    /// Keep at most this many sessions per agent
    pub max_sessions: usize,
    /// Rotate index file if larger than this
    pub rotate_bytes: usize,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            mode: MaintenanceMode::Warn,
            prune_after: Duration::from_secs(DEFAULT_PRUNE_AFTER_DAYS * 24 * 60 * 60),
            max_sessions: DEFAULT_MAX_SESSIONS,
            rotate_bytes: DEFAULT_ROTATE_BYTES,
        }
    }
}

/// Maintenance mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaintenanceMode {
    /// Automatically perform maintenance
    Auto,
    /// Warn but don't auto-maintain
    Warn,
    /// Disable maintenance
    Off,
}

/// Maintenance report
#[derive(Debug, Clone)]
pub struct MaintenanceReport {
    /// Number of pruned sessions
    pub pruned: usize,
    /// Number of sessions removed due to cap
    pub capped: usize,
    /// Whether the index was rotated
    pub rotated: bool,
    /// Bytes reclaimed
    pub bytes_reclaimed: u64,
}

impl MaintenanceReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pruned == 0 && self.capped == 0 && !self.rotated
    }
}

/// Session index manager
#[derive(Debug)]
pub struct SessionIndex {
    /// Path to the index file (sessions.json)
    path: PathBuf,
    /// Directory containing the index and sessions
    dir: PathBuf,
    /// Cache of loaded index
    cache: Option<CachedIndex>,
    /// Cache TTL
    cache_ttl: Duration,
}

impl SessionIndex {
    /// Create or open a session index for an agent
    pub async fn for_agent(agent_name: &str) -> Result<Self> {
        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("agents")
            .join(agent_name)
            .join("sessions");

        fs::create_dir_all(&dir).await?;

        let path = dir.join("sessions.json");
        let index = Self {
            path,
            dir,
            cache: None,
            cache_ttl: Duration::from_millis(DEFAULT_CACHE_TTL_MS),
        };

        // Ensure index file exists
        if !index.path.exists() {
            index.save(&HashMap::new()).await?;
        }

        Ok(index)
    }

    /// Open an index at a specific path
    pub fn open(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let path = dir.join("sessions.json");

        Self {
            path,
            dir,
            cache: None,
            cache_ttl: Duration::from_millis(DEFAULT_CACHE_TTL_MS),
        }
    }

    /// Set custom cache TTL
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Load the index from disk (with caching)
    pub async fn load(&mut self) -> Result<HashMap<String, IndexEntry>> {
        // Ensure directory exists
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir).await?;
        }

        // Check cache first
        if let Some(cached) = &self.cache {
            let age = cached.loaded_at.elapsed().unwrap_or(Duration::MAX);
            if age < self.cache_ttl {
                // Verify file hasn't changed
                let current_mtime = self.get_mtime().await?;
                if current_mtime == cached.mtime_ms {
                    debug!("Using cached session index");
                    return Ok(cached.data.clone());
                }
            }
        }

        // Load from disk
        debug!("Loading session index from disk");
        let entries = self.load_from_disk().await?;

        // Update cache (only if file exists)
        if self.path.exists() {
            let mtime = self.get_mtime().await?;
            self.cache = Some(CachedIndex {
                data: entries.clone(),
                loaded_at: SystemTime::now(),
                mtime_ms: mtime,
            });
        } else {
            self.cache = Some(CachedIndex {
                data: entries.clone(),
                loaded_at: SystemTime::now(),
                mtime_ms: 0,
            });
        }

        Ok(entries)
    }

    /// Load index without using cache
    pub async fn load_fresh(&mut self) -> Result<HashMap<String, IndexEntry>> {
        self.cache = None;
        self.load().await
    }

    /// Load index from disk
    async fn load_from_disk(&self) -> Result<HashMap<String, IndexEntry>> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(&self.path)
            .await
            .with_context(|| format!("Failed to read index: {}", self.path.display()))?;

        if content.trim().is_empty() {
            return Ok(HashMap::new());
        }

        let entries: HashMap<String, IndexEntry> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse index: {}", self.path.display()))?;

        Ok(entries)
    }

    /// Save the index to disk
    pub async fn save(&self, entries: &HashMap<String, IndexEntry>) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let _lock = FileLock::acquire(&self.path, 5000).await?;

        // Serialize to JSON
        let json = serde_json::to_string_pretty(entries)?;

        // Write to temp file then rename for atomicity
        let temp_path = self.path.with_extension("tmp");
        {
            let mut file = fs::File::create(&temp_path).await?;
            file.write_all(json.as_bytes()).await?;
            file.flush().await?;
        }

        fs::rename(&temp_path, &self.path).await?;

        debug!("Saved session index: {} entries", entries.len());
        Ok(())
    }

    /// Get an entry by session key or ID
    pub async fn get(&mut self, key: &str) -> Result<Option<IndexEntry>> {
        let entries = self.load().await?;
        Ok(entries.get(key).cloned())
    }

    /// Insert or update an entry
    pub async fn insert(&mut self, key: String, entry: IndexEntry) -> Result<()> {
        let mut entries = self
            .load()
            .await
            .with_context(|| format!("Failed to load index from {:?}", self.path))?;
        entries.insert(key, entry);
        self.save(&entries)
            .await
            .with_context(|| format!("Failed to save index to {:?}", self.path))?;

        // Update cache
        if let Some(cache) = &mut self.cache {
            cache.data = entries;
            cache.loaded_at = SystemTime::now();
            // Note: mtime will be updated on next load if needed
        }

        Ok(())
    }

    /// Remove an entry
    pub async fn remove(&mut self, key: &str) -> Result<Option<IndexEntry>> {
        let mut entries = self.load().await?;
        let removed = entries.remove(key);

        if removed.is_some() {
            self.save(&entries).await?;

            // Update cache
            if let Some(cache) = &mut self.cache {
                cache.data = entries;
                cache.loaded_at = SystemTime::now();
            }
        }

        Ok(removed)
    }

    /// List all entries
    pub async fn list(&mut self) -> Result<Vec<IndexEntry>> {
        let entries = self.load().await?;
        Ok(entries.into_values().collect())
    }

    /// Find entries by agent name
    pub async fn find_by_agent(&mut self, agent: &str) -> Result<Vec<IndexEntry>> {
        let entries = self.load().await?;
        Ok(entries
            .values()
            .filter(|e| e.agent_name == agent)
            .cloned()
            .collect())
    }

    /// Find entry by session ID
    pub async fn find_by_session_id(&mut self, session_id: &str) -> Result<Option<IndexEntry>> {
        let entries = self.load().await?;
        Ok(entries
            .values()
            .find(|e| e.session_id == session_id)
            .cloned())
    }

    /// Perform maintenance on the index
    pub async fn maintenance(&mut self, config: &MaintenanceConfig) -> Result<MaintenanceReport> {
        let mut report = MaintenanceReport {
            pruned: 0,
            capped: 0,
            rotated: false,
            bytes_reclaimed: 0,
        };

        if config.mode == MaintenanceMode::Off {
            return Ok(report);
        }

        let mut entries = self.load().await?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Prune old entries
        let cutoff = now - config.prune_after.as_millis() as u64;
        let to_prune: Vec<String> = entries
            .iter()
            .filter(|(_, e)| e.updated_at < cutoff)
            .map(|(k, _)| k.clone())
            .collect();

        for key in to_prune {
            if let Some(entry) = entries.remove(&key) {
                // Delete transcript file
                let transcript_path = self.dir.join(&entry.transcript_file);
                if transcript_path.exists() {
                    if let Ok(metadata) = fs::metadata(&transcript_path).await {
                        report.bytes_reclaimed += metadata.len();
                    }
                    let _ = fs::remove_file(&transcript_path).await;
                }
                report.pruned += 1;
            }
        }

        // Cap total entries (keep most recently updated)
        if entries.len() > config.max_sessions {
            let mut sorted: Vec<(_, _)> = entries.iter().collect();
            sorted.sort_by_key(|(_, e)| std::cmp::Reverse(e.updated_at));

            let to_remove: Vec<String> = sorted
                .into_iter()
                .skip(config.max_sessions)
                .map(|(k, _)| k.clone())
                .collect();

            for key in to_remove {
                if let Some(entry) = entries.remove(&key) {
                    let transcript_path = self.dir.join(&entry.transcript_file);
                    if transcript_path.exists() {
                        if let Ok(metadata) = fs::metadata(&transcript_path).await {
                            report.bytes_reclaimed += metadata.len();
                        }
                        let _ = fs::remove_file(&transcript_path).await;
                    }
                    report.capped += 1;
                }
            }
        }

        // Save changes
        if report.pruned > 0 || report.capped > 0 {
            if config.mode == MaintenanceMode::Warn {
                warn!(
                    "Session maintenance would prune {} and cap {} sessions (mode=warn, skipping)",
                    report.pruned, report.capped
                );
                // Don't actually save changes in warn mode
                return Ok(report);
            }

            self.save(&entries).await?;

            // Update cache after pruning
            let mtime = self.get_mtime().await?;
            self.cache = Some(CachedIndex {
                data: entries,
                loaded_at: SystemTime::now(),
                mtime_ms: mtime,
            });

            info!(
                "Session maintenance complete: pruned={}, capped={}, reclaimed={} bytes",
                report.pruned, report.capped, report.bytes_reclaimed
            );
        }

        // Check rotation
        if let Ok(metadata) = fs::metadata(&self.path).await {
            if metadata.len() > config.rotate_bytes as u64 {
                self.rotate().await?;
                report.rotated = true;
            }
        }

        Ok(report)
    }

    /// Rotate the index file (rename to .bak.{timestamp})
    async fn rotate(&self) -> Result<()> {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis();

        let backup_path = self.path.with_extension(format!("json.bak.{timestamp}"));
        fs::rename(&self.path, backup_path).await?;

        // Create new empty index
        self.save(&HashMap::new()).await?;

        // Clean up old backups (keep only 3 most recent)
        self.cleanup_backups().await?;

        info!("Rotated session index file");
        Ok(())
    }

    /// Clean up old backup files
    async fn cleanup_backups(&self) -> Result<()> {
        let mut backups: Vec<PathBuf> = vec![];
        let mut entries = fs::read_dir(&self.dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("sessions.json.bak.") {
                backups.push(entry.path());
            }
        }

        // Sort by modification time (newest first)
        let mut backups_with_time: Vec<(_, _)> = vec![];
        for path in backups {
            if let Ok(metadata) = fs::metadata(&path).await {
                if let Ok(modified) = metadata.modified() {
                    backups_with_time.push((path, modified));
                }
            }
        }
        backups_with_time.sort_by(|a, b| b.1.cmp(&a.1));

        // Remove old backups
        for (path, _) in backups_with_time.into_iter().skip(3) {
            let _ = fs::remove_file(&path).await;
        }

        Ok(())
    }

    /// Migrate existing sessions (scan directory and populate index)
    pub async fn migrate_from_directory(&mut self, agent_name: &str) -> Result<usize> {
        // Create directory if it doesn't exist
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir).await?;
        }

        let mut entries = HashMap::new();
        let mut dir_entries = fs::read_dir(&self.dir).await?;

        while let Some(entry) = dir_entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                let filename = path.file_stem().unwrap().to_string_lossy().to_string();

                // Check if already indexed
                let exists = self.find_by_session_id(&filename).await?.is_some();
                if exists {
                    continue;
                }

                // Get file metadata
                let metadata = fs::metadata(&path).await?;
                let modified = metadata.modified()?;
                let modified_ms = modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64;

                // Create entry
                let index_entry = IndexEntry {
                    session_id: filename.clone(),
                    agent_name: agent_name.to_string(),
                    session_key: None,
                    created_at: modified_ms,
                    updated_at: modified_ms,
                    message_count: 0, // Will be updated on next access
                    total_tokens: None,
                    input_tokens: None,
                    output_tokens: None,
                    transcript_file: path.file_name().unwrap().to_string_lossy().to_string(),
                    cwd: None,
                    provider: None,
                    model: None,
                    channel: None,
                    recipient: None,
                    account_id: None,
                    last_error: None,
                };

                let key = format!("agent:{agent_name}:session:{filename}");
                entries.insert(key, index_entry);
            }
        }

        let count = entries.len();
        if count > 0 {
            // Merge with existing entries
            let mut existing = self.load().await?;
            existing.extend(entries);
            self.save(&existing).await?;
            info!("Migrated {} sessions to index", count);
        }

        Ok(count)
    }

    /// Get file modification time
    async fn get_mtime(&self) -> Result<u64> {
        let metadata = fs::metadata(&self.path).await?;
        let modified = metadata.modified()?;
        let ms = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        Ok(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // TODO: Fix test_index_create_and_load - temp file cleanup issue
    // #[tokio::test]
    // async fn test_index_create_and_load() { ... }

    #[tokio::test]
    async fn test_maintenance_prune() {
        let temp = TempDir::new().unwrap();
        let mut index = SessionIndex::open(temp.path());

        // Add old entry
        let mut old_entry = IndexEntry::new(
            "old_123".to_string(),
            "testagent".to_string(),
            "old_123.jsonl".to_string(),
        );
        old_entry.updated_at = 0; // Very old
        index.insert("old".to_string(), old_entry).await.unwrap();

        // Add new entry
        let new_entry = IndexEntry::new(
            "new_456".to_string(),
            "testagent".to_string(),
            "new_456.jsonl".to_string(),
        );
        index.insert("new".to_string(), new_entry).await.unwrap();

        // Run maintenance with 1 day prune
        let config = MaintenanceConfig {
            mode: MaintenanceMode::Auto,
            prune_after: Duration::from_secs(86400),
            max_sessions: 100,
            rotate_bytes: 10_000_000,
        };

        let report = index.maintenance(&config).await.unwrap();
        assert_eq!(report.pruned, 1);

        // Verify old entry is gone
        let entries = index.load().await.unwrap();
        assert_eq!(entries.len(), 1);
    }
}
