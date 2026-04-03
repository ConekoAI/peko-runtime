//! WriteFile tool - Write or append to files
//!
//! Granular write access for agents. Creates parent directories automatically.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::tools::Tool;

/// WriteFile tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileArgs {
    /// Path to the file (relative to workspace or absolute)
    pub path: String,
    /// Content to write
    pub content: String,
    /// Write mode: overwrite (default), append, create_new
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Content encoding: utf8 (default) or base64
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

fn default_mode() -> String {
    "overwrite".to_string()
}

fn default_encoding() -> String {
    "utf8".to_string()
}

/// WriteFile tool - Write files with various modes
pub struct WriteFileTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl WriteFileTool {
    /// Create a new WriteFile tool
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace_dir: None,
        }
    }

    /// Configure workspace directory (default for relative paths)
    #[must_use]
    pub fn with_workspace(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_dir = Some(path.into());
        self
    }

    /// Resolve a path - converts relative paths to absolute using workspace
    fn resolve_path(&self, path: &str) -> PathBuf {
        let path_buf = PathBuf::from(path);
        if path_buf.is_absolute() {
            path_buf
        } else if let Some(ref workspace) = self.workspace_dir {
            workspace.join(path)
        } else {
            path_buf
        }
    }

    /// Write file with specified mode and encoding
    async fn write_file(
        &self,
        path: &str,
        content: &str,
        mode: &str,
        encoding: &str,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        // Decode content if base64 encoded
        let decoded_content = if encoding == "base64" {
            base64_decode(content).map_err(|e| anyhow::anyhow!("Invalid base64: {}", e))?
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
                return Err(anyhow::anyhow!("Invalid mode: {}. Use overwrite, append, or create_new", other));
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

impl Default for WriteFileTool {
    fn default() -> Self {
        Self::new()
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input)
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Write or append content to files"
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Write or append content to files. Creates parent directories automatically.

Use when: Creating new files, overwriting existing files, or appending to logs.
Don't use when: Making targeted edits to existing files (use StrReplaceFile instead).

## Parameters

### path (required)
Path to the file. Can be relative to workspace or absolute.

### content (required)
Content to write to the file.

### mode (optional)
- "overwrite" (default): Replace existing file or create new
- "append": Append to existing file (create if not exists)
- "create_new": Fail if file already exists

### encoding (optional)
- "utf8" (default): Content is UTF-8 text
- "base64": Content is base64-encoded binary data

## Examples

Create/overwrite a file:
```json
{"path": "config.toml", "content": "[settings]\nkey = \"value\""}
```

Append to a file:
```json
{"path": "log.txt", "content": "New log entry\n", "mode": "append"}
```

Write binary data:
```json
{"path": "data.bin", "content": "SGVsbG8=", "encoding": "base64"}
```"#
        .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file (relative to workspace or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                },
                "mode": {
                    "type": "string",
                    "description": "Write mode",
                    "enum": ["overwrite", "append", "create_new"],
                    "default": "overwrite"
                },
                "encoding": {
                    "type": "string",
                    "description": "Content encoding",
                    "enum": ["utf8", "base64"],
                    "default": "utf8"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: WriteFileArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        self.write_file(&args.path, &args.content, &args.mode, &args.encoding)
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
    async fn test_write_file_overwrite() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteFileTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "path": "test.txt",
            "content": "Hello, World!"
        });

        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 13);
        assert_eq!(result["mode"], "overwrite");

        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_write_file_append() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteFileTool::new().with_workspace(temp_dir.path());

        // Initial write
        fs::write(temp_dir.path().join("log.txt"), "Line 1\n").await.unwrap();

        // Append
        let params = json!({
            "path": "log.txt",
            "content": "Line 2\n",
            "mode": "append"
        });

        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["bytes_written"], 7);

        let content = fs::read_to_string(temp_dir.path().join("log.txt")).await.unwrap();
        assert_eq!(content, "Line 1\nLine 2\n");
    }

    #[tokio::test]
    async fn test_write_file_create_new_fails_on_existing() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteFileTool::new().with_workspace(temp_dir.path());

        // Create file
        fs::write(temp_dir.path().join("existing.txt"), "content").await.unwrap();

        // Try to create_new
        let params = json!({
            "path": "existing.txt",
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
        let tool = WriteFileTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "path": "level1/level2/level3/file.txt",
            "content": "nested content"
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.is_object());

        let content = fs::read_to_string(temp_dir.path().join("level1/level2/level3/file.txt")).await.unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn test_write_file_base64() {
        let temp_dir = TempDir::new().unwrap();
        let tool = WriteFileTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "path": "binary.bin",
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
        let tool = WriteFileTool::new(); // No workspace

        let file_path = temp_dir.path().join("absolute.txt");
        let params = json!({
            "path": file_path.to_str().unwrap(),
            "content": "absolute content"
        });

        let result = tool.execute(params).await.unwrap();
        assert!(result.is_object());

        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "absolute content");
    }
}
