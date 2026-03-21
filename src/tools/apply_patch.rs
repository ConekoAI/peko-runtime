//! Apply patch tool with hunks-based matching and atomic rollback
//!
//! Implements CAPABILITY_INTERFACE.md §3.3
//! - Hunks-based matching with context_before
//! - Atomic operations with rollback on failure
//! - Automatic backup creation
//! - All four operations: create, modify, delete, move

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info};

use crate::tools::traits::Tool;

/// Apply patch tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyPatchConfig {
    /// Enable the tool
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Restrict to workspace only
    #[serde(default = "default_true")]
    pub workspace_only: bool,
    /// Allow file deletion
    #[serde(default = "default_false")]
    pub allow_deletion: bool,
    /// Create backup files (.bak)
    #[serde(default = "default_true")]
    pub create_backups: bool,
    /// Maximum file size (bytes)
    #[serde(default = "default_max_size")]
    pub max_file_size: usize,
    /// Maximum number of operations per call
    #[serde(default = "default_max_operations")]
    pub max_operations: usize,
}

impl Default for ApplyPatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            workspace_only: true,
            allow_deletion: false,
            create_backups: true,
            max_file_size: 10 * 1024 * 1024, // 10MB
            max_operations: 50,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

fn default_max_size() -> usize {
    10 * 1024 * 1024
}

fn default_max_operations() -> usize {
    50
}

/// Single hunk for modify operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hunk {
    /// Context lines immediately before the change
    pub context_before: String,
    /// Old content to match
    pub old: String,
    /// New content to replace with
    pub new: String,
}

/// Single file operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Create a new file
    Create { path: String, content: String },
    /// Modify an existing file using hunks
    Modify { path: String, hunks: Vec<Hunk> },
    /// Delete a file
    Delete { path: String },
    /// Move/rename a file
    Move { path: String, to: String },
}

/// Apply patch arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyPatchArgs {
    /// List of operations to apply
    pub operations: Vec<Operation>,
    /// Preview changes without applying (dry run)
    #[serde(default)]
    pub dry_run: bool,
    /// Working directory for relative paths
    #[serde(default)]
    pub working_dir: Option<String>,
}

