//! Extension Validation Service
//!
//! Provides ADR-024 two-tier extension manifest validation:
//! Tier 1: Ecosystem standards (SKILL.md, server.json)
//! Tier 2: Unified manifest (manifest.yaml with `extension_type`)

use std::path::Path;

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
    pub async fn validate(path: &Path, verbose: bool) -> anyhow::Result<ValidationReport> {
        use crate::extension::adapters::extract_extension_type_from_yaml;
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
                        }
                        "gateway" => {
                            if verbose {
                                println!("  ✓ Gateway extension validated");
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
        // Create a minimal skill directory structure that discover_skills can find
        let skill_dir = temp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nid: test-skill\nname: Test Skill\ndescription: A test skill\n---\n# Test\n",
        )
        .unwrap();
        // Also write a SKILL.md at the root so Tier 1 detection triggers
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
