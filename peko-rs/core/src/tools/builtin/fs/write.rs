//! `Write` tool - Write or append to files
//!
//! Granular write access for agents. Creates parent directories automatically.

use anyhow::{Context, Result};
use async_trait::async_trait;
use peko_fs_persistence::{WorkspaceFileLock, DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use peko_tools_core::Tool;

/// `Write` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteArgs {
    /// Path to the file (relative to workspace or absolute)
    pub file_path: String,
    /// Content to write
    pub content: String,
    /// Write mode: `overwrite` (default; Claude Code parity), `create_new`
    /// (refuse if the file already exists), or `append`
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Content encoding: utf8 (default) or base64
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

fn default_mode() -> String {
    // Default to `overwrite` so callers trained on Claude Code's Write
    // tool (where Write always replaces the destination) behave as
    // expected. Callers who want a safer "fail on existing" semantic
    // can opt in via `mode: "create_new"`. `append` is the third mode
    // for log-style writes.
    "overwrite".to_string()
}

fn default_encoding() -> String {
    "utf8".to_string()
}

/// `Write` tool - Write files with various modes
pub struct WriteTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
    /// Directory holding cross-agent workspace lock files (ADR-065).
    /// `None` disables locking (legacy/test construction sites); the
    /// daemon wiring passes `<data_dir>/locks`.
    lock_dir: Option<PathBuf>,
}

