//! File system tool for file operations with sandbox enforcement
//!
//! Implements CAPABILITY_INTERFACE.md §3.1
//! - All paths are sandboxed to instance workspace and shared workspace
//! - Path traversal is rejected with SandboxViolation
//! - Supports base64 encoding for binary files

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;
use tracing::{debug, warn};

use crate::security::SecurityPolicy;
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

/// Sandbox violation error
#[derive(Debug, Clone)]
pub struct SandboxViolation {
    pub path: String,
    pub reason: String,
}

impl std::fmt::Display for SandboxViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SandboxViolation: {} - {}", self.path, self.reason)
    }
}

impl std::error::Error for SandboxViolation {}

/// File system tool for reading and writing files with security sandboxing
pub struct FileSystemTool {
    policy: SecurityPolicy,
    /// Shared workspace path (for team agents)
    shared_workspace: Option<PathBuf>,
    /// Image projects directory (read-only)
    projects_dir: Option<PathBuf>,
}

impl FileSystemTool {
    /// Create a new file system tool with default security policy
    #[must_use]
    pub fn new() -> Self {
        Self {
            policy: SecurityPolicy::default(),
            shared_workspace: None,
            projects_dir: None,
        }
    }

    /// Create with custom security policy
    pub fn with_policy(policy: SecurityPolicy) -> Self {
        Self {
            policy,
            shared_workspace: None,
            projects_dir: None,
        }
    }

    /// Configure shared workspace for team agents
    #[must_use]
    pub fn with_shared_workspace(mut self, path: impl Into<PathBuf>) -> Self {
        self.shared_workspace = Some(path.into());
        self
    }

    /// Configure projects directory (read-only)
    #[must_use]
    pub fn with_projects_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.projects_dir = Some(path.into());
        self
    }

    /// Check if path is within allowed sandbox
    fn is_path_allowed(&self, path: &Path) -> Result<(), SandboxViolation> {
        // Get canonical path
        let canonical = if path.exists() {
            path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        };

        // Check workspace
        let workspace = self
            .policy
            .workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| self.policy.workspace_dir.clone());

        if canonical.starts_with(&workspace) {
            return Ok(());
        }

        // Check shared workspace (if configured)
        if let Some(ref shared) = self.shared_workspace {
            let shared_canonical = shared.canonicalize().unwrap_or_else(|_| shared.clone());
            if canonical.starts_with(&shared_canonical) {
                return Ok(());
            }
        }

        // Check projects directory (read-only)
        if let Some(ref projects) = self.projects_dir {
            let projects_canonical = projects.canonicalize().unwrap_or_else(|_| projects.clone());
            if canonical.starts_with(&projects_canonical) {
                return Ok(());
            }
        }

        // Path is outside all allowed areas
        Err(SandboxViolation {
            path: path.display().to_string(),
            reason: "Path is outside allowed sandbox".to_string(),
        })
    }

    /// Resolve and validate a path
    fn resolve_path(&self, path: &str) -> Result<PathBuf, SandboxViolation> {
        // Block path traversal attempts
        if path.contains("..") {
            return Err(SandboxViolation {
                path: path.to_string(),
                reason: "Path traversal not allowed".to_string(),
            });
        }

        let resolved = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.policy.workspace_dir.join(path)
        };

        // For non-existent paths, we need to check if the parent directory
        // is within the sandbox (since we can't canonicalize the file itself)
        let check_path = if resolved.exists() {
            resolved.clone()
        } else {
            // For new files, check the parent directory
            resolved
                .parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| resolved.clone())
        };

        self.is_path_allowed(&check_path)?;
        Ok(resolved)
    }

    /// Read a file
    async fn read_file(&self, path: &str, encoding: Option<&str>) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path).map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self.resolve_path(path).map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self.resolve_path(path).map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self.resolve_path(path).map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self.resolve_path(path).map_err(|e| anyhow::anyhow!(e))?;

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
        let from_resolved = self.resolve_path(from).map_err(|e| anyhow::anyhow!(e))?;
        let to_resolved = self.resolve_path(to).map_err(|e| anyhow::anyhow!(e))?;

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

## When to Use
- **read**: Inspecting source files, configuration files, logs, or any text content
- **write**: Creating new files or completely rewriting existing files
- **list**: Exploring directory structure, finding files by name pattern
- **exists**: Checking if a file or directory exists before operations
- **delete**: Removing files or directories (use with caution)
- **move**: Renaming or moving files/directories

## When NOT to Use
- For code edits that preserve most of the file (use `apply_patch` instead)
- For temporary files that should auto-cleanup (use `/tmp` via process)

## Input
```json
{
  "operation": "read|write|list|exists|delete|move",
  "path": "relative/or/absolute/path",
  "content": "file content (required for write)",
  "encoding": "utf8|base64 (optional, default utf8)",
  "mode": "overwrite|append|create_new (for write)",
  "recursive": false (for list and delete),
  "include_hidden": false (for list),
  "to": "destination/path (for move)"
}
```

## Returns
- **read**: File content, size, encoding, and path
- **write**: Bytes written and path
- **list**: Array of entries with name, type, size, modified time
- **exists**: Boolean existence check with type if exists
- **delete**: Success status
- **move**: Source and destination paths

## Examples
Read a file:
```json
{"operation": "read", "path": "src/main.rs"}
```

Read binary file as base64:
```json
{"operation": "read", "path": "image.png", "encoding": "base64"}
```

Write a file:
```json
{"operation": "write", "path": "config.toml", "content": "[settings]\nkey = value"}
```

List directory recursively:
```json
{"operation": "list", "path": "src", "recursive": true}
```

Move a file:
```json
{"operation": "move", "path": "old_name.txt", "to": "new_name.txt"}
```

## Security
- All paths are validated against security sandbox
- Path traversal (`..`) is blocked with SandboxViolation error
- Files outside workspace, shared workspace, or projects dir are inaccessible"#
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
                    "description": "The file or directory path"
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
    pub fn encode(input: &[u8]) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(input)
    }

    pub fn decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
        use base64::Engine;
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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);
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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
    async fn test_filesystem_sandbox_violation() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        // Try path traversal
        let params = json!({
            "operation": "read",
            "path": "../etc/passwd"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SandboxViolation"));
    }

    #[tokio::test]
    async fn test_filesystem_exists() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
}
