//! File Watcher for orchestration layer
//!
//! Watches filesystem changes and emits SystemEvent::File events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use notify::{Config, Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::orchestration::events::{FileChangeType, SystemEvent};

/// Configuration for watching a path
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Path to watch
    pub path: PathBuf,
    /// Agent to invoke on changes
    pub agent_id: String,
    /// File pattern filter (glob string, e.g., "*.rs")
    pub filter: Option<String>,
    /// Debounce duration in milliseconds
    pub debounce_ms: u64,
    /// Watch recursively
    pub recursive: bool,
}

impl WatchConfig {
    /// Create a new watch configuration
    pub fn new(path: impl AsRef<Path>, agent_id: impl Into<String>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            agent_id: agent_id.into(),
            filter: None,
            debounce_ms: 1000,
            recursive: true,
        }
    }

    /// Set file pattern filter
    pub fn with_filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }

    /// Set debounce duration
    pub fn with_debounce(mut self, ms: u64) -> Self {
        self.debounce_ms = ms;
        self
    }

    /// Set recursive mode
    pub fn with_recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }
}

/// File watcher that emits system events on file changes
pub struct FileWatcher {
    /// Watch configurations indexed by path
    configs: HashMap<PathBuf, WatchConfig>,
    /// Channel for receiving notify events
    event_tx: mpsc::Sender<SystemEvent>,
    /// Watcher instances (kept alive to maintain watches)
    _watchers: Vec<RecommendedWatcher>,
}

impl FileWatcher {
    /// Create a new file watcher with an event sender
    ///
    /// The caller should create a channel and pass the sender here,
    /// then receive events from the receiver and route them appropriately.
    pub fn new(event_tx: mpsc::Sender<SystemEvent>) -> anyhow::Result<Self> {
        Ok(Self {
            configs: HashMap::new(),
            event_tx,
            _watchers: Vec::new(),
        })
    }

    /// Add a path to watch
    pub fn watch(&mut self, config: WatchConfig) -> anyhow::Result<()> {
        let path = config.path.clone();
        let event_tx = self.event_tx.clone();
        let filter = config.filter.clone();

        // Check if path exists
        if !path.exists() {
            return Err(anyhow::anyhow!(
                "Cannot watch non-existent path: {:?}",
                path
            ));
        }

        // Create watcher for this path
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<NotifyEvent, notify::Error>| {
                Self::handle_notify_event(res, &event_tx, &filter);
            },
            Config::default(),
        )?;

        // Start watching
        let recursive_mode = if config.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        watcher.watch(&path, recursive_mode)?;

        info!(
            "Started watching: {:?} (recursive={}, filter={:?})",
            path, config.recursive, config.filter
        );

        self._watchers.push(watcher);
        self.configs.insert(path, config);

        Ok(())
    }

    /// Handle a notify event and convert to SystemEvent
    fn handle_notify_event(
        res: Result<NotifyEvent, notify::Error>,
        event_tx: &mpsc::Sender<SystemEvent>,
        filter: &Option<String>,
    ) {
        match res {
            Ok(event) => {
                // Apply filter if specified (simple substring match)
                if let Some(ref pattern) = filter {
                    let matches = event
                        .paths
                        .iter()
                        .any(|p| p.to_string_lossy().contains(pattern));
                    if !matches {
                        debug!("Path filtered out by pattern: {}", pattern);
                        return;
                    }
                }

                // Convert notify event to SystemEvents
                for path in &event.paths {
                    let change_type = match event.kind {
                        notify::EventKind::Create(_) => FileChangeType::Created,
                        notify::EventKind::Modify(_) => FileChangeType::Modified,
                        notify::EventKind::Remove(_) => FileChangeType::Deleted,
                        _ => {
                            debug!("Skipping unhandled event kind: {:?}", event.kind);
                            continue;
                        }
                    };

                    let system_event = SystemEvent::File {
                        path: path.clone(),
                        change_type,
                        timestamp: chrono::Utc::now(),
                    };

                    if let Err(e) = event_tx.try_send(system_event) {
                        error!("Failed to send file event: {}", e);
                    }
                }
            }
            Err(e) => {
                error!("File watcher error: {}", e);
            }
        }
    }

    /// Get active watch configurations
    pub fn get_watches(&self) -> &HashMap<PathBuf, WatchConfig> {
        &self.configs
    }

    /// Get count of active watches
    pub fn watch_count(&self) -> usize {
        self.configs.len()
    }

    /// Check if a path is being watched
    pub fn is_watching(&self, path: &Path) -> bool {
        self.configs.contains_key(path)
    }
}

/// Builder for creating a file watcher with multiple paths
pub struct FileWatcherBuilder {
    event_tx: mpsc::Sender<SystemEvent>,
    configs: Vec<WatchConfig>,
}

impl FileWatcherBuilder {
    /// Create a new builder with an event sender
    pub fn new(event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            event_tx,
            configs: Vec::new(),
        }
    }

    /// Add a watch configuration
    pub fn add_watch(mut self, config: WatchConfig) -> Self {
        self.configs.push(config);
        self
    }

    /// Build the file watcher with all configured watches
    pub fn build(self) -> anyhow::Result<FileWatcher> {
        let mut watcher = FileWatcher::new(self.event_tx)?;

        for config in self.configs {
            if let Err(e) = watcher.watch(config) {
                warn!("Failed to add watch: {}", e);
            }
        }

        Ok(watcher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    // Helper to create a temp directory for testing
    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("pekobot_test_{}", std::process::id()))
    }

    #[test]
    fn test_watch_config_builder() {
        let config = WatchConfig::new("/tmp/test", "agent-1")
            .with_filter("*.rs")
            .with_debounce(500)
            .with_recursive(false);

        assert_eq!(config.path, PathBuf::from("/tmp/test"));
        assert_eq!(config.agent_id, "agent-1");
        assert_eq!(config.filter, Some("*.rs".to_string()));
        assert_eq!(config.debounce_ms, 500);
        assert!(!config.recursive);
    }

    #[test]
    fn test_file_watcher_new() {
        // This would need a mock EventRouter
        // For now just test the structure compiles
    }

    #[test]
    fn test_handle_notify_event_created() {
        use notify::event::{CreateKind, EventKind};

        let (tx, mut rx) = mpsc::channel(10);
        let notify_event = NotifyEvent {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/test/file.txt")],
            attrs: Default::default(),
        };

        FileWatcher::handle_notify_event(Ok(notify_event), &tx, &None);

        // The event should be sent
        // Note: In actual test we'd need tokio runtime
    }

    #[test]
    fn test_filter_matching() {
        use notify::event::{CreateKind, EventKind};

        let (tx, _rx) = mpsc::channel(10);
        let notify_event = NotifyEvent {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/test/file.rs")],
            attrs: Default::default(),
        };

        // Should match *.rs pattern
        FileWatcher::handle_notify_event(Ok(notify_event.clone()), &tx, &Some("*.rs".to_string()));

        // Should not match *.txt pattern
        // (in real test we'd verify the event wasn't sent)
    }
}
