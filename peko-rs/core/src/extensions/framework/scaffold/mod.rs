//! Extension Scaffolding — `peko ext init`
//!
//! Provides templates and rendering for creating new extensions.
//! All templates are embedded in the binary via `include_str!` to ensure
//! version-lock with the runtime.

mod engine;

pub use engine::{build_vars, Template};

use std::path::{Path, PathBuf};

/// Options for scaffolding an extension
#[derive(Debug, Clone)]
pub struct ScaffoldOptions {
    /// Extension ID (kebab-case identifier)
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Description
    pub description: String,
    /// Output directory
    pub output_dir: PathBuf,
    /// Programming language for stub code (when applicable)
    pub lang: ScaffoldLang,
    /// For MCP: create bare server.json instead of manifest.yaml wrapper
    pub bare_mcp: bool,
}

/// Supported languages for stub code
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldLang {
    Python,
    JavaScript,
}

impl ScaffoldLang {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "python" | "py" => Some(Self::Python),
            "javascript" | "js" | "node" => Some(Self::JavaScript),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::JavaScript => "javascript",
        }
    }

    pub fn handler_extension(&self) -> &'static str {
        match self {
            Self::Python => "py",
            Self::JavaScript => "js",
        }
    }
}

impl Default for ScaffoldLang {
    fn default() -> Self {
        Self::Python
    }
}

/// The scaffold engine
pub struct ScaffoldEngine;

impl ScaffoldEngine {
    /// Scaffold a new extension at the given path
    pub fn scaffold(ext_type: &str, options: &ScaffoldOptions) -> anyhow::Result<PathBuf> {
        let output = &options.output_dir;
        std::fs::create_dir_all(output)?;

        match ext_type {
            "skill" => Self::scaffold_skill(output, options),
            "mcp" => Self::scaffold_mcp(output, options),
            "universal-tool" | "tool" => Self::scaffold_universal_tool(output, options),
            "general" => Self::scaffold_general(output, options),
            // Sprint 9 Commit 3: gateway scaffold retired.
            "gateway" => anyhow::bail!(
                "Extension type 'gateway' was retired in Sprint 9 Commit 3; \
                 chat-gateway adapter framework removed."
            ),
            other => anyhow::bail!(
                "Unknown extension type '{}'. Supported: skill, mcp, universal-tool, general",
                other
            ),
        }
    }

    fn scaffold_skill(output: &Path, options: &ScaffoldOptions) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(output)?;
        std::fs::write(
            output.join("SKILL.md"),
            format!(
                "---\nid: {}\nname: {}\ndescription: {}\n---\n# {}\n",
                options.id, options.name, options.description, options.name
            ),
        )?;
        std::fs::write(output.join("README.md"), format!("# {}\n", options.name))?;
        std::fs::write(output.join(".gitignore"), "*.pyc\n__pycache__\n")?;
        Ok(output.to_path_buf())
    }

    fn scaffold_mcp(output: &Path, options: &ScaffoldOptions) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(output)?;
        if options.bare_mcp {
            std::fs::write(
                output.join("server.json"),
                format!(
                    "{{\"name\": \"{}\", \"description\": \"{}\"}}\n",
                    options.id, options.description
                ),
            )?;
        } else {
            std::fs::write(
                output.join("manifest.yaml"),
                format!(
                    "id: {}\nname: {}\ndescription: {}\nextension_type: \"mcp\"\n",
                    options.id, options.name, options.description
                ),
            )?;
        }
        std::fs::write(output.join("README.md"), format!("# {}\n", options.name))?;
        std::fs::write(output.join(".gitignore"), "*.pyc\n")?;
        Ok(output.to_path_buf())
    }

    fn scaffold_universal_tool(
        output: &Path,
        options: &ScaffoldOptions,
    ) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(output)?;
        std::fs::write(
            output.join("manifest.yaml"),
            format!("id: {}\nextension_type: \"universal-tool\"\n", options.id),
        )?;
        let handler_file = format!("handler.{}", options.lang.handler_extension());
        std::fs::write(output.join(&handler_file), format!("# {}\n", options.name))?;
        std::fs::write(output.join("README.md"), format!("# {}\n", options.name))?;
        std::fs::write(output.join(".gitignore"), "*.pyc\n__pycache__\n")?;
        Ok(output.to_path_buf())
    }

    fn scaffold_general(output: &Path, options: &ScaffoldOptions) -> anyhow::Result<PathBuf> {
        std::fs::create_dir_all(output)?;
        std::fs::write(
            output.join("manifest.yaml"),
            format!(
                "id: {}\nname: {}\nextension_type: \"general\"\n",
                options.id, options.name
            ),
        )?;
        std::fs::write(output.join("README.md"), format!("# {}\n", options.name))?;
        std::fs::write(output.join(".gitignore"), "*.pyc\n")?;
        Ok(output.to_path_buf())
    }
}

