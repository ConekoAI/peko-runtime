//! Markdown memory backing - Human-readable memory files
//!
//! Matches `OpenClaw`'s memory layout:
//! - memory/YYYY-MM-DD.md - Daily logs (append-only)
//! - MEMORY.md - Curated long-term memory
//!
//! These are plain Markdown files that serve as source of truth.

use crate::memory::types::MemoryEntry;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::fs;
use tracing::{debug, info};

/// Markdown memory configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkdownMemoryConfig {
    /// Base workspace directory
    pub workspace_dir: PathBuf,
    /// Daily memory directory (relative to workspace)
    pub daily_dir: String,
    /// Long-term memory filename
    pub long_term_file: String,
    /// Auto-create directories
    pub auto_create: bool,
    /// Include source citations
    pub include_citations: bool,
}

impl Default for MarkdownMemoryConfig {
    fn default() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            daily_dir: "memory".to_string(),
            long_term_file: "MEMORY.md".to_string(),
            auto_create: true,
            include_citations: true,
        }
    }
}

/// Markdown memory backend
pub struct MarkdownMemory {
    config: MarkdownMemoryConfig,
    cache: HashMap<String, Vec<MemoryEntry>>,
}

impl MarkdownMemory {
    /// Create a new Markdown memory backend
    #[must_use]
    pub fn new(config: MarkdownMemoryConfig) -> Self {
        Self {
            config,
            cache: HashMap::new(),
        }
    }

    /// Create with default config
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(MarkdownMemoryConfig::default())
    }

    /// Get daily memory file path for a date
    #[must_use]
    pub fn daily_file_path(&self, date: chrono::DateTime<Utc>) -> PathBuf {
        self.config
            .workspace_dir
            .join(&self.config.daily_dir)
            .join(format!("{}.md", date.format("%Y-%m-%d")))
    }

    /// Get long-term memory file path
    #[must_use]
    pub fn long_term_path(&self) -> PathBuf {
        self.config.workspace_dir.join(&self.config.long_term_file)
    }

    /// Ensure directories exist
    async fn ensure_dirs(&self) -> Result<()> {
        if !self.config.auto_create {
            return Ok(());
        }

        let daily_dir = self.config.workspace_dir.join(&self.config.daily_dir);
        if !daily_dir.exists() {
            fs::create_dir_all(&daily_dir)
                .await
                .context("Failed to create daily memory directory")?;
            debug!("Created daily memory directory: {}", daily_dir.display());
        }

        Ok(())
    }

    /// Append entry to today's daily memory file
    pub async fn append_daily(
        &mut self,
        content: &str,
        metadata: Option<HashMap<String, String>>,
    ) -> Result<()> {
        self.ensure_dirs().await?;

        let today = Utc::now();
        let path = self.daily_file_path(today);

        // Format entry as Markdown
        let timestamp = today.format("%H:%M");
        let mut entry_text = format!("\n## {timestamp}\n\n{content}");

        // Add metadata as HTML comments
        if let Some(meta) = metadata {
            entry_text.push_str("\n\n<!--\n");
            for (key, value) in meta {
                entry_text.push_str(&format!("{key}: {value}\n"));
            }
            entry_text.push_str("-->");
        }

        // Append to file (create if doesn't exist)
        let file_exists = path.exists();
        let mut file_content = if file_exists {
            fs::read_to_string(&path).await.unwrap_or_default()
        } else {
            format!("# Memory Log - {}\n", today.format("%Y-%m-%d"))
        };

        file_content.push_str(&entry_text);
        file_content.push('\n');

        fs::write(&path, file_content)
            .await
            .context("Failed to write daily memory")?;

        info!("Appended to daily memory: {}", path.display());
        Ok(())
    }

    /// Write to long-term MEMORY.md
    pub async fn write_long_term(&mut self, section: &str, content: &str) -> Result<()> {
        self.ensure_dirs().await?;

        let path = self.long_term_path();

        let file_exists = path.exists();
        let mut file_content = if file_exists {
            fs::read_to_string(&path).await.unwrap_or_default()
        } else {
            "# Long-Term Memory\n\nThis file contains curated memories and important information.\n"
                .to_string()
        };

        // Check if section exists
        let section_header = format!("## {section}");
        if file_content.contains(&section_header) {
            // Append to existing section
            let parts: Vec<&str> = file_content.splitn(2, &section_header).collect();
            if parts.len() == 2 {
                let after_section = parts[1];
                // Find next ## or end of file
                let insert_pos = if let Some(next_pos) = after_section.find("\n## ") {
                    parts[0].len() + section_header.len() + next_pos
                } else {
                    file_content.len()
                };

                let new_entry = format!("\n\n{content}\n");
                file_content.insert_str(insert_pos, &new_entry);
            }
        } else {
            // Add new section
            file_content.push_str(&format!("\n\n## {section}\n\n{content}\n"));
        }

        fs::write(&path, file_content)
            .await
            .context("Failed to write long-term memory")?;

        info!(
            "Updated long-term memory: {} (section: {})",
            path.display(),
            section
        );
        Ok(())
    }

    /// Read daily memory file
    pub async fn read_daily(&self, date: chrono::DateTime<Utc>) -> Result<Option<String>> {
        let path = self.daily_file_path(date);

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read daily memory")?;

        Ok(Some(content))
    }

    /// Read long-term MEMORY.md
    pub async fn read_long_term(&self) -> Result<Option<String>> {
        let path = self.long_term_path();

        if !path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&path)
            .await
            .context("Failed to read long-term memory")?;

        Ok(Some(content))
    }

    /// Read today's + yesterday's memory (like `OpenClaw`)
    pub async fn read_recent(&self) -> Result<Vec<(String, String)>> {
        let mut results = vec![];

        // Read today
        let today = Utc::now();
        if let Some(content) = self.read_daily(today).await? {
            results.push((today.format("%Y-%m-%d").to_string(), content));
        }

        // Read yesterday
        let yesterday = today - chrono::Duration::days(1);
        if let Some(content) = self.read_daily(yesterday).await? {
            results.push((yesterday.format("%Y-%m-%d").to_string(), content));
        }

        Ok(results)
    }

    /// Parse Markdown content into memory entries
    #[must_use]
    pub fn parse_entries(&self, content: &str, source: &str) -> Vec<ParsedEntry> {
        let mut entries = vec![];
        let mut current_heading: Option<String> = None;
        let mut current_content = String::new();

        for line in content.lines() {
            if line.starts_with("## ") {
                // Save previous entry if exists
                if let Some(heading) = current_heading.take() {
                    if !current_content.trim().is_empty() {
                        entries.push(ParsedEntry {
                            heading,
                            content: current_content.trim().to_string(),
                            source: source.to_string(),
                        });
                    }
                }
                current_heading = Some(line[3..].trim().to_string());
                current_content.clear();
            } else if !line.starts_with("<!--") && !line.starts_with("-->") {
                // Skip HTML comments (metadata)
                current_content.push_str(line);
                current_content.push('\n');
            }
        }

        // Don't forget the last entry
        if let Some(heading) = current_heading {
            if !current_content.trim().is_empty() {
                entries.push(ParsedEntry {
                    heading,
                    content: current_content.trim().to_string(),
                    source: source.to_string(),
                });
            }
        }

        entries
    }

    /// List all daily memory files
    pub async fn list_daily_files(&self) -> Result<Vec<PathBuf>> {
        let daily_dir = self.config.workspace_dir.join(&self.config.daily_dir);

        if !daily_dir.exists() {
            return Ok(vec![]);
        }

        let mut files = vec![];
        let mut entries = fs::read_dir(&daily_dir)
            .await
            .context("Failed to read daily memory directory")?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                files.push(path);
            }
        }

        // Sort by filename (date)
        files.sort();

        Ok(files)
    }

    /// Get status summary
    pub async fn status(&self) -> Result<String> {
        let daily_files = self.list_daily_files().await?;
        let long_term_exists = self.long_term_path().exists();

        let today = Utc::now();
        let today_file = self.daily_file_path(today);
        let today_exists = today_file.exists();

        Ok(format!(
            "📝 Markdown Memory: {} daily files, MEMORY.md: {}, Today: {}",
            daily_files.len(),
            if long_term_exists { "✓" } else { "✗" },
            if today_exists { "✓" } else { "✗" }
        ))
    }
}

