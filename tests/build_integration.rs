//! Integration tests for `pekobot agent build`
//!
//! End-to-end: create temp agent directory → build → verify `.agent` structure

use std::path::Path;

/// Create a minimal agent directory structure for testing
async fn create_test_agent_dir(base: &Path) -> anyhow::Result<()> {
    // config/agent.toml
    let config_dir = base.join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    let agent_toml = r#"
name = "test-agent"
description = "A test agent for build integration"
auto_accept_trusted = false
default_timeout_seconds = 300

[provider]
provider_type = "open_a_i"
api_key = "sk-test"
default_model = "gpt-4"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.gpt-4]
name = "gpt-4"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0
"#;
    tokio::fs::write(config_dir.join("agent.toml"), agent_toml).await?;

    // identity/did.json
    let identity_dir = base.join("identity");
    tokio::fs::create_dir_all(&identity_dir).await?;
    tokio::fs::write(
        identity_dir.join("did.json"),
        r#"{"id":"did:pekobot:build-test"}"#,
    )
    .await?;

    // skills/test-skill/SKILL.md
    let skills_dir = base.join("skills").join("test-skill");
    tokio::fs::create_dir_all(&skills_dir).await?;
    tokio::fs::write(
        skills_dir.join("SKILL.md"),
        "# Test Skill\n\nA skill for testing.",
    )
    .await?;

    // workspace/SYSTEM.md
    let workspace_dir = base.join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    tokio::fs::write(workspace_dir.join("SYSTEM.md"), "# System Notes").await?;

    Ok(())
}

#[tokio::test]
async fn test_build_produces_valid_agent_package() {
    let temp_dir = tempfile::tempdir().unwrap();
    let agent_dir = temp_dir.path().join("test-agent");
    create_test_agent_dir(&agent_dir).await.unwrap();

    let registry_dir = temp_dir.path().join("registry");
    let registry = pekobot::portable::AgentRegistry::new(&registry_dir);
    registry.init().await.unwrap();

    let result = pekobot::portable::AgentBuilder::build_from_directory(
        &agent_dir,
        "test-agent:v1.0",
        &registry,
        |_| {},
    )
    .await
    .unwrap();

    // ── Basic result checks ──────────────────────────────────────────
    assert_eq!(result.tag, "test-agent:v1.0");
    assert!(result.layer_count >= 3); // config, identity, skills, workspace
    assert!(result.total_size_bytes > 0);
    assert!(result.manifest_digest.starts_with("sha256:"));

    // ── Package file exists ──────────────────────────────────────────
    assert!(
        result.package_path.exists(),
        ".agent package should exist at {}",
        result.package_path.display()
    );

    // ── Manifest checks ──────────────────────────────────────────────
    let manifest = result.manifest;
    assert_eq!(manifest.agent.name, "test-agent");
    assert_eq!(
        manifest.agent.description,
        Some("A test agent for build integration".to_string())
    );

    // Clean manifest: no capabilities, tools, mcp, tool_sources
    // (These fields don't exist on AgentManifest, so this is implicitly verified)

    // ── Layer checks ─────────────────────────────────────────────────
    let layers = manifest.layers.expect("manifest should have layers");
    assert!(layers.config.is_some(), "config layer should exist");
    assert!(layers.identity.is_some(), "identity layer should exist");
    assert!(layers.skills.is_some(), "skills layer should exist");
    assert!(layers.workspace.is_some(), "workspace layer should exist");
    assert!(layers.sessions.is_none(), "sessions layer should be absent");
    assert!(layers.mcp.is_none(), "mcp layer should be absent");

    // ── Registry checks ──────────────────────────────────────────────
    // Manifest stored by tag
    let by_tag = registry
        .get_manifest_by_tag("test-agent:v1.0")
        .await
        .unwrap();
    assert_eq!(by_tag.agent.name, "test-agent");

    // Layers stored
    assert!(registry.has_layer(layers.config.as_ref().unwrap()));
    assert!(registry.has_layer(layers.identity.as_ref().unwrap()));
    assert!(registry.has_layer(layers.skills.as_ref().unwrap()));
    assert!(registry.has_layer(layers.workspace.as_ref().unwrap()));

    // ── Package can be inspected ─────────────────────────────────────
    let info = pekobot::portable::get_package_info(&result.package_path)
        .await
        .unwrap();
    assert_eq!(info.name, "test-agent");
    assert!(info.valid);
}

#[tokio::test]
async fn test_build_missing_config_agent_toml_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    let agent_dir = temp_dir.path().join("bad-agent");
    tokio::fs::create_dir_all(&agent_dir).await.unwrap();
    // No config/agent.toml

    let registry = pekobot::portable::AgentRegistry::new(temp_dir.path().join("registry"));
    registry.init().await.unwrap();

    let result = pekobot::portable::AgentBuilder::build_from_directory(
        &agent_dir,
        "bad:v1",
        &registry,
        |_| {},
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("config/agent.toml"));
}

#[tokio::test]
async fn test_build_layer_deduplication() {
    let temp_dir = tempfile::tempdir().unwrap();
    let agent_dir = temp_dir.path().join("agent");
    create_test_agent_dir(&agent_dir).await.unwrap();

    let registry = pekobot::portable::AgentRegistry::new(temp_dir.path().join("registry"));
    registry.init().await.unwrap();

    // Build twice with the same source
    let result1 = pekobot::portable::AgentBuilder::build_from_directory(
        &agent_dir,
        "agent:v1",
        &registry,
        |_| {},
    )
    .await
    .unwrap();

    let result2 = pekobot::portable::AgentBuilder::build_from_directory(
        &agent_dir,
        "agent:v2",
        &registry,
        |_| {},
    )
    .await
    .unwrap();

    // Same source → same layer digests
    let layers1 = result1.manifest.layers.unwrap();
    let layers2 = result2.manifest.layers.unwrap();
    assert_eq!(layers1.config, layers2.config);
    assert_eq!(layers1.identity, layers2.identity);
    assert_eq!(layers1.skills, layers2.skills);
    assert_eq!(layers1.workspace, layers2.workspace);
}
