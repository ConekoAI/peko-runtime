//! `Edit` tool - Targeted string replacement in files
//!
//! Granular file modification with exact matching. Safer than full file rewrites.

use anyhow::{Context, Result};
use async_trait::async_trait;
use peko_fs_persistence::{WorkspaceFileLock, DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

use peko_tools_core::Tool;

/// `Edit` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditArgs {
    /// Path to the file
    pub file_path: String,
    /// The exact text to find (must match completely)
    pub old_string: String,
    /// The text to replace with
    pub new_string: String,
    /// Replace all occurrences instead of requiring a unique match
    #[serde(default)]
    pub replace_all: bool,
}

/// Per-call edit summary returned to the caller. Intentionally
/// minimal: the model already knows `old_string` / `new_string`, so
/// echoing them back would just inflate the context window. The
/// useful new information is `occurrences` (how many matches the
/// `old_string` resolved to and were replaced).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplacementResult {
    pub occurrences: usize,
}

/// `Edit` tool - Targeted string replacement
pub struct EditTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
    /// Directory holding cross-agent workspace lock files (ADR-065).
    /// `None` disables locking (legacy/test construction sites); the
    /// daemon wiring passes `<data_dir>/locks`.
    lock_dir: Option<PathBuf>,
}

impl EditTool {
    /// Create a new `Edit` tool
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
    /// When set, every edit acquires a fail-fast per-file lock keyed
    /// on the canonical target path. Because the lock is taken before
    /// the read-modify-write, a losing editor waits (or fails busy)
    /// and then re-reads fresh content — its stale view of the file
    /// never gets applied.
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

    /// Apply string replacements to a file
    async fn replace_in_file(
        &self,
        file_path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(file_path);

        // ADR-065: fail-fast cross-agent lock keyed on the canonical
        // target path. Taken before the read so the read-modify-write
        // is atomic with respect to other locked writers; held until
        // the end of this function (Drop).
        let _lock = match &self.lock_dir {
            Some(dir) => Some(
                WorkspaceFileLock::acquire_in(dir, &resolved, DEFAULT_WORKSPACE_LOCK_TIMEOUT_MS)
                    .await?,
            ),
            None => None,
        };

        // Check file exists
        if !resolved.exists() {
            return Err(anyhow::anyhow!(
                "File does not exist: {}",
                resolved.display()
            ));
        }

        // Read current content
        let content = fs::read_to_string(&resolved)
            .await
            .with_context(|| format!("Failed to read file: {}", resolved.display()))?;

        // Check for empty old string
        if old_string.is_empty() {
            return Err(anyhow::anyhow!("Empty 'old_string' is not allowed"));
        }

        // Count occurrences before replacement
        let occurrences = content.matches(old_string).count();

        if occurrences == 0 {
            return Err(anyhow::anyhow!(
                "String not found in file: '{}'",
                truncate_str(old_string, 50)
            ));
        }

        if !replace_all && occurrences > 1 {
            return Err(anyhow::anyhow!(
                "String found {occurrences} times (must be unique). Use a more specific 'old_string' or set replace_all=true."
            ));
        }

        // Apply the replacement
        let modified_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        // Write modified content
        fs::write(&resolved, modified_content)
            .await
            .with_context(|| format!("Failed to write file: {}", resolved.display()))?;

        Ok(serde_json::json!({
            "path": resolved.display().to_string(),
            "replacements": [ReplacementResult { occurrences }],
            "total_replacements": occurrences,
            "success": true,
        }))
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "Edit"
    }