/// A parsed memory entry from Markdown
#[derive(Debug, Clone)]
pub struct ParsedEntry {
    /// Heading/timestamp
    pub heading: String,
    /// Content
    pub content: String,
    /// Source file
    pub source: String,
}

impl Default for MarkdownMemory {
    fn default() -> Self {
        Self::default_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn create_test_memory() -> (MarkdownMemory, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let config = MarkdownMemoryConfig {
            workspace_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MarkdownMemory::new(config);
        (memory, temp_dir)
    }

    #[tokio::test]
    async fn test_daily_file_path() {
        let (memory, _temp) = create_test_memory().await;
        let date = Utc::now();
        let path = memory.daily_file_path(date);

        assert!(path.to_string_lossy().contains("memory/"));
        assert!(path
            .to_string_lossy()
            .contains(&date.format("%Y-%m-%d").to_string()));
    }

    #[tokio::test]
    async fn test_append_and_read_daily() {
        let (mut memory, _temp) = create_test_memory().await;

        memory
            .append_daily("Test memory entry", None)
            .await
            .unwrap();

        let content = memory.read_daily(Utc::now()).await.unwrap();
        assert!(content.is_some());
        assert!(content.unwrap().contains("Test memory entry"));
    }

    #[tokio::test]
    async fn test_write_and_read_long_term() {
        let (mut memory, _temp) = create_test_memory().await;

        memory
            .write_long_term("Preferences", "User prefers dark mode")
            .await
            .unwrap();
        memory
            .write_long_term("Preferences", "User likes cats")
            .await
            .unwrap();

        let content = memory.read_long_term().await.unwrap();
        assert!(content.is_some());
        let text = content.unwrap();
        assert!(text.contains("Preferences"));
        assert!(text.contains("dark mode"));
        assert!(text.contains("cats"));
    }

    #[test]
    fn test_parse_entries() {
        let memory = MarkdownMemory::default_config();

        let content = r#"# Memory Log

## 10:00

First entry content.

## 11:00

Second entry content.

<!--
tag: test
-->
"#;

        let entries = memory.parse_entries(content, "test.md");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].heading, "10:00");
        assert!(entries[0].content.contains("First entry"));
        assert_eq!(entries[1].heading, "11:00");
    }

    #[tokio::test]
    async fn test_read_recent() {
        let (mut memory, _temp) = create_test_memory().await;

        memory.append_daily("Today entry", None).await.unwrap();

        let recent = memory.read_recent().await.unwrap();
        assert!(!recent.is_empty());
        assert!(recent[0].1.contains("Today entry"));
    }
}
