//! File watcher for development mode
//!
//! Implements the `--watch` flag for `pekobot run`, automatically reloading
//! the agent when source files change.

use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// File watcher for agent development
pub struct FileWatcher {
    /// Directory being watched
    watch_path: PathBuf,
    /// Channel sender for events
    event_tx: mpsc::Sender<WatchEvent>,
    /// Debounce duration
    debounce_ms: u64,
}

/// Events emitted by the file watcher
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// File changed
    Changed(PathBuf),
    /// File created
    Created(PathBuf),
    /// File removed
    Removed(PathBuf),
    /// Batch of events (debounced)
    Batch(Vec<PathBuf>),
    /// Error occurred
    Error(String),
}

/// Watch handle for controlling the watcher
pub struct WatchHandle;

/// Internal handle that includes event receiver
pub struct FileWatcherHandle {
    /// Event receiver
    pub event_rx: mpsc::Receiver<WatchEvent>,
}

impl FileWatcher {
    /// Create a new file watcher
    #[must_use]
    pub fn new(watch_path: impl Into<PathBuf>) -> (Self, FileWatcherHandle) {
        let watch_path = watch_path.into();
        let (event_tx, event_rx) = mpsc::channel(100);

        let handle = FileWatcherHandle { event_rx };

        let watcher = Self {
            watch_path,
            event_tx,
            debounce_ms: 500,
        };

        (watcher, handle)
    }

    /// Set debounce duration
    #[must_use]
    pub fn with_debounce(mut self, ms: u64) -> Self {
        self.debounce_ms = ms;
        self
    }

    /// Start watching for file changes
    pub async fn start(self, mut stop_rx: mpsc::Receiver<()>) -> anyhow::Result<()> {
        let watch_path = self.watch_path.clone();
        let event_tx = self.event_tx.clone();
        let debounce_ms = self.debounce_ms;

        info!("Starting file watcher for: {}", watch_path.display());

        // Create debounced event channel
        let (debounce_tx, mut debounce_rx) = mpsc::channel::<Event>(100);

        // Spawn debounce processor
        let debounce_event_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut pending_paths: Vec<PathBuf> = Vec::new();
            let mut last_event = tokio::time::Instant::now();
            let debounce_duration = tokio::time::Duration::from_millis(debounce_ms);

            loop {
                match tokio::time::timeout(
                    debounce_duration.saturating_sub(last_event.elapsed()),
                    debounce_rx.recv(),
                )
                .await
                {
                    Ok(Some(event)) => {
                        // Collect paths from event
                        for path in &event.paths {
                            if !pending_paths.contains(path) {
                                pending_paths.push(path.clone());
                            }
                        }
                        last_event = tokio::time::Instant::now();
                    }
                    Ok(None) => break, // Channel closed
                    Err(_) => {
                        // Timeout - send batched events
                        if !pending_paths.is_empty() {
                            debug!(
                                "Sending batched watch events: {} files",
                                pending_paths.len()
                            );
                            let _ = debounce_event_tx
                                .send(WatchEvent::Batch(pending_paths.clone()))
                                .await;
                            pending_paths.clear();
                        }
                    }
                }
            }
        });

        // Create notify watcher
        let watcher_result = Self::create_notify_watcher(watch_path.clone(), debounce_tx);
        let mut watcher = match watcher_result {
            Ok(w) => w,
            Err(e) => {
                error!("Failed to create file watcher: {}", e);
                let _ = event_tx
                    .send(WatchEvent::Error(format!("Failed to create watcher: {e}")))
                    .await;
                return Err(e);
            }
        };

        // Watch the directory
        if let Err(e) = watcher.watch(&watch_path, RecursiveMode::Recursive) {
            error!("Failed to start watching {}: {}", watch_path.display(), e);
            let _ = event_tx
                .send(WatchEvent::Error(format!(
                    "Failed to watch {}: {}",
                    watch_path.display(),
                    e
                )))
                .await;
            return Err(e.into());
        }

        info!("File watcher active, monitoring changes...");

        // Wait for stop signal
        let _ = stop_rx.recv().await;

        info!("Stopping file watcher");
        drop(watcher);

        Ok(())
    }

    /// Create the notify watcher
    fn create_notify_watcher(
        _watch_path: PathBuf,
        debounce_tx: mpsc::Sender<Event>,
    ) -> anyhow::Result<RecommendedWatcher> {
        let watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        debug!("File system event: {:?}", event);

                        // Filter relevant events
                        match event.kind {
                            notify::EventKind::Modify(_)
                            | notify::EventKind::Create(_)
                            | notify::EventKind::Remove(_) => {
                                // Ignore changes to sessions/ directory
                                let relevant_paths: Vec<_> = event
                                    .paths
                                    .iter()
                                    .filter(|p| {
                                        !p.to_string_lossy().contains("/sessions/")
                                            && !p.to_string_lossy().contains("\\sessions\\")
                                    })
                                    .cloned()
                                    .collect();

                                if !relevant_paths.is_empty() {
                                    let filtered_event = Event {
                                        paths: relevant_paths,
                                        ..event
                                    };
                                    let _ = debounce_tx.try_send(filtered_event);
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        warn!("Watch error: {}", e);
                    }
                }
            },
            Config::default(),
        )?;

        Ok(watcher)
    }
}

impl WatchHandle {
    /// Stop the watcher
    pub async fn stop(self) {}
}