impl WriteTool {
    /// Create a new `Write` tool
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace_dir: None,
            lock_dir: None,
        }
    }

    /// Configure workspace directory (default for relative paths)
    #[must_use]
    pub fn with_workspace(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_dir = Some(path.into());
        self
    }

    /// Configure the workspace lock directory (ADR-065).
    ///
    /// When set, every write acquires a fail-fast per-file lock keyed
    /// on the canonical target path so concurrent agents on the same
    /// runtime cannot silently clobber each other's edits.
    #[must_use]
    pub fn with_lock_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.lock_dir = Some(path.into());
        self
    }

    /// Resolve a path - expands `~`, then converts relative paths to
    /// absolute using workspace.
    fn resolve_path(&self, path: &str) -> PathBuf {
        let path_buf = crate::tools::builtin::paths::expand_tilde(path);
        if path_buf.is_absolute() {
            path_buf
        } else if let Some(ref workspace) = self.workspace_dir {
            workspace.join(path_buf)
        } else {
            path_buf
        }
    }

    /// Write file with specified mode and encoding
    async fn write(
        &self,
        file_path: &str,
        content: &str,
        mode: &str,
        encoding: &str,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(file_path);

        // ADR-065: fail-fast cross-agent lock keyed on the canonical
        // target path. Held until the end of this function (Drop).
        let _lock = match &self.lock_dir {
            Some(dir) => Some(
                WorkspaceFileLock::acquire_in(dir, &resolved, DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS)
                    .await?,
            ),
            None => None,
        };

        // Decode content if base64 encoded
        let decoded_content = if encoding == "base64" {
            base64_decode(content).map_err(|e| anyhow::anyhow!("Invalid base64: {e}"))?
        } else {
            content.as_bytes().to_vec()
        };

        // Check mode constraints
        match mode {
            "create_new" => {
                if resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "File already exists and mode is 'create_new': {}",
                        resolved.display()
                    ));
                }
            }
            "append" => {
                // Will append below
            }
            "overwrite" => {
                // Default: overwrite
            }
            other => {
                return Err(anyhow::anyhow!(
                    "Invalid mode: {other}. Use overwrite, append, or create_new"
                ));
            }
        }

        // Create parent directories if needed
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
        }

        // Write file based on mode
        let bytes_written = if mode == "append" {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&resolved)
                .await
                .with_context(|| {
                    format!("Failed to open file for append: {}", resolved.display())
                })?;
            file.write_all(&decoded_content).await?;
            decoded_content.len()
        } else {
            fs::write(&resolved, &decoded_content)
                .await
                .with_context(|| format!("Failed to write file: {}", resolved.display()))?;
            decoded_content.len()
        };

        // Get file info after write
        let size_bytes = fs::metadata(&resolved).await.map(|m| m.len()).unwrap_or(0);

        Ok(serde_json::json!({
            "path": resolved.display().to_string(),
            "bytes_written": bytes_written,
            "size_bytes": size_bytes,
            "mode": mode,
            "encoding": encoding,
        }))
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input)
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "Write"
    }

    fn description(&self) -> String {
        r#"## Purpose
Write or append content to files. Creates parent directories automatically.

Use when: Creating new files, overwriting existing files, or appending to logs.
Don't use when: Making targeted edits to existing files (use Edit instead).

## Parameters

### file_path (required)
Path to the file. Can be relative to workspace or absolute.

### content (required)
Content to write to the file.

### mode (optional)
- "overwrite" (default): Replace the file's contents, or create it if missing
- "create_new": Fail if the file already exists (safe-create for new files)
- "append": Append to existing file (create if not exists)

### encoding (optional)
- "utf8" (default): Content is UTF-8 text
- "base64": Content is base64-encoded binary data

## Examples

Create/overwrite a file:
```json
{"file_path": "config.toml", "content": "[settings]\nkey = \"value\""}
```

Append to a file:
```json
{"file_path": "log.txt", "content": "New log entry\n", "mode": "append"}
```

Write binary data:
```json
{"file_path": "data.bin", "content": "SGVsbG8=", "encoding": "base64"}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file (relative to workspace or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                },
                "mode": {
                    "type": "string",
                    "description": "Write mode (default: overwrite)",
                    "enum": ["overwrite", "create_new", "append"],
                    "default": "overwrite"
                },
                "encoding": {
                    "type": "string",
                    "description": "Content encoding",
                    "enum": ["utf8", "base64"],
                    "default": "utf8"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    /// F33: filesystem-mutating tool — opt out of parallel dispatch.
    /// Two concurrent `Write` calls could clobber each other; a `Read`
    /// racing with `Write` can observe a half-written file.
    /// (ADR-065 covers *cross-agent* contention via `WorkspaceFileLock`;
    /// this flag covers contention within one agent's runtime.)
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: WriteArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.write(&args.file_path, &args.content, &args.mode, &args.encoding)
            .await
    }

    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        100 // Fast operation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_write_file_default_overwrite() {
        // Default mode is `overwrite` for Claude Code parity — Write
        // replaces the destination's contents by default. The reported
        // mode in the result confirms what the tool actually did.
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "file_path": "test.txt",
            "content": "Hello, World!"
        });

        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 13);
        assert_eq!(result["mode"], "overwrite");

        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_write_file_overwrite_replaces_existing() {
        // The default `overwrite` mode replaces existing content — a
        // safety-conscious caller who wants the old behavior should
        // pass `mode: "create_new"` explicitly.
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("existing.txt"), "old")
            .await
            .unwrap();

        let params = json!({
            "file_path": "existing.txt",
            "content": "new"
        });
        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["mode"], "overwrite");

        let content = fs::read_to_string(temp_dir.path().join("existing.txt"))
            .await
            .unwrap();
        assert_eq!(content, "new");
    }

    #[tokio::test]
    async fn test_write_file_append() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        // Initial write
        fs::write(temp_dir.path().join("log.txt"), "Line 1\n")
            .await
            .unwrap();

        // Append
        let params = json!({
            "file_path": "log.txt",
            "content": "Line 2\n",
            "mode": "append"
        });

        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 7);

        let content = fs::read_to_string(temp_dir.path().join("log.txt"))
            .await
            .unwrap();
        assert_eq!(content, "Line 1\nLine 2\n");
    }

    #[tokio::test]
    async fn test_write_file_create_new_fails_on_existing() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        // Create file
        fs::write(temp_dir.path().join("existing.txt"), "content")
            .await
            .unwrap();

        // Try to create_new
        let params = json!({
            "file_path": "existing.txt",
            "content": "new content",
            "mode": "create_new"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "file_path": "level1/level2/level3/file.txt",
            "content": "nested content"
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.is_object());

        let content = fs::read_to_string(temp_dir.path().join("level1/level2/level3/file.txt"))
            .await
            .unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn test_write_file_base64() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "file_path": "binary.bin",
            "content": "SGVsbG8sIFdvcmxkIQ==", // "Hello, World!" in base64
            "encoding": "base64"
        });

        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 13);

        let content = fs::read(temp_dir.path().join("binary.bin")).await.unwrap();
        assert_eq!(content, b"Hello, World!");
    }

    #[tokio::test]
    async fn test_write_file_absolute_path() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new(); // No workspace

        let file_path = temp_dir.path().join("absolute.txt");
        let params = json!({
            "file_path": file_path.to_str().unwrap(),
            "content": "absolute content"
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.is_object());

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "absolute content");
    }

    #[tokio::test]
    async fn test_write_lock_dir_busy_error_on_contended_file() {
        // ADR-065: with a lock dir configured, a Write targeting a file
        // whose workspace lock is already held must fail fast with a
        // "file busy" error instead of silently clobbering.
        let temp_dir = TempDir::new().unwrap();
        let lock_dir = temp_dir.path().join("locks");
        let target = temp_dir.path().join("shared.txt");
        fs::write(&target, "original").await.unwrap();

        let tool = WriteTool::new()
            .with_workspace(temp_dir.path())
            .with_lock_dir(&lock_dir);

        let held = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();

        let params = json!({
            "file_path": "shared.txt",
            "content": "clobber attempt"
        });
        let result = tool.execute(params.clone()).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("file busy"), "unexpected error: {err}");

        // The failed write must not have touched the file.
        let content = fs::read_to_string(&target).await.unwrap();
        assert_eq!(content, "original");

        // After the holder releases, the write goes through.
        held.release().await.unwrap();
        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 15);
    }

    #[tokio::test]
    async fn test_write_without_lock_dir_skips_locking() {
        // Legacy construction (no lock dir) must not require any lock
        // infrastructure — the pre-ADR-065 behavior.
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "file_path": "plain.txt",
            "content": "no locks here"
        });
        tool.execute(params).await.unwrap();

        let content = fs::read_to_string(temp_dir.path().join("plain.txt"))
            .await
            .unwrap();
        assert_eq!(content, "no locks here");
    }
}
