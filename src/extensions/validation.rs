//! Extension Validation Service
//!
//! Provides ADR-024 two-tier extension manifest validation:
//! Tier 1: Ecosystem standards (SKILL.md, server.json)
//! Tier 2: Unified manifest (manifest.yaml with `extension_type`)
//!
//! ADR-036 adds semantic validation depth levels:
//! - Static: syntax and required fields only
//! - Semantic: + referenced files exist, commands in $PATH, schemas valid
//! - Functional: + spawn runtime and verify health check (future)

use std::path::Path;

/// Validation depth level (ADR-036)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Default)]
pub enum ValidationDepth {
    /// Syntax and required fields only (default)
    #[default]
    Static,
    /// + Referenced files exist, commands are in $PATH, schemas are valid
    Semantic,
    /// + Spawn runtime and verify health check (future)
    Functional,
}

impl ValidationDepth {
    pub fn from_flags(semantic: bool, _functional: bool) -> Self {
        if semantic {
            Self::Semantic
        } else {
            Self::Static
        }
    }
}

/// Result of validating an extension directory or manifest
#[derive(Debug, Clone)]
pub struct ValidationReport {
    /// The detected extension type
    pub detected_type: String,
    /// Validation errors (fatal)
    pub errors: Vec<String>,
    /// Validation warnings (non-fatal)
    pub warnings: Vec<String>,
}

/// Service for validating extension manifests
pub struct ExtensionValidationService;

impl ExtensionValidationService {
    /// Validate an extension at the given path
    ///
    /// Uses the ADR-024 two-tier detection hierarchy:
    /// Tier 1: Ecosystem standards (SKILL.md, server.json)
    /// Tier 2: Unified manifest (manifest.yaml with `extension_type`)
    pub async fn validate(
        path: &Path,
        verbose: bool,
    ) -> anyhow::Result<ValidationReport> {
        Self::validate_with_depth(path, verbose, ValidationDepth::Static).await
    }

    /// Validate with explicit depth level (ADR-036)
    pub async fn validate_with_depth(
        path: &Path,
        verbose: bool,
        depth: ValidationDepth,
    ) -> anyhow::Result<ValidationReport> {
        use crate::extensions::framework::adapters::extract_extension_type_from_yaml;
        use crate::extensions::general::discover_general_extensions;
        use crate::extensions::mcp::McpAdapter;
        use crate::extensions::skill::SkillAdapter;
        use crate::extensions::universal::UniversalToolAdapter;

        if !path.exists() {
            anyhow::bail!("Path does not exist: {}", path.display());
        }

        let mut errors = Vec::new();
        let mut warnings = Vec::new();

        // ─── TIER 1: Ecosystem Standards ─────────────────────────────────────────

        if path.join("SKILL.md").exists() {
            if verbose {
                println!("✓ Detected as: skill extension (SKILL.md) [Tier 1 ecosystem standard]");
            }

            let skill_adapter = SkillAdapter::new();
            let skills = skill_adapter.discover_skills(path);
            if skills.is_empty() {
                errors.push("No valid skills found in directory".to_string());
            } else if verbose {
                for skill in &skills {
                    println!(
                        "  ✓ Skill: {} - {}",
                        skill.manifest.name, skill.manifest.description
                    );
                }
            }

            if depth >= ValidationDepth::Semantic {
                Self::semantic_check_skill(path, &mut errors, &mut warnings, verbose);
            }

            return Ok(ValidationReport {
                detected_type: "skill".to_string(),
                errors,
                warnings,
            });
        }

        if path.join("server.json").exists() {
            if verbose {
                println!(
                    "✓ Detected as: MCP server extension (server.json) [Tier 1 ecosystem standard]"
                );
            }

            let server_json_path = path.join("server.json");
            match std::fs::read_to_string(&server_json_path) {
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(manifest) => {
                        if manifest.get("name").is_none() {
                            warnings.push("server.json missing 'name' field".to_string());
                        }
                        if verbose {
                            if let Some(name) = manifest.get("name").and_then(|v| v.as_str()) {
                                println!("  ✓ Server: {}", name);
                            }
                        }

                        if depth >= ValidationDepth::Semantic {
                            Self::semantic_check_server_json(
                                path,
                                &manifest,
                                &mut errors,
                                &mut warnings,
                                verbose,
                            );
                        }
                    }
                    Err(e) => errors.push(format!("Invalid server.json: {}", e)),
                },
                Err(e) => errors.push(format!("Failed to read server.json: {}", e)),
            }

            return Ok(ValidationReport {
                detected_type: "mcp".to_string(),
                errors,
                warnings,
            });
        }

