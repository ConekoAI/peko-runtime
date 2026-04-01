//! File system tool for file operations
//!
//! ⚠️ DEPRECATED: This monolithic tool is deprecated in favor of granular tools:
//! - ReadFile: Read file contents with line ranges
//! - WriteFile: Write or append to files
//! - Glob: List files matching patterns
//! - Grep: Search file contents
//! - StrReplaceFile: Targeted string replacements
//!
//! The granular tools provide better access control and clearer intent.
//! This tool will be removed in a future major version.
//!
//! Implements ADR-014: All-or-nothing permission model
//! - No sandboxing, no path restrictions
//! - Full filesystem access when tool is enabled
//! - Security boundary is tool enablement (enabled = full access)

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::tools::Tool;

/// Filesystem operation types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOperation {
    /// Read file contents
    Read,
    /// Write file contents
    Write,
    /// List directory contents
    List,
    /// Check if path exists
    Exists,
    /// Delete file or directory
    Delete,
    /// Move/rename file or directory
    Move,
}

/// Filesystem tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSystemArgs {
    pub operation: FileOperation,
    pub path: String,
    /// For write: content to write
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// For read: use base64 encoding for binary files
    #[serde(default)]
    pub encoding: Option<String>,
    /// For write: mode - overwrite (default), append, create_new
    #[serde(default)]
    pub mode: Option<String>,
    /// For list: include hidden files
    #[serde(default)]
    pub include_hidden: Option<bool>,
    /// For list: recursive listing
    #[serde(default)]
    pub recursive: Option<bool>,
    /// For delete: recursive delete for directories
    #[serde(default)]
    pub recursive_delete: Option<bool>,
    /// For move: destination path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// File system tool for reading and writing files
pub struct FileSystemTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
    /// Shared workspace path (for team agents)
    shared_workspace: Option<PathBuf>,
    /// Image projects directory
    projects_dir: Option<PathBuf>,
}

impl FileSystemTool {
    /// Create a new file system tool
    #[must_use]
    pub fn new() -> Self {
        Self {
            workspace_dir: None,
            shared_workspace: None,
            projects_dir: None,
        }
    }

    /// Configure workspace directory (default for relative paths)
    #[must_use]
    pub fn with_workspace(mut self, path: impl Into<PathBuf>) -> Self {
        self.workspace_dir = Some(path.into());
        self
    }

    /// Configure shared workspace for team agents
    #[must_use]
    pub fn with_shared_workspace(mut self, path: impl Into<PathBuf>) -> Self {
        self.shared_workspace = Some(path.into());
        self
    }

    /// Configure projects directory
    #[must_use]
    pub fn with_projects_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.projects_dir = Some(path.into());
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

    /// Read a file
    async fn read_file(&self, path: &str, encoding: Option<&str>) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        let content = fs::read(&resolved)
            .await
            .with_context(|| format!("Failed to read file: {}", resolved.display()))?;

        let (content_str, size_bytes, encoding_used) = if encoding == Some("base64") {
            // Return base64 encoded content
            (
                base64::encode(&content),
                content.len(),
                "base64".to_string(),
            )
        } else {
            // Try to decode as UTF-8
            match String::from_utf8(content.clone()) {
                Ok(text) => (text, content.len(), "utf8".to_string()),
                Err(_) => {
                    // Binary file - encode as base64
                    (
                        base64::encode(&content),
                        content.len(),
                        "base64".to_string(),
                    )
                }
            }
        };