    fn description(&self) -> String {
        r#"## Purpose
Make targeted string replacements in files. The 'old_string' must appear exactly once unless replace_all is true.

Use when: Making precise edits to existing files (safer than full file rewrites).
Don't use when: Creating new files (use Write) or making multiple unrelated changes (make multiple calls).

## Key Rules

1. The 'old_string' must match EXACTLY (including whitespace)
2. The 'old_string' must appear EXACTLY ONCE in the file unless replace_all=true
3. If either rule is violated, NO changes are made (atomic)

## Parameters

### file_path (required)
Path to the file to modify.

### old_string (required)
The exact text to find (must be unique unless replace_all=true).

### new_string (required)
The text to replace with.

### replace_all (optional, default false)
When true, replace every occurrence of old_string. When false, old_string must appear exactly once.

## Examples

Single replacement:
```json
{
  "file_path": "src/main.rs",
  "old_string": "println!(\"hello\");",
  "new_string": "println!(\"hello, world!\");"
}
```

Replace all occurrences:
```json
{
  "file_path": "src/main.rs",
  "old_string": "foo",
  "new_string": "bar",
  "replace_all": true
}
```

## Tips for Success

1. Read the file first with Read to get the exact content
2. Include enough context in 'old_string' to make it unique
3. Use the exact whitespace (spaces vs tabs)
4. For multi-line replacements, include the newline characters"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to find (must be unique unless replace_all=true)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences instead of requiring a unique match",
                    "default": false
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    /// F33: filesystem-mutating tool — opt out of parallel dispatch.
    /// Concurrent `Edit` calls on the same path would race on
    /// read-modify-write; mixed `Read + Edit` can observe torn state.
    /// (ADR-065 covers *cross-agent* contention via `WorkspaceFileLock`;
    /// this flag covers contention within one agent's runtime.)
    fn parallelizable(&self) -> bool {
        false
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: EditArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.replace_in_file(
            &args.file_path,
            &args.old_string,
            &args.new_string,
            args.replace_all,
        )
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
    async fn test_replace_single() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(temp_dir.path().join("test.txt"), "Hello, World!")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "World",
            "new_string": "Rust"
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 1);
        assert_eq!(result["success"], true);
        assert_eq!(result["replacements"][0]["occurrences"], 1);

        // Pin the slim shape: the model already knows old_string /
        // new_string, so the response must NOT echo them back.
        let entry = &result["replacements"][0];
        assert!(entry.get("old").is_none(), "old echo should be removed");
        assert!(entry.get("new").is_none(), "new echo should be removed");
        assert!(entry.get("success").is_none(), "success field is unused");
        assert!(entry.get("error").is_none(), "error field is unused");

        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "Hello, Rust!");
    }

    #[tokio::test]
    async fn test_replace_all() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "foo foo foo")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "foo",
            "new_string": "bar",
            "replace_all": true
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 3);
        assert_eq!(result["replacements"][0]["occurrences"], 3);

        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "bar bar bar");
    }

    #[tokio::test]
    async fn test_replace_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "Hello, World!")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "NonExistent",
            "new_string": "Replacement"
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("String not found"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_replace_not_unique() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        // Create file with duplicate content
        fs::write(temp_dir.path().join("test.txt"), "foo foo foo")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "foo",
            "new_string": "bar"
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("3 times"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "foo foo foo");
    }

    #[tokio::test]
    async fn test_replace_multiline() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "line1\nline2\nline3")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "line1\nline2",
            "new_string": "replaced"
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 1);

        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "replaced\nline3");
    }

    #[tokio::test]
    async fn test_replace_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "file_path": "nonexistent.txt",
            "old_string": "old",
            "new_string": "new"
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_edit_lock_dir_busy_error_on_contended_file() {
        // ADR-065: with a lock dir configured, an Edit targeting a file
        // whose workspace lock is already held must fail fast with a
        // "file busy" error. Because the lock is taken before the
        // read, the loser's stale view never gets applied.
        let temp_dir = TempDir::new().unwrap();
        let lock_dir = temp_dir.path().join("locks");
        let target = temp_dir.path().join("shared.txt");
        fs::write(&target, "line one\nline two\n").await.unwrap();

        let tool = EditTool::new()
            .with_workspace(temp_dir.path())
            .with_lock_dir(&lock_dir);

        let held = WorkspaceFileLock::acquire_in(&lock_dir, &target, 1_000)
            .await
            .unwrap();

        let params = json!({
            "file_path": "shared.txt",
            "old_string": "line two",
            "new_string": "line TWO (racing edit)"
        });
        let err = tool.execute(params.clone()).await.unwrap_err().to_string();
        assert!(err.contains("file busy"), "unexpected error: {err}");

        // The contended edit must not have touched the file.
        let content = fs::read_to_string(&target).await.unwrap();
        assert_eq!(content, "line one\nline two\n");

        // After the holder releases, the same edit succeeds.
        held.release().await.unwrap();
        tool.execute(params).await.unwrap();
        let content = fs::read_to_string(&target).await.unwrap();
        assert_eq!(content, "line one\nline TWO (racing edit)\n");
    }

    #[tokio::test]
    async fn test_replace_empty_old_string() {
        let temp_dir = TempDir::new().unwrap();
        let tool = EditTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "Hello, World!")
            .await
            .unwrap();

        let params = json!({
            "file_path": "test.txt",
            "old_string": "",
            "new_string": "replacement"
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Empty 'old_string'"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "Hello, World!");
    }
}
