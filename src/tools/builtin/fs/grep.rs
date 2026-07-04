//! Grep tool - Search file contents using regex
//!
//! Content-based file discovery and search for agents. The body of
//! every response is a plain-text `output` string in ripgrep-style
//! formatting, with shape driven by `output_mode`:
//! - `"content"` (default) — `path:line:content\n` per match and per
//!   context line, matching Claude Code's Grep
//! - `"files_with_matches"` — one path per line for files that
//!   contain at least one match
//! - `"count"` — `path:count\n` per file

use anyhow::{Context, Result};
use async_trait::async_trait;
use glob::Pattern as GlobPattern;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::PathBuf;
use tokio::fs;

use crate::tools::core::Tool;

/// Grep tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepArgs {
    /// Regex pattern to search for
    pub pattern: String,
    /// Path to search (file or directory, default: workspace)
    #[serde(default)]
    pub path: Option<String>,
    /// Glob pattern to filter files (e.g., "*.rs"). Named `include`
    /// to match Claude Code's Grep; same semantics — files whose
    /// basename does not match are skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<String>,
    /// Maximum number of matches to return (default: 100)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Include line content in results (default: true). Only consulted
    /// when `output_mode == "content"`; ignored for `files_with_matches`
    /// and `count` modes.
    #[serde(default = "default_true")]
    pub include_content: bool,
    /// Number of context lines before each match (default: 0). Only
    /// consulted when `output_mode == "content"`.
    #[serde(default)]
    pub context_before: usize,
    /// Number of context lines after each match (default: 0). Only
    /// consulted when `output_mode == "content"`.
    #[serde(default)]
    pub context_after: usize,
    /// Number of context lines to show before AND after each match
    /// (shortcut for `context_before` + `context_after` to the same
    /// value). When set, takes precedence over the separate
    /// `context_before` / `context_after` parameters. Only consulted
    /// when `output_mode == "content"`. Mirrors Claude Code's `-C N`.
    #[serde(default)]
    pub context: Option<usize>,
    /// Case insensitive search (default: false)
    #[serde(default)]
    pub case_insensitive: bool,
    /// Include hidden files (default: false)
    #[serde(default)]
    pub include_hidden: bool,
    /// Output shape: `"content"` (default) returns per-match records
    /// with line numbers and (optionally) the matching line + context;
    /// `"files_with_matches"` returns just the unique file paths that
    /// contain at least one match; `"count"` returns a `{path: count}`
    /// map of match counts per file.
    #[serde(default = "default_output_mode")]
    pub output_mode: String,
}

fn default_output_mode() -> String {
    "content".to_string()
}

fn default_limit() -> usize {
    100
}

fn default_true() -> bool {
    true
}

/// Match result for a single occurrence
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub path: String,
    pub line: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_before: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_after: Option<Vec<String>>,
}

