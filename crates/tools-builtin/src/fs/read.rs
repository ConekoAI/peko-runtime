//! `Read` tool - Read file contents with optional line ranges
//!
//! Granular read-only file access for agents. Matches Claude Code's `Read`
//! tool surface — output is in `cat -n` format (each line of `content` is
//! prefixed with `<line_number>\t<text>`), so the model can read line numbers
//! directly from the response without cross-referencing separate metadata
//! fields. peko extensions over the Claude Code surface are binary
//! auto-detection and an explicit `encoding: "base64"` request.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

use peko_tools_core::Tool;

/// `Read` tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadArgs {
    /// Path to the file (relative to workspace or absolute)
    pub file_path: String,
    /// Starting line number (1-indexed, inclusive)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    /// Maximum number of lines to read
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Use base64 encoding for binary files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

/// `Read` tool - Read file contents with granular control
pub struct ReadTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl ReadTool {
    /// Create a new `Read` tool
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

    /// Resolve a path - expands `~`, then converts relative paths to
    /// absolute using workspace.
    fn resolve_path(&self, path: &str) -> PathBuf {
        let path_buf = crate::paths::expand_tilde(path);
        if path_buf.is_absolute() {
            path_buf
        } else if let Some(ref workspace) = self.workspace_dir {
            workspace.join(path_buf)
        } else {
            path_buf
        }
    }

    /// Read file contents with optional line range
    async fn read_file(
        &self,
        file_path: &str,
        offset: Option<usize>,
        limit: Option<usize>,
        encoding: Option<&str>,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(file_path);

        // Verify it's a file
        let metadata = fs::metadata(&resolved)
            .await
            .with_context(|| format!("Failed to read file metadata: {}", resolved.display()))?;

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

        // Apply line range if specified and text content. Text output is
        // formatted in `cat -n` style: each line is prefixed with its
        // 1-indexed line number and a tab, so the model can see line numbers
        // inline with the content (matching Claude Code's Read behavior).
        let (final_content, total_lines, start_line, end_line) =
            if is_binary || encoding_used == "base64" {
                // For binary, line range doesn't apply
                (content_str, None, None, None)
            } else {
                let lines: Vec<&str> = content_str.lines().collect();
                let total = lines.len();

                let start = offset.map_or(0, |o| o.saturating_sub(1));
                let end = limit.map_or(total, |n| (start + n).min(total));

                let selected: Vec<&str> = lines.get(start..end).unwrap_or(&[]).to_vec();

                let numbered = selected
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{}\t{}", start + i + 1, line))
                    .collect::<Vec<_>>()
                    .join("\n");

                (
                    numbered,
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

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(input)
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "Read"
    }

    fn description(&self) -> String {
        r#"## Purpose
Read file contents with support for partial reading (line ranges) and binary files.

Text output is in `cat -n` format: each line of `content` is prefixed with
its 1-indexed line number and a tab, so line numbers are visible inline.

Use when: Reading source code, configuration files, logs, or any text file.
Don't use when: You need to write files (use Write) or search across files (use Grep).

## Parameters

### file_path (required)
Path to the file. Can be relative to workspace or absolute.

### offset (optional)
Starting line number (1-indexed). If omitted, starts from beginning.

### limit (optional)
Maximum number of lines to read. If omitted, reads to end of file.

### encoding (optional)
- "utf8" (default): Read as text, content returned in `cat -n` format
- "base64": Force base64 encoding for binary data

## Examples

Read entire file:
```json
{"file_path": "src/main.rs"}
```

Read specific lines:
```json
{"file_path": "src/main.rs", "offset": 10, "limit": 20}
```

Read binary file:
```json
{"file_path": "image.png", "encoding": "base64"}
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
                "offset": {
                    "type": "integer",
                    "description": "Starting line number (1-indexed, inclusive)",
                    "minimum": 1
                },
                "limit": {
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
            "required": ["file_path"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: ReadArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.read_file(
            &args.file_path,
            args.offset,
            args.limit,
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
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(temp_dir.path().join("test.txt"), "Hello, World!")
            .await
            .unwrap();

        let params = json!({"file_path": "test.txt"});
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["content"], "1\tHello, World!");
        assert_eq!(result["encoding"], "utf8");
        assert_eq!(result["size_bytes"], 13);
    }

    #[tokio::test]
    async fn test_read_file_with_line_range() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        // Create test file with multiple lines
        let content = "line1\nline2\nline3\nline4\nline5";
        fs::write(temp_dir.path().join("lines.txt"), content)
            .await
            .unwrap();

        // Read lines 2-3
        let params = json!({
            "file_path": "lines.txt",
            "offset": 2,
            "limit": 2
        });
        let result = tool.execute(params).await.unwrap();

        // Content is in `cat -n` format: each line prefixed with its
        // 1-indexed line number and a tab.
        assert_eq!(result["content"], "2\tline2\n3\tline3");
        assert_eq!(result["start_line"], 2);
        assert_eq!(result["end_line"], 3);
        assert_eq!(result["total_lines"], 5);
    }

    #[tokio::test]
    async fn test_read_file_full_cat_n() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        let content = "alpha\nbeta\ngamma";
        fs::write(temp_dir.path().join("abc.txt"), content)
            .await
            .unwrap();

        let result = tool.execute(json!({"file_path": "abc.txt"})).await.unwrap();
        assert_eq!(result["content"], "1\talpha\n2\tbeta\n3\tgamma");
        assert_eq!(result["start_line"], 1);
        assert_eq!(result["end_line"], 3);
    }

    #[tokio::test]
    async fn test_read_file_binary() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        // Create binary file
        let binary_content = vec![0u8, 1, 2, 3, 255, 254, 253];
        fs::write(temp_dir.path().join("binary.bin"), &binary_content)
            .await
            .unwrap();

        let params = json!({"file_path": "binary.bin", "encoding": "base64"});
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["encoding"], "base64");

        // Verify we can decode it back
        let base64_content = result["content"].as_str().unwrap();
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(base64_content)
            .unwrap();
        assert_eq!(decoded, binary_content);
    }

    #[tokio::test]
    async fn test_read_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        let params = json!({"file_path": "nonexistent.txt"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_file_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        // Create a directory
        fs::create_dir(temp_dir.path().join("mydir")).await.unwrap();

        let params = json!({"file_path": "mydir"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not a file"));
    }

    #[tokio::test]
    async fn test_read_file_pages_field_dropped() {
        // The `pages` parameter was removed (it was a dead surface area —
        // declared in the schema but always errored at runtime). This test
        // pins that an unknown `pages` field is ignored without error.
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("doc.txt"), "text")
            .await
            .unwrap();

        let params = json!({"file_path": "doc.txt", "pages": "1-2"});
        let result = tool.execute(params).await.unwrap();
        assert_eq!(result["content"], "1\ttext");
    }
}
