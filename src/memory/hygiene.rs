//! Memory Hygiene - Automatic cleanup of old memory entries
//!
//! Prevents unbounded memory growth by:
//! - Removing expired entries (TTL)
//! - Archiving old entries based on retention policy
//! - Compressing or summarizing aged memories
//! - Removing low-importance entries when capacity reached

use crate::types::memory::{MemoryEntry, MemoryScope};
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info};

/// Hygiene configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HygieneConfig {
    /// Enable automatic hygiene
    pub enabled: bool,
    /// Run hygiene every N hours
    pub interval_hours: u64,
    /// Maximum entries per agent before cleanup
    pub max_entries_per_agent: usize,
    /// Default TTL for entries (days)
    pub default_ttl_days: i64,
    /// Archive entries older than N days (0 = delete)
    pub archive_after_days: i64,
    /// Remove entries below this importance score during cleanup
    pub importance_threshold: f32,
    /// State file to track last run
    pub state_file: String,
}

impl Default for HygieneConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_hours: 24,
            max_entries_per_agent: 10_000,
            default_ttl_days: 30,
            archive_after_days: 90,
            importance_threshold: 0.3,
            state_file: "hygiene_state.json".to_string(),
        }
    }
}

/// Hygiene state tracking
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HygieneState {
    /// Last hygiene run timestamp
    pub last_run: Option<DateTime<Utc>>,
    /// Total entries cleaned up
    pub total_cleaned: u64,
    /// Entries cleaned by agent
    pub cleaned_by_agent: HashMap<String, u64>,
    /// Last run duration (seconds)
    pub last_duration_secs: u64,
}

impl HygieneState {
    /// Check if hygiene is due based on interval
    #[must_use] 
    pub fn is_due(&self, interval_hours: u64) -> bool {
        match self.last_run {
            None => true,
            Some(last) => {
                let elapsed = Utc::now() - last;
                elapsed >= Duration::hours(interval_hours as i64)
            }
        }
    }
    
    /// Record a cleanup run
    pub fn record_run(&mut self, duration_secs: u64) {
        self.last_run = Some(Utc::now());
        self.last_duration_secs = duration_secs;
    }
    
    /// Increment cleaned count for agent
    pub fn record_cleaned(&mut self, agent_id: &str, count: u64) {
        self.total_cleaned += count;
        *self.cleaned_by_agent.entry(agent_id.to_string()).or_insert(0) += count;
    }
}

/// Memory hygiene runner
pub struct HygieneRunner {
    config: HygieneConfig,
}

impl HygieneRunner {
    /// Create new hygiene runner
    #[must_use] 
    pub fn new(config: HygieneConfig) -> Self {
        Self { config }
    }
    
    /// Create with default config
    #[must_use] 
    pub fn default_config() -> Self {
        Self::new(HygieneConfig::default())
    }
    
    /// Load state from file
    pub fn load_state(&self, workspace_dir: &Path) -> Result<HygieneState> {
        let state_path = workspace_dir.join(&self.config.state_file);
        
        if !state_path.exists() {
            return Ok(HygieneState::default());
        }
        
        let content = std::fs::read_to_string(&state_path)
            .context("Failed to read hygiene state")?;
        
        let state: HygieneState = serde_json::from_str(&content)
            .context("Failed to parse hygiene state")?;
        
        Ok(state)
    }
    
    /// Save state to file
    pub fn save_state(&self, workspace_dir: &Path, state: &HygieneState) -> Result<()> {
        let state_path = workspace_dir.join(&self.config.state_file);
        
        let content = serde_json::to_string_pretty(state)
            .context("Failed to serialize hygiene state")?;
        
        std::fs::write(&state_path, content)
            .context("Failed to write hygiene state")?;
        
        Ok(())
    }
    
    /// Check if hygiene should run
    pub fn should_run(&self, workspace_dir: &Path) -> Result<bool> {
        if !self.config.enabled {
            return Ok(false);
        }
        
        let state = self.load_state(workspace_dir)?;
        Ok(state.is_due(self.config.interval_hours))
    }
    
