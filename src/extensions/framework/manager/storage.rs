//! Extension storage backend
//!
//! Handles file-system operations for extension installation and removal.

use crate::extensions::framework::types::ExtensionId;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Maximum retries for Windows-locked directory operations.
const WINDOWS_REMOVE_RETRIES: u32 = 10;
/// Delay between retries (milliseconds).
const WINDOWS_REMOVE_RETRY_DELAY_MS: u64 = 200;

/// Try to remove a directory, with retries on Windows to handle transient
/// locks held by recently-terminated child processes.
fn remove_dir_all_with_retry(path: &Path) -> std::io::Result<()> {
    let mut last_err = None;
    for attempt in 0..WINDOWS_REMOVE_RETRIES {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < WINDOWS_REMOVE_RETRIES {
                    thread::sleep(Duration::from_millis(WINDOWS_REMOVE_RETRY_DELAY_MS));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "remove_dir_all retry exhausted")
    }))
}

/// Try to rename a directory, with retries on Windows to handle transient
/// locks held by recently-terminated child processes.
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last_err = None;
    for attempt in 0..WINDOWS_REMOVE_RETRIES {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt + 1 < WINDOWS_REMOVE_RETRIES {
                    thread::sleep(Duration::from_millis(WINDOWS_REMOVE_RETRY_DELAY_MS));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "rename retry exhausted")
    }))
}

/// Remove a directory that may be locked on Windows.
///
/// 1. Try `remove_dir_all` with retries (fast path).
/// 2. On failure, atomically rename to a temp name with retries.
/// 3. Best-effort delete the renamed directory.
///
/// Returns `Ok(())` if the directory is either deleted or renamed away.
fn remove_locked_dir(parent_dir: &Path, dir_name: &str) -> Result<()> {
    let target_dir = parent_dir.join(dir_name);
    if !target_dir.exists() {
        return Ok(());
    }

    // Fast path: direct removal with retries.
    if remove_dir_all_with_retry(&target_dir).is_ok() {
        return Ok(());
    }

    // On Windows the directory may still be locked by the daemon or a child
    // process (MCP server, gateway, etc.).  Unix allows unlinking open files;
    // Windows does not.  Fall back to an atomic rename so the extension is
    // logically removed, then best-effort delete the renamed directory.
    let temp_name = format!(
        "{}_uninstalled_{}",
        dir_name,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let temp_dir = parent_dir.join(&temp_name);

    rename_with_retry(&target_dir, &temp_dir).with_context(|| {
        format!("Failed to rename locked extension directory {target_dir:?} to {temp_dir:?}")
    })?;

    // Best-effort cleanup of the renamed directory.  It may succeed once the
    // holding process releases its handles.
    let _ = remove_dir_all_with_retry(&temp_dir);

    Ok(())
}

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

        // Preserve the existing `.source` file across the
        // delete-and-recopy — `peko ext pull` writes `.source`
        // first (so `peko principal export`'s extension-ref resolver
        // can see it), then IPC-installs the .ext, which calls
        // `copy_to_storage` here. Without preserving, the
        // install would erase the .source that pull just wrote,
        // and the principal export would silently skip the ext
        // (manifest.source = None → no ExtensionRef emitted).
        // Phase D3 flow 5b is the first end-to-end test that
        // exercised this code path and surfaced the regression.
        let preserved_source = if target_dir.exists() {
            let p = target_dir.join(".source");
            std::fs::read_to_string(&p).ok()
        } else {
            None
        };

        if target_dir.exists() {
            // On Windows, a previous process may still hold a handle to the directory.
            // Try removing; if it fails, try renaming to a unique temp name first.
            if remove_dir_all_with_retry(&target_dir).is_err() {
                let temp_name = format!(
                    "{}_old_{}",
                    extension_id.0,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                );
                let temp_dir = storage_dir.join(&temp_name);
                if rename_with_retry(&target_dir, &temp_dir).is_ok() {
                    // Best-effort cleanup of the renamed directory
                    let _ = remove_dir_all_with_retry(&temp_dir);
                } else {
                    return Err(anyhow::anyhow!(
                        "Failed to remove existing extension at {target_dir:?}: directory is locked by another process"
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

        // Restore the preserved `.source` (see comment above).
        if let Some(contents) = preserved_source {
            let p = target_dir.join(".source");
            let _ = std::fs::write(&p, contents);
        }

        Ok(target_dir)
    }

    pub fn remove_from_storage(&self, extension_id: &ExtensionId) -> Result<()> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;

        remove_locked_dir(storage_dir, &extension_id.0)
    }

    /// Write the source registry reference for an installed extension
    pub fn write_source(&self, extension_id: &ExtensionId, registry_ref: &str) -> Result<()> {
        let storage_dir = self
            .storage_dir
            .as_ref()
            .context("Storage directory not configured")?;
        let source_path = storage_dir.join(&extension_id.0).join(".source");
        std::fs::write(&source_path, registry_ref)
            .with_context(|| format!("Failed to write source file at {source_path:?}"))?;
        Ok(())
    }

    /// Read the source registry reference for an installed extension
    pub fn read_source(&self, extension_id: &ExtensionId) -> Option<String> {
        let storage_dir = self.storage_dir.as_ref()?;
        let source_path = storage_dir.join(&extension_id.0).join(".source");
        std::fs::read_to_string(&source_path).ok()
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