/// List supported extension types for scaffolding
pub fn supported_types() -> Vec<&'static str> {
    // Sprint 9 Commit 3: gateway retired.
    vec!["skill", "mcp", "universal-tool", "general"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_options(id: &str, ext_type: &str) -> ScaffoldOptions {
        ScaffoldOptions {
            id: id.to_string(),
            name: format!("Test {}", ext_type),
            description: "A test extension".to_string(),
            output_dir: std::path::PathBuf::from("."),
            lang: ScaffoldLang::Python,
            bare_mcp: false,
        }
    }

    #[test]
    fn test_supported_types() {
        // Sprint 9 Commit 3: gateway retired.
        let types = supported_types();
        assert!(types.contains(&"skill"));
        assert!(types.contains(&"mcp"));
        assert!(types.contains(&"universal-tool"));
        assert!(types.contains(&"general"));
        assert!(!types.contains(&"gateway"));
    }

    #[test]
    fn test_scaffold_skill() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-skill", "skill");
        opts.output_dir = temp.path().join("my-skill");

        let result = ScaffoldEngine::scaffold("skill", &opts);
        assert!(result.is_ok(), "Scaffold failed: {:?}", result.err());

        assert!(opts.output_dir.join("SKILL.md").exists());
        assert!(opts.output_dir.join("README.md").exists());
        assert!(opts.output_dir.join(".gitignore").exists());

        let skill_content = std::fs::read_to_string(opts.output_dir.join("SKILL.md")).unwrap();
        assert!(skill_content.contains("my-skill"));
        assert!(skill_content.contains("Test skill"));
    }

    #[test]
    fn test_scaffold_universal_tool_python() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-tool", "universal-tool");
        opts.output_dir = temp.path().join("my-tool");
        opts.lang = ScaffoldLang::Python;

        let result = ScaffoldEngine::scaffold("universal-tool", &opts);
        assert!(result.is_ok());

        assert!(opts.output_dir.join("manifest.yaml").exists());
        assert!(opts.output_dir.join("handler.py").exists());
        assert!(!opts.output_dir.join("handler.js").exists());
    }

    #[test]
    fn test_scaffold_universal_tool_javascript() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-tool", "universal-tool");
        opts.output_dir = temp.path().join("my-tool");
        opts.lang = ScaffoldLang::JavaScript;

        let result = ScaffoldEngine::scaffold("universal-tool", &opts);
        assert!(result.is_ok());

        assert!(opts.output_dir.join("handler.js").exists());
        assert!(!opts.output_dir.join("handler.py").exists());
    }

    #[test]
    fn test_scaffold_gateway_retired() {
        // Sprint 9 Commit 3: the gateway scaffold was retired. The
        // scaffold engine now rejects "gateway" with a clear error
        // pointing at the chat-gateway adapter framework retirement.
        let temp = TempDir::new().unwrap();
        let opts = test_options("my-gateway", "gateway");
        let _ = opts.output_dir.clone();
        let _opts = ScaffoldOptions {
            output_dir: temp.path().join("my-gateway"),
            ..opts
        };

        let mut opts = test_options("my-gateway", "gateway");
        opts.output_dir = temp.path().join("my-gateway");
        let err = ScaffoldEngine::scaffold("gateway", &opts).unwrap_err();
        assert!(
            err.to_string().contains("retired"),
            "expected retirement error, got: {err}"
        );
    }

    #[test]
    fn test_scaffold_mcp_bare() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-mcp", "mcp");
        opts.output_dir = temp.path().join("my-mcp");
        opts.bare_mcp = true;

        let result = ScaffoldEngine::scaffold("mcp", &opts);
        assert!(result.is_ok());

        assert!(opts.output_dir.join("server.json").exists());
        assert!(!opts.output_dir.join("manifest.yaml").exists());
    }

    #[test]
    fn test_scaffold_mcp_wrapper() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-mcp", "mcp");
        opts.output_dir = temp.path().join("my-mcp");
        opts.bare_mcp = false;

        let result = ScaffoldEngine::scaffold("mcp", &opts);
        assert!(result.is_ok());

        assert!(opts.output_dir.join("manifest.yaml").exists());
        assert!(!opts.output_dir.join("server.json").exists());
    }

    #[test]
    fn test_scaffold_general() {
        let temp = TempDir::new().unwrap();
        let mut opts = test_options("my-general", "general");
        opts.output_dir = temp.path().join("my-general");

        let result = ScaffoldEngine::scaffold("general", &opts);
        assert!(result.is_ok());

        assert!(opts.output_dir.join("manifest.yaml").exists());
        let manifest = std::fs::read_to_string(opts.output_dir.join("manifest.yaml")).unwrap();
        assert!(manifest.contains("extension_type: \"general\""));
    }

    #[test]
    fn test_unknown_type_fails() {
        let opts = test_options("x", "unknown");
        let result = ScaffoldEngine::scaffold("unknown", &opts);
        assert!(result.is_err());
    }
}
