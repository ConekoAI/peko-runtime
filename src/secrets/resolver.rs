//! Secret resolver for configuration values
//!
//! Handles `${secret:NAME}` syntax for resolving secrets from the secret store.
//!
//! ## Supported syntax
//!
//! - `${secret:NAME}` — Global secret
//! - `${secret.agent:DID:NAME}` — Per-agent secret
//! - `${env:VARNAME}` — Environment variable (for consistency)
//!
//! ## Example
//!
//! ```rust
//! use pekobot::secrets::SecretResolver;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let resolver = SecretResolver::new().await?;
//! resolver.unlock("password").await?;
//!
//! // Resolve a secret reference
//! let api_key = resolver.resolve("${secret:OPENAI_API_KEY}").await?;
//! # Ok(())
//! # }
//! ```

use crate::secrets::{SecretManager, SecretScope};
use regex::Regex;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Resolves secret references in configuration values
pub struct SecretResolver {
    /// The underlying secret manager
    manager: Arc<Mutex<SecretManager>>,
    /// Regex for parsing secret references
    secret_regex: Regex,
    /// Regex for parsing env references
    env_regex: Regex,
}

impl SecretResolver {
    /// Create a new secret resolver with the default secret store
    pub async fn new() -> anyhow::Result<Self> {
        let manager = SecretManager::new().await?;
        
        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
            secret_regex: Regex::new(r"\$\{secret:(?:(agent):([^:]+):)?([^}]+)\}").unwrap(),
            env_regex: Regex::new(r"\$\{env:([^}]+)\}").unwrap(),
        })
    }

    /// Create a new secret resolver with a specific store path
    pub fn open(path: impl Into<std::path::PathBuf>) -> anyhow::Result<Self> {
        let manager = SecretManager::open(path)?;
        
        Ok(Self {
            manager: Arc::new(Mutex::new(manager)),
            secret_regex: Regex::new(r"\$\{secret:(?:(agent):([^:]+):)?([^}]+)\}").unwrap(),
            env_regex: Regex::new(r"\$\{env:([^}]+)\}").unwrap(),
        })
    }

    /// Check if the resolver is unlocked
    pub async fn is_unlocked(&self) -> bool {
        self.manager.lock().await.is_unlocked()
    }

    /// Unlock the secret store
    pub async fn unlock(&self, password: &str) -> anyhow::Result<()> {
        let mut manager = self.manager.lock().await;
        manager.unlock(password).await
    }

    /// Lock the secret store
    pub async fn lock(&self) {
        let mut manager = self.manager.lock().await;
        manager.lock();
    }

    /// Check if a value contains secret references
    pub fn contains_secrets(&self, value: &str) -> bool {
        self.secret_regex.is_match(value)
    }

    /// Resolve all secret references in a string
    ///
    /// Replaces `${secret:NAME}` with the actual secret value.
    /// If a secret is not found, returns an error with helpful context.
    pub async fn resolve(&self, value: &str) -> anyhow::Result<String> {
        // First resolve env variables
        let value = self.resolve_env(value)?;
        
        // Then resolve secrets
        self.resolve_secrets(&value).await
    }

    /// Resolve environment variable references
    fn resolve_env(&self, value: &str) -> anyhow::Result<String> {
        let mut result = value.to_string();
        
        for cap in self.env_regex.captures_iter(value) {
            let var_name = &cap[1];
            let full_match = cap.get(0).unwrap().as_str();
            
            match std::env::var(var_name) {
                Ok(var_value) => {
                    result = result.replace(full_match, &var_value);
                }
                Err(_) => {
                    anyhow::bail!(
                        "Environment variable '{}' not found (referenced in '{}')",
                        var_name,
                        full_match
                    );
                }
            }
        }
        
        Ok(result)
    }

    /// Resolve secret references
    async fn resolve_secrets(&self, value: &str) -> anyhow::Result<String> {
        let mut result = value.to_string();
        
        for cap in self.secret_regex.captures_iter(value) {
            let full_match = cap.get(0).unwrap().as_str();
            
            // Determine scope
            let scope = if cap.get(1).is_some() {
                // Agent scope: ${secret.agent:DID:NAME}
                let did = cap.get(2).unwrap().as_str();
                SecretScope::Agent { did: did.to_string() }
            } else {
                // Global scope: ${secret:NAME}
                SecretScope::Global
            };
            
            let name = cap.get(3).unwrap().as_str();
            
            // Look up secret
            let manager = self.manager.lock().await;
            match manager.get(name, &scope).await {
                Ok(Some(secret_value)) => {
                    result = result.replace(full_match, &secret_value);
                }
                Ok(None) => {
                    let scope_str = match &scope {
                        SecretScope::Global => "global".to_string(),
                        SecretScope::Agent { did } => format!("agent:{}", did),
                    };
                    anyhow::bail!(
                        "Secret '{}' not found in scope '{}' (referenced in '{}'). \
                         Set it with: pekobot secret set {} --scope {}",
                        name,
                        scope_str,
                        full_match,
                        name,
                        scope_str
                    );
                }
                Err(e) => {
                    anyhow::bail!(
                        "Failed to resolve secret '{}': {}. \
                         Is the secret store unlocked?",
                        name,
                        e
                    );
                }
            }
        }
        
        Ok(result)
    }

    /// Resolve a single secret reference
    ///
    /// Similar to `resolve` but expects exactly one secret reference
    /// and returns just the secret value.
    pub async fn resolve_one(&self, reference: &str) -> anyhow::Result<String> {
        let trimmed = reference.trim();
        
        // Check if it's a secret reference
        if let Some(cap) = self.secret_regex.captures(trimmed) {
            let scope = if cap.get(1).is_some() {
                let did = cap.get(2).unwrap().as_str();
                SecretScope::Agent { did: did.to_string() }
            } else {
                SecretScope::Global
            };
            
            let name = cap.get(3).unwrap().as_str();
            
            let manager = self.manager.lock().await;
            match manager.get(name, &scope).await {
                Ok(Some(value)) => Ok(value),
                Ok(None) => {
                    let scope_str = match &scope {
                        SecretScope::Global => "global".to_string(),
                        SecretScope::Agent { did } => format!("agent:{}", did),
                    };
                    anyhow::bail!(
                        "Secret '{}' not found in scope '{}'. \
                         Set it with: pekobot secret set {} --scope {}",
                        name,
                        scope_str,
                        name,
                        scope_str
                    );
                }
                Err(e) => Err(e),
            }
        } else if let Some(cap) = self.env_regex.captures(trimmed) {
            // Environment variable
            let var_name = &cap[1];
            std::env::var(var_name).map_err(|_| {
                anyhow::anyhow!(
                    "Environment variable '{}' not found",
                    var_name
                )
            })
        } else {
            // Not a reference, return as-is
            Ok(reference.to_string())
        }
    }
}

