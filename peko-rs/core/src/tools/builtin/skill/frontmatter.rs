//! YAML frontmatter parser for SKILL.md files.
//!
//! Self-contained copy of the helpers from
//! `crate::extensions::framework::adapters::parsing`, kept here so
//! `peko_tools_builtin` does not need to import framework internals.
//! If the parser ever needs more than what is below, prefer
//! extending THIS helper over pulling in a wider framework dep.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::Deserialize;

/// YAML frontmatter from SKILL.md
#[derive(Debug, Deserialize)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// Positional argument names (e.g. `[issue, branch]` → `$issue`,
    /// `$branch`). Order maps to the `args` array passed to the
    /// `Skill` tool. Optional; missing `arguments:` means the skill
    /// has no parameterization — any `$name` in the body is left
    /// unsubstituted.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Shell selector for injected `` !`cmd` `` / `` ```! `` blocks.
    /// Today: `"bash"` (default) only. Powershell deferred — no
    /// cross-platform shell runner exists in Peko today. Any other
    /// value is fail-closed: the preprocessor refuses to run.
    #[serde(default, rename = "shell")]
    pub shell: Option<String>,
    /// Glob allowlist for shell commands the preprocessor may run
    /// from `!`cmd`` blocks. Empty list = all commands allowed.
    /// Match is case-sensitive and anchored (must match the full
    /// trimmed command).
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Vec<String>,
}

/// Parse a YAML frontmatter block from `content`. Returns the raw
/// YAML text and the body that followed.
pub fn parse_yaml_frontmatter(content: &str) -> Result<(String, String)> {
    let mut lines = content.lines().peekable();
    match lines.next() {
        Some("---") => {}
        _ => anyhow::bail!("YAML frontmatter must start with ---"),
    }
    let mut frontmatter_lines = Vec::new();
    let mut found_end = false;
    for line in lines.by_ref() {
        if line == "---" {
            found_end = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !found_end {
        anyhow::bail!("YAML frontmatter must end with ---");
    }
    let body = lines.collect::<Vec<_>>().join("\n");
    Ok((frontmatter_lines.join("\n"), body))
}

/// Parse a YAML frontmatter block from `content` and decode it into
/// the caller-supplied type.
pub fn parse_yaml_frontmatter_typed<T: DeserializeOwned>(content: &str) -> Result<(T, String)> {
    let (frontmatter, body) = parse_yaml_frontmatter(content)?;
    let metadata: T =
        serde_yaml::from_str(&frontmatter).context("Failed to parse YAML frontmatter")?;
    Ok((metadata, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_minimal() {
        let yaml = "---\nname: x\ndescription: y\n---\nbody\n";
        let (raw, body) = parse_yaml_frontmatter(yaml).unwrap();
        assert_eq!(raw, "name: x\ndescription: y");
        assert_eq!(body, "body");
    }

    #[test]
    fn typed_decodes_into_struct() {
        let yaml = "---\nname: x\ndescription: y\nallowed-tools: [\"git *\"]\n---\nbody\n";
        let (fm, body): (SkillFrontmatter, String) = parse_yaml_frontmatter_typed(yaml).unwrap();
        assert_eq!(fm.name, "x");
        assert_eq!(fm.allowed_tools, vec!["git *"]);
        assert_eq!(body, "body");
    }

    #[test]
    fn missing_open_fence_errors() {
        let yaml = "name: x\ndescription: y\n---\nbody\n";
        assert!(parse_yaml_frontmatter(yaml).is_err());
    }

    #[test]
    fn missing_close_fence_errors() {
        let yaml = "---\nname: x\ndescription: y\nbody\n";
        assert!(parse_yaml_frontmatter(yaml).is_err());
    }
}
