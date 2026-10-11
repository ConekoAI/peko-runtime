//! Glob tool - List files matching patterns
//!
//! Granular directory listing with pattern matching support.

use anyhow::Result;
use async_trait::async_trait;
use glob::Pattern as GlobPattern;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use peko_tools_core::Tool;

/// Glob tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobArgs {
    /// Pattern to match (glob syntax, e.g., "src/**/*.rs")
    pub pattern: String,
    /// Working directory for relative patterns; relative paths resolve
    /// against the agent's workspace, absolute paths are used as-is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Include hidden files (starting with .)
    #[serde(default)]
    pub include_hidden: bool,
    /// Include files excluded by .gitignore / .ignore
    #[serde(default)]
    pub include_ignored: bool,
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

    /// Resolve the search directory - expands `~`, then converts relative
    /// paths to absolute using workspace.
    fn resolve_directory(&self, path: Option<&str>) -> PathBuf {
        if let Some(dir) = path {
            let path = crate::tools::builtin::paths::expand_tilde(dir);
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
        path: Option<&str>,
        include_hidden: bool,
        include_ignored: bool,
        include_dirs: bool,
        limit: usize,
    ) -> Result<serde_json::Value> {
        let base_dir = self.resolve_directory(path);

        // A pattern with `**` searches recursively from the directory before
        // its first `**`, matching the rest against each entry's path below
        // that directory (`**/tests/*.py` matches `a/tests/x.py`). Without
        // `**`, the last segment matches names in one directory.
        let (search_root, match_pattern, recursive) = match pattern.find("**") {
            Some(index) => {
                let prefix = pattern[..index].trim_end_matches(['/', '\\']);
                let root = if prefix.is_empty() {
                    base_dir.clone()
                } else {
                    base_dir.join(prefix)
                };
                (root, pattern[index..].to_string(), true)
            }
            None => {
                let pattern_path = PathBuf::from(pattern);
                let parent = pattern_path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
                let file_name = pattern_path
                    .file_name()
                    .map_or_else(|| "*".to_string(), |s| s.to_string_lossy().to_string());
                (base_dir.join(parent), file_name, false)
            }
        };
        let matcher = GlobPattern::new(&match_pattern)
            .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{pattern}': {e}"))?;
        let options = glob::MatchOptions {
            case_sensitive: true,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };

        // Ensure search root exists
        if !search_root.exists() {
            return Err(anyhow::anyhow!(
                "Search directory does not exist: {}",
                search_root.display()
            ));
        }

        let found = super::walk(
            search_root.clone(),
            if recursive { None } else { Some(1) },
            include_hidden,
            include_ignored,
        )
        .await?;

        let mut entries = Vec::new();
        let mut matched = 0usize;
        for found in found {
            let is_dir = found.metadata.is_dir();
            // Patterns match below the search root; results name paths
            // from the base directory, so they can be passed to Read as-is.
            let below_root = found
                .path
                .strip_prefix(&search_root)
                .unwrap_or(&found.path)
                .to_string_lossy()
                .replace('\\', "/");
            let relative_path = found
                .path
                .strip_prefix(&base_dir)
                .unwrap_or(&found.path)
                .to_string_lossy()
                .replace('\\', "/");
            let relative_path = relative_path
                .strip_prefix("./")
                .unwrap_or(&relative_path)
                .to_string();
            let name = found
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let hit = if recursive {
                matcher.matches_with(&below_root, options)
            } else {
                matcher.matches(&name)
            };
            // A directory counts as a match only when directories are
            // returned, so `total_matched` / `truncated` describe the
            // entries the caller can actually get.
            if !hit || (is_dir && !include_dirs) {
                continue;
            }
            matched += 1;
            if entries.len() < limit {
                entries.push(serde_json::json!({
                    "name": name,
                    "path": relative_path,
                    "type": if is_dir { "dir" } else { "file" },
                    "size_bytes": if is_dir { None } else { Some(found.metadata.len()) },
                    "modified_at": found.metadata.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| chrono::DateTime::from_timestamp(d.as_secs() as i64, 0))
                        .map(|dt| dt.map(|d| d.to_rfc3339()).unwrap_or_default()),
                }));
            }
        }

        let truncated = matched > limit;

        Ok(serde_json::json!({
            "pattern": pattern,
            "path": search_root.display().to_string(),
            "entries": entries,
            "total_matched": matched,
            "returned": entries.len(),
            "truncated": truncated,
        }))
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
        r#"Find files (and optionally directories) by glob pattern. Use Grep to search contents and Read to read files.

Patterns: `*` and `?` match within one path segment; `**` matches any number of directories. Without `**`, the pattern lists one directory: `*.toml` (the workspace root), `src/*.rs`. With `**`, the search is recursive from the directory before the first `**`, and the rest matches each path below it: `**/*.rs`, `src/**/*.rs`, `**/tests/*.py`.

`path` sets the directory patterns are relative to (default: the workspace). Like ripgrep, the search skips hidden names (`include_hidden` to include) and anything excluded by `.gitignore` / `.ignore` files (`include_ignored` to include), so build output and dependencies stay out of results.

Returns `{entries, total_matched, returned, truncated}` in path order; each entry has `path` (relative to `path` / the workspace, ready for Read), `name`, `type`, `size_bytes`, and `modified_at`. `limit` defaults to 1000.

Examples:
```json
{"pattern": "src/**/*.rs"}
{"pattern": "**/test_*.py"}
{"pattern": "*.toml", "include_hidden": true}
{"pattern": "**/*.log", "include_ignored": true}
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
                "path": {
                    "type": "string",
                    "description": "Working directory for relative patterns (default: workspace root)"
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Include hidden files (starting with .)",
                    "default": false
                },
                "include_ignored": {
                    "type": "boolean",
                    "description": "Include files excluded by .gitignore / .ignore",
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
            args.path.as_deref(),
            args.include_hidden,
            args.include_ignored,
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
    use tokio::fs;

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

    /// A directory that itself matches a `**` pattern is searched too, and
    /// counts as a match only when directories are returned.
    #[tokio::test]
    async fn test_glob_recurses_into_matching_directories() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join("a/tests"))
            .await
            .unwrap();
        fs::write(temp_dir.path().join("a/tests/test_x.rs"), "")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("test_y.rs"), "")
            .await
            .unwrap();

        let files = tool.execute(json!({"pattern": "**/test*"})).await.unwrap();
        let paths: Vec<_> = files["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap().replace('\\', "/"))
            .collect();
        assert_eq!(paths, ["a/tests/test_x.rs", "test_y.rs"], "{files}");
        assert_eq!(files["total_matched"], 2, "{files}");

        let with_dirs = tool
            .execute(json!({"pattern": "**/test*", "include_dirs": true}))
            .await
            .unwrap();
        assert_eq!(with_dirs["returned"], 3, "{with_dirs}");
        assert_eq!(with_dirs["total_matched"], 3, "{with_dirs}");
    }

    /// Exactly `limit` matches is a complete result, not a truncated one.
    #[tokio::test]
    async fn exactly_limit_matches_is_not_truncated() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());
        for name in ["a.rs", "b.rs"] {
            fs::write(temp_dir.path().join(name), "").await.unwrap();
        }
        let exact = tool
            .execute(json!({"pattern": "*.rs", "limit": 2}))
            .await
            .unwrap();
        assert_eq!(exact["truncated"], false, "{exact}");
        let over = tool
            .execute(json!({"pattern": "*.rs", "limit": 1}))
            .await
            .unwrap();
        assert_eq!(over["truncated"], true, "{over}");
    }

    fn touch(root: &std::path::Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn paths(result: &serde_json::Value) -> Vec<String> {
        result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap().to_string())
            .collect()
    }

    /// After `**`, the pattern matches each path below the search root, so
    /// a directory segment narrows the match; every `**` counts.
    #[tokio::test]
    async fn recursive_patterns_match_paths_below_the_root() {
        let temp_dir = TempDir::new().unwrap();
        for file in [
            "tests/a.py",
            "pkg/tests/b.py",
            "pkg/tests/deep/c.py",
            "pkg/src/d.py",
            "a/b/c/e.rs",
            "a/x/f.rs",
        ] {
            touch(temp_dir.path(), file, "");
        }
        let tool = GlobTool::new().with_workspace(temp_dir.path());
        let result = tool
            .execute(json!({"pattern": "**/tests/*.py"}))
            .await
            .unwrap();
        assert_eq!(paths(&result), ["pkg/tests/b.py", "tests/a.py"]);
        let result = tool
            .execute(json!({"pattern": "a/**/b/**/*.rs"}))
            .await
            .unwrap();
        assert_eq!(
            paths(&result),
            ["a/b/c/e.rs"],
            "paths start at the base directory"
        );
        let result = tool
            .execute(json!({"pattern": "**/b/**/*.rs"}))
            .await
            .unwrap();
        assert_eq!(paths(&result), ["a/b/c/e.rs"]);
        let error = tool
            .execute(json!({"pattern": "**/[.py"}))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("Invalid glob pattern"),
            "{error}"
        );
    }

    /// Files excluded by .gitignore / .ignore stay out of results — in or
    /// out of a git repository — unless `include_ignored` asks for them.
    #[tokio::test]
    async fn ignored_files_are_skipped_unless_requested() {
        let temp_dir = TempDir::new().unwrap();
        touch(temp_dir.path(), ".gitignore", "target/\n*.log\n");
        touch(temp_dir.path(), "sub/.ignore", "generated.rs\n");
        for file in [
            "src/main.rs",
            "target/debug/build.rs",
            "app.log",
            "sub/generated.rs",
            "sub/kept.rs",
        ] {
            touch(temp_dir.path(), file, "");
        }
        let tool = GlobTool::new().with_workspace(temp_dir.path());
        let result = tool.execute(json!({"pattern": "**/*"})).await.unwrap();
        assert_eq!(paths(&result), ["src/main.rs", "sub/kept.rs"]);
        let result = tool
            .execute(json!({"pattern": "**/*", "include_ignored": true}))
            .await
            .unwrap();
        assert_eq!(
            paths(&result),
            [
                "app.log",
                "src/main.rs",
                "sub/generated.rs",
                "sub/kept.rs",
                "target/debug/build.rs"
            ]
        );
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

    #[tokio::test]
    async fn test_glob_path_param_replaces_directory() {
        // The `path` parameter is the renamed `directory` field; it
        // should accept the same value, drive the search, and the
        // response should report a `path` field (not `directory`).
        let temp_dir = TempDir::new().unwrap();
        let sub = temp_dir.path().join("nested");
        tokio::fs::create_dir_all(&sub).await.unwrap();
        tokio::fs::write(sub.join("hit.txt"), "").await.unwrap();

        let tool = GlobTool::new().with_workspace(temp_dir.path());
        let result = tool
            .execute(json!({"pattern": "*.txt", "path": "nested"}))
            .await
            .unwrap();
        let path_str = result["path"].as_str().unwrap();
        let sub_str = sub.display().to_string();
        assert!(
            path_str.starts_with(&sub_str),
            "path '{path_str}' should be rooted at '{sub_str}'"
        );
        assert!(
            result.get("directory").is_none(),
            "old `directory` field should be gone"
        );
    }

    #[tokio::test]
    async fn test_glob_include_dirs_adds_directory_entries() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GlobTool::new().with_workspace(temp_dir.path());
        fs::create_dir_all(temp_dir.path().join("sub"))
            .await
            .unwrap();
        fs::write(temp_dir.path().join("a.txt"), "").await.unwrap();

        let kinds = |result: serde_json::Value| -> Vec<(String, String)> {
            let mut kinds: Vec<_> = result["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    (
                        e["name"].as_str().unwrap().to_string(),
                        e["type"].as_str().unwrap().to_string(),
                    )
                })
                .collect();
            kinds.sort();
            kinds
        };
        let files_only = tool.execute(json!({"pattern": "*"})).await.unwrap();
        assert_eq!(kinds(files_only), vec![("a.txt".into(), "file".into())]);
        let with_dirs = tool
            .execute(json!({"pattern": "*", "include_dirs": true}))
            .await
            .unwrap();
        assert_eq!(
            kinds(with_dirs),
            vec![
                ("a.txt".into(), "file".into()),
                ("sub".into(), "dir".into())
            ]
        );
    }
}
