use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A thin Markdown prompt file registered with a Principal.
#[derive(Debug, Clone)]
pub struct AgentPrompt {
    pub name: String,
    pub path: PathBuf,
    pub frontmatter: AgentPromptFrontmatter,
    pub body: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPromptFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub role: Option<String>,
    pub color: Option<String>,
}

/// Load an agent prompt from a Markdown file.
///
/// Supports an optional YAML frontmatter block delimited by `---` lines.
/// The frontmatter is parsed into `AgentPromptFrontmatter`; everything
/// after the frontmatter is the prompt body.
pub fn load_agent_prompt(path: &PathBuf) -> anyhow::Result<AgentPrompt> {
    let content = std::fs::read_to_string(path)?;
    let file_stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "agent".to_string());

    let (frontmatter, body) = parse_frontmatter(&content);

    Ok(AgentPrompt {
        name: frontmatter.name.clone().unwrap_or(file_stem),
        path: path.clone(),
        frontmatter,
        body,
    })
}

fn parse_frontmatter(content: &str) -> (AgentPromptFrontmatter, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (AgentPromptFrontmatter::default(), content.to_string());
    }

    // Find end of frontmatter (second --- on its own line)
    let after_first = &trimmed[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let fm_text = &after_first[..end_idx];
        let body_start = end_idx + 4; // after \n---
        let body = after_first[body_start..].trim_start().to_string();

        match serde_yaml::from_str::<AgentPromptFrontmatter>(fm_text) {
            Ok(fm) => return (fm, body),
            Err(_) => return (AgentPromptFrontmatter::default(), content.to_string()),
        }
    }

    (AgentPromptFrontmatter::default(), content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_agent_prompt_no_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.md");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"You are a code reviewer.").unwrap();

        let prompt = load_agent_prompt(&path).unwrap();
        assert_eq!(prompt.name, "reviewer");
        assert_eq!(prompt.body, "You are a code reviewer.");
        assert!(prompt.frontmatter.name.is_none());
    }

    #[test]
    fn test_load_agent_prompt_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.md");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            b"---\nname: reviewer\ndescription: Code reviewer\n---\nYou are a code reviewer."
        )
        .unwrap();

        let prompt = load_agent_prompt(&path).unwrap();
        assert_eq!(prompt.name, "reviewer");
        assert_eq!(prompt.frontmatter.description.as_deref(), Some("Code reviewer"));
        assert_eq!(prompt.body, "You are a code reviewer.");
    }
}
