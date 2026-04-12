//! StrReplaceFile tool - Targeted string replacement in files
//!
//! Granular file modification with exact matching. Safer than full file rewrites.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;

use crate::tools::Tool;

/// Single replacement operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replacement {
    /// The exact text to find (must match completely)
    pub old: String,
    /// The text to replace with
    pub new: String,
}

/// StrReplaceFile tool arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrReplaceFileArgs {
    /// Path to the file
    pub path: String,
    /// Single replacement or list of replacements
    #[serde(with = "serde_replacement")]
    pub edit: ReplacementArg,
}

/// Helper module for flexible replacement serialization
mod serde_replacement {
    use super::{Replacement, ReplacementArg};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &ReplacementArg, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            ReplacementArg::Single(r) => r.serialize(serializer),
            ReplacementArg::Multiple(r) => r.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<ReplacementArg, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Try to deserialize as Vec<Replacement> first
        let value: serde_json::Value = Deserialize::deserialize(deserializer)?;
        
        if let Ok(replacements) = serde_json::from_value::<Vec<Replacement>>(value.clone()) {
            Ok(ReplacementArg::Multiple(replacements))
        } else if let Ok(replacement) = serde_json::from_value::<Replacement>(value) {
            Ok(ReplacementArg::Single(replacement))
        } else {
            Err(serde::de::Error::custom("expected replacement or array of replacements"))
        }
    }
}

/// Single or multiple replacements
#[derive(Debug, Clone)]
pub enum ReplacementArg {
    Single(Replacement),
    Multiple(Vec<Replacement>),
}

impl ReplacementArg {
    fn into_vec(self) -> Vec<Replacement> {
        match self {
            ReplacementArg::Single(r) => vec![r],
            ReplacementArg::Multiple(r) => r,
        }
    }
}

/// Result of a single replacement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplacementResult {
    pub old: String,
    pub new: String,
    pub occurrences: usize,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// StrReplaceFile tool - Targeted string replacement
pub struct StrReplaceFileTool {
    /// Default workspace directory (for relative paths)
    workspace_dir: Option<PathBuf>,
}

impl StrReplaceFileTool {
    /// Create a new StrReplaceFile tool
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

    /// Apply string replacements to a file
    async fn replace_in_file(
        &self,
        path: &str,
        replacements: Vec<Replacement>,
    ) -> Result<serde_json::Value> {
        let resolved = self.resolve_path(path);

        // Check file exists
        if !resolved.exists() {
            return Err(anyhow::anyhow!(
                "File does not exist: {}",
                resolved.display()
            ));
        }

        // Read current content
        let content = fs::read_to_string(&resolved)
            .await
            .with_context(|| format!("Failed to read file: {}", resolved.display()))?;

        // Track which old strings we've seen to detect duplicates
        let mut seen_olds: HashMap<String, usize> = HashMap::new();

        // Apply replacements
        let mut modified_content = content.clone();
        let mut results = Vec::new();
        let mut total_replacements = 0usize;

        for (i, replacement) in replacements.iter().enumerate() {
            // Check for empty old string
            if replacement.old.is_empty() {
                results.push(ReplacementResult {
                    old: replacement.old.clone(),
                    new: replacement.new.clone(),
                    occurrences: 0,
                    success: false,
                    error: Some("Empty 'old' string is not allowed".to_string()),
                });
                continue;
            }

            // Check for duplicate old strings
            if let Some(first_idx) = seen_olds.get(&replacement.old) {
                results.push(ReplacementResult {
                    old: replacement.old.clone(),
                    new: replacement.new.clone(),
                    occurrences: 0,
                    success: false,
                    error: Some(format!(
                        "Duplicate 'old' string at index {} (first seen at index {})",
                        i, first_idx
                    )),
                });
                continue;
            }
            seen_olds.insert(replacement.old.clone(), i);

            // Count occurrences before replacement
            let occurrences = modified_content.matches(&replacement.old).count();

            if occurrences == 0 {
                results.push(ReplacementResult {
                    old: replacement.old.clone(),
                    new: replacement.new.clone(),
                    occurrences: 0,
                    success: false,
                    error: Some(format!(
                        "String not found in file: '{}'",
                        truncate_str(&replacement.old, 50)
                    )),
                });
                continue;
            }

            if occurrences > 1 {
                results.push(ReplacementResult {
                    old: replacement.old.clone(),
                    new: replacement.new.clone(),
                    occurrences,
                    success: false,
                    error: Some(format!(
                        "String found {} times (must be unique). Use a more specific 'old' string.",
                        occurrences
                    )),
                });
                continue;
            }

            // Apply the replacement
            modified_content = modified_content.replacen(&replacement.old, &replacement.new, 1);
            total_replacements += 1;

            results.push(ReplacementResult {
                old: replacement.old.clone(),
                new: replacement.new.clone(),
                occurrences: 1,
                success: true,
                error: None,
            });
        }

        // Check if all replacements succeeded
        let all_success = results.iter().all(|r| r.success);

        if !all_success {
            // Return error with details
            let failed: Vec<&ReplacementResult> = results.iter().filter(|r| !r.success).collect();
            let error_msg = failed
                .iter()
                .map(|r| format!(
                    "Replacement failed: '{}' - {}",
                    truncate_str(&r.old, 30),
                    r.error.as_ref().unwrap()
                ))
                .collect::<Vec<_>>()
                .join("\n");

            return Err(anyhow::anyhow!(
                "Some replacements failed (file not modified):\n{}",
                error_msg
            ));
        }

        // Write modified content
        fs::write(&resolved, modified_content)
            .await
            .with_context(|| format!("Failed to write file: {}", resolved.display()))?;

        Ok(serde_json::json!({
            "path": resolved.display().to_string(),
            "replacements": results,
            "total_replacements": total_replacements,
            "success": true,
        }))
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}

impl Default for StrReplaceFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for StrReplaceFileTool {
    fn name(&self) -> &'static str {
        "str_replace_file"
    }

