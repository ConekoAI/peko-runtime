//! Extension storage backend
//!
//! Handles file-system operations for extension installation and removal.

use crate::extension::types::ExtensionId;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Storage backend for extension metadata
#[derive(Debug)]
pub struct ExtensionStorage {
    storage_dir: Option<PathBuf>,
}

impl ExtensionStorage {
    #[must_use]
    pub fn new() -> Self {
        Self { storage_dir: None }
    }

    #[must_use]
    pub fn with_dir(storage_dir: PathBuf) -> Self {
        Self {
            storage_dir: Some(storage_dir),
        }
    }

    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.storage_dir.as_deref()
    }

    pub fn copy_to_storage(&self, source: &Path, extension_id: &ExtensionId) -> Result<PathBuf> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;

        let target_dir = storage_dir.join(&extension_id.0);

        if target_dir.exists() {
            // On Windows, a previous process may still hold a handle to the directory.
            // Try removing; if it fails, try renaming to a unique temp name first.
            if let Err(e) = std::fs::remove_dir_all(&target_dir) {
                let temp_name = format!("{}_old_{}", extension_id.0, std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs());
                let temp_dir = storage_dir.join(&temp_name);
                if std::fs::rename(&target_dir, &temp_dir).is_ok() {
                    // Best-effort cleanup of the renamed directory
                    let _ = std::fs::remove_dir_all(&temp_dir);
                } else {
                    return Err(anyhow::anyhow!(
                        "Failed to remove existing extension at {target_dir:?}: {e}"
                    ));
                }
            }
        }

        // Handle single file (e.g., MCP config.toml) vs directory
        if source.is_file() {
            // Create target directory and copy the file into it
            std::fs::create_dir_all(&target_dir)
                .with_context(|| format!("Failed to create target directory {target_dir:?}"))?;
            let file_name = source.file_name().context("Invalid source file name")?;
            let target_file = target_dir.join(file_name);
            std::fs::copy(source, &target_file).with_context(|| {
                format!("Failed to copy file from {source:?} to {target_file:?}")
            })?;
        } else {
            copy_dir_recursive(source, &target_dir).with_context(|| {
                format!("Failed to copy extension from {source:?} to {target_dir:?}")
            })?;
        }

        Ok(target_dir)
    }

    pub fn remove_from_storage(&self, extension_id: &ExtensionId) -> Result<()> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;

        let target_dir = storage_dir.join(&extension_id.0);

        if target_dir.exists() {
            std::fs::remove_dir_all(&target_dir)
                .with_context(|| format!("Failed to remove extension at {target_dir:?}"))?;
        }

        Ok(())
    }
}

impl Default for ExtensionStorage {
    fn default() -> Self {
        Self::new()
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}
