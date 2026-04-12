//! ReadFile tool - Read file contents with optional line ranges
//!
//! Granular read-only file access for agents.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::tools::Tool;

/// ReadFile tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadFileArgs {
    /// Path to the file (relative to workspace or absolute)
    pub path: String,
    /// Starting line number (1-indexed, inclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_offset: Option<usize>,
    /// Maximum number of lines to read
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n_lines: Option<usize>,
    /// Use base64 encoding for binary files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// ReadFile tool - Read file contents with granular control
pub struct ReadFileTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl ReadFileTool {
    /// Create a new ReadFile tool
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

    /// Read file contents with optional line range
    async fn read_file(
        &self,
        path: &str,
        line_offset: Option<usize>,
        n_lines: Option<usize>,
        encoding: Option<&str>,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        // Verify it's a file
        let metadata = fs::metadata(&resolved).await.with_context(|| {
            format!("Failed to read file metadata: {}", resolved.display())
        })?;

        if !metadata.is_file() {
            return Err(anyhow::anyhow!(
                "Path is not a file: {}",
                resolved.display()
            ));
        }

        // Read file content
        let content = fs::read(&resolved)
            .await
            .with_context(|| format!("Failed to read file: {}", resolved.display()))?;

        // Determine encoding and process content
        let (content_str, encoding_used, is_binary) = if encoding == Some("base64") {
            // Explicitly requested base64
            (base64_encode(&content), "base64", true)
        } else {
            // Try to decode as UTF-8
            match String::from_utf8(content.clone()) {
                Ok(text) => (text, "utf8", false),
                Err(_) => {
                    // Binary file - encode as base64
                    (base64_encode(&content), "base64", true)
                }
            }
        };

        // Apply line range if specified and text content
        let (final_content, total_lines, start_line, end_line) = if is_binary || encoding_used == "base64" {
            // For binary, line range doesn't apply
            (content_str, None, None, None)
        } else {
            let lines: Vec<&str> = content_str.lines().collect();
            let total = lines.len();
            
            let start = line_offset.map(|o| o.saturating_sub(1)).unwrap_or(0);
            let end = n_lines.map(|n| (start + n).min(total)).unwrap_or(total);
            
            let selected: Vec<&str> = lines.get(start..end).unwrap_or(&[]).to_vec();
            
            (
                selected.join("\n"),
                Some(total),
                Some(start + 1), // Convert back to 1-indexed
                Some(end),
            )
        };

        let mut result = serde_json::json!({
            "content": final_content,
            "path": resolved.display().to_string(),
            "size_bytes": content.len(),
            "encoding": encoding_used,
        });

        // Add line info if applicable
        if let (Some(total), Some(start), Some(end)) = (total_lines, start_line, end_line) {
            let obj = result.as_object_mut().unwrap();
            obj.insert("total_lines".to_string(), total.into());
            obj.insert("start_line".to_string(), start.into());
            obj.insert("end_line".to_string(), end.into());
        }

        Ok(result)
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> String {
        r#"## Purpose
Read file contents with support for partial reading (line ranges) and binary files.

Use when: Reading source code, configuration files, logs, or any text file.
Don't use when: You need to write files (use WriteFile) or search across files (use Grep).

## Parameters

### path (required)
Path to the file. Can be relative to workspace or absolute.

### line_offset (optional)
Starting line number (1-indexed). If omitted, starts from beginning.

### n_lines (optional)
Maximum number of lines to read. If omitted, reads to end of file.

### encoding (optional)
- "utf8" (default): Read as text
- "base64": Force base64 encoding for binary data

## Examples

Read entire file:
```json
{"path": "src/main.rs"}
```

Read specific lines:
```json
{"path": "src/main.rs", "line_offset": 10, "n_lines": 20}
```

Read binary file:
```json
{"path": "image.png", "encoding": "base64"}
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
                "line_offset": {
                    "type": "integer",
                    "description": "Starting line number (1-indexed, inclusive)",
                    "minimum": 1
                },
                "n_lines": {
                    "type": "integer",
                    "description": "Maximum number of lines to read",
                    "minimum": 1
                },
                "encoding": {
                    "type": "string",
                    "description": "Encoding for the content",
                    "enum": ["utf8", "base64"]
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ReadFileArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        self.read_file(
            &args.path,
            args.line_offset,
            args.n_lines,
            args.encoding.as_deref(),
        )
        .await
    }

    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        50 // Fast operation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_read_file_basic() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadFileTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(temp_dir.path().join("test.txt"), "Hello, World!").await.unwrap();

        let params = json!({"path": "test.txt"});
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["content"], "Hello, World!");
        assert_eq!(result["encoding"], "utf8");
        assert_eq!(result["size_bytes"], 13);
    }

    #[tokio::test]
    async fn test_read_file_with_line_range() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadFileTool::new().with_workspace(temp_dir.path());

        // Create test file with multiple lines
        let content = "line1\nline2\nline3\nline4\nline5";
        fs::write(temp_dir.path().join("lines.txt"), content).await.unwrap();

        // Read lines 2-3
        let params = json!({
            "path": "lines.txt",
            "line_offset": 2,
            "n_lines": 2
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["content"], "line2\nline3");
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 3);
        assert_eq!(result["total_lines"], 5);
    }

    #[tokio::test]
    async fn test_read_file_binary() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadFileTool::new().with_workspace(temp_dir.path());

        // Create binary file
        let binary_content = vec![0u8, 1, 2, 3, 255, 254, 253];
        fs::write(temp_dir.path().join("binary.bin"), &binary_content).await.unwrap();

        let params = json!({"path": "binary.bin", "encoding": "base64"});
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["encoding"], "base64");
        
        // Verify we can decode it back
        let base64_content = result["content"].as_str().unwrap();
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD.decode(base64_content).unwrap();
        assert_eq!(decoded, binary_content);
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadFileTool::new().with_workspace(temp_dir.path());

        let params = json!({"path": "nonexistent.txt"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_file_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadFileTool::new().with_workspace(temp_dir.path());

        // Create a directory
        fs::create_dir(temp_dir.path().join("mydir")).await.unwrap();

        let params = json!({"path": "mydir"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a file"));
    }
}