/// Grep tool - Search file contents
pub struct GrepTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl GrepTool {
    /// Create a new Grep tool
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
        let path_buf = crate::common::paths::expand_tilde(path);
        if path_buf.is_absolute() {
            path_buf
        } else if let Some(ref workspace) = self.workspace_dir {
            workspace.join(path_buf)
        } else {
            path_buf
        }
    }

    /// Execute grep search
    async fn grep(
        &self,
        pattern: &str,
        path: Option<&str>,
        include: Option<&str>,
        limit: usize,
        include_content: bool,
        context_before: usize,
        context_after: usize,
        context: Option<usize>,
        case_insensitive: bool,
        include_hidden: bool,
        output_mode: &str,
    ) -> Result<serde_json::Value> {
        if !matches!(output_mode, "content" | "files_with_matches" | "count") {
            return Err(anyhow::anyhow!(
                "Invalid output_mode '{output_mode}': expected 'content', 'files_with_matches', or 'count'"
            ));
        }

        // The combined `context` parameter (Claude Code's `-C N`) wins
        // when set; otherwise honor the separate `context_before` /
        // `context_after` overrides.
        let (effective_before, effective_after) = match context {
            Some(n) => (n, n),
            None => (context_before, context_after),
        };
        // Compile regex
        let regex = if case_insensitive {
            Regex::new(&format!("(?i){pattern}"))
        } else {
            Regex::new(pattern)
        }
        .map_err(|e| anyhow::anyhow!("Invalid regex pattern: {e}"))?;

        // Determine search path
        let search_path = path.map_or_else(
            || {
                self.workspace_dir
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("."))
            },
            |p| self.resolve_path(p),
        );

        // Collect matches
        let mut matches = Vec::new();
        let mut files_searched = 0usize;
        let mut files_with_matches = 0usize;

        if search_path.is_file() {
            // Search single file
            files_searched = 1;
            let file_matches = self
                .search_file(
                    &search_path,
                    &regex,
                    limit,
                    include_content,
                    effective_before,
                    effective_after,
                )
                .await?;
            if !file_matches.is_empty() {
                files_with_matches = 1;
                matches.extend(file_matches);
            }
        } else if search_path.is_dir() {
            // Search directory
            self.search_directory(
                &search_path,
                &regex,
                include,
                limit,
                include_content,
                effective_before,
                effective_after,
                include_hidden,
                &mut matches,
                &mut files_searched,
                &mut files_with_matches,
            )
            .await?;
        } else {
            return Err(anyhow::anyhow!(
                "Path does not exist: {}",
                search_path.display()
            ));
        }

        // Sort matches by path then line number
        matches.sort_by(|a, b| {
            let a_path = a["path"].as_str().unwrap_or("");
            let b_path = b["path"].as_str().unwrap_or("");
            let path_cmp = a_path.cmp(b_path);
            if path_cmp == std::cmp::Ordering::Equal {
                let a_line = a["line"].as_u64().unwrap_or(0);
                let b_line = b["line"].as_u64().unwrap_or(0);
                a_line.cmp(&b_line)
            } else {
                path_cmp
            }
        });

        let truncated = matches.len() >= limit;

        let mut response = serde_json::json!({
            "pattern": pattern,
            "path": search_path.display().to_string(),
            "files_searched": files_searched,
            "files_with_matches": files_with_matches,
            "truncated": truncated,
            "output_mode": output_mode,
        });

        let obj = response.as_object_mut().unwrap();

        // The body of the response is a plain-text `output` string in
        // ripgrep-style formatting — `path:line:content` for content
        // mode, one path per line for files_with_matches, and
        // `path:count` per line for count mode. The JSON envelope
        // keeps the metadata (files_searched, files_with_matches,
        // truncated, output_mode) so callers can drive downstream
        // behavior without re-parsing the output.
        let output = match output_mode {
            "content" => format_content_output(&matches),
            "files_with_matches" => {
                let mut files: Vec<String> = matches
                    .iter()
                    .filter_map(|m| m.get("path").and_then(|p| p.as_str()).map(String::from))
                    .collect();
                files.sort();
                files.dedup();
                files.join("\n") + if files.is_empty() { "" } else { "\n" }
            }
            "count" => {
                let mut counts: std::collections::BTreeMap<String, usize> =
                    std::collections::BTreeMap::new();
                for m in &matches {
                    if let Some(p) = m.get("path").and_then(|p| p.as_str()) {
                        *counts.entry(p.to_string()).or_insert(0) += 1;
                    }
                }
                counts.iter().fold(String::new(), |mut acc, (p, c)| {
                    let _ = writeln!(acc, "{p}:{c}");
                    acc
                })
            }
            _ => unreachable!("validated above"),
        };

        obj.insert("output".to_string(), output.into());

        Ok(response)
    }

    /// Search a single file
    async fn search_file(
        &self,
        path: &PathBuf,
        regex: &Regex,
        limit: usize,
        include_content: bool,
        context_before: usize,
        context_after: usize,
    ) -> Result<Vec<serde_json::Value>> {
        let mut matches = Vec::new();

        // Read file content
        let content = match fs::read_to_string(path).await {
            Ok(c) => c,
            Err(_) => {
                // Skip binary or unreadable files silently
                return Ok(matches);
            }
        };

        let lines: Vec<&str> = content.lines().collect();

        for (line_num, line) in lines.iter().enumerate() {
            if regex.is_match(line) {
                let mut match_obj = serde_json::json!({
                    "path": path.display().to_string(),
                    "line": line_num + 1, // 1-indexed
                });

                if include_content {
                    match_obj["content"] = line.to_string().into();
                }

                // Add context lines
                if context_before > 0 {
                    let start = line_num.saturating_sub(context_before);
                    let before_ctx: Vec<String> = lines[start..line_num]
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect();
                    match_obj["context_before"] = before_ctx.into();
                }

                if context_after > 0 {
                    let end = (line_num + context_after + 1).min(lines.len());
                    let after_ctx: Vec<String> = lines[line_num + 1..end]
                        .iter()
                        .map(std::string::ToString::to_string)
                        .collect();
                    match_obj["context_after"] = after_ctx.into();
                }

                matches.push(match_obj);

                if matches.len() >= limit {
                    break;
                }
            }
        }

        Ok(matches)
    }

    /// Recursively search directory
    #[allow(clippy::too_many_arguments)]
    async fn search_directory(
        &self,
        dir: &PathBuf,
        regex: &Regex,
        include: Option<&str>,
        limit: usize,
        include_content: bool,
        context_before: usize,
        context_after: usize,
        include_hidden: bool,
        matches: &mut Vec<serde_json::Value>,
        files_searched: &mut usize,
        files_with_matches: &mut usize,
    ) -> Result<()> {
        let mut entries = fs::read_dir(dir)
            .await
            .with_context(|| format!("Failed to read directory: {}", dir.display()))?;

        while let Some(entry) = entries.next_entry().await? {
            // Check if we've hit the limit
            if matches.len() >= limit {
                return Ok(());
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip hidden files unless requested
            if !include_hidden && name_str.starts_with('.') {
                continue;
            }

            let path = entry.path();
            let metadata = entry.metadata().await?;

            if metadata.is_dir() {
                // Recurse into subdirectories
                Box::pin(self.search_directory(
                    &path,
                    regex,
                    include,
                    limit,
                    include_content,
                    context_before,
                    context_after,
                    include_hidden,
                    matches,
                    files_searched,
                    files_with_matches,
                ))
                .await?;
            } else if metadata.is_file() {
                // Check include filter if provided
                if let Some(pattern) = include {
                    if !Self::simple_glob_match(&name_str, pattern) {
                        continue;
                    }
                }

                *files_searched += 1;
                let file_matches = self
                    .search_file(
                        &path,
                        regex,
                        limit - matches.len(),
                        include_content,
                        context_before,
                        context_after,
                    )
                    .await?;

                if !file_matches.is_empty() {
                    *files_with_matches += 1;
                    matches.extend(file_matches);
                }
            }
        }

        Ok(())
    }

    /// Simple glob matching for file filtering
    fn simple_glob_match(name: &str, pattern: &str) -> bool {
        GlobPattern::new(pattern).is_ok_and(|p: GlobPattern| p.matches(name))
    }
}

