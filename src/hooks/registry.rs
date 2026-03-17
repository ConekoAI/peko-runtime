//! Hook Registry
//!
//! Manages registration and lookup of hooks by instance.

use super::{HookAction, HookType, RegisteredHook, SessionTarget, TokenValidationResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Hook registry for managing all registered hooks
pub struct HookRegistry {
    /// All registered hooks indexed by hook ID
    hooks: Arc<RwLock<HashMap<String, RegisteredHook>>>,
    /// Index of webhook hooks by (instance_id, path)
    webhooks: Arc<RwLock<HashMap<(String, String), String>>>,
    /// Index of event hooks by topic
    event_hooks: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Index of file watch hooks by (instance_id, path)
    file_watches: Arc<RwLock<HashMap<(String, String), String>>>,
}

impl HookRegistry {
    /// Create a new empty hook registry
    pub fn new() -> Self {
        Self {
            hooks: Arc::new(RwLock::new(HashMap::new())),
            webhooks: Arc::new(RwLock::new(HashMap::new())),
            event_hooks: Arc::new(RwLock::new(HashMap::new())),
            file_watches: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a hook
    pub async fn register(&self, hook: RegisteredHook) -> anyhow::Result<()> {
        let hook_id = hook.id.clone();
        let instance_id = hook.instance_id.clone();

        // Add to main hooks map
        {
            let mut hooks = self.hooks.write().await;
            hooks.insert(hook_id.clone(), hook.clone());
        }

        // Add to appropriate index
        match &hook.hook_type {
            HookType::Webhook { path, token } => {
                let mut webhooks = self.webhooks.write().await;
                webhooks.insert((instance_id.clone(), path.clone()), hook_id.clone());
                info!(
                    "Registered webhook hook {} for instance {} at path {}",
                    hook_id, instance_id, path
                );
            }
            HookType::Event { topic } => {
                let mut event_hooks = self.event_hooks.write().await;
                event_hooks
                    .entry(topic.clone())
                    .or_default()
                    .push(hook_id.clone());
                info!(
                    "Registered event hook {} for instance {} on topic {}",
                    hook_id, instance_id, topic
                );
            }
            HookType::FileWatch { path, pattern } => {
                let mut file_watches = self.file_watches.write().await;
                file_watches.insert((instance_id.clone(), path.clone()), hook_id.clone());
                info!(
                    "Registered file watch hook {} for instance {} on path {} (pattern: {:?})",
                    hook_id, instance_id, path, pattern
                );
            }
            HookType::Cron { schedule } => {
                info!(
                    "Registered cron hook {} for instance {} with schedule {}",
                    hook_id, instance_id, schedule
                );
            }
        }

        Ok(())
    }

    /// Unregister a hook by ID
    pub async fn unregister(&self, hook_id: &str) -> anyhow::Result<bool> {
        let hook = {
            let mut hooks = self.hooks.write().await;
            hooks.remove(hook_id)
        };

        if let Some(hook) = hook {
            // Remove from appropriate index
            match &hook.hook_type {
                HookType::Webhook { path, .. } => {
                    let mut webhooks = self.webhooks.write().await;
                    webhooks.remove(&(hook.instance_id.clone(), path.clone()));
                }
                HookType::Event { topic } => {
                    let mut event_hooks = self.event_hooks.write().await;
                    if let Some(hooks) = event_hooks.get_mut(topic) {
                        hooks.retain(|id| id != hook_id);
                        if hooks.is_empty() {
                            event_hooks.remove(topic);
                        }
                    }
                }
                HookType::FileWatch { path, .. } => {
                    let mut file_watches = self.file_watches.write().await;
                    file_watches.remove(&(hook.instance_id.clone(), path.clone()));
                }
                HookType::Cron { .. } => {
                    // Cron jobs are managed separately via cron.json
                }
            }

            info!("Unregistered hook {}", hook_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Unregister all hooks for an instance
    pub async fn unregister_all_for_instance(&self, instance_id: &str) -> anyhow::Result<u32> {
        let hook_ids: Vec<String> = {
            let hooks = self.hooks.read().await;
            hooks
                .values()
                .filter(|h| h.instance_id == instance_id)
                .map(|h| h.id.clone())
                .collect()
        };

        let mut count = 0;
        for hook_id in hook_ids {
            if self.unregister(&hook_id).await? {
                count += 1;
            }
        }

        info!("Unregistered {} hooks for instance {}", count, instance_id);
        Ok(count)
    }

    /// Get a hook by ID
    pub async fn get(&self, hook_id: &str) -> Option<RegisteredHook> {
        let hooks = self.hooks.read().await;
        hooks.get(hook_id).cloned()
    }

    /// Get all hooks for an instance
    pub async fn get_for_instance(&self, instance_id: &str) -> Vec<RegisteredHook> {
        let hooks = self.hooks.read().await;
        hooks
            .values()
            .filter(|h| h.instance_id == instance_id)
            .cloned()
            .collect()
    }

    /// Get webhook hook by instance and path
    pub async fn get_webhook(
        &self,
        instance_id: &str,
        path: &str,
    ) -> Option<(RegisteredHook, Option<String>)> {
        let webhooks = self.webhooks.read().await;
        let hook_id = webhooks
            .get(&(instance_id.to_string(), path.to_string()))
            .cloned()?;
        drop(webhooks);

        let hooks = self.hooks.read().await;
        hooks.get(&hook_id).cloned().map(|hook| {
            let token = match &hook.hook_type {
                HookType::Webhook { token, .. } => token.clone(),
                _ => None,
            };
            (hook, token)
        })
    }

    /// Validate webhook token
    pub async fn validate_webhook_token(
        &self,
        instance_id: &str,
        path: &str,
        provided_token: Option<&str>,
    ) -> TokenValidationResult {
        let webhooks = self.webhooks.read().await;
        let key = (instance_id.to_string(), path.to_string());

        let hook_id = match webhooks.get(&key) {
            Some(id) => id,
            None => return TokenValidationResult::Invalid,
        };

        let hooks = self.hooks.read().await;
        let hook = match hooks.get(hook_id) {
            Some(h) => h,
            None => return TokenValidationResult::Invalid,
        };

        match &hook.hook_type {
            HookType::Webhook {
                token: expected_token,
                ..
            } => {
                match (expected_token.as_ref(), provided_token) {
                    (None, _) => TokenValidationResult::NotRequired,
                    (Some(expected), Some(provided)) => {
                        // Use constant-time comparison to prevent timing attacks
                        if Self::constant_time_compare(expected, provided) {
                            TokenValidationResult::Valid
                        } else {
                            TokenValidationResult::Invalid
                        }
                    }
                    (Some(_), None) => TokenValidationResult::Missing,
                }
            }
            _ => TokenValidationResult::Invalid,
        }
    }

    /// Get event hooks for a topic
    pub async fn get_event_hooks(&self, topic: &str) -> Vec<RegisteredHook> {
        let event_hooks = self.event_hooks.read().await;
        let hook_ids = event_hooks.get(topic).cloned().unwrap_or_default();
        drop(event_hooks);

        let hooks = self.hooks.read().await;
        hook_ids
            .into_iter()
            .filter_map(|id| hooks.get(&id).cloned())
            .collect()
    }

    /// Get file watch hook for instance and path
    pub async fn get_file_watch(&self, instance_id: &str, path: &str) -> Option<RegisteredHook> {
        let file_watches = self.file_watches.read().await;
        let hook_id = file_watches
            .get(&(instance_id.to_string(), path.to_string()))
            .cloned()?;
        drop(file_watches);

        let hooks = self.hooks.read().await;
        hooks.get(&hook_id).cloned()
    }

    /// List all registered hooks
    pub async fn list_all(&self) -> Vec<RegisteredHook> {
        let hooks = self.hooks.read().await;
        hooks.values().cloned().collect()
    }

    /// Register hooks from agent config for an instance
    pub async fn register_from_config(
        &self,
        instance_id: &str,
        config: &crate::image::config::AgentConfig,
    ) -> anyhow::Result<Vec<String>> {
        let mut hook_ids = Vec::new();

        for (i, hook_config) in config.hooks.iter().enumerate() {
            if !hook_config.enabled {
                debug!("Skipping disabled hook {} for instance {}", i, instance_id);
                continue;
            }

            let hook_id = format!("hook_{}_{}", instance_id, i);
            let hook_type = Self::convert_hook_type(&hook_config.hook_type)?;
            let session_target = SessionTarget::from_str(&hook_config.session);

            let hook = RegisteredHook {
                id: hook_id.clone(),
                instance_id: instance_id.to_string(),
                hook_type,
                action: HookAction::Run {
                    message: format!("Hook triggered: {}", hook_config.action),
                },
                session_target,
                enabled: hook_config.enabled,
            };

            self.register(hook).await?;
            hook_ids.push(hook_id);
        }

        info!(
            "Registered {} hooks from config for instance {}",
            hook_ids.len(),
            instance_id
        );
        Ok(hook_ids)
    }

    /// Convert config hook type to internal hook type
    fn convert_hook_type(config_type: &crate::image::config::HookType) -> anyhow::Result<HookType> {
        match config_type {
            crate::image::config::HookType::Cron { schedule } => Ok(HookType::Cron {
                schedule: schedule.clone(),
            }),
            crate::image::config::HookType::Webhook { path, token } => Ok(HookType::Webhook {
                path: path.clone(),
                token: token.clone(),
            }),
            crate::image::config::HookType::Event { topic } => Ok(HookType::Event {
                topic: topic.clone(),
            }),
            crate::image::config::HookType::FileWatch { path, pattern } => {
                Ok(HookType::FileWatch {
                    path: path.clone(),
                    pattern: pattern.clone(),
                })
            }
        }
    }

    /// Constant-time string comparison to prevent timing attacks
    fn constant_time_compare(a: &str, b: &str) -> bool {
        if a.len() != b.len() {
            return false;
        }
        a.bytes()
            .zip(b.bytes())
            .fold(0, |acc, (x, y)| acc | (x ^ y))
            == 0
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::config::{Hook, HookType as ConfigHookType};

    #[tokio::test]
    async fn test_register_and_get_webhook() {
        let registry = HookRegistry::new();

        let hook = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Webhook {
                path: "/github".to_string(),
                token: Some("secret_token".to_string()),
            },
            action: HookAction::Run {
                message: "Webhook triggered".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        registry.register(hook).await.unwrap();

        let result = registry.get_webhook("inst_123", "/github").await;
        assert!(result.is_some());

        let (retrieved_hook, token) = result.unwrap();
        assert_eq!(retrieved_hook.id, "hook_001");
        assert_eq!(token, Some("secret_token".to_string()));
    }

    #[tokio::test]
    async fn test_validate_webhook_token() {
        let registry = HookRegistry::new();

        let hook = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Webhook {
                path: "/github".to_string(),
                token: Some("secret_token".to_string()),
            },
            action: HookAction::Run {
                message: "Webhook triggered".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        registry.register(hook).await.unwrap();

        // Valid token
        let result = registry
            .validate_webhook_token("inst_123", "/github", Some("secret_token"))
            .await;
        assert_eq!(result, TokenValidationResult::Valid);

        // Invalid token
        let result = registry
            .validate_webhook_token("inst_123", "/github", Some("wrong_token"))
            .await;
        assert_eq!(result, TokenValidationResult::Invalid);

        // Missing token
        let result = registry
            .validate_webhook_token("inst_123", "/github", None)
            .await;
        assert_eq!(result, TokenValidationResult::Missing);

        // Unknown path
        let result = registry
            .validate_webhook_token("inst_123", "/unknown", Some("secret_token"))
            .await;
        assert_eq!(result, TokenValidationResult::Invalid);
    }

    #[tokio::test]
    async fn test_unregister() {
        let registry = HookRegistry::new();

        let hook = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Webhook {
                path: "/github".to_string(),
                token: None,
            },
            action: HookAction::Run {
                message: "Webhook triggered".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        registry.register(hook).await.unwrap();
        assert!(registry.get("hook_001").await.is_some());

        registry.unregister("hook_001").await.unwrap();
        assert!(registry.get("hook_001").await.is_none());
    }

    #[tokio::test]
    async fn test_event_hooks() {
        let registry = HookRegistry::new();

        let hook1 = RegisteredHook {
            id: "hook_001".to_string(),
            instance_id: "inst_123".to_string(),
            hook_type: HookType::Event {
                topic: "team.tasks".to_string(),
            },
            action: HookAction::Run {
                message: "Event received".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        let hook2 = RegisteredHook {
            id: "hook_002".to_string(),
            instance_id: "inst_456".to_string(),
            hook_type: HookType::Event {
                topic: "team.tasks".to_string(),
            },
            action: HookAction::Run {
                message: "Event received".to_string(),
            },
            session_target: SessionTarget::New,
            enabled: true,
        };

        registry.register(hook1).await.unwrap();
        registry.register(hook2).await.unwrap();

        let hooks = registry.get_event_hooks("team.tasks").await;
        assert_eq!(hooks.len(), 2);
    }
}