        // ─── TIER 2: Unified Manifest ────────────────────────────────────────────

        let manifest_yaml = path.join("manifest.yaml");
        if manifest_yaml.exists() {
            match extract_extension_type_from_yaml(&manifest_yaml) {
                Ok(Some(ext_type)) => {
                    if verbose {
                        println!(
                            "✓ Detected as: {} extension (manifest.yaml) [Tier 2 unified manifest]",
                            ext_type
                        );
                    }

                    match ext_type.as_str() {
                        "universal-tool" => {
                            let adapter = UniversalToolAdapter::new();
                            let tools = adapter.discover_tools(path).await;
                            if tools.is_empty() {
                                errors.push("No valid tools found in directory".to_string());
                            } else if verbose {
                                for tool in &tools {
                                    println!(
                                        "  ✓ Tool: {} - {}",
                                        tool.manifest.name, tool.manifest.description
                                    );
                                }
                            }

                            if depth >= ValidationDepth::Semantic {
                                Self::semantic_check_universal_tool(
                                    path,
                                    &mut errors,
                                    &mut warnings,
                                    verbose,
                                );
                            }
                        }
                        "mcp" => {
                            let adapter = McpAdapter::with_default_manager();
                            let servers = adapter.discover_servers(path).await;
                            if servers.is_empty() {
                                errors.push("No valid MCP servers found in directory".to_string());
                            } else if verbose {
                                for server in &servers {
                                    println!("  ✓ Server: {}", server.manifest.name);
                                }
                            }

                            if depth >= ValidationDepth::Semantic {
                                Self::semantic_check_mcp_wrapper(
                                    path,
                                    &mut errors,
                                    &mut warnings,
                                    verbose,
                                );
                            }
                        }
                        "gateway" => {
                            if verbose {
                                println!("  ✓ Gateway extension validated");
                            }

                            if depth >= ValidationDepth::Semantic {
                                Self::semantic_check_gateway(
                                    path,
                                    &mut errors,
                                    &mut warnings,
                                    verbose,
                                );
                            }
                        }
                        "general" => {
                            let extensions = discover_general_extensions(path).await?;
                            if extensions.is_empty() {
                                errors.push(
                                    "No valid general extensions found in directory".to_string(),
                                );
                            } else if verbose {
                                for ext in &extensions {
                                    println!(
                                        "  ✓ Extension: {} - {}",
                                        ext.manifest.name, ext.manifest.description
                                    );
                                }
                            }

                            if depth >= ValidationDepth::Semantic {
                                Self::semantic_check_general(
                                    path,
                                    &mut errors,
                                    &mut warnings,
                                    verbose,
                                );
                            }
                        }
                        custom if custom.starts_with("custom:") => {
                            if verbose {
                                println!("  ✓ Custom extension type: {}", custom);
                            }
                        }
                        other => {
                            warnings.push(format!(
                                "Unknown extension_type '{}'. Supported: universal-tool, mcp, gateway, general, custom:*",
                                other
                            ));
                        }
                    }

                    return Ok(ValidationReport {
                        detected_type: ext_type,
                        errors,
                        warnings,
                    });
                }
                Ok(None) => {
                    // manifest.yaml exists but has no extension_type
                }
                Err(e) => {
                    warnings.push(format!("Failed to parse manifest.yaml: {}", e));
                }
            }
        }