/// Format the `content` mode output as a ripgrep-style plain-text
/// string. Each match is emitted as `path:line:content`, with
/// surrounding context lines (when `context_before` / `context_after`
/// are set) interleaved in the right order and with the right line
/// numbers (computed from the match line minus/plus the context
/// position).
///
/// Paths are emitted with `/` separators regardless of platform so the
/// output is stable and easy for callers (and LLMs) to parse.
fn format_content_output(matches: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for m in matches {
        let path = match m.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => continue,
        };
        let path = path.replace('\\', "/");
        let line = m.get("line").and_then(|l| l.as_u64()).unwrap_or(0);
        if line == 0 {
            continue;
        }
        if let Some(before) = m.get("context_before").and_then(|v| v.as_array()) {
            let count = before.len() as u64;
            for (i, content) in before.iter().enumerate() {
                let ln = line.saturating_sub(count).saturating_add(i as u64);
                let s = content.as_str().unwrap_or("");
                out.push_str(&format!("{path}:{ln}:{s}\n"));
            }
        }
        let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
        out.push_str(&format!("{path}:{line}:{content}\n"));
        if let Some(after) = m.get("context_after").and_then(|v| v.as_array()) {
            for (i, content) in after.iter().enumerate() {
                let ln = line.saturating_add(1).saturating_add(i as u64);
                let s = content.as_str().unwrap_or("");
                out.push_str(&format!("{path}:{ln}:{s}\n"));
            }
        }
    }
    out
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "Grep"
    }

    fn description(&self) -> String {
        r#"## Purpose
Search file contents using regular expressions. Finds files by their content.

Use when: Finding files containing specific text, searching for patterns across files, locating function definitions.
Don't use when: You need to list files by name (use Glob) or read file contents (use ReadFile).

## Parameters

### pattern (required)
Regex pattern to search for. Examples:
- `"fn main"` - Find main function definitions
- `"TODO|FIXME"` - Find TODO or FIXME comments
- `"^pub fn"` - Find public function definitions
- `"struct \w+"` - Find struct declarations

### path (optional)
File or directory to search. Defaults to workspace root.

### include (optional)
Glob pattern to filter files (e.g., "*.rs"). Named `include` to match
Claude Code's Grep; same semantics.

### limit (optional)
Maximum number of matches. Default: 100.

### include_content (optional)
Include the matching line content. Default: true.

### context_before (optional)
Number of context lines before each match. Default: 0.

### context_after (optional)
Number of context lines after each match. Default: 0.

### context (optional)
Number of context lines to show before AND after each match. When
set, takes precedence over `context_before` / `context_after`. Useful
for `context: 3` style "show me the surrounding code" requests.

### case_insensitive (optional)
Case-insensitive search. Default: false.

### include_hidden (optional)
Search hidden files. Default: false.

### output_mode (optional)
Shape of the response. The body is always a single `output` field —
a plain-text string in ripgrep-style formatting:
- `"content"` (default) — one line per match and per context line,
  formatted as `path:line:content\n`. With `context_before` /
  `context_after` (or the combined `context`), surrounding lines are
  interleaved in the right order and at the right line numbers.
- `"files_with_matches"` — one path per line for files that contain
  at least one match. Cheap when you only need to know *which* files.
- `"count"` — one `path:count\n` line per file.

JSON metadata (`files_searched`, `files_with_matches`, `truncated`,
`output_mode`, `pattern`, `path`) is always returned alongside
`output`.

## Examples

Find function definitions:
```json
{"pattern": "^pub fn ", "include": "*.rs"}
```

Search for TODO comments:
```json
{"pattern": "TODO|FIXME", "case_insensitive": true}
```

Find where a function is called:
```json
{"pattern": "my_function\\(", "context_before": 1, "context_after": 1}
```"#
        .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search (default: workspace root)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g., '*.rs')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches",
                    "default": 100,
                    "minimum": 1,
                    "maximum": 1000
                },
                "include_content": {
                    "type": "boolean",
                    "description": "Include the matching line content",
                    "default": true
                },
                "context_before": {
                    "type": "integer",
                    "description": "Number of context lines before each match",
                    "default": 0,
                    "minimum": 0,
                    "maximum": 10
                },
                "context_after": {
                    "type": "integer",
                    "description": "Number of context lines after each match",
                    "default": 0,
                    "minimum": 0,
                    "maximum": 10
                },
                "context": {
                    "type": "integer",
                    "description": "Number of context lines to show before AND after each match (shortcut for context_before + context_after). When set, takes precedence over the separate context_before/context_after parameters. Mirrors Claude Code's -C N.",
                    "minimum": 0,
                    "maximum": 10
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Case-insensitive search",
                    "default": false
                },
                "include_hidden": {
                    "type": "boolean",
                    "description": "Search hidden files",
                    "default": false
                },
                "output_mode": {
                    "type": "string",
                    "description": "Output shape: 'content' (default, per-match records with line numbers), 'files_with_matches' (unique file paths only), or 'count' (per-file match counts).",
                    "enum": ["content", "files_with_matches", "count"],
                    "default": "content"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: GrepArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {e}"))?;

        self.grep(
            &args.pattern,
            args.path.as_deref(),
            args.include.as_deref(),
            args.limit,
            args.include_content,
            args.context_before,
            args.context_after,
            args.context,
            args.case_insensitive,
            args.include_hidden,
            &args.output_mode,
        )
        .await
    }

    fn estimated_duration_ms(&self, params: &serde_json::Value) -> u64 {
        // Estimate based on search scope
        if let Some(path) = params.get("path").and_then(|p| p.as_str()) {
            if PathBuf::from(path).is_file() {
                return 100; // Single file
            }
        }
        500 // Directory search
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    /// Parse a ripgrep-style `output` string into `(path, line, content)`
    /// tuples. Skips blank lines.
    ///
    /// Handles Windows absolute paths (`C:/...` or `C:\...`) so the
    /// drive-letter colon is not mistaken for the field separator.
    fn parse_ripgrep_output(s: &str) -> Vec<(String, u64, String)> {
        fn split_one_line(l: &str) -> (String, u64, String) {
            // Strip a leading Windows drive letter (e.g. `C:`) before
            // looking for the path/line separator, then put it back.
            let (prefix, rest) =
                if l.len() >= 2 && l.as_bytes()[1] == b':' && l.as_bytes()[0].is_ascii_alphabetic()
                {
                    (&l[..2], if l.len() == 2 { "" } else { &l[2..] })
                } else {
                    ("", l)
                };

            if let Some(sep) = rest.find(':') {
                let path = format!("{}{}", prefix, &rest[..sep]);
                let rest = &rest[sep + 1..];
                if let Some(sep2) = rest.find(':') {
                    let line = rest[..sep2].parse().unwrap_or(0);
                    let content = rest[sep2 + 1..].to_string();
                    return (path, line, content);
                }
            }
            (l.to_string(), 0, String::new())
        }

        s.lines()
            .filter(|l| !l.is_empty())
            .map(split_one_line)
            .collect()
    }

    #[tokio::test]
    async fn test_grep_single_file() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(
            temp_dir.path().join("test.txt"),
            "Hello, World!\nHello, Rust!\nGoodbye!",
        )
        .await
        .unwrap();

        let params = json!({
            "pattern": "Hello",
            "path": "test.txt"
        });
        let result = tool.execute(params).await.unwrap();

        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 2);
        assert!(entries[0].0.ends_with("test.txt"));
        assert_eq!(entries[0].1, 1);
        assert_eq!(entries[0].2, "Hello, World!");
        assert_eq!(entries[1].1, 2);
    }

    #[tokio::test]
    async fn test_grep_directory() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        // Create test files
        fs::write(temp_dir.path().join("file1.rs"), "fn main() {}")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("file2.rs"), "fn helper() {}")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("file.txt"), "fn not searched")
            .await
            .unwrap();

        let params = json!({
            "pattern": "fn ",
            "include": "*.rs"
        });
        let result = tool.execute(params).await.unwrap();

        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 2);
        assert_eq!(result["files_searched"], 2);
        assert_eq!(result["files_with_matches"], 2);
    }

    #[tokio::test]
    async fn test_grep_case_insensitive() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "HELLO\nhello\nHello")
            .await
            .unwrap();

        // Case sensitive (default)
        let params = json!({
            "pattern": "hello",
            "path": "test.txt"
        });
        let result = tool.execute(params).await.unwrap();
        assert_eq!(
            parse_ripgrep_output(result["output"].as_str().unwrap()).len(),
            1
        );

        // Case insensitive
        let params = json!({
            "pattern": "hello",
            "path": "test.txt",
            "case_insensitive": true
        });
        let result = tool.execute(params).await.unwrap();
        assert_eq!(
            parse_ripgrep_output(result["output"].as_str().unwrap()).len(),
            3
        );
    }

    #[tokio::test]
    async fn test_grep_with_context() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(
            temp_dir.path().join("test.txt"),
            "line1\nline2\nline3\nline4\nline5",
        )
        .await
        .unwrap();

        let params = json!({
            "pattern": "line3",
            "path": "test.txt",
            "context_before": 1,
            "context_after": 1
        });
        let result = tool.execute(params).await.unwrap();

        // Output is 3 ripgrep lines: the context_before line, the
        // match line, and the context_after line.
        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].1, 2);
        assert_eq!(entries[0].2, "line2");
        assert_eq!(entries[1].1, 3);
        assert_eq!(entries[1].2, "line3");
        assert_eq!(entries[2].1, 4);
        assert_eq!(entries[2].2, "line4");
    }

    #[tokio::test]
    async fn test_grep_regex() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(
            temp_dir.path().join("test.txt"),
            "fn foo()\nfn bar()\nstruct Baz",
        )
        .await
        .unwrap();

        let params = json!({
            "pattern": "^fn ",
            "path": "test.txt"
        });
        let result = tool.execute(params).await.unwrap();

        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_grep_limit() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        // Create file with many matches
        let content: String = (0..100).fold(String::new(), |mut acc, i| {
            use std::fmt::Write;
            let _ = writeln!(acc, "line{i}");
            acc
        });
        fs::write(temp_dir.path().join("test.txt"), content)
            .await
            .unwrap();

        let params = json!({
            "pattern": "line",
            "path": "test.txt",
            "limit": 10
        });
        let result = tool.execute(params).await.unwrap();

        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 10);
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn test_grep_invalid_regex() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        let params = json!({"pattern": "[invalid"});
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid regex"));
    }

    #[tokio::test]
    async fn test_grep_context_param_sets_both_sides() {
        // The combined `context` parameter mirrors Claude Code's `-C N`:
        // it sets both before and after to the same value. When
        // `context` is set, it takes precedence over the separate
        // `context_before` / `context_after` fields.
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(
            temp_dir.path().join("test.txt"),
            "line1\nline2\nTARGET\nline4\nline5",
        )
        .await
        .unwrap();

        let result = tool
            .execute(json!({
                "pattern": "TARGET",
                "path": "test.txt",
                "context": 1
            }))
            .await
            .unwrap();

        // Output should be 3 ripgrep lines: line2 (before), TARGET
        // (match), line4 (after).
        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].1, 2);
        assert_eq!(entries[0].2, "line2");
        assert_eq!(entries[1].1, 3);
        assert_eq!(entries[1].2, "TARGET");
        assert_eq!(entries[2].1, 4);
        assert_eq!(entries[2].2, "line4");
    }

    #[tokio::test]
    async fn test_grep_context_param_overrides_separate() {
        // When `context` is set, the separate `context_before` /
        // `context_after` values must be ignored.
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "a\nb\nTARGET\nd\ne")
            .await
            .unwrap();

        let result = tool
            .execute(json!({
                "pattern": "TARGET",
                "path": "test.txt",
                "context": 1,
                "context_before": 5,
                "context_after": 5,
            }))
            .await
            .unwrap();

        let entries = parse_ripgrep_output(result["output"].as_str().unwrap());
        assert_eq!(entries.len(), 3);
        // 'b' and 'd' — one line of context each side, not five.
        assert_eq!(entries[0].2, "b");
        assert_eq!(entries[2].2, "d");
    }

    #[test]
    fn test_simple_glob_match() {
        assert!(GrepTool::simple_glob_match("test.rs", "*.rs"));
        assert!(GrepTool::simple_glob_match("file.txt", "*.txt"));
        assert!(!GrepTool::simple_glob_match("test.rs", "*.txt"));
        assert!(GrepTool::simple_glob_match("any", "*"));
        assert!(GrepTool::simple_glob_match("file1.txt", "file?.txt"));
        assert!(!GrepTool::simple_glob_match("file10.txt", "file?.txt"));
        assert!(GrepTool::simple_glob_match("test.rs", "**/*.rs"));
    }

    #[tokio::test]
    async fn test_grep_output_mode_files_with_matches() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("a.rs"), "fn alpha() {}\nfn beta() {}")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("b.rs"), "fn gamma() {}")
            .await
            .unwrap();
        fs::write(temp_dir.path().join("c.txt"), "no match here")
            .await
            .unwrap();

        let result = tool
            .execute(json!({
                "pattern": "fn ",
                "include": "*.rs",
                "output_mode": "files_with_matches"
            }))
            .await
            .unwrap();

        let out = result["output"].as_str().unwrap();
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(result["files_with_matches"], 2);
        // No line-level fields when in files_with_matches mode.
        assert!(result.get("matches").is_none());
        assert!(result.get("files").is_none());
    }

    #[tokio::test]
    async fn test_grep_output_mode_count() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());

        fs::write(
            temp_dir.path().join("a.rs"),
            "fn a() {}\nfn b() {}\nfn c() {}",
        )
        .await
        .unwrap();
        fs::write(temp_dir.path().join("b.rs"), "fn d() {}")
            .await
            .unwrap();

        let result = tool
            .execute(json!({
                "pattern": "fn ",
                "include": "*.rs",
                "output_mode": "count"
            }))
            .await
            .unwrap();

        // Output is two lines, each `path:count\n`.
        let out = result["output"].as_str().unwrap();
        let entries: Vec<(String, u64)> = out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (p, c) = l.rsplit_once(':').unwrap();
                (p.to_string(), c.parse().unwrap())
            })
            .collect();
        assert_eq!(entries.len(), 2);
        let a = entries.iter().find(|(p, _)| p.ends_with("a.rs")).unwrap();
        let b = entries.iter().find(|(p, _)| p.ends_with("b.rs")).unwrap();
        assert_eq!(a.1, 3);
        assert_eq!(b.1, 1);
        assert!(result.get("counts").is_none());
    }

    #[tokio::test]
    async fn test_grep_output_mode_invalid() {
        let temp_dir = TempDir::new().unwrap();
        let tool = GrepTool::new().with_workspace(temp_dir.path());
        let result = tool
            .execute(json!({"pattern": "x", "output_mode": "bogus"}))
            .await;
        assert!(result.is_err());
    }
}
