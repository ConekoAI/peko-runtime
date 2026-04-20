//! File Watch Hook Integration
//!
//! Connects the file watcher system to the hook registry for `file_watch` hooks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::hooks::{HookRegistry, HookTrigger, TriggerSource};

/// File watch hook manager
///
/// Bridges filesystem events to hook triggers for configured `file_watch` hooks.
pub struct FileWatchHookManager {
    registry: std::sync::Arc<HookRegistry>,
    watch_configs: HashMap<String, WatchConfig>,
}

/// Configuration for a file watch
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Hook ID that owns this watch
    pub hook_id: String,
    /// Instance ID
    pub instance_id: String,
    /// Absolute path to watch
    pub path: PathBuf,
    /// Optional glob pattern filter
    pub pattern: Option<String>,
}

impl FileWatchHookManager {
    /// Create a new file watch hook manager
    #[must_use]
    pub fn new(registry: std::sync::Arc<HookRegistry>) -> Self {
        Self {
            registry,
            watch_configs: HashMap::new(),
        }
    }

    /// Register file watches for an instance
    pub async fn register_instance(&mut self, instance_id: &str) -> anyhow::Result<Vec<String>> {
        let hooks = self.registry.get_for_instance(instance_id).await;
        let mut registered = Vec::new();

        for hook in hooks {
            if let crate::hooks::HookType::FileWatch { path, pattern } = &hook.hook_type {
                let watch_path = PathBuf::from(path);

                let config = WatchConfig {
                    hook_id: hook.id.clone(),
                    instance_id: instance_id.to_string(),
                    path: watch_path,
                    pattern: pattern.clone(),
                };

                self.watch_configs.insert(hook.id.clone(), config);
                registered.push(hook.id.clone());

                info!(
                    "Registered file watch hook {} for instance {} on path {:?}",
                    hook.id, instance_id, path
                );
            }
        }

        Ok(registered)
    }

    /// Unregister all file watches for an instance
    pub async fn unregister_instance(&mut self, instance_id: &str) -> u32 {
        let to_remove: Vec<String> = self
            .watch_configs
            .iter()
            .filter(|(_, config)| config.instance_id == instance_id)
            .map(|(hook_id, _)| hook_id.clone())
            .collect();

        let count = to_remove.len() as u32;
        for hook_id in to_remove {
            self.watch_configs.remove(&hook_id);
            debug!("Unregistered file watch hook {}", hook_id);
        }

        count
    }

    /// Handle a file change event
    pub async fn handle_file_event(
        &self,
        path: &Path,
        change_type: &str,
    ) -> Vec<HookTriggerResult> {
        let mut results = Vec::new();

        for (hook_id, config) in &self.watch_configs {
            // Check if path matches this watch
            if !self.path_matches_watch(path, config) {
                continue;
            }

            // Check pattern filter if specified
            if let Some(ref pattern) = config.pattern {
                if !self.matches_pattern(path, pattern) {
                    continue;
                }
            }

            // Get the hook
            if let Some(hook) = self.registry.get(hook_id).await {
                if !hook.enabled {
                    continue;
                }

                let trigger_source = TriggerSource::FileWatch {
                    path: path.to_string_lossy().to_string(),
                    change_type: change_type.to_string(),
                };

                let _trigger = HookTrigger::new(hook, trigger_source);

                // In a real implementation, this would process the trigger
                // For now, just record that we found a match
                results.push(HookTriggerResult {
                    hook_id: hook_id.clone(),
                    instance_id: config.instance_id.clone(),
                    triggered: true,
                });

                info!(
                    "File watch hook {} triggered by {} of {:?}",
                    hook_id, change_type, path
                );
            }
        }

        results
    }

    /// Check if a path matches a watch configuration
    fn path_matches_watch(&self, path: &Path, config: &WatchConfig) -> bool {
        // Check if path is under the watched directory
        path.starts_with(&config.path) || path == config.path.as_path()
    }

    /// Check if a path matches a glob pattern
    fn matches_pattern(&self, path: &Path, pattern: &str) -> bool {
        // Simple pattern matching - could use glob crate for full support
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // Support simple wildcard patterns like "*.txt"
        if pattern.contains('*') {
            let parts: Vec<&str> = pattern.split('*').collect();
            if parts.len() == 2 {
                let prefix = parts[0];
                let suffix = parts[1];
                return file_name.starts_with(prefix) && file_name.ends_with(suffix);
            }
        }

        // Exact match
        file_name == pattern
    }

