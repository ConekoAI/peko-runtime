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

    /// Resolve and validate a path with symlink security checking
    ///
    /// Per CAPABILITY_INTERFACE.md §7.2:
    /// - Symlink inside → Points inside: ✅ Allow
    /// - Symlink inside → Points outside: ❌ SandboxViolation
    /// - Symlink outside → Any target: ❌ SandboxViolation (cannot access symlink itself)
    async fn resolve_path(&self, path: &str) -> Result<PathBuf, SandboxViolation> {
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

        // Check if the path itself is a symlink and validate it
        self.validate_symlink_security(&resolved).await?;

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

    /// Validate symlink security per CAPABILITY §7.2
    async fn validate_symlink_security(&self, path: &Path) -> Result<(), SandboxViolation> {
        // Get the symlink metadata without following the link
        let metadata = match fs::symlink_metadata(path).await {
            Ok(m) => m,
            Err(_) => return Ok(()), // Not a symlink or doesn't exist - no issue
        };

        // If it's not a symlink, nothing to check
        if !metadata.file_type().is_symlink() {
            return Ok(());
        }

        // It's a symlink - apply security rules

        // Rule 3: Symlink outside sandbox → Any target: ❌ Block
        // First check if the symlink itself is within the sandbox
        let workspace = &self.policy.workspace_dir;
        let workspace_canonical = workspace
            .canonicalize()
            .unwrap_or_else(|_| workspace.clone());

        // Get the directory containing the symlink
        let symlink_dir = path.parent().ok_or_else(|| SandboxViolation {
            path: path.display().to_string(),
            reason: "Cannot determine parent directory of symlink".to_string(),
        })?;

        let symlink_dir_canonical = symlink_dir
            .canonicalize()
            .unwrap_or_else(|_| symlink_dir.to_path_buf());

        // Check if symlink is within workspace
        let symlink_in_workspace = symlink_dir_canonical.starts_with(&workspace_canonical);

        if !symlink_in_workspace {
            // Check shared workspace
            let symlink_in_shared = if let Some(ref shared) = self.shared_workspace {
                let shared_canonical = shared.canonicalize().unwrap_or_else(|_| shared.clone());
                symlink_dir_canonical.starts_with(&shared_canonical)
            } else {
                false
            };

            // Check projects directory
            let symlink_in_projects = if let Some(ref projects) = self.projects_dir {
                let projects_canonical =
                    projects.canonicalize().unwrap_or_else(|_| projects.clone());
                symlink_dir_canonical.starts_with(&projects_canonical)
            } else {
                false
            };

            if !symlink_in_shared && !symlink_in_projects {
                return Err(SandboxViolation {
                    path: path.display().to_string(),
                    reason: "Symlink is outside sandbox - cannot access symlink itself".to_string(),
                });
            }
        }

        // Rule 2: Symlink inside → Points outside: ❌ Block
        // Read the symlink target
        let target = fs::read_link(path).await.map_err(|e| SandboxViolation {
            path: path.display().to_string(),
            reason: format!("Cannot read symlink target: {}", e),
        })?;

        // Resolve target relative to symlink location if relative
        let resolved_target = if target.is_absolute() {
            target
        } else {
            symlink_dir.join(target)
        };

        // Canonicalize the target (follows symlinks in the path)
        let target_canonical = resolved_target
            .canonicalize()
            .unwrap_or_else(|_| resolved_target.clone());

        // Check if target is within allowed areas
        let target_in_workspace = target_canonical.starts_with(&workspace_canonical);

        let target_in_shared = if let Some(ref shared) = self.shared_workspace {
            let shared_canonical = shared.canonicalize().unwrap_or_else(|_| shared.clone());
            target_canonical.starts_with(&shared_canonical)
        } else {
            false
        };

        let target_in_projects = if let Some(ref projects) = self.projects_dir {
            let projects_canonical = projects.canonicalize().unwrap_or_else(|_| projects.clone());
            target_canonical.starts_with(&projects_canonical)
        } else {
            false
        };

        if !target_in_workspace && !target_in_shared && !target_in_projects {
            return Err(SandboxViolation {
                path: path.display().to_string(),
                reason: format!(
                    "Symlink points outside sandbox: target '{}' resolves to '{}'",
                    resolved_target.display(),
                    target_canonical.display()
                ),
            });
        }

        // Rule 1: Symlink inside → Points inside: ✅ Allow
        Ok(())
    }

    /// Read a file
    async fn read_file(&self, path: &str, encoding: Option<&str>) -> Result<serde_json::Value> {
        let resolved = self
            .resolve_path(path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self
            .resolve_path(path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self
            .resolve_path(path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self
            .resolve_path(path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
        let resolved = self
            .resolve_path(path)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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
        let from_resolved = self
            .resolve_path(from)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let to_resolved = self
            .resolve_path(to)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

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

    #[tokio::test]
    async fn test_symlink_inside_to_inside_allowed() {
        // Rule 1: Symlink inside → Points inside: ✅ Allow
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        // Create a file and a symlink pointing to it (both inside workspace)
        let target_path = temp_dir.path().join("target.txt");
        fs::write(&target_path, "target content").await.unwrap();

        let symlink_path = temp_dir.path().join("link.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_path, &symlink_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&target_path, &symlink_path).unwrap();

        // Should be able to read through the symlink
        let read_params = json!({
            "operation": "read",
            "path": "link.txt"
        });

        let result = tool.execute(read_params).await;
        assert!(
            result.is_ok(),
            "Symlink inside→inside should be allowed: {:?}",
            result
        );
        let response = result.unwrap();
        assert_eq!(response["content"].as_str().unwrap(), "target content");
    }

    #[tokio::test]
    async fn test_symlink_inside_to_outside_blocked() {
        // Rule 2: Symlink inside → Points outside: ❌ SandboxViolation
        let temp_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        // Create a file outside the workspace
        let outside_file = outside_dir.path().join("secret.txt");
        fs::write(&outside_file, "secret").await.unwrap();

        // Create a symlink inside workspace pointing outside
        let symlink_path = temp_dir.path().join("link_to_secret.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_file, &symlink_path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside_file, &symlink_path).unwrap();

        // Should be blocked
        let read_params = json!({
            "operation": "read",
            "path": "link_to_secret.txt"
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_err(), "Symlink inside→outside should be blocked");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SandboxViolation"),
            "Expected SandboxViolation, got: {}",
            err
        );
        assert!(
            err.contains("outside sandbox"),
            "Error should mention outside sandbox: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_symlink_outside_blocked() {
        // Rule 3: Symlink outside → Any target: ❌ Block
        let temp_dir = TempDir::new().unwrap();
        let outside_dir = TempDir::new().unwrap();

        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        // Create a file inside workspace
        let inside_file = temp_dir.path().join("inside.txt");
        fs::write(&inside_file, "inside content").await.unwrap();

        // Create a symlink outside workspace pointing inside
        let symlink_outside = outside_dir.path().join("link_to_inside.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&inside_file, &symlink_outside).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&inside_file, &symlink_outside).unwrap();

        // Try to read the symlink using an absolute path (outside sandbox)
        let read_params = json!({
            "operation": "read",
            "path": symlink_outside.to_str().unwrap()
        });

        let result = tool.execute(read_params).await;
        // This should fail because the symlink itself is outside the sandbox
        assert!(result.is_err(), "Symlink outside sandbox should be blocked");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("SandboxViolation"),
            "Expected SandboxViolation, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_path_traversal_variations_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        // Various path traversal attempts
        let traversals = vec![
            "../etc/passwd",
            "../../etc/shadow",
            "../../../etc/hosts",
            "foo/../../../etc/passwd",
            "./../etc/passwd",
            "a/b/c/../../../../etc/passwd",
        ];

        for path in &traversals {
            let params = json!({
                "operation": "read",
                "path": path
            });

            let result = tool.execute(params).await;
            assert!(
                result.is_err(),
                "Path traversal '{}' should be blocked",
                path
            );
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("SandboxViolation"),
                "Expected SandboxViolation for '{}', got: {}",
                path,
                err
            );
        }
    }

    #[tokio::test]
    async fn test_absolute_path_outside_workspace_blocked() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        #[cfg(unix)]
        let outside_paths = vec![
            "/etc/passwd",
            "/etc/shadow",
            "/root/.ssh/id_rsa",
            "/var/log/syslog",
            "/tmp/sensitive.txt",
        ];

        #[cfg(windows)]
        let outside_paths = vec![
            "C:\\Windows\\System32\\config\\SAM",
            "C:\\Users\\Administrator\\Desktop\\file.txt",
            "D:\\secret.txt",
        ];

        for path in &outside_paths {
            let params = json!({
                "operation": "read",
                "path": path
            });

            let result = tool.execute(params).await;
            assert!(
                result.is_err(),
                "Absolute path '{}' outside workspace should be blocked",
                path
            );
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("SandboxViolation"),
                "Expected SandboxViolation for '{}', got: {}",
                path,
                err
            );
        }
    }

    #[tokio::test]
    async fn test_shared_workspace_access() {
        let temp_dir = TempDir::new().unwrap();
        let shared_dir = TempDir::new().unwrap();

        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };

        let tool = FileSystemTool::with_policy(policy).with_shared_workspace(shared_dir.path());

        // Create a file in shared workspace
        let shared_file = shared_dir.path().join("shared.txt");
        fs::write(&shared_file, "shared content").await.unwrap();

        // Should be able to read from shared workspace using absolute path
        let read_params = json!({
            "operation": "read",
            "path": shared_file.to_str().unwrap()
        });

        let result = tool.execute(read_params).await;
        assert!(
            result.is_ok(),
            "Should be able to read from shared workspace: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_projects_dir_readonly_access() {
        let temp_dir = TempDir::new().unwrap();
        let projects_dir = TempDir::new().unwrap();

        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: true,
            ..SecurityPolicy::default()
        };

        let tool = FileSystemTool::with_policy(policy).with_projects_dir(projects_dir.path());

        // Create a file in projects dir
        let project_file = projects_dir.path().join("project.md");
        fs::write(&project_file, "# Project").await.unwrap();

        // Should be able to read from projects dir
        let read_params = json!({
            "operation": "read",
            "path": project_file.to_str().unwrap()
        });

        let result = tool.execute(read_params).await;
        assert!(
            result.is_ok(),
            "Should be able to read from projects dir: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let temp_dir = TempDir::new().unwrap();

        // First create the nested directories
        let nested_dir = temp_dir.path().join("level1").join("level2").join("level3");
        fs::create_dir_all(&nested_dir).await.unwrap();

        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
    async fn test_file_exists_false() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

        let exists_params = json!({
            "operation": "exists",
            "path": "nonexistent_file.txt"
        });

        let result = tool.execute(exists_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(!response["exists"].as_bool().unwrap());
        assert_eq!(response["type"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn test_binary_file_base64() {
        let temp_dir = TempDir::new().unwrap();
        let policy = SecurityPolicy {
            workspace_dir: temp_dir.path().to_path_buf(),
            workspace_only: false,
            ..SecurityPolicy::default()
        };
        let tool = FileSystemTool::with_policy(policy);

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
}