    /// Run hygiene on a collection of entries
    /// Returns (`entries_to_keep`, `entries_to_remove`, `entries_to_archive`)
    pub fn clean_entries(
        &self,
        entries: &[MemoryEntry],
        agent_id: &str,
    ) -> (Vec<MemoryEntry>, Vec<MemoryEntry>, Vec<MemoryEntry>) {
        let now = Utc::now();
        let mut to_keep = vec![];
        let mut to_remove = vec![];
        let mut to_archive = vec![];
        
        // Sort by importance (highest first) and timestamp (newest first)
        let mut sorted: Vec<&MemoryEntry> = entries.iter().collect();
        sorted.sort_by(|a, b| {
            // First by importance (descending)
            let importance_cmp = b.importance.partial_cmp(&a.importance).unwrap();
            if importance_cmp != std::cmp::Ordering::Equal {
                return importance_cmp;
            }
            // Then by timestamp (newest first)
            b.created_at.cmp(&a.created_at)
        });
        
        for (idx, entry) in sorted.iter().enumerate() {
            // Check if expired
            if let Some(expires) = entry.expires_at {
                if expires < now {
                    debug!("Entry {} expired, removing", entry.id);
                    to_remove.push((*entry).clone());
                    continue;
                }
            }
            
            // Check age for archiving
            let age = now - entry.created_at;
            if self.config.archive_after_days > 0 
                && age >= Duration::days(self.config.archive_after_days) {
                debug!("Entry {} aged, archiving", entry.id);
                to_archive.push((*entry).clone());
                continue;
            }
            
            // Check if over capacity (remove lowest importance oldest)
            if idx >= self.config.max_entries_per_agent
                && entry.importance < self.config.importance_threshold {
                    debug!("Entry {} low importance over capacity, removing", entry.id);
                    to_remove.push((*entry).clone());
                    continue;
                }
            
            to_keep.push((*entry).clone());
        }
        
        info!(
            "Hygiene for {}: {} keep, {} remove, {} archive",
            agent_id,
            to_keep.len(),
            to_remove.len(),
            to_archive.len()
        );
        
        (to_keep, to_remove, to_archive)
    }
    
    /// Archive entries to separate storage
    pub fn archive_entries(
        &self,
        entries: &[MemoryEntry],
        _workspace_dir: &Path,
    ) -> Result<Vec<String>> {
        // In a full implementation, this would write to an archive file
        // For now, we just log the archive action
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        
        info!("Archived {} entries: {:?}", entries.len(), ids);
        
        Ok(ids)
    }
    
    /// Get status
    pub fn status(&self, workspace_dir: &Path) -> Result<String> {
        let state = self.load_state(workspace_dir)?;
        
        Ok(format!(
            "🧹 Hygiene: {} | Last: {} | Total cleaned: {} | Config: max={}, ttl={}d",
            if self.config.enabled { "enabled" } else { "disabled" },
            state.last_run.map_or_else(|| "never".to_string(), |t| t.format("%Y-%m-%d %H:%M").to_string()),
            state.total_cleaned,
            self.config.max_entries_per_agent,
            self.config.default_ttl_days
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_hygiene_state_is_due() {
        let mut state = HygieneState::default();
        
        // Never run = due
        assert!(state.is_due(24));
        
        // Run recently = not due
        state.last_run = Some(Utc::now());
        assert!(!state.is_due(24));
        
        // Run long ago = due
        state.last_run = Some(Utc::now() - Duration::hours(25));
        assert!(state.is_due(24));
    }

    #[test]
    fn test_clean_entries_expired() {
        let runner = HygieneRunner::default_config();
        
        let entries = vec![
            MemoryEntry::new("agent1", MemoryScope::Agent, "test", serde_json::json!({}))
                .with_expiration(-1), // Already expired
        ];
        
        let (keep, remove, _archive) = runner.clean_entries(&entries, "agent1");
        
        assert_eq!(keep.len(), 0);
        assert_eq!(remove.len(), 1);
    }

    #[test]
    fn test_clean_entries_by_importance() {
        let mut config = HygieneConfig::default();
        config.max_entries_per_agent = 2;
        config.importance_threshold = 0.5;
        
        let runner = HygieneRunner::new(config);
        
        let entries = vec![
            MemoryEntry::new("agent1", MemoryScope::Agent, "high", serde_json::json!({}))
                .with_importance(0.9),
            MemoryEntry::new("agent1", MemoryScope::Agent, "med", serde_json::json!({}))
                .with_importance(0.6),
            MemoryEntry::new("agent1", MemoryScope::Agent, "low", serde_json::json!({}))
                .with_importance(0.2),
        ];
        
        let (keep, remove, _archive) = runner.clean_entries(&entries, "agent1");
        
        // Should keep high importance, remove low over capacity
        assert_eq!(keep.len(), 2);
        assert_eq!(remove.len(), 1);
    }

    #[test]
    fn test_save_and_load_state() {
        let tmp = TempDir::new().unwrap();
        let runner = HygieneRunner::default_config();
        
        let mut state = HygieneState::default();
        state.record_run(60);
        state.record_cleaned("agent1", 5);
        
        runner.save_state(tmp.path(), &state).unwrap();
        
        let loaded = runner.load_state(tmp.path()).unwrap();
        assert!(loaded.last_run.is_some());
        assert_eq!(loaded.total_cleaned, 5);
    }
}