    /// Get active watch count
    #[must_use]
    pub fn watch_count(&self) -> usize {
        self.watch_configs.len()
    }
}

/// Result of attempting to trigger a hook
#[derive(Debug, Clone)]
pub struct HookTriggerResult {
    pub hook_id: String,
    pub instance_id: String,
    pub triggered: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookAction, HookType, RegisteredHook, SessionTarget};
    use std::sync::Arc;

    async fn create_test_registry() -> Arc<HookRegistry> {
        Arc::new(HookRegistry::new())
    }

    fn create_test_file_watch_hook(id: &str, instance_id: &str, path: &str) -> RegisteredHook {
        RegisteredHook {
            id: id.to_string(),
            instance_id: instance_id.to_string(),
            hook_type: HookType::FileWatch {
                path: path.to_string(),
                pattern: Some("*.txt".to_string()),
            },
            action: HookAction::Run {
                message: "File changed".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        }
    }

    #[tokio::test]
    async fn test_file_watch_manager_creation() {
        let registry = create_test_registry().await;
        let manager = FileWatchHookManager::new(registry);
        assert_eq!(manager.watch_count(), 0);
    }

    #[tokio::test]
    async fn test_register_instance() {
        let registry = create_test_registry().await;

        // Register a file watch hook
        let hook = create_test_file_watch_hook("hook_001", "inst_123", "/tmp/watch");
        registry.register(hook).await.unwrap();

        let mut manager = FileWatchHookManager::new(registry);
        let registered = manager.register_instance("inst_123").await.unwrap();

        assert_eq!(registered.len(), 1);
        assert_eq!(manager.watch_count(), 1);
    }

    #[tokio::test]
    async fn test_path_matches_watch() {
        let registry = create_test_registry().await;
        let manager = FileWatchHookManager::new(registry);

        let config = WatchConfig {
            hook_id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            path: PathBuf::from("/tmp/watch"),
            pattern: None,
        };

        // Path under watch directory
        assert!(manager.path_matches_watch(Path::new("/tmp/watch/file.txt"), &config));

        // Exact match
        assert!(manager.path_matches_watch(Path::new("/tmp/watch"), &config));

        // Path outside watch directory
        assert!(!manager.path_matches_watch(Path::new("/other/file.txt"), &config));
    }

    #[tokio::test]
    async fn test_matches_pattern() {
        let registry = create_test_registry().await;
        let manager = FileWatchHookManager::new(registry);

        // Wildcard pattern
        assert!(manager.matches_pattern(Path::new("/tmp/test.txt"), "*.txt"));
        assert!(manager.matches_pattern(Path::new("/tmp/file.txt"), "*.txt"));
        assert!(!manager.matches_pattern(Path::new("/tmp/test.rs"), "*.txt"));

        // Exact match
        assert!(manager.matches_pattern(Path::new("/tmp/config.toml"), "config.toml"));
        assert!(!manager.matches_pattern(Path::new("/tmp/other.toml"), "config.toml"));
    }

    #[tokio::test]
    async fn test_handle_file_event() {
        let registry = create_test_registry().await;

        // Register a file watch hook
        let hook = create_test_file_watch_hook("hook_001", "inst_123", "/tmp/watch");
        registry.register(hook).await.unwrap();

        let mut manager = FileWatchHookManager::new(registry);
        manager.register_instance("inst_123").await.unwrap();

        // Test matching file
        let results = manager
            .handle_file_event(Path::new("/tmp/watch/test.txt"), "created")
            .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].triggered);

        // Test non-matching file (wrong extension)
        let results = manager
            .handle_file_event(Path::new("/tmp/watch/test.rs"), "created")
            .await;

        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_unregister_instance() {
        let registry = create_test_registry().await;

        // Register file watch hooks for two instances
        let hook1 = create_test_file_watch_hook("hook_001", "inst_123", "/tmp/watch1");
        let hook2 = create_test_file_watch_hook("hook_002", "inst_123", "/tmp/watch2");
        let hook3 = create_test_file_watch_hook("hook_003", "inst_456", "/tmp/watch3");

        registry.register(hook1).await.unwrap();
        registry.register(hook2).await.unwrap();
        registry.register(hook3).await.unwrap();

        let mut manager = FileWatchHookManager::new(registry);
        manager.register_instance("inst_123").await.unwrap();
        manager.register_instance("inst_456").await.unwrap();

        assert_eq!(manager.watch_count(), 3);

        // Unregister one instance
        let count = manager.unregister_instance("inst_123").await;
        assert_eq!(count, 2);
        assert_eq!(manager.watch_count(), 1);
    }
}