    fn description(&self) -> String {
        r#"## Purpose
Make targeted string replacements in files. Each 'old' string must appear exactly once in the file.

Use when: Making precise edits to existing files (safer than full file rewrites).
Don't use when: Creating new files (use WriteFile) or making multiple unrelated changes (make multiple calls).

## Key Rules

1. The 'old' string must match EXACTLY (including whitespace)
2. The 'old' string must appear EXACTLY ONCE in the file
3. Each 'old' string in a batch must be UNIQUE
4. If either rule is violated, NO changes are made (atomic)

## Parameters

### path (required)
Path to the file to modify.

### edit (required)
Either a single replacement object or array of replacements:
- `old`: The exact text to find (must be unique in file)
- `new`: The text to replace with

## Examples

Single replacement:
```json
{
  "path": "src/main.rs",
  "edit": {
    "old": "println!(\"hello\");",
    "new": "println!(\"hello, world!\");"
  }
}
```

Multiple replacements (each 'old' must be unique):
```json
{
  "path": "src/main.rs",
  "edit": [
    {
      "old": "fn foo() {",
      "new": "fn foo() -> i32 {"
    },
    {
      "old": "    return 42;",
      "new": "    return 42;\n    // Returns the answer"
    }
  ]
}
```

## Tips for Success

1. Read the file first with ReadFile to get the exact content
2. Include enough context in 'old' to make it unique
3. Use the exact whitespace (spaces vs tabs)
4. For multi-line replacements, include the newline characters"#
        .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to modify"
                },
                "edit": {
                    "oneOf": [
                        {
                            "type": "object",
                            "description": "Single replacement",
                            "properties": {
                                "old": {
                                    "type": "string",
                                    "description": "The exact text to find (must appear exactly once)"
                                },
                                "new": {
                                    "type": "string",
                                    "description": "The text to replace with"
                                }
                            },
                            "required": ["old", "new"]
                        },
                        {
                            "type": "array",
                            "description": "Multiple replacements (each old must be unique)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "old": {
                                        "type": "string",
                                        "description": "The exact text to find (must appear exactly once and be unique in this batch)"
                                    },
                                    "new": {
                                        "type": "string",
                                        "description": "The text to replace with"
                                    }
                                },
                                "required": ["old", "new"]
                            }
                        }
                    ]
                }
            },
            "required": ["path", "edit"]
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let args: StrReplaceFileArgs = serde_json::from_value(params)
            .map_err(|e| anyhow::anyhow!("Invalid arguments: {}", e))?;

        let replacements = args.edit.into_vec();
        self.replace_in_file(&args.path, replacements).await
    }

    fn estimated_duration_ms(&self, _params: &serde_json::Value) -> u64 {
        100 // Fast operation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_replace_single() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(temp_dir.path().join("test.txt"), "Hello, World!").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": {
                "old": "World",
                "new": "Rust"
            }
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 1);
        assert_eq!(result["success"], true);

        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "Hello, Rust!");
    }

    #[tokio::test]
    async fn test_replace_multiple() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        // Create test file
        fs::write(temp_dir.path().join("test.txt"), "A\nB\nC").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": [
                {"old": "A", "new": "X"},
                {"old": "B", "new": "Y"},
                {"old": "C", "new": "Z"}
            ]
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 3);

        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "X\nY\nZ");
    }

    #[tokio::test]
    async fn test_replace_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "Hello, World!").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": {
                "old": "NonExistent",
                "new": "Replacement"
            }
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("String not found"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "Hello, World!");
    }

    #[tokio::test]
    async fn test_replace_duplicate_old() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "A B C").await.unwrap();

        // Try to use the same 'old' twice in one batch
        let params = json!({
            "path": "test.txt",
            "edit": [
                {"old": "A", "new": "X"},
                {"old": "A", "new": "Y"}  // Duplicate!
            ]
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Duplicate"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "A B C");
    }

    #[tokio::test]
    async fn test_replace_not_unique() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        // Create file with duplicate content
        fs::write(temp_dir.path().join("test.txt"), "foo foo foo").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": {
                "old": "foo",
                "new": "bar"
            }
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("3 times"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "foo foo foo");
    }

    #[tokio::test]
    async fn test_replace_multiline() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "line1\nline2\nline3").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": {
                "old": "line1\nline2",
                "new": "replaced"
            }
        });
        let result = tool.execute(params).await.unwrap();

        assert_eq!(result["total_replacements"], 1);

        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "replaced\nline3");
    }

    #[tokio::test]
    async fn test_replace_file_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        let params = json!({
            "path": "nonexistent.txt",
            "edit": {
                "old": "old",
                "new": "new"
            }
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_replace_empty_old_string() {
        let temp_dir = TempDir::new().unwrap();
        let tool = StrReplaceFileTool::new().with_workspace(temp_dir.path());

        fs::write(temp_dir.path().join("test.txt"), "Hello, World!").await.unwrap();

        let params = json!({
            "path": "test.txt",
            "edit": {
                "old": "",
                "new": "replacement"
            }
        });
        let result = tool.execute(params).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Empty 'old' string"));

        // Verify file was not modified
        let content = fs::read_to_string(temp_dir.path().join("test.txt")).await.unwrap();
        assert_eq!(content, "Hello, World!");
    }
}