/// Result of a single operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub op: String,
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of applying a patch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchResult {
    pub applied: bool,
    pub operations: Vec<OperationResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Backup entry for rollback
#[derive(Debug, Clone)]
struct BackupEntry {
    path: PathBuf,
    backup_path: Option<PathBuf>,
    original_content: Option<Vec<u8>>,
}

/// Apply patch tool
pub struct ApplyPatchTool {
    config: ApplyPatchConfig,
    workspace_root: PathBuf,
}

impl ApplyPatchTool {
    /// Create a new apply patch tool
    pub fn new(config: ApplyPatchConfig, workspace_root: impl AsRef<Path>) -> Self {
        Self {
            config,
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Resolve a path relative to workspace
    fn resolve_path(&self, path: &str, working_dir: Option<&str>) -> anyhow::Result<PathBuf> {
        // Block obvious path traversal attempts at parse time
        if path.contains("..") || path.contains("~") {
            return Err(anyhow::anyhow!(
                "SandboxViolation: Path '{}' contains invalid characters",
                path
            ));
        }

        let path = PathBuf::from(path);

        // If absolute, check workspace restriction
        if path.is_absolute() {
            if self.config.workspace_only {
                let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                let workspace_canonical = self.workspace_root.canonicalize()?;

                if !canonical.starts_with(&workspace_canonical) {
                    return Err(anyhow::anyhow!(
                        "SandboxViolation: Path {} is outside workspace ({})",
                        path.display(),
                        self.workspace_root.display()
                    ));
                }
            }
            return Ok(path);
        }

        // Relative path - resolve against working_dir or workspace
        let base = working_dir
            .map(PathBuf::from)
            .map(|p| {
                if p.is_absolute() {
                    p
                } else {
                    self.workspace_root.join(p)
                }
            })
            .unwrap_or_else(|| self.workspace_root.clone());

        let resolved = base.join(&path);

        // Security check: ensure resolved path stays within workspace
        if self.config.workspace_only {
            // For consistency, use non-canonicalized paths
            // On Windows, canonicalize() adds UNC prefix (\\?\) which causes
            // mismatches when comparing with non-canonicalized paths from TempDir
            let workspace_str = self.workspace_root.to_string_lossy();
            let resolved_str = resolved.to_string_lossy();

            if !resolved_str.starts_with(&*workspace_str) {
                return Err(anyhow::anyhow!(
                    "SandboxViolation: Resolved path {} escapes workspace ({})",
                    resolved.display(),
                    self.workspace_root.display()
                ));
            }
        }

        Ok(resolved)
    }

    /// Create backup of file
    fn create_backup(&self, path: &Path) -> anyhow::Result<Option<PathBuf>> {
        if !self.config.create_backups || !path.exists() {
            return Ok(None);
        }

        let backup_path = path.with_extension(format!(
            "bak.{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        fs::copy(path, &backup_path)?;
        debug!(
            "Created backup: {} -> {}",
            path.display(),
            backup_path.display()
        );

        Ok(Some(backup_path))
    }

    /// Check if file size is within limits
    fn check_file_size(&self, path: &Path) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }

        let metadata = fs::metadata(path)?;
        if metadata.len() > self.config.max_file_size as u64 {
            return Err(anyhow::anyhow!(
                "File {} exceeds max size ({} > {} bytes)",
                path.display(),
                metadata.len(),
                self.config.max_file_size
            ));
        }

        Ok(())
    }

    /// Apply hunks to file content
    fn apply_hunks(&self, content: &str, hunks: &[Hunk]) -> anyhow::Result<String> {
        let mut result = content.to_string();

        for (i, hunk) in hunks.iter().enumerate() {
            // Build the search pattern from context + old content
            let search_pattern = format!("{}{}", hunk.context_before, hunk.old);

            // Try to find the pattern in the current result
            if let Some(pos) = result.find(&search_pattern) {
                // Found it - replace the old content with new
                // Keep everything before the context
                let before = &result[..pos];
                // Keep everything after the old content
                let after_pos = pos + search_pattern.len();
                let after = &result[after_pos..];

                // Reconstruct: before + context + new + after
                result = format!("{}{}{}{}", before, hunk.context_before, hunk.new, after);
            } else {
                return Err(anyhow::anyhow!(
                    "Hunk {}: Could not find matching context. Expected pattern '{}' not found.",
                    i + 1,
                    search_pattern
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(50)
                        .collect::<String>()
                ));
            }
        }

        Ok(result)
    }

    /// Execute a single operation and return backup info for rollback
    fn execute_operation(
        &self,
        op: &Operation,
        working_dir: Option<&str>,
    ) -> anyhow::Result<(OperationResult, BackupEntry)> {
        match op {
            Operation::Create { path, content } => {
                let resolved = self.resolve_path(path, working_dir)?;

                if resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "Cannot create file: {} already exists",
                        resolved.display()
                    ));
                }

                // Create parent directories
                if let Some(parent) = resolved.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Write file
                fs::write(&resolved, content)?;

                let result = OperationResult {
                    op: "create".to_string(),
                    path: path.clone(),
                    status: "ok".to_string(),
                    backup_path: None,
                    error: None,
                };

                let backup = BackupEntry {
                    path: resolved,
                    backup_path: None,
                    original_content: None,
                };

                Ok((result, backup))
            }

            Operation::Modify { path, hunks } => {
                let resolved = self.resolve_path(path, working_dir)?;

                if !resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "Cannot modify file: {} does not exist",
                        resolved.display()
                    ));
                }

                self.check_file_size(&resolved)?;

                // Read current content
                let original_content = fs::read_to_string(&resolved)?;

                // Create backup
                let backup_path = self.create_backup(&resolved)?;

                // Apply hunks
                let new_content = self.apply_hunks(&original_content, hunks)?;

                // Write modified content
                fs::write(&resolved, new_content)?;

                let result = OperationResult {
                    op: "modify".to_string(),
                    path: path.clone(),
                    status: "ok".to_string(),
                    backup_path: backup_path.as_ref().map(|p| p.display().to_string()),
                    error: None,
                };

                let backup = BackupEntry {
                    path: resolved,
                    backup_path,
                    original_content: Some(original_content.into_bytes()),
                };

                Ok((result, backup))
            }

            Operation::Delete { path } => {
                let resolved = self.resolve_path(path, working_dir)?;

                if !resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "Cannot delete file: {} does not exist",
                        resolved.display()
                    ));
                }

                if !self.config.allow_deletion {
                    return Err(anyhow::anyhow!(
                        "File deletion not allowed (enable allow_deletion config)"
                    ));
                }

                // Create backup before deletion
                let backup_path = self.create_backup(&resolved)?;

                // Store original content for rollback
                let original_content = fs::read(&resolved).ok();

                // Delete file
                fs::remove_file(&resolved)?;

                let result = OperationResult {
                    op: "delete".to_string(),
                    path: path.clone(),
                    status: "ok".to_string(),
                    backup_path: backup_path.as_ref().map(|p| p.display().to_string()),
                    error: None,
                };

                let backup = BackupEntry {
                    path: resolved,
                    backup_path,
                    original_content,
                };

                Ok((result, backup))
            }

            Operation::Move { path, to } => {
                let from_resolved = self.resolve_path(path, working_dir)?;
                let to_resolved = self.resolve_path(to, working_dir)?;

                if !from_resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "Cannot move file: {} does not exist",
                        from_resolved.display()
                    ));
                }

                if to_resolved.exists() {
                    return Err(anyhow::anyhow!(
                        "Cannot move file: destination {} already exists",
                        to_resolved.display()
                    ));
                }

                // Create backup of source file
                let backup_path = self.create_backup(&from_resolved)?;

                // Store original state for rollback
                let original_content = fs::read(&from_resolved).ok();

                // Create parent directories for destination
                if let Some(parent) = to_resolved.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Move file
                fs::rename(&from_resolved, &to_resolved)?;

                let result = OperationResult {
                    op: "move".to_string(),
                    path: path.clone(),
                    status: "ok".to_string(),
                    backup_path: backup_path.as_ref().map(|p| p.display().to_string()),
                    error: None,
                };

                let backup = BackupEntry {
                    path: from_resolved,
                    backup_path,
                    original_content,
                };

                Ok((result, backup))
            }
        }
    }

    /// Rollback operations using backup info
    fn rollback(&self, backups: &[BackupEntry]) {
        for backup in backups.iter().rev() {
            debug!("Rolling back: {}", backup.path.display());

            // Restore from backup file if it exists
            if let Some(ref backup_path) = backup.backup_path {
                if backup_path.exists() {
                    if let Err(e) = fs::copy(backup_path, &backup.path) {
                        error!("Failed to restore backup: {}", e);
                    }
                    // Clean up backup file
                    let _ = fs::remove_file(backup_path);
                }
            } else if let Some(ref content) = backup.original_content {
                // Restore from stored content
                if let Err(e) = fs::write(&backup.path, content) {
                    error!("Failed to restore content: {}", e);
                }
            } else {
                // File was created, delete it
                if backup.path.exists() {
                    if let Err(e) = fs::remove_file(&backup.path) {
                        error!("Failed to remove created file: {}", e);
                    }
                }
            }
        }
    }

    /// Preview changes without applying
    fn preview_changes(
        &self,
        operations: &[Operation],
        _working_dir: Option<&str>,
    ) -> anyhow::Result<Vec<(String, String, String)>> {
        let mut preview = Vec::new();

        for op in operations {
            let (op_type, path) = match op {
                Operation::Create { path, .. } => ("create".to_string(), path.clone()),
                Operation::Modify { path, .. } => ("modify".to_string(), path.clone()),
                Operation::Delete { path } => ("delete".to_string(), path.clone()),
                Operation::Move { path, to } => ("move".to_string(), format!("{} -> {}", path, to)),
            };

            preview.push((op_type, path, "would apply".to_string()));
        }

        Ok(preview)
    }
}