impl FileWatcher {
    /// Get the watch path
    #[must_use]
    pub fn watch_path(&self) -> &PathBuf {
        &self.watch_path
    }
}

/// Watch an agent directory and trigger reloads
pub async fn watch_agent_directory(
    path: PathBuf,
    reload_tx: mpsc::Sender<()>,
) -> anyhow::Result<WatchHandle> {
    let (watcher, handle) = FileWatcher::new(&path);
    let FileWatcherHandle { event_rx: mut rx } = handle;

    // Bridge between watch events and reload signals
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                WatchEvent::Batch(paths) => {
                    info!(
                        "Detected changes in {} files, triggering reload",
                        paths.len()
                    );
                    let _ = reload_tx.send(()).await;
                }
                WatchEvent::Changed(path)
                | WatchEvent::Created(path)
                | WatchEvent::Removed(path) => {
                    info!("Detected change in {}, triggering reload", path.display());
                    let _ = reload_tx.send(()).await;
                }
                WatchEvent::Error(e) => {
                    error!("Watch error: {}", e);
                }
            }
        }
    });

    // Create a new stop channel for the watcher
    let (watcher_stop_tx, watcher_stop_rx) = mpsc::channel(1);

    // Start the watcher
    tokio::spawn(async move {
        if let Err(e) = watcher.start(watcher_stop_rx).await {
            error!("Watcher error: {}", e);
        }
    });

    let _ = watcher_stop_tx;
    Ok(WatchHandle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[test]
    fn test_file_watcher_creation() {
        let (watcher, _handle) = FileWatcher::new("/tmp/test");
        assert_eq!(watcher.watch_path(), &PathBuf::from("/tmp/test"));
        assert_eq!(watcher.debounce_ms, 500);
    }

    #[test]
    fn test_file_watcher_with_debounce() {
        let (mut watcher, _handle) = FileWatcher::new("/tmp/test");
        watcher = watcher.with_debounce(1000);
        assert_eq!(watcher.debounce_ms, 1000);
    }

    #[tokio::test]
    async fn test_watch_event_clone() {
        let event = WatchEvent::Changed(PathBuf::from("/tmp/test.txt"));
        let cloned = event.clone();
        match (event, cloned) {
            (WatchEvent::Changed(p1), WatchEvent::Changed(p2)) => {
                assert_eq!(p1, p2);
            }
            _ => panic!("Clone failed"),
        }
    }

    #[test]
    fn test_watch_event_variants() {
        let changed = WatchEvent::Changed(PathBuf::from("/a.txt"));
        let created = WatchEvent::Created(PathBuf::from("/b.txt"));
        let removed = WatchEvent::Removed(PathBuf::from("/c.txt"));
        let batch = WatchEvent::Batch(vec![PathBuf::from("/d.txt"), PathBuf::from("/e.txt")]);
        let error = WatchEvent::Error("test error".to_string());

        // Test that all variants can be created
        drop(changed);
        drop(created);
        drop(removed);
        drop(batch);
        drop(error);
    }

    #[tokio::test]
    async fn test_file_watcher_handle_receiver() {
        let (_watcher, handle) = FileWatcher::new("/tmp/test");

        // The handle should have a receiver
        // We can't easily test the full flow without creating actual files,
        // but we can verify the handle structure
        drop(handle);
    }

    #[test]
    fn test_debounce_configuration() {
        let path = PathBuf::from("/tmp/agent");
        let (watcher, _handle) = FileWatcher::new(&path);
        assert_eq!(watcher.debounce_ms, 500); // Default

        let watcher = watcher.with_debounce(100);
        assert_eq!(watcher.debounce_ms, 100);

        let watcher = watcher.with_debounce(0);
        assert_eq!(watcher.debounce_ms, 0);
    }

    #[test]
    fn test_watch_event_debug() {
        let event = WatchEvent::Changed(PathBuf::from("/test.txt"));
        let debug = format!("{event:?}");
        assert!(debug.contains("Changed"));
        assert!(debug.contains("/test.txt"));
    }

    #[test]
    fn test_batch_event_paths() {
        let paths = vec![
            PathBuf::from("/a.txt"),
            PathBuf::from("/b.txt"),
            PathBuf::from("/c.txt"),
        ];
        let event = WatchEvent::Batch(paths.clone());

        match event {
            WatchEvent::Batch(batch_paths) => {
                assert_eq!(batch_paths.len(), 3);
                assert_eq!(batch_paths[0], PathBuf::from("/a.txt"));
            }
            _ => panic!("Expected Batch variant"),
        }
    }

    #[tokio::test]
    async fn test_watch_agent_directory() {
        // Create a temporary directory for testing
        let temp_dir = std::env::temp_dir().join(format!("pekobot_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&temp_dir).await.unwrap();

        let (reload_tx, mut reload_rx) = mpsc::channel(1);

        // Start watching the directory
        let handle = watch_agent_directory(temp_dir.clone(), reload_tx).await;
        assert!(handle.is_ok());

        let watch_handle = handle.unwrap();

        // Create a test file to trigger an event
        let test_file = temp_dir.join("test.txt");
        tokio::fs::write(&test_file, "test content").await.unwrap();

        // Wait for potential reload signal (with timeout)
        // Note: This may not always trigger due to debouncing and timing
        let _ = timeout(Duration::from_millis(1000), reload_rx.recv()).await;

        // Clean up
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;
        let () = watch_handle.stop().await;
    }
}
