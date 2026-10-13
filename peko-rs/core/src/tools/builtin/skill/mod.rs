//! `Skill` tool surface + `SkillRuntime` port.
//!
//! The tool does not scan the workspace itself. It speaks to a runtime
//! port ([`SkillRuntime`]) that the daemon implements with
//! `extensions::skill::reader::WorkspaceSkillRuntime`, which reads
//! `<workspace>/skills/<name>/SKILL.md` directly. Tests point the same
//! runtime at a tempdir.

pub mod body;
pub mod frontmatter;
pub mod tool;

pub use body::{preprocess_dynamic_context, SHELL_TIMEOUT_MS};
pub use frontmatter::{parse_yaml_frontmatter, parse_yaml_frontmatter_typed, SkillFrontmatter};
pub use tool::{SkillTool, ESCAPE_SENTINEL};

use std::path::PathBuf;
use std::sync::Arc;

/// Maximum bytes read from one skill definition, before UTF-8/YAML parsing.
pub const MAX_SKILL_FILE_BYTES: usize = 1024 * 1024;

/// Limit the combined procedure after dynamic context and argument expansion.
pub const MAX_SKILL_BODY_BYTES: usize = 1024 * 1024;

pub(crate) fn append_skill_text(output: &mut String, text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        output.len().saturating_add(text.len()) <= MAX_SKILL_BODY_BYTES,
        "rendered skill body exceeds {MAX_SKILL_BODY_BYTES} bytes"
    );
    output.push_str(text);
    Ok(())
}

/// Read with a limit while consuming the file, rather than after allocating it.
pub fn read_skill_file(path: &std::path::Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_SKILL_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_SKILL_FILE_BYTES,
        "skill definition exceeds {MAX_SKILL_FILE_BYTES} bytes"
    );
    Ok(String::from_utf8(bytes)?)
}

// ─── DTOs ──────────────────────────────────────────────────────────

/// Entry for a single discovered skill.
///
#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Invocation key (the skill directory name).
    pub name: String,
    /// Absolute path to the skill's `SKILL.md`.
    pub path: PathBuf,
}

// ─── SkillRuntime port trait ───────────────────────────────────────

/// Runtime port the `SkillTool` uses to resolve skill files.
///
/// The daemon side implements this with `WorkspaceSkillRuntime`
/// (root's `src/extensions/skill/reader.rs`), which reads directly
/// from the principal's workspace `skills/` directory. Tests point
/// the same runtime at a tempdir.
#[async_trait::async_trait]
pub trait SkillRuntime: Send + Sync {
    /// Resolve a skill by name. Returns `None` if no such skill is
    /// registered.
    fn resolve_skill(&self, name: &str) -> Option<SkillEntry>;

    /// Return all registered skill names, sorted.
    fn list_skills(&self) -> Vec<String>;
}

/// Type alias for the shared runtime handle threaded through every
/// `SkillTool` constructor.
pub type SharedSkillRuntime = Arc<dyn SkillRuntime>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_reads_reject_oversized_and_invalid_utf8_files() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("SKILL.md");
        std::fs::File::create(&path)
            .unwrap()
            .set_len((MAX_SKILL_FILE_BYTES + 1) as u64)
            .unwrap();
        assert!(read_skill_file(&path)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
        std::fs::write(&path, [0xff]).unwrap();
        assert!(read_skill_file(&path).is_err());
    }
}
