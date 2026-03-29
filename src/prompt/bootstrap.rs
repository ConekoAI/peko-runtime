//! Bootstrap file injection for system prompts
//!
//! Matches `OpenClaw`'s AGENTS.md, SOUL.md, TOOLS.md, etc. injection

use std::path::PathBuf;
use tracing::{debug, warn};

/// Bootstrap file configuration
#[derive(Debug, Clone)]
pub struct BootstrapConfig {
    pub workspace_dir: PathBuf,
    pub max_chars_per_file: usize,
    pub files: Vec<BootstrapFile>,
}

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self::with_default_files()
    }
}

impl BootstrapConfig {
    /// Create config with default bootstrap files
    pub fn with_default_files() -> Self {
        Self {
            workspace_dir: PathBuf::from("."),
            max_chars_per_file: 20_000, // Match OpenClaw default
            files: vec![
                BootstrapFile::required("AGENTS.md"),   // Operating instructions
                BootstrapFile::optional("SOUL.md"),     // Persona/tone
                BootstrapFile::optional("TOOLS.md"),    // Tool guidance
                BootstrapFile::optional("IDENTITY.md"), // Agent identity
                BootstrapFile::optional("USER.md"),     // User info
                BootstrapFile::optional("MEMORY.md"),   // Long-term memory
                                                        // Note: HEARTBEAT.md is NOT injected - it's read proactively on heartbeat polls
            ],
        }
    }

    /// Create config with a custom list of files (all treated as optional)
    /// 
    /// If `files` is None, returns an empty file list.
    /// Use `with_default_files()` explicitly if you want the default file list.
    pub fn with_files(files: Option<Vec<String>>, workspace_dir: PathBuf) -> Self {
        let files = files
            .unwrap_or_default()
            .into_iter()
            .map(|name| BootstrapFile::optional(&name))
            .collect();

        Self {
            workspace_dir,
            max_chars_per_file: 20_000,
            files,
        }
    }

    /// Create config with default files and specified workspace
    fn with_default_files_with_workspace(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            max_chars_per_file: 20_000,
            files: vec![
                BootstrapFile::required("AGENTS.md"),
                BootstrapFile::optional("SOUL.md"),
                BootstrapFile::optional("TOOLS.md"),
                BootstrapFile::optional("IDENTITY.md"),
                BootstrapFile::optional("USER.md"),
                BootstrapFile::optional("MEMORY.md"),
            ],
        }
    }
}

/// Bootstrap file definition
#[derive(Debug, Clone)]
pub struct BootstrapFile {
    pub name: String,
    pub required: bool,
    pub section_name: String,
}

impl BootstrapFile {
    pub fn required(name: &str) -> Self {
        Self {
            name: name.to_string(),
            required: true,
            section_name: name.replace(".md", "").to_uppercase(),
        }
    }

    pub fn optional(name: &str) -> Self {
        Self {
            name: name.to_string(),
            required: false,
            section_name: name.replace(".md", "").to_uppercase(),
        }
    }
}

/// Injected content from bootstrap files
#[derive(Debug, Clone)]
pub struct InjectedContext {
    pub sections: Vec<InjectedSection>,
    pub total_chars: usize,
}

#[derive(Debug, Clone)]
pub struct InjectedSection {
    pub name: String,
    pub content: String,
    pub source_file: String,
    pub truncated: bool,
}

/// Load and inject bootstrap files
pub fn inject_bootstrap_files(config: &BootstrapConfig) -> InjectedContext {
    let mut sections = vec![];
    let mut total_chars = 0;

    for file_def in &config.files {
        let path = config.workspace_dir.join(&file_def.name);

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let content_len = content.len();
                let (content, truncated) = if content_len > config.max_chars_per_file {
                    let truncated_content = format!("{}...", &content[..config.max_chars_per_file]);
                    (truncated_content, true)
                } else {
                    (content, false)
                };

                total_chars += content.len();

                sections.push(InjectedSection {
                    name: file_def.section_name.clone(),
                    content,
                    source_file: file_def.name.clone(),
                    truncated,
                });

                debug!("Injected {} ({} chars)", file_def.name, content_len);
            }
            Err(_) if file_def.required => {
                warn!("Required bootstrap file not found: {}", file_def.name);
                sections.push(InjectedSection {
                    name: file_def.section_name.clone(),
                    content: format!("<!-- {}: file not found -->", file_def.name),
                    source_file: file_def.name.clone(),
                    truncated: false,
                });
            }
            Err(_) => {
                // Optional file missing - log warning
                warn!("Bootstrap file not found: {}", file_def.name);
            }
        }
    }

    InjectedContext {
        sections,
        total_chars,
    }
}

