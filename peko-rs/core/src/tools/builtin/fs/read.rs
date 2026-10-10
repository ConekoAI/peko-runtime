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

/// Lines returned when `limit` is omitted.
pub const DEFAULT_LINE_LIMIT: usize = 2000;

/// Cap on the returned text (and on a binary file's size). Larger reads
/// come back `truncated` with a `next_offset` to continue from.
pub const MAX_CONTENT_BYTES: usize = 256 * 1024;

/// Marker appended to a single line too long for the content budget.
const LINE_CUT_MARKER: &str = "...(line truncated)";

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
        let path_buf = crate::tools::builtin::paths::expand_tilde(path);
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
        let binary_requested = encoding == Some("base64");
        let is_utf8 = !binary_requested && std::str::from_utf8(&content).is_ok();
        if !is_utf8 && content.len() > MAX_CONTENT_BYTES {
            return Err(anyhow::anyhow!(
                "{} is a {}-byte binary file, over Read's {MAX_CONTENT_BYTES}-byte limit for \
                 binary content; inspect it with Bash (e.g. `xxd FILE | head`, `head -c N FILE`)",
                resolved.display(),
                content.len()
            ));
        }
        let (content_str, encoding_used, is_binary) = if binary_requested {
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
        //
        // Output is bounded: at most `limit` lines (DEFAULT_LINE_LIMIT when
        // omitted) and MAX_CONTENT_BYTES of text, cut at a line boundary. A
        // bounded read reports `truncated` and the `next_offset` to pass back.
        let mut truncated = false;
        let (final_content, total_lines, start_line, end_line) =
            if is_binary || encoding_used == "base64" {
                // For binary, line range doesn't apply
                (content_str, None, None, None)
            } else {
                let lines: Vec<&str> = content_str.lines().collect();
                let total = lines.len();

                let start = offset.map_or(0, |o| o.saturating_sub(1));
                let end = (start + limit.unwrap_or(DEFAULT_LINE_LIMIT)).min(total);

                let mut numbered = String::new();
                let mut last = start;
                for (i, line) in lines.get(start..end).unwrap_or(&[]).iter().enumerate() {
                    let entry = format!("{}\t{}", start + i + 1, line);
                    let sep = usize::from(!numbered.is_empty());
                    if numbered.len() + sep + entry.len() > MAX_CONTENT_BYTES {
                        if numbered.is_empty() {
                            // One line alone exceeds the budget: cut it.
                            let budget = MAX_CONTENT_BYTES - LINE_CUT_MARKER.len();
                            numbered = format!(
                                "{}{LINE_CUT_MARKER}",
                                &entry[..floor_char_boundary(&entry, budget)]
                            );
                            last = start + i + 1;
                        }
                        truncated = true;
                        break;
                    }
                    if sep == 1 {
                        numbered.push('\n');
                    }
                    numbered.push_str(&entry);
                    last = start + i + 1;
                }
                truncated |= last < total && limit.is_none();

                (
                    numbered,
                    Some(total),
                    Some(start + 1), // Convert back to 1-indexed
                    Some(last),
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
            obj.insert("truncated".to_string(), truncated.into());
            if truncated {
                obj.insert("next_offset".to_string(), (end + 1).into());
            }
        }

        Ok(result)
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

/// The largest char boundary in `s` at or below `index`.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
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
Maximum number of lines to read. Defaults to 2000.

### encoding (optional)
- "utf8" (default): Read as text, content returned in `cat -n` format
- "base64": Force base64 encoding for binary data

## Output limits

A read returns at most `limit` lines (2000 when omitted) and at most 256 KiB
of text, cut at a line boundary. When lines remain beyond that, the response
has `truncated: true` and `next_offset` — pass it as `offset` to continue.
A single line longer than 256 KiB is cut and ends with `...(line truncated)`.
Binary files over 256 KiB are refused; inspect them with Bash (`xxd`, `head -c`).

## Examples

Read a file (first 2000 lines):
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
                    "description": "Maximum number of lines to read (default 2000). Output is also capped at 256 KiB; a bounded read returns truncated: true and next_offset",
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

    fn lines(n: usize, width: usize) -> String {
        use std::fmt::Write as _;
        (1..=n).fold(String::new(), |mut out, i| {
            let _ = writeln!(out, "{i:0width$}");
            out
        })
    }

    /// Without `limit`, a read stops at DEFAULT_LINE_LIMIT and points at
    /// the next line; continuing from there returns the rest.
    #[tokio::test]
    async fn omitted_limit_returns_the_first_2000_lines_then_pages() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());
        fs::write(temp_dir.path().join("log.txt"), lines(2500, 4))
            .await
            .unwrap();

        let first = tool.execute(json!({"file_path": "log.txt"})).await.unwrap();
        assert_eq!(first["end_line"], 2000);
        assert_eq!(first["truncated"], true);
        assert_eq!(first["next_offset"], 2001);
        assert_eq!(first["total_lines"], 2500);

        let rest = tool
            .execute(json!({"file_path": "log.txt", "offset": 2001}))
            .await
            .unwrap();
        assert_eq!(rest["start_line"], 2001);
        assert_eq!(rest["end_line"], 2500);
        assert_eq!(rest["truncated"], false);
        assert!(rest.get("next_offset").is_none());

        // An explicit smaller limit is the caller's choice, not truncation.
        let some = tool
            .execute(json!({"file_path": "log.txt", "limit": 10}))
            .await
            .unwrap();
        assert_eq!(some["end_line"], 10);
        assert_eq!(some["truncated"], false);
    }

    /// Text is capped at MAX_CONTENT_BYTES at a line boundary; following
    /// `next_offset` returns every line exactly once.
    #[tokio::test]
    async fn byte_cap_pages_through_every_line_once() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());
        fs::write(temp_dir.path().join("wide.txt"), lines(3000, 200))
            .await
            .unwrap();

        let mut offset = 1;
        let mut seen = 0;
        loop {
            let page = tool
                .execute(json!({"file_path": "wide.txt", "offset": offset, "limit": 3000}))
                .await
                .unwrap();
            let content = page["content"].as_str().unwrap();
            assert!(content.len() <= MAX_CONTENT_BYTES, "{}", content.len());
            for (i, line) in content.lines().enumerate() {
                let (number, text) = line.split_once('\t').unwrap();
                assert_eq!(number.parse::<usize>().unwrap(), offset + i);
                assert_eq!(text.len(), 200, "whole lines only");
            }
            seen += content.lines().count();
            if page["truncated"] == false {
                break;
            }
            offset = page["next_offset"].as_u64().unwrap() as usize;
        }
        assert_eq!(seen, 3000);
    }

    #[tokio::test]
    async fn a_line_longer_than_the_cap_is_cut_and_marked() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());
        let long = "é".repeat(MAX_CONTENT_BYTES);
        fs::write(temp_dir.path().join("min.js"), format!("{long}\nnext\n"))
            .await
            .unwrap();

        let page = tool.execute(json!({"file_path": "min.js"})).await.unwrap();
        let content = page["content"].as_str().unwrap();
        assert!(content.len() <= MAX_CONTENT_BYTES);
        assert!(content.ends_with(LINE_CUT_MARKER));
        assert_eq!(page["truncated"], true);
        assert_eq!(page["next_offset"], 2);
    }

    #[tokio::test]
    async fn large_binary_files_are_refused_small_ones_read() {
        let temp_dir = TempDir::new().unwrap();
        let tool = ReadTool::new().with_workspace(temp_dir.path());
        fs::write(
            temp_dir.path().join("big.bin"),
            vec![0xFFu8; MAX_CONTENT_BYTES + 1],
        )
        .await
        .unwrap();
        fs::write(temp_dir.path().join("small.bin"), vec![0xFFu8; 16])
            .await
            .unwrap();

        let error = tool
            .execute(json!({"file_path": "big.bin"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("binary file"), "{error}");
        assert!(error.to_string().contains("Bash"), "{error}");
        let small = tool
            .execute(json!({"file_path": "small.bin"}))
            .await
            .unwrap();
        assert_eq!(small["encoding"], "base64");
    }

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