        // Nothing detected
        anyhow::bail!(
            "Could not detect extension type. Expected one of:\n\
             - SKILL.md (skill extension) [Tier 1]\n\
             - server.json (bare MCP server) [Tier 1]\n\
             - manifest.yaml with extension_type (unified manifest) [Tier 2]"
        );
    }

    // =========================================================================
    // Semantic Validation Checks (ADR-036)
    // =========================================================================

    fn semantic_check_skill(
        path: &Path,
        errors: &mut Vec<String>,
        _warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        let skill_md = path.join("SKILL.md");
        match std::fs::read_to_string(&skill_md) {
            Ok(content) => {
                // Check for frontmatter delimiters
                if !content.starts_with("---") {
                    errors.push(
                        "SKILL.md missing YAML frontmatter delimiters (must start with ---)"
                            .to_string(),
                    );
                }
                // Check for at least one heading
                if !content.contains("# ") {
                    errors.push(
                        "SKILL.md has no Markdown headings — add at least one # heading".to_string(),
                    );
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read SKILL.md for semantic check: {}", e));
            }
        }
    }

    fn semantic_check_server_json(
        _path: &Path,
        manifest: &serde_json::Value,
        _errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        // Check required fields
        if manifest.get("transport").is_none() {
            warnings.push("server.json missing 'transport' field".to_string());
        }

        // If transport.command is specified, check it's in PATH
        if let Some(transport) = manifest.get("transport") {
            if let Some(cmd) = transport.get("command").and_then(|v| v.as_str()) {
                let cmd_name = cmd.split_whitespace().next().unwrap_or(cmd);
                if !Self::is_in_path(cmd_name) {
                    warnings.push(format!(
                        "Transport command '{}' not found in PATH — extension may not work on this machine",
                        cmd_name
                    ));
                }
            }
        }
    }

    fn semantic_check_universal_tool(
        path: &Path,
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        let manifest_path = path.join("manifest.yaml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(content) => {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    // Check for parameters schema
                    if yaml.get("parameters").is_none() {
                        warnings.push(
                            "universal-tool manifest missing 'parameters' schema".to_string(),
                        );
                    }

                    // Check if a handler file exists
                    let handler_names = ["handler.py", "handler.js", "handler.sh", "handler"];
                    let has_handler = handler_names.iter().any(|name| path.join(name).exists());
                    if !has_handler {
                        warnings.push(
                            "No handler file found (expected handler.py, handler.js, or handler)"
                                .to_string(),
                        );
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read manifest.yaml for semantic check: {}", e));
            }
        }
    }

    fn semantic_check_mcp_wrapper(
        path: &Path,
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        let manifest_path = path.join("manifest.yaml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(content) => {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    if yaml.get("mcp_servers").is_none() {
                        warnings.push(
                            "MCP wrapper manifest missing 'mcp_servers' section".to_string(),
                        );
                    }

                    // Check commands in mcp_servers
                    if let Some(servers) = yaml.get("mcp_servers").and_then(|v| v.as_mapping()) {
                        for (name, config) in servers {
                            let server_name = name.as_str().unwrap_or("unknown");
                            if let Some(cmd) = config.get("command").and_then(|v| v.as_str()) {
                                let cmd_name = cmd.split_whitespace().next().unwrap_or(cmd);
                                if !Self::is_in_path(cmd_name) {
                                    warnings.push(format!(
                                        "MCP server '{}' command '{}' not found in PATH",
                                        server_name, cmd_name
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read manifest.yaml for semantic check: {}", e));
            }
        }
    }

    fn semantic_check_gateway(
        path: &Path,
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        let manifest_path = path.join("manifest.yaml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(content) => {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    let gateway_type = yaml
                        .get("gateway_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let known_types = [
                        "http",
                        "websocket",
                        "pubsub",
                        "grpc",
                        "cli",
                        "custom",
                        "out-of-process",
                        "external",
                    ];
                    if !known_types.contains(&gateway_type) {
                        warnings.push(format!(
                            "Unknown gateway_type '{}'. Known types: {}",
                            gateway_type,
                            known_types.join(", ")
                        ));
                    }

                    // For out-of-process, check command exists
                    if gateway_type == "out-of-process" {
                        if let Some(config) = yaml.get("config") {
                            if let Some(cmd) = config.get("command").and_then(|v| v.as_str()) {
                                let cmd_name = cmd.split_whitespace().next().unwrap_or(cmd);
                                if !Self::is_in_path(cmd_name) {
                                    warnings.push(format!(
                                        "Gateway command '{}' not found in PATH",
                                        cmd_name
                                    ));
                                }
                            }

                            // Check for handler file
                            let handler_names = ["gateway.py", "gateway.js", "gateway.sh", "gateway"];
                            let has_handler = handler_names.iter().any(|name| path.join(name).exists());
                            if !has_handler {
                                warnings.push(
                                    "No gateway handler file found (expected gateway.py, gateway.js, or gateway)"
                                        .to_string(),
                                );
                            }
                        }
                    }

                    // For external, check endpoint is valid URL
                    if gateway_type == "external" {
                        if let Some(config) = yaml.get("config") {
                            if let Some(endpoint) = config.get("endpoint").and_then(|v| v.as_str()) {
                                if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                                    errors.push(format!(
                                        "External gateway endpoint '{}' must be an HTTP(S) URL",
                                        endpoint
                                    ));
                                }
                            } else {
                                errors.push(
                                    "External gateway missing 'config.endpoint' field".to_string(),
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read manifest.yaml for semantic check: {}", e));
            }
        }
    }

    fn semantic_check_general(
        path: &Path,
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        _verbose: bool,
    ) {
        let manifest_path = path.join("manifest.yaml");
        match std::fs::read_to_string(&manifest_path) {
            Ok(content) => {
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
                    let known_hooks = [
                        "prompt.system_section",
                        "prompt.pre_process",
                        "prompt.post_process",
                        "tool.register",
                        "tool.execute",
                        "tool.execute_async",
                        "tool.check_status",
                        "tool.cancel",
                        "tool.result_transform",
                        "session.state_change",
                        "session.compaction",
                        "session.context_build",
                        "channel.input",
                        "channel.output",
                        "message.pre_send",
                        "message.post_receive",
                        "event.subscribe",
                        "event.emit",
                        "agent.init",
                        "agent.shutdown",
                        "agent.iteration",
                    ];

                    if let Some(hooks) = yaml.get("hooks").and_then(|v| v.as_sequence()) {
                        for (i, hook) in hooks.iter().enumerate() {
                            if let Some(point) = hook.get("point").and_then(|v| v.as_str()) {
                                let base_point = if point.starts_with("event.subscribe.") {
                                    "event.subscribe"
                                } else if point.starts_with("prompt.") {
                                    "prompt.system_section"
                                } else if point.starts_with("tool.execute.") {
                                    "tool.execute"
                                } else {
                                    point
                                };

                                if !known_hooks.contains(&base_point) {
                                    warnings.push(format!(
                                        "Hook #{} has unknown point '{}'. Known: {}",
                                        i + 1,
                                        point,
                                        known_hooks.join(", ")
                                    ));
                                }
                            } else {
                                errors.push(format!("Hook #{} missing 'point' field", i + 1));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                errors.push(format!("Failed to read manifest.yaml for semantic check: {}", e));
            }
        }
    }

    /// Check if a command exists in PATH
    fn is_in_path(cmd: &str) -> bool {
        if which::which(cmd).is_ok() {
            return true;
        }
        // Also check common interpreter prefixes
        for prefix in &["python3", "python", "node", "npx", "npm", "deno", "bun"] {
            if cmd == *prefix {
                return which::which(cmd).is_ok();
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_validate_nonexistent_path() {
        let result = ExtensionValidationService::validate(Path::new("/nonexistent"), false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_empty_directory() {
        let temp = TempDir::new().unwrap();
        let result = ExtensionValidationService::validate(temp.path(), false).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_skill_tier1() {
        let temp = TempDir::new().unwrap();
        let skill_dir = temp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nid: test-skill\nname: Test Skill\ndescription: A test skill\n---\n# Test\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("SKILL.md"),
            "---\nid: test\nname: Test Skill\ndescription: A test skill\n---\n# Test\n",
        )
        .unwrap();

        let report = ExtensionValidationService::validate(temp.path(), false)
            .await
            .unwrap();
        assert_eq!(report.detected_type, "skill");
    }

    #[tokio::test]
    async fn test_validate_skill_semantic_missing_frontmatter() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("SKILL.md"),
            "# No frontmatter here\n",
        )
        .unwrap();

        let report = ExtensionValidationService::validate_with_depth(
            temp.path(),
            false,
            ValidationDepth::Semantic,
        )
        .await
        .unwrap();
        assert_eq!(report.detected_type, "skill");
        assert!(
            report.errors.iter().any(|e| e.contains("frontmatter")),
            "Expected frontmatter error, got: {:?}",
            report.errors
        );
    }

    #[tokio::test]
    async fn test_validate_server_json_tier1() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("server.json"),
            r#"{"name": "test-server", "version": "1.0.0"}"#,
        )
        .unwrap();

        let report = ExtensionValidationService::validate(temp.path(), false)
            .await
            .unwrap();
        assert_eq!(report.detected_type, "mcp");
        assert!(report.errors.is_empty());
    }

    #[tokio::test]
    async fn test_validate_universal_tool_tier2() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("manifest.yaml"),
            "id: test\nname: Test Tool\nextension_type: universal-tool\n",
        )
        .unwrap();

        let report = ExtensionValidationService::validate(temp.path(), false)
            .await
            .unwrap();
        assert_eq!(report.detected_type, "universal-tool");
    }

    #[tokio::test]
    async fn test_validate_universal_tool_semantic_missing_handler() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("manifest.yaml"),
            "id: test\nname: Test Tool\nextension_type: universal-tool\n",
        )
        .unwrap();

        let report = ExtensionValidationService::validate_with_depth(
            temp.path(),
            false,
            ValidationDepth::Semantic,
        )
        .await
        .unwrap();
        assert_eq!(report.detected_type, "universal-tool");
        assert!(
            report.warnings.iter().any(|w| w.contains("handler")),
            "Expected handler warning, got: {:?}",
            report.warnings
        );
    }

    #[tokio::test]
    async fn test_validate_gateway_semantic_unknown_type() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("manifest.yaml"),
            r#"id: test
name: Test Gateway
extension_type: gateway
gateway_type: unknown-thing
config:
  command: "node"
  args: ["gateway.js"]
"#,
        )
        .unwrap();

        let report = ExtensionValidationService::validate_with_depth(
            temp.path(),
            false,
            ValidationDepth::Semantic,
        )
        .await
        .unwrap();
        assert_eq!(report.detected_type, "gateway");
        assert!(
            report.warnings.iter().any(|w| w.contains("gateway_type")),
            "Expected gateway_type warning, got: {:?}",
            report.warnings
        );
    }

    #[tokio::test]
    async fn test_validate_unknown_extension_type() {
        let temp = TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("manifest.yaml"),
            "id: test\nname: Test\nextension_type: unknown-type\n",
        )
        .unwrap();

        let report = ExtensionValidationService::validate(temp.path(), false)
            .await
            .unwrap();
        assert_eq!(report.detected_type, "unknown-type");
        assert!(!report.warnings.is_empty());
    }
}
