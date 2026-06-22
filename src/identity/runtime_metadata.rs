//! Runtime Metadata
//!
//! This module provides runtime metadata including host info, version,
//! capabilities, and timestamps.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use tracing::info;

use crate::common::paths::PathResolver;

/// Runtime metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeMetadata {
    /// Unique runtime identifier (DID)
    pub runtime_id: String,
    /// Human-readable display name
    pub display_name: String,
    /// When the runtime was first created
    pub created_at: DateTime<Utc>,
    /// When the runtime was last seen/active
    pub last_seen_at: DateTime<Utc>,
    /// Software version
    pub version: String,
    /// List of capabilities
    pub capabilities: Vec<String>,
    /// Host system information
    pub host_info: HostInfo,
}

/// Host system information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    /// Operating system name
    pub os: String,
    /// CPU architecture
    pub arch: String,
    /// Hostname
    pub hostname: String,
}

impl HostInfo {
    /// Detect host information from the current system
    #[must_use]
    pub fn detect() -> Self {
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());

        Self { os, arch, hostname }
    }
}

impl RuntimeMetadata {
    /// Create new runtime metadata with detected host info
    #[must_use]
    pub fn new(runtime_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            runtime_id: runtime_id.into(),
            display_name: "Pekobot Runtime".to_string(),
            created_at: now,
            last_seen_at: now,
            version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec!["filesystem".to_string(), "network".to_string()],
            host_info: HostInfo::detect(),
        }
    }

    /// Load metadata from disk, or create new with detected host info
    pub fn load_or_create(resolver: &PathResolver, runtime_id: &str) -> Result<Self> {
        let runtime_path = resolver.runtime_dir().join("runtime.toml");

        if runtime_path.exists() {
            let content = fs::read_to_string(&runtime_path)
                .with_context(|| format!("Failed to read runtime metadata: {runtime_path:?}"))?;
            let mut metadata: RuntimeMetadata =
                toml::from_str(&content).with_context(|| "Failed to parse runtime.toml")?;

            // Update last_seen_at on every load
            metadata.last_seen_at = Utc::now();

            // Save updated metadata
            let toml = toml::to_string_pretty(&metadata)
                .with_context(|| "Failed to serialize runtime metadata")?;
            fs::write(&runtime_path, toml)
                .with_context(|| format!("Failed to write runtime metadata: {runtime_path:?}"))?;

            info!("Loaded runtime metadata from: {:?}", runtime_path);
            return Ok(metadata);
        }

        let metadata = Self::new(runtime_id);

        // Ensure parent directory exists
        if let Some(parent) = runtime_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create runtime directory: {parent:?}"))?;
        }

        let toml = toml::to_string_pretty(&metadata)
            .with_context(|| "Failed to serialize runtime metadata")?;
        fs::write(&runtime_path, toml)
            .with_context(|| format!("Failed to write runtime metadata: {runtime_path:?}"))?;

        info!("Created new runtime metadata at: {:?}", runtime_path);
        Ok(metadata)
    }

    /// Update the last_seen_at timestamp and save to disk
    pub fn touch(&mut self, resolver: &PathResolver) -> Result<()> {
        self.last_seen_at = Utc::now();
        let runtime_path = resolver.runtime_dir().join("runtime.toml");
        let toml =
            toml::to_string_pretty(self).with_context(|| "Failed to serialize runtime metadata")?;
        fs::write(&runtime_path, toml)
            .with_context(|| format!("Failed to write runtime metadata: {runtime_path:?}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_info_detect() {
        let info = HostInfo::detect();
        assert!(!info.os.is_empty());
        assert!(!info.arch.is_empty());
        assert!(!info.hostname.is_empty());
    }

    #[test]
    fn test_runtime_metadata_new() {
        let meta = RuntimeMetadata::new("did:key:z6MkTest");
        assert_eq!(meta.runtime_id, "did:key:z6MkTest");
        assert_eq!(meta.version, env!("CARGO_PKG_VERSION"));
        assert!(meta.capabilities.contains(&"filesystem".to_string()));
        assert!(meta.capabilities.contains(&"network".to_string()));
        assert!(!meta.host_info.os.is_empty());
    }

    #[test]
    fn test_runtime_metadata_serde() {
        let meta = RuntimeMetadata::new("did:key:z6MkTest");
        let toml_str = toml::to_string_pretty(&meta).unwrap();
        let parsed: RuntimeMetadata = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.runtime_id, meta.runtime_id);
        assert_eq!(parsed.version, meta.version);
        assert_eq!(parsed.capabilities, meta.capabilities);
        assert_eq!(parsed.host_info.os, meta.host_info.os);
        assert_eq!(parsed.host_info.arch, meta.host_info.arch);
    }
}