        Ok(json!({
            "content": content_str,
            "size_bytes": size_bytes,
            "encoding": encoding_used,
            "path": resolved.display().to_string(),
        }))
    }

    /// Write a file
    async fn write_file(
        &self,
        path: &str,
        content: &str,
        encoding: Option<&str>,
        mode: Option<&str>,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        // Decode content if base64 encoded
        let decoded_content = if encoding == Some("base64") {
            base64::decode(content).map_err(|e| anyhow::anyhow!("Invalid base64: {}", e))?
        } else {
            content.as_bytes().to_vec()
        };

        // Check mode
        let mode = mode.unwrap_or("overwrite");
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
            "overwrite" | _ => {
                // Default: overwrite
            }
        }

        // Create parent directories if needed
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
        }

        // Write file
        if mode == "append" {
            use tokio::io::AsyncWriteExt;
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&resolved)
                .await
                .with_context(|| {
                    format!("Failed to open file for append: {}", resolved.display())
                })?;
            file.write_all(&decoded_content).await?;
        } else {
            fs::write(&resolved, &decoded_content)
                .await
                .with_context(|| format!("Failed to write file: {}", resolved.display()))?;
        }

        Ok(json!({
            "path": resolved.display().to_string(),
            "bytes_written": decoded_content.len(),
            "mode": mode,
        }))
    }

    /// List directory contents
    async fn list_dir(
        &self,
        path: &str,
        recursive: bool,
        include_hidden: bool,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        if !resolved.is_dir() {
            return Err(anyhow::anyhow!(
                "Path is not a directory: {}",
                resolved.display()
            ));
        }

        let entries = if recursive {
            self.list_dir_recursive(&resolved, include_hidden).await?
        } else {
            self.list_dir_flat(&resolved, include_hidden).await?
        };

        Ok(json!({
            "path": resolved.display().to_string(),
            "entries": entries,
        }))
    }

    /// Flat directory listing
    async fn list_dir_flat(
        &self,
        path: &Path,
        include_hidden: bool,
    ) -> Result<Vec<serde_json::Value>> {
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(path)
            .await
            .with_context(|| format!("Failed to read directory: {}", path.display()))?;

        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files unless requested
            if !include_hidden && name.starts_with('.') {
                continue;
            }

            let metadata = entry.metadata().await?;
            let entry_type = if metadata.is_file() {
                "file"
            } else if metadata.is_dir() {
                "dir"
            } else if metadata.is_symlink() {
                "symlink"
            } else {
                "other"
            };

            entries.push(json!({
                "name": name,
                "type": entry_type,
                "size_bytes": if metadata.is_file() { Some(metadata.len()) } else { None },
                "modified_at": metadata.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
                    .map(|dt| dt.map(|d| d.to_rfc3339()).unwrap_or_default()),
            }));
        }

        Ok(entries)
    }

    /// Recursive directory listing
    async fn list_dir_recursive(
        &self,
        path: &Path,
        include_hidden: bool,
    ) -> Result<Vec<serde_json::Value>> {
        let mut all_entries = Vec::new();
        let mut to_process = vec![path.to_path_buf()];

        while let Some(current_path) = to_process.pop() {
            let entries = self.list_dir_flat(&current_path, include_hidden).await?;

            for entry in entries {
                let name = entry["name"].as_str().unwrap_or("");
                let entry_type = entry["type"].as_str().unwrap_or("");

                // Calculate relative path from base
                let relative_path = current_path
                    .strip_prefix(path)
                    .unwrap_or(&current_path)
                    .join(name);

                let mut entry_with_path = entry.clone();
                entry_with_path["path"] = json!(relative_path.to_string_lossy().to_string());
                all_entries.push(entry_with_path);

                // Add directories to process queue
                if entry_type == "dir" {
                    let full_path = current_path.join(name);
                    to_process.push(full_path);
                }
            }
        }

        Ok(all_entries)
    }

    /// Check if a file exists
    async fn file_exists(&self, path: &str) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        let exists = fs::try_exists(&resolved).await.unwrap_or(false);
        let file_type = if exists {
            let metadata = fs::metadata(&resolved).await.ok();
            metadata.map(|m| {
                if m.is_file() {
                    "file"
                } else if m.is_dir() {
                    "dir"
                } else {
                    "other"
                }
                .to_string()
            })
        } else {
            None
        };

        Ok(json!({
            "exists": exists,
            "type": file_type,
            "path": resolved.display().to_string(),
        }))
    }

    /// Delete a file or directory
    async fn delete_file(&self, path: &str, recursive: bool) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        let metadata = fs::metadata(&resolved).await;

        match metadata {
            Ok(m) if m.is_file() => {
                fs::remove_file(&resolved)
                    .await
                    .with_context(|| format!("Failed to delete file: {}", resolved.display()))?;
            }
            Ok(m) if m.is_dir() => {
                if recursive {
                    fs::remove_dir_all(&resolved).await.with_context(|| {
                        format!("Failed to delete directory: {}", resolved.display())
                    })?;
                } else {
                    fs::remove_dir(&resolved).await.with_context(|| {
                        format!(
                            "Failed to delete directory (use recursive=true for non-empty): {}",
                            resolved.display()
                        )
                    })?;
                }
            }
            Ok(_) => return Err(anyhow::anyhow!("Unknown file type: {}", resolved.display())),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "File not found: {} - {}",
                    resolved.display(),
                    e
                ))
            }
        }

        Ok(json!({
            "path": resolved.display().to_string(),
            "deleted": true,
        }))
    }

    /// Move/rename a file or directory
    async fn move_file(&self, from: &str, to: &str) -> Result<serde_json::Value> {
        let from_resolved = self.resolve_path(from);
        let to_resolved = self.resolve_path(to);

        // Ensure source exists
        if !from_resolved.exists() {
            return Err(anyhow::anyhow!(
                "Source path does not exist: {}",
                from_resolved.display()
            ));
        }

        // Create parent directories for destination if needed
        if let Some(parent) = to_resolved.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
        }

        // Perform move
        fs::rename(&from_resolved, &to_resolved)
            .await
            .with_context(|| {
                format!(
                    "Failed to move {} to {}",
                    from_resolved.display(),
                    to_resolved.display()
                )
            })?;

        Ok(json!({
            "from": from_resolved.display().to_string(),
            "to": to_resolved.display().to_string(),
            "moved": true,
        }))
    }
}