/// Normalize whitespace for matching
fn normalize_whitespace(s: &str) -> String {
    s.lines()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Apply structured patches to files atomically with automatic backup"
    }

    fn llm_description(&self) -> String {
        r#"## Purpose
Apply structured patches to files atomically with automatic backup.

Prefer `apply_patch` over `filesystem.write` for modifying existing files. It is safer (atomic, backed up), more auditable, and cheaper in context (send a diff, not the full file).

## Operations

### create
Create a new file. Fails if file exists.
```json
{"op": "create", "path": "src/utils.rs", "content": "pub fn helper() {}"}
```

### modify
Apply hunks to an existing file using context matching.
```json
{
  "op": "modify",
  "path": "src/main.rs",
  "hunks": [
    {
      "context_before": "fn main() {",
      "old": "    println!(\"hello\");",
      "new": "    println!(\"hello, world!\");"
    }
  ]
}
```

### delete
Delete a file.
```json
{"op": "delete", "path": "src/old.rs"}
```

### move
Move or rename a file.
```json
{"op": "move", "path": "draft.md", "to": "final.md"}
```

## Hunk Matching
Each hunk is matched by `context_before` + `old`. If no match, entire patch is rejected and no files are modified.

## Atomicity
If any operation fails, all are rolled back (backups restored).

## Safety
- Automatic backups created before modifications
- Sandbox enforcement - paths must be within workspace
- File size limits enforced"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operations": {
                    "type": "array",
                    "description": "List of file operations to apply",
                    "items": {
                        "type": "object",
                        "oneOf": [
                            {
                                "type": "object",
                                "properties": {
                                    "op": { "enum": ["create"] },
                                    "path": { "type": "string" },
                                    "content": { "type": "string" }
                                },
                                "required": ["op", "path", "content"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": { "enum": ["modify"] },
                                    "path": { "type": "string" },
                                    "hunks": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "context_before": { "type": "string" },
                                                "old": { "type": "string" },
                                                "new": { "type": "string" }
                                            },
                                            "required": ["context_before", "old", "new"]
                                        }
                                    }
                                },
                                "required": ["op", "path", "hunks"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": { "enum": ["delete"] },
                                    "path": { "type": "string" }
                                },
                                "required": ["op", "path"]
                            },
                            {
                                "type": "object",
                                "properties": {
                                    "op": { "enum": ["move"] },
                                    "path": { "type": "string" },
                                    "to": { "type": "string" }
                                },
                                "required": ["op", "path", "to"]
                            }
                        ]
                    }
                },
                "dry_run": {
                    "type": "boolean",
                    "description": "Preview changes without applying",
                    "default": false
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for relative paths"
                }
            },
            "required": ["operations"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let args: ApplyPatchArgs = serde_json::from_value(args)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        // Validate operations
        if args.operations.is_empty() {
            return Err(anyhow::anyhow!("No operations provided"));
        }

        if args.operations.len() > self.config.max_operations {
            return Err(anyhow::anyhow!(
                "Too many operations ({} > {})",
                args.operations.len(),
                self.config.max_operations
            ));
        }

        // Dry run - preview only
        if args.dry_run {
            let preview = self.preview_changes(&args.operations, args.working_dir.as_deref())?;

            let operations: Vec<OperationResult> = preview
                .into_iter()
                .map(|(op, path, status)| OperationResult {
                    op,
                    path,
                    status,
                    backup_path: None,
                    error: None,
                })
                .collect();

            return Ok(serde_json::json!({
                "applied": false,
                "dry_run": true,
                "operations": operations
            }));
        }

        // Apply operations with rollback on failure
        let mut results = Vec::new();
        let mut backups = Vec::new();
        let mut success_count = 0;
        let mut failed_op: Option<(String, String)> = None;

        for (i, op) in args.operations.iter().enumerate() {
            match self.execute_operation(op, args.working_dir.as_deref()) {
                Ok((result, backup)) => {
                    results.push(result);
                    backups.push(backup);
                    success_count += 1;
                }
                Err(e) => {
                    // Record failure
                    let (op_type, path) = match op {
                        Operation::Create { path, .. } => ("create", path),
                        Operation::Modify { path, .. } => ("modify", path),
                        Operation::Delete { path } => ("delete", path),
                        Operation::Move { path, .. } => ("move", path),
                    };

                    results.push(OperationResult {
                        op: op_type.to_string(),
                        path: path.clone(),
                        status: "failed".to_string(),
                        backup_path: None,
                        error: Some(e.to_string()),
                    });

                    failed_op = Some((op_type.to_string(), path.clone()));

                    // Rollback previous operations
                    error!(
                        "Operation {} failed ({} on {}), rolling back {} previous operations",
                        i + 1,
                        op_type,
                        path,
                        success_count
                    );
                    self.rollback(&backups);

                    break;
                }
            }
        }

        let all_succeeded = failed_op.is_none();

        // Clean up backup files on success
        if all_succeeded {
            for backup in &backups {
                if let Some(ref backup_path) = backup.backup_path {
                    let _ = fs::remove_file(backup_path);
                }
            }
        }

        info!(
            "Applied {} operations, {} failed",
            success_count,
            args.operations.len() - success_count
        );

        let result = PatchResult {
            applied: all_succeeded,
            operations: results,
            error: failed_op.map(|(op, path)| format!("Operation {} on {} failed", op, path)),
        };

        if !all_succeeded {
            return Err(anyhow::anyhow!(
                "Patch failed and was rolled back. Check operation results for details."
            ));
        }

        Ok(serde_json::to_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_tool(temp: &TempDir) -> ApplyPatchTool {
        let config = ApplyPatchConfig {
            allow_deletion: true,
            ..Default::default()
        };
        ApplyPatchTool::new(config, temp.path())
    }

    #[test]
    fn test_resolve_path_relative() {
        let temp = TempDir::new().unwrap();
        let tool = create_tool(&temp);

        let resolved = tool.resolve_path("src/main.rs", None).unwrap();
        assert_eq!(resolved, temp.path().join("src/main.rs"));
    }

    #[test]
    fn test_workspace_only_blocks_escape() {
        let temp = TempDir::new().unwrap();
        let subdir = temp.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let config = ApplyPatchConfig {
            workspace_only: true,
            ..Default::default()
        };
        let tool = ApplyPatchTool::new(config, &subdir);

        // Try to escape workspace with .. traversal
        let result = tool.resolve_path("../outside.txt", None);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("SandboxViolation"));
    }

    #[tokio::test]
    async fn test_create_new_file() {
        let temp = TempDir::new().unwrap();
        let tool = create_tool(&temp);

        let args = serde_json::json!({
            "operations": [
                {
                    "op": "create",
                    "path": "new_file.txt",
                    "content": "Hello, World!"
                }
            ]
        });

        let result = tool.execute(args).await;
        assert!(result.is_ok());

        let content = fs::read_to_string(temp.path().join("new_file.txt")).unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_modify_with_hunks() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("existing.txt");
        fs::write(&file_path, "line1\nline2\nline3\n").unwrap();

        let tool = create_tool(&temp);

        let args = serde_json::json!({
            "operations": [
                {
                    "op": "modify",
                    "path": "existing.txt",
                    "hunks": [
                        {
                            "context_before": "line1\n",
                            "old": "line2",
                            "new": "modified_line2"
                        }
                    ]
                }
            ]
        });

        let result = tool.execute(args).await;
        assert!(result.is_ok(), "Modify failed: {:?}", result);

        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("modified_line2"));
    }

    #[tokio::test]
    async fn test_atomic_rollback() {
        let temp = TempDir::new().unwrap();

        // Create two files
        fs::write(temp.path().join("file1.txt"), "original1").unwrap();
        fs::write(temp.path().join("file2.txt"), "original2").unwrap();

        let tool = create_tool(&temp);

        // Try to modify both, but second will fail (no matching context)
        let args = serde_json::json!({
            "operations": [
                {
                    "op": "modify",
                    "path": "file1.txt",
                    "hunks": [
                        {
                            "context_before": "",
                            "old": "original1",
                            "new": "modified1"
                        }
                    ]
                },
                {
                    "op": "modify",
                    "path": "file2.txt",
                    "hunks": [
                        {
                            "context_before": "nonexistent context",
                            "old": "old",
                            "new": "new"
                        }
                    ]
                }
            ]
        });

        let result = tool.execute(args).await;
        assert!(result.is_err());

        // Verify first file was rolled back
        let content1 = fs::read_to_string(temp.path().join("file1.txt")).unwrap();
        assert_eq!(content1, "original1");
    }

    #[tokio::test]
    async fn test_move_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("old.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = create_tool(&temp);

        let args = serde_json::json!({
            "operations": [
                {
                    "op": "move",
                    "path": "old.txt",
                    "to": "new.txt"
                }
            ]
        });

        let result = tool.execute(args).await;
        assert!(result.is_ok(), "Move failed: {:?}", result);

        assert!(!file_path.exists());
        assert!(temp.path().join("new.txt").exists());
        assert_eq!(
            fs::read_to_string(temp.path().join("new.txt")).unwrap(),
            "content"
        );
    }

    #[tokio::test]
    async fn test_delete_file() {
        let temp = TempDir::new().unwrap();
        let file_path = temp.path().join("to_delete.txt");
        fs::write(&file_path, "content").unwrap();

        let tool = create_tool(&temp);

        let args = serde_json::json!({
            "operations": [
                {
                    "op": "delete",
                    "path": "to_delete.txt"
                }
            ]
        });

        let result = tool.execute(args).await;
        assert!(result.is_ok());

        assert!(!file_path.exists());
    }

    #[tokio::test]
    async fn test_dry_run() {
        let temp = TempDir::new().unwrap();
        let tool = create_tool(&temp);

        let args = serde_json::json!({
            "operations": [
                {
                    "op": "create",
                    "path": "should_not_create.txt",
                    "content": "content"
                }
            ],
            "dry_run": true
        });

        let result = tool.execute(args).await;
        assert!(result.is_ok());

        let response = result.unwrap();
        assert_eq!(response["applied"].as_bool().unwrap(), false);
        assert_eq!(response["dry_run"].as_bool().unwrap(), true);

        // Verify file was NOT created
        assert!(!temp.path().join("should_not_create.txt").exists());
    }
}