/// Extension trait for resolving secrets in configuration structs
pub trait ResolveSecrets {
    /// Resolve all secret references in this config
    async fn resolve_secrets(&mut self, resolver: &SecretResolver) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_resolve_env() {
        // Set test env var
        std::env::set_var("TEST_VAR", "test_value");
        
        let resolver = SecretResolver::open(tempdir().unwrap().path().join("test.db")).unwrap();
        let result = resolver.resolve_env("Value: ${env:TEST_VAR}").unwrap();
        
        assert_eq!(result, "Value: test_value");
        
        // Clean up
        std::env::remove_var("TEST_VAR");
    }

    #[tokio::test]
    async fn test_resolve_missing_env() {
        let resolver = SecretResolver::open(tempdir().unwrap().path().join("test.db")).unwrap();
        let result = resolver.resolve_env("Value: ${env:DEFINITELY_NOT_SET}");
        
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_contains_secrets() {
        let resolver = SecretResolver::open(tempdir().unwrap().path().join("test.db")).unwrap();
        
        assert!(resolver.contains_secrets("${secret:API_KEY}"));
        assert!(resolver.contains_secrets("Bearer ${secret:TOKEN}"));
        assert!(!resolver.contains_secrets("plain text"));
        assert!(!resolver.contains_secrets(""));
    }
}