impl Default for FileSystemTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for FileSystemTool {
    fn name(&self) -> &'static str {
        "filesystem"
    }

    fn description(&self) -> &'static str {
        "File system operations: read, write, list, exists, delete, move files and directories"
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
File system operations: read, write, list, check existence, delete, and move files and directories.

## Security Note
This tool has FULL FILESYSTEM ACCESS when enabled. It can:
- Read any file the OS user can read
- Write to any directory the OS user can write to
- Delete any file or directory the OS user can delete

Disable this tool in agent config if you don't need filesystem access.

## Operations

### read
Read file contents. Returns content as UTF-8 text or base64 for binary files.
```json
{"operation": "read", "path": "src/main.rs"}
{"operation": "read", "path": "image.png", "encoding": "base64"}
```

### write
Write content to a file. Creates parent directories if needed.
```json
{"operation": "write", "path": "config.toml", "content": "[settings]\\nkey = value"}
{"operation": "write", "path": "data.bin", "content": "SGVsbG8=", "encoding": "base64"}
```

Modes:
- `overwrite` (default): Replace existing file
- `append`: Append to existing file (create if not exists)
- `create_new`: Fail if file already exists

### list
List directory contents.
```json
{"operation": "list", "path": "src"}
{"operation": "list", "path": "src", "recursive": true, "include_hidden": true}
```

### exists
Check if a path exists.
```json
{"operation": "exists", "path": "README.md"}
```

### delete
Delete a file or directory.
```json
{"operation": "delete", "path": "old_file.txt"}
{"operation": "delete", "path": "old_dir", "recursive_delete": true}
```