/// Get default workspace directory
pub fn default_workspace_dir() -> PathBuf {
    dirs::data_dir()
        .map(|d| d.join("pekobot").join("workspaces"))
        .or_else(|| dirs::home_dir().map(|h| h.join(".pekobot").join("workspaces")))
        .unwrap_or_else(|| PathBuf::from("./workspaces"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_bootstrap_config_default() {
        let config = BootstrapConfig::default();
        assert_eq!(config.max_chars_per_file, 20_000);
        assert_eq!(config.files.len(), 6); // AGENTS, SOUL, TOOLS, IDENTITY, USER, MEMORY (HEARTBEAT.md is read proactively, not injected)
    }

    #[test]
    fn test_inject_bootstrap_files() {
        let tmp = TempDir::new().unwrap();

        // Create a test AGENTS.md
        std::fs::write(
            tmp.path().join("AGENTS.md"),
            "# Test Agent\n\nInstructions here.",
        )
        .unwrap();

        // Create optional SOUL.md
        std::fs::write(
            tmp.path().join("SOUL.md"),
            "# Persona\n\nFriendly and helpful.",
        )
        .unwrap();

        let config = BootstrapConfig {
            workspace_dir: tmp.path().to_path_buf(),
            max_chars_per_file: 20_000,
            files: vec![
                BootstrapFile::required("AGENTS.md"),
                BootstrapFile::optional("SOUL.md"),
                BootstrapFile::optional("MISSING.md"),
            ],
        };

        let injected = inject_bootstrap_files(&config);

        assert_eq!(injected.sections.len(), 2); // AGENTS and SOUL, not MISSING
        assert!(injected.sections.iter().any(|s| s.name == "AGENTS"));
        assert!(injected.sections.iter().any(|s| s.name == "SOUL"));
        assert!(!injected.sections.iter().any(|s| s.name == "MISSING"));
    }

    #[test]
    fn test_truncate_long_file() {
        let tmp = TempDir::new().unwrap();

        // Create a file longer than max_chars
        let long_content = "a".repeat(100);
        std::fs::write(tmp.path().join("LONG.md"), &long_content).unwrap();

        let config = BootstrapConfig {
            workspace_dir: tmp.path().to_path_buf(),
            max_chars_per_file: 50, // Small limit for testing
            files: vec![BootstrapFile::optional("LONG.md")],
        };

        let injected = inject_bootstrap_files(&config);

        assert_eq!(injected.sections.len(), 1);
        assert!(injected.sections[0].truncated);
        assert!(injected.sections[0].content.ends_with("..."));
    }

    #[test]
    fn test_bootstrap_config_with_custom_files() {
        let tmp = TempDir::new().unwrap();

        // Create custom files
        std::fs::write(tmp.path().join("CUSTOM.md"), "# Custom").unwrap();
        std::fs::write(tmp.path().join("ANOTHER.md"), "# Another").unwrap();

        // Test with custom file list
        let config = BootstrapConfig::with_files(
            Some(vec!["CUSTOM.md".to_string(), "ANOTHER.md".to_string()]),
            tmp.path().to_path_buf(),
        );

        assert_eq!(config.files.len(), 2);
        assert_eq!(config.files[0].name, "CUSTOM.md");
        assert_eq!(config.files[1].name, "ANOTHER.md");
        // All custom files should be optional
        assert!(!config.files[0].required);
        assert!(!config.files[1].required);

        let injected = inject_bootstrap_files(&config);
        assert_eq!(injected.sections.len(), 2);
        assert!(injected.sections.iter().any(|s| s.name == "CUSTOM"));
        assert!(injected.sections.iter().any(|s| s.name == "ANOTHER"));
    }

    #[test]
    fn test_bootstrap_config_with_files_empty_uses_empty() {
        let tmp = TempDir::new().unwrap();

        // Test with empty list uses empty file list (no fallback)
        let config = BootstrapConfig::with_files(Some(vec![]), tmp.path().to_path_buf());

        // Should use empty file list
        assert_eq!(config.files.len(), 0);

        let injected = inject_bootstrap_files(&config);
        assert_eq!(injected.sections.len(), 0);
    }

    #[test]
    fn test_bootstrap_config_with_files_none_uses_empty() {
        let tmp = TempDir::new().unwrap();

        // Test with None uses empty file list (no fallback)
        let config = BootstrapConfig::with_files(None, tmp.path().to_path_buf());

        // Should use empty file list
        assert_eq!(config.files.len(), 0);
    }

    #[test]
    fn test_bootstrap_custom_files_missing_logs_warning() {
        let tmp = TempDir::new().unwrap();

        // Don't create any files - all missing
        let config = BootstrapConfig::with_files(
            Some(vec!["MISSING1.md".to_string(), "MISSING2.md".to_string()]),
            tmp.path().to_path_buf(),
        );

        let injected = inject_bootstrap_files(&config);
        // All files missing, so no sections injected
        assert_eq!(injected.sections.len(), 0);
    }
}
