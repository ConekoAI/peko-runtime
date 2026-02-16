//! File system tool for file operations

use async_trait::async_trait;
use serde_json::json;
use anyhow::{Context, Result};
use std::path::Path;
use tokio::fs;

use crate::tools::Tool;

/// File system tool for reading and writing files
pub struct FileSystemTool;

impl FileSystemTool {
    /// Create a new file system tool
    pub fn new() -> Self {
        Self
    }

    /// Read a file
    async fn read_file(path: &str) -> Result<serde_json::Value> {
        let path = Path::new(path);
        
        // Security check: prevent path traversal
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow::anyhow!("Path traversal not allowed"));
        }

        let content = fs::read_to_string(path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        Ok(json!({
            "content": content,
            "path": path.display().to_string(),
            "size": content.len(),
            "success": true,
        }))
    }

    /// Write a file
    async fn write_file(path: &str, content: &str) -> Result<serde_json::Value> {
        let path = Path::new(path);
        
        // Security check: prevent path traversal
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow::anyhow!("Path traversal not allowed"));
        }

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directories: {}", parent.display()))?;
        }

        fs::write(path, content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        Ok(json!({
            "path": path.display().to_string(),
            "bytes_written": content.len(),
            "success": true,
        }))
    }

    /// List directory contents
    async fn list_dir(path: &str) -> Result<serde_json::Value> {
        let path = Path::new(path);
        
        // Security check: prevent path traversal
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow::anyhow!("Path traversal not allowed"));
        }

        let mut entries = Vec::new();
        let mut dir = fs::read_dir(path)
            .await
            .with_context(|| format!("Failed to read directory: {}", path.display()))?;

        while let Some(entry) = dir.next_entry().await? {
            let metadata = entry.metadata().await?;
            let file_type = if metadata.is_file() {
                "file"
            } else if metadata.is_dir() {
                "directory"
            } else if metadata.is_symlink() {
                "symlink"
            } else {
                "other"
            };

            entries.push(json!({
                "name": entry.file_name().to_string_lossy().to_string(),
                "type": file_type,
                "size": if metadata.is_file() { Some(metadata.len()) } else { None },
                "modified": metadata.modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            }));
        }

        Ok(json!({
            "path": path.display().to_string(),
            "entries": entries,
            "success": true,
        }))
    }

    /// Check if a file exists
    async fn file_exists(path: &str) -> Result<serde_json::Value> {
        let path = Path::new(path);
        
        // Security check: prevent path traversal
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow::anyhow!("Path traversal not allowed"));
        }

        let exists = fs::try_exists(path).await.unwrap_or(false);
        let metadata = if exists {
            fs::metadata(path).await.ok().map(|m| json!({
                "is_file": m.is_file(),
                "is_dir": m.is_dir(),
                "size": m.len(),
            }))
        } else {
            None
        };

        Ok(json!({
            "path": path.display().to_string(),
            "exists": exists,
            "metadata": metadata,
            "success": true,
        }))
    }

    /// Delete a file
    async fn delete_file(path: &str) -> Result<serde_json::Value> {
        let path = Path::new(path);
        
        // Security check: prevent path traversal
        if path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(anyhow::anyhow!("Path traversal not allowed"));
        }

        let metadata = fs::metadata(path).await;
        
        match metadata {
            Ok(m) if m.is_file() => {
                fs::remove_file(path)
                    .await
                    .with_context(|| format!("Failed to delete file: {}", path.display()))?;
            }
            Ok(m) if m.is_dir() => {
                fs::remove_dir_all(path)
                    .await
                    .with_context(|| format!("Failed to delete directory: {}", path.display()))?;
            }
            Ok(_) => return Err(anyhow::anyhow!("Unknown file type: {}", path.display())),
            Err(e) => return Err(anyhow::anyhow!("File not found: {} - {}", path.display(), e)),
        }

        Ok(json!({
            "path": path.display().to_string(),
            "success": true,
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
    fn name(&self) -> &str {
        "filesystem"
    }

    fn description(&self) -> &str {
        "File system operations. Actions: read, write, list, exists, delete. Parameters: {\"action\": \"read|write|list|exists|delete\", \"path\": string, \"content\": string (for write)}"
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: action"))?;

        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: path"))?;

        match action {
            "read" => Self::read_file(path).await,
            "write" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing required parameter: content for write action"))?;
                Self::write_file(path, content).await
            }
            "list" => Self::list_dir(path).await,
            "exists" => Self::file_exists(path).await,
            "delete" => Self::delete_file(path).await,
            _ => Err(anyhow::anyhow!("Unknown action: {}. Use read, write, list, exists, or delete", action)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_filesystem_tool_creation() {
        let tool = FileSystemTool::new();
        assert_eq!(tool.name(), "filesystem");
        assert!(!tool.description().is_empty());
    }

    #[tokio::test]
    async fn test_filesystem_write_and_read() {
        let tool = FileSystemTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");

        // Write a file
        let write_params = json!({
            "action": "write",
            "path": test_file.to_str().unwrap(),
            "content": "Hello, World!"
        });

        let result = tool.execute(write_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.get("success").unwrap().as_bool().unwrap());

        // Read the file
        let read_params = json!({
            "action": "read",
            "path": test_file.to_str().unwrap()
        });

        let result = tool.execute(read_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.get("content").unwrap().as_str().unwrap(), "Hello, World!");
    }

    #[tokio::test]
    async fn test_filesystem_exists() {
        let tool = FileSystemTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");

        // Check non-existent file
        let exists_params = json!({
            "action": "exists",
            "path": test_file.to_str().unwrap()
        });

        let result = tool.execute(exists_params.clone()).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(!response.get("exists").unwrap().as_bool().unwrap());

        // Create the file
        fs::write(&test_file, "test").await.unwrap();

        // Check existing file
        let result = tool.execute(exists_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        assert!(response.get("exists").unwrap().as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_filesystem_list() {
        let tool = FileSystemTool::new();
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a test file
        fs::write(temp_dir.path().join("test.txt"), "test").await.unwrap();

        let list_params = json!({
            "action": "list",
            "path": temp_dir.path().to_str().unwrap()
        });

        let result = tool.execute(list_params).await;
        assert!(result.is_ok());
        let response = result.unwrap();
        let entries = response.get("entries").unwrap().as_array().unwrap();
        assert!(!entries.is_empty());
    }

    #[tokio::test]
    async fn test_filesystem_delete() {
        let tool = FileSystemTool::new();
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");

        // Create the file
        fs::write(&test_file, "test").await.unwrap();
        assert!(test_file.exists());

        // Delete the file
        let delete_params = json!({
            "action": "delete",
            "path": test_file.to_str().unwrap()
        });

        let result = tool.execute(delete_params).await;
        assert!(result.is_ok());
        assert!(!test_file.exists());
    }

    #[tokio::test]
    async fn test_filesystem_path_traversal_protection() {
        let tool = FileSystemTool::new();

        let params = json!({
            "action": "read",
            "path": "../etc/passwd"
        });

        let result = tool.execute(params).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Path traversal"));
    }
}
