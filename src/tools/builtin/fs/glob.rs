//! Glob tool - List files matching patterns
//!
//! Granular directory listing with pattern matching support.

use anyhow::{Context, Result};
use async_trait::async_trait;
use glob::Pattern as GlobPattern;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

use crate::tools::core::Tool;

/// Glob tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobArgs {
    /// Pattern to match (glob syntax, e.g., "src/**/*.rs")
    pub pattern: String,
    /// Working directory for relative patterns
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    /// Include hidden files (starting with .)
    #[serde(default)]
    pub include_hidden: bool,
    /// Include directories in results
    #[serde(default)]
    pub include_dirs: bool,
    /// Limit number of results (default: 1000)
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    1000
}

/// Glob tool - Find files matching patterns
pub struct GlobTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl GlobTool {
    /// Create a new Glob tool
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

    /// Resolve the search directory
    fn resolve_directory(&self, directory: Option<&str>) -> PathBuf {
        if let Some(dir) = directory {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                path
            } else if let Some(ref workspace) = self.workspace_dir {
                workspace.join(path)
            } else {
                path
            }
        } else if let Some(ref workspace) = self.workspace_dir {
            workspace.clone()
        } else {
            PathBuf::from(".")
        }
    }

    /// Execute glob pattern matching
    async fn glob(
        &self,
        pattern: &str,
        directory: Option<&str>,
        include_hidden: bool,
        include_dirs: bool,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let base_dir = self.resolve_directory(directory);

        // Parse pattern - handle "**" specially
        let (search_root, glob_pattern) = if pattern.contains("**") {
            // For patterns with **, find the directory before **
            let parts: Vec<&str> = pattern.split("**").collect();
            let prefix = parts[0];
            let rest = if parts.len() > 1 { parts[1] } else { "" };

            // Get the directory part before **
            let root_part = if prefix.is_empty() {
                base_dir.clone()
            } else {
                let prefix_path = PathBuf::from(prefix.trim_end_matches(['/', '\\']));
                base_dir.join(prefix_path)
            };

            // The pattern to match is ** followed by the rest
            let match_pattern = if rest.is_empty() || rest == "/" || rest == "\\" {
                "**".to_string()
            } else {
                format!("**{rest}")
            };

            (root_part, match_pattern)
        } else {
            // Simple pattern without **
            let pattern_path = PathBuf::from(pattern);
            let parent = pattern_path
                .parent()
                .map(|p| {
                    if p.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        p.to_path_buf()
                    }
                })
                .unwrap_or_else(|| PathBuf::from("."));
            let file_name = pattern_path
                .file_name()
                .map_or_else(|| "*".to_string(), |s| s.to_string_lossy().to_string());
            (base_dir.join(parent), file_name)
        };

        // Ensure search root exists
        if !search_root.exists() {
            return Err(anyhow::anyhow!(
                "Search directory does not exist: {}",
                search_root.display()
            ));
        }

        // Collect matching entries
        let mut entries = Vec::new();
        let mut matched = 0usize;

        self.collect_matches(
            &search_root,
            &search_root,
            &glob_pattern,
            include_hidden,
            include_dirs,
            limit,
            &mut entries,
            &mut matched,
        )
        .await?;

        // Sort entries by path for consistent output
        entries.sort_by(|a, b| {
            a["path"]
                .as_str()
                .unwrap_or("")
                .cmp(b["path"].as_str().unwrap_or(""))
        });

        let truncated = matched > limit;

        Ok(serde_json::json!({
            "pattern": pattern,
            "directory": search_root.display().to_string(),
            "entries": entries,
            "total_matched": matched,
            "returned": entries.len(),
            "truncated": truncated,
        }))
    }

    /// Recursively collect matching entries
    async fn collect_matches(
        &self,
        search_root: &PathBuf,
        current_dir: &PathBuf,
        pattern: &str,
        include_hidden: bool,
        include_dirs: bool,
        limit: usize,
        entries: &mut Vec<serde_json::Value>,
        matched: &mut usize,
    ) -> Result<()> {
        if entries.len() >= limit {
            return Ok(());
        }

        let mut dir = fs::read_dir(current_dir)
            .await
            .with_context(|| format!("Failed to read directory: {}", current_dir.display()))?;

        while let Some(entry) = dir.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip hidden files unless requested
            if !include_hidden && name_str.starts_with('.') {
                continue;
            }

            let entry_path = entry.path();
            let metadata = entry.metadata().await?;
            let is_dir = metadata.is_dir();

            // Check if this entry matches the pattern
            let is_match = Self::glob_match(&name_str, pattern);

            if is_match {
                *matched += 1;
                if entries.len() < limit {
                    let relative_path = entry_path
                        .strip_prefix(search_root)
                        .unwrap_or(&entry_path)
                        .to_string_lossy()
                        .to_string();

                    let entry_json = serde_json::json!({
                        "name": name_str.to_string(),
                        "path": relative_path,
                        "type": if is_dir { "dir" } else { "file" },
                        "size_bytes": if is_dir { None } else { Some(metadata.len()) },
                        "modified_at": metadata.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
                            .map(|dt| dt.map(|d| d.to_rfc3339()).unwrap_or_default()),
                    });

                    if is_dir {
                        if include_dirs {
                            entries.push(entry_json);
                        }
                        // Recurse into subdirectories for patterns like "**/*.rs"
                        if pattern.starts_with("**") {
                            Box::pin(self.collect_matches(
                                search_root,
                                &entry_path,
                                pattern,
                                include_hidden,
                                include_dirs,
                                limit,
                                entries,
                                matched,
                            ))
                            .await?;
                        }
                    } else {
                        entries.push(entry_json);
                    }
                }
            } else if is_dir {
                // Recurse into subdirectories for patterns like "**/*.rs"
                if pattern.starts_with("**") {
                    Box::pin(self.collect_matches(
                        search_root,
                        &entry_path,
                        pattern,
                        include_hidden,
                        include_dirs,
                        limit,
                        entries,
                        matched,
                    ))
                    .await?;
                }
            }
        }

        Ok(())
    }

    /// Simple glob pattern matching
    fn glob_match(name: &str, pattern: &str) -> bool {
        // Handle **/ prefix (recursive match)
        if let Some(rest) = pattern.strip_prefix("**/") {
            // Match any path depth
            return Self::simple_glob_match(name, rest);
        }

        Self::simple_glob_match(name, pattern)
    }

    /// Simple glob matching using the glob crate
    fn simple_glob_match(name: &str, pattern: &str) -> bool {
        GlobPattern::new(pattern).is_ok_and(|p: GlobPattern| p.matches(name))
    }
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &'static str {
        "Glob"
    }

    fn description(&self) -> String {
        r#"## Purpose
Find files and directories matching a glob pattern.

Use when: Exploring project structure, finding files by pattern, listing directory contents.
Don't use when: You need file contents (use ReadFile) or content search (use Grep).

## Glob Pattern Syntax

- `*` - Match any sequence of characters (except path separator)
- `?` - Match any single character
- `**` - Match any sequence of directories recursively

## Parameters

### pattern (required)
Glob pattern to match. Examples:
- `*.rs` - All Rust files in current directory
- `src/**/*.rs` - All Rust files under src/ recursively
- `*.toml` - All TOML files
- `test_*.py` - Python test files

### directory (optional)
Working directory for relative patterns. Defaults to workspace root.

### include_hidden (optional)
Include hidden files (starting with .). Default: false.

### include_dirs (optional)
Include directories in results. Default: false.

### limit (optional)
Maximum number of results. Default: 1000.

## Examples

List all Rust files:
```json
{"pattern": "src/**/*.rs"}
```

List configuration files including hidden:
```json
{"pattern": "*.toml", "include_hidden": true}
```

Find test files:
```json
{"pattern": "**/test_*.py"}
```"#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g., 'src/**/*.rs')"
                },
                "directory": {
                    "type": "string",
                    "description": "Working directory for relative patterns"
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (starting with .)",
                    "default": false
                },
                "include_dirs": {
                    "type": "boolean",
                    "description": "Include directories in results",
                    "default": false
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results",
                    "default": 1000,
                    "minimum": 1,
                    "maximum": 10000
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: GlobArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.glob(
            &args.pattern,
            args.directory.as_deref(),
            args.include_hidden,
            args.include_dirs,
            args.limit,
        )
        .await
    }

    fn estimated_duration_ms(&self, params: &serde_json::Value) -> u64 {
        // Estimate based on whether pattern includes recursion
        if let Some(pattern) = params.get("pattern").and_then(|p| p.as_str()) {
            if pattern.contains("**") {
                return 500; // Recursive search
            }
        }
        100 // Simple pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_glob_simple_pattern() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());

        // Create test files
        fs::write(temp_dir.path().join("file1.rs"), "")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("file2.rs"), "")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("file.txt"), "")
            .await
            .unwrap();

        let params = json!({"pattern": "*.rs"});
        let result = tool.execute(params).await.unwrap();

        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);

        let names: Vec<&str> = entries
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"file1.rs"));
        assert!(names.contains(&"file2.rs"));
    }

    #[tokio::test]
    async fn test_glob_recursive_pattern() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());

        // Create nested files
        fs::create_dir_all(temp_dir.path().join("src/components"))
            .await
            .unwrap();
        fs::write(temp_dir.path().join("src/main.rs"), "")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("src/lib.rs"), "")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("src/components/mod.rs"), "")
            .await
            .unwrap();

        let params = json!({"pattern": "src/**/*.rs"});
        let result = tool.execute(params).await.unwrap();

        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[tokio::test]
    async fn test_glob_include_hidden() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());

        // Create hidden and normal files
        fs::write(temp_dir.path().join("visible.txt"), "")
            .await
            .unwrap();
        #[cfg(unix)]
        fs::write(temp_dir.path().join(".hidden"), "")
            .await
            .unwrap();

        // Without include_hidden
        let params = json!({"pattern": "*"});
        let result = tool.execute(params).await.unwrap();
        let entries = result["entries"].as_array().unwrap();

        // On Unix: 1 file (excluding .hidden), on Windows: depends
        if cfg!(unix) {
            assert_eq!(entries.len(), 1);
        }

        // With include_hidden
        #[cfg(unix)]
        {
            let params = json!({"pattern": "*", "include_hidden": true});
            let result = tool.execute(params).await.unwrap();
            let entries = result["entries"].as_array().unwrap();
            assert_eq!(entries.len(), 2);
        }
    }

    #[tokio::test]
    async fn test_glob_limit() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());

        // Create many files
        for i in 0..10 {
            fs::write(temp_dir.path().join(format!("file{i}.txt")), "")
                .await
                .unwrap();
        }

        let params = json!({"pattern": "*.txt", "limit": 5});
        let result = tool.execute(params).await.unwrap();

        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(result["total_matched"], 10);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn test_glob_nonexistent_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());

        let params = json!({"pattern": "nonexistent/*"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
    }

    #[test]
    fn test_glob_match_simple() {
        assert!(GlobTool::simple_glob_match("test.rs", "*.rs"));
        assert!(GlobTool::simple_glob_match("file.txt", "*.txt"));
        assert!(!GlobTool::simple_glob_match("test.rs", "*.txt"));
    }

    #[test]
    fn test_glob_match_question() {
        assert!(GlobTool::simple_glob_match("test.rs", "tes?.rs"));
        assert!(GlobTool::simple_glob_match("file1.txt", "file?.txt"));
        assert!(!GlobTool::simple_glob_match("file10.txt", "file?.txt"));
    }

    #[test]
    fn test_glob_match_star() {
        assert!(GlobTool::simple_glob_match("test.rs", "*.rs"));
        assert!(GlobTool::simple_glob_match("my_test_file.rs", "*.rs"));
        assert!(GlobTool::simple_glob_match("test.rs", "test.*"));
    }
}