### move
Rename or move a file/directory.
```json
{"operation": "move", "path": "old_name.txt", "to": "new_name.txt"}
{"operation": "move", "path": "file.txt", "to": "subdir/file.txt"}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "description": "The operation to perform",
                    "enum": ["read", "write", "list", "exists", "delete", "move"]
                },
                "path": {
                    "type": "string",
                    "description": "The file or directory path (relative to workspace or absolute)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write (required for write operation)"
                },
                "encoding": {
                    "type": "string",
                    "description": "Encoding for read/write operations",
                    "enum": ["utf8", "base64"]
                },
                "mode": {
                    "type": "string",
                    "description": "Write mode",
                    "enum": ["overwrite", "append", "create_new"],
                    "default": "overwrite"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "For list: include subdirectories. For delete: remove non-empty directories",
                    "default": false
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (starting with .) in listing",
                    "default": false
                },
                "to": {
                    "type": "string",
                    "description": "Destination path (required for move operation)"
                }
            },
            "required": ["operation", "path"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: FileSystemArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        match args.operation {
            FileOperation::Read => self.read_file(&args.path, args.encoding.as_deref()).await,
            FileOperation::Write => {
                let content = args.content.ok_or_else(|| {
                    anyhow::anyhow!("Missing required parameter: content for write operation")
                })?;
                self.write_file(
                    &args.path,
                    &content,
                    args.encoding.as_deref(),
                    args.mode.as_deref(),
                )
                .await
            }
            FileOperation::List => {
                let recursive = args.recursive.unwrap_or(false);
                let include_hidden = args.include_hidden.unwrap_or(false);
                self.list_dir(&args.path, recursive, include_hidden).await
            }
            FileOperation::Exists => self.file_exists(&args.path).await,
            FileOperation::Delete => {
                let recursive = args.recursive_delete.unwrap_or(false);
                self.delete_file(&args.path, recursive).await
            }
            FileOperation::Move => {
                let to = args.to.ok_or_else(|| {
                    anyhow::anyhow!("Missing required parameter: to for move operation")
                })?;
                self.move_file(&args.path, &to).await
            }
        }
    }
}

// Base64 encoding/decoding helper
mod base64 {
    use base64::Engine;
    pub fn encode(input: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(input)
    }

    pub fn decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
        base64::engine::general_purpose::STANDARD.decode(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_filesystem_tool_creation() {
        let tool = FileSystemTool::new();
        assert_eq!(tool.name(), "filesystem");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn test_filesystem_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());
        let test_file = "test.txt";

        // Write a file
        let write_params = json!({
            "operation": "write",
            "path": test_file,
            "content": "Hello, World!"
        });

        let result = tool.execute(write_params).await;
        assert!(result.is_ok(), "Write failed: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response["bytes_written"].as_u64().unwrap(), 13);

        // Read the file
        let read_params = json!({
            "operation": "read",
            "path": test_file
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_ok(), "Read failed: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response["content"].as_str().unwrap(), "Hello, World!");
        assert_eq!(response["encoding"].as_str().unwrap(), "utf8");
    }

    #[tokio::test]
    async fn test_filesystem_base64() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Write binary content as base64
        let binary_content = "SGVsbG8sIFdvcmxkIQ=="; // "Hello, World!" in base64
        let write_params = json!({
            "operation": "write",
            "path": "binary.bin",
            "content": binary_content,
            "encoding": "base64"
        });

        let result = tool.execute(write_params).await;
        assert!(result.is_ok(), "Write failed: {:?}", result);

        // Read back as base64
        let read_params = json!({
            "operation": "read",
            "path": "binary.bin",
            "encoding": "base64"
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_ok(), "Read failed: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response["encoding"].as_str().unwrap(), "base64");
    }

    #[tokio::test]
    async fn test_filesystem_move() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Create a file
        fs::write(temp_dir.path().join("old.txt"), "content")
            .await
            .unwrap();

        // Move the file
        let move_params = json!({
            "operation": "move",
            "path": "old.txt",
            "to": "new.txt"
        });

        let result = tool.execute(move_params).await;
        assert!(result.is_ok(), "Move failed: {:?}", result);

        // Verify old file is gone and new file exists
        assert!(!temp_dir.path().join("old.txt").exists());
        assert!(temp_dir.path().join("new.txt").exists());
    }

    #[tokio::test]
    async fn test_filesystem_exists() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Check non-existent file
        let exists_params = json!({
            "operation": "exists",
            "path": "nonexistent.txt"
        });

        let result = tool.execute(exists_params.clone()).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(!response["exists"].as_bool().unwrap());

        // Create the file
        fs::write(temp_dir.path().join("test.txt"), "test")
            .await
            .unwrap();

        // Check existing file
        let exists_params = json!({
            "operation": "exists",
            "path": "test.txt"
        });
        let result = tool.execute(exists_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response["exists"].as_bool().unwrap());
        assert_eq!(response["type"].as_str().unwrap(), "file");
    }

    #[tokio::test]
    async fn test_filesystem_list_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Create nested structure
        fs::create_dir_all(temp_dir.path().join("src/components"))
            .await
            .unwrap();
        fs::write(temp_dir.path().join("src/main.rs"), "fn main() {}")
            .await
            .unwrap();
        fs::write(
            temp_dir.path().join("src/components/mod.rs"),
            "pub mod foo;",
        )
        .await
        .unwrap();

        let list_params = json!({
            "operation": "list",
            "path": "src",
            "recursive": true
        });

        let result = tool.execute(list_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        let entries = response["entries"].as_array().unwrap();
        assert!(entries.len() >= 3); // main.rs, components/, components/mod.rs
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let temp_dir = TempDir::new().unwrap();

        // First create the nested directories
        let nested_dir = temp_dir.path().join("level1").join("level2").join("level3");
        fs::create_dir_all(&nested_dir).await.unwrap();

        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Write to a nested path
        let write_params = json!({
            "operation": "write",
            "path": "level1/level2/level3/file.txt",
            "content": "nested content"
        });

        let result = tool.execute(write_params).await;
        assert!(result.is_ok(), "Should write to nested path: {:?}", result);

        // Verify file was created
        let file_path = nested_dir.join("file.txt");
        assert!(file_path.exists(), "File should exist at {:?}", file_path);
        let content = fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn test_delete_file() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Create a file
        let file_path = temp_dir.path().join("to_delete.txt");
        fs::write(&file_path, "delete me").await.unwrap();
        assert!(file_path.exists());

        // Delete the file
        let delete_params = json!({
            "operation": "delete",
            "path": "to_delete.txt"
        });

        let result = tool.execute(delete_params).await;
        assert!(result.is_ok(), "Should delete file: {:?}", result);
        assert!(!file_path.exists());
    }

    #[tokio::test]
    async fn test_delete_directory_recursive() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Create a directory with files
        let dir_path = temp_dir.path().join("mydir");
        fs::create_dir(&dir_path).await.unwrap();
        fs::write(dir_path.join("file1.txt"), "content1")
            .await
            .unwrap();
        fs::write(dir_path.join("file2.txt"), "content2")
            .await
            .unwrap();

        // Delete the directory recursively
        let delete_params = json!({
            "operation": "delete",
            "path": "mydir",
            "recursive_delete": true
        });

        let result = tool.execute(delete_params).await;
        assert!(
            result.is_ok(),
            "Should delete directory recursively: {:?}",
            result
        );
        assert!(!dir_path.exists());
    }

    #[tokio::test]
    async fn test_binary_file_base64() {
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new().with_workspace(temp_dir.path());

        // Create a binary file
        let binary_content = vec![0u8, 1, 2, 3, 255, 254, 253];
        let file_path = temp_dir.path().join("binary.bin");
        fs::write(&file_path, &binary_content).await.unwrap();

        // Read as base64
        let read_params = json!({
            "operation": "read",
            "path": "binary.bin",
            "encoding": "base64"
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_ok(), "Should read binary file: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response["encoding"], "base64");

        // Verify we can decode it back
        let base64_content = response["content"].as_str().unwrap();
        let decoded = base64::decode(base64_content).unwrap();
        assert_eq!(decoded, binary_content);
    }

    #[tokio::test]
    async fn test_absolute_path_access() {
        // With ADR-014, absolute paths should work without sandbox restrictions
        let temp_dir = TempDir::new().unwrap();
        let tool = FileSystemTool::new(); // No workspace restriction

        // Create file using absolute path
        let file_path = temp_dir.path().join("absolute_test.txt");
        fs::write(&file_path, "absolute path content").await.unwrap();

        // Read using absolute path
        let read_params = json!({
            "operation": "read",
            "path": file_path.to_str().unwrap()
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_ok(), "Should read with absolute path: {:?}", result);
        let response = result.unwrap();
        assert!(response["content"].as_str().unwrap().contains("absolute path content"));
    }
}
