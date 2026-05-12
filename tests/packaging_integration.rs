//! Full packaging integration test (Phase 7 — PKG-I1)
//!
//! End-to-end pipeline:
//!   export .agent → push → pull → import → create team → export .team → import team
//!
//! This test is marked `#[ignore]` because it requires the Python mock registry
//! server to be running. Start it first:
//!
//!   python e2e_tests/mock_registry/main.py --port 18765
//!
//! Then run:
//!
//!   cargo test --test packaging_integration -- --ignored

use pekobot::identity::{did::DIDScope, Identity};
use pekobot::portable::{
    export_team, import_team_with_base_dir, inspect_team, AgentManifest,
    AgentRegistry, ExportOptions, ImportOptions, Packager, TeamExportOptions, TeamImportOptions,
};
use pekobot::registry::{RegistryClient, RegistryConfig, RegistryManifest, RegistrySource};
use pekobot::types::agent::AgentConfig;
use std::path::Path;

// ── Helpers ──────────────────────────────────────────────────────────

/// Create a minimal agent directory structure for testing.
///
/// The identity directory is populated with a valid DID document and key export
/// so that `Unpackager::import()` can reconstruct the `Identity`.
async fn create_test_agent_dir(base: &Path) -> anyhow::Result<()> {
    // config/agent.toml
    let config_dir = base.join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    let agent_toml = r#"
name = "integration-agent"
description = "A test agent for full integration"
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

    // identity/ — generate a real Identity and write its files
    let identity = Identity::generate(DIDScope::Local, Some("integration"))?;
    let identity_dir = base.join("identity");
    tokio::fs::create_dir_all(&identity_dir).await?;

    let did_doc = identity.to_did_document()?;
    let did_json = serde_json::to_vec_pretty(&did_doc)?;
    tokio::fs::write(identity_dir.join("did.json"), did_json).await?;

    let keypair = identity.keypair.as_ref().unwrap();
    let key_export = keypair.export();
    let key_json = serde_json::to_vec(&key_export)?;
    tokio::fs::write(identity_dir.join("keys.enc"), key_json).await?;

    // skills/test-skill/SKILL.md
    let skills_dir = base.join("skills").join("test-skill");
    tokio::fs::create_dir_all(&skills_dir).await?;
    tokio::fs::write(
        skills_dir.join("SKILL.md"),
        "# Test Skill\n\nIntegration test skill.",
    )
    .await?;

    // workspace/SYSTEM.md
    let workspace_dir = base.join("workspace");
    tokio::fs::create_dir_all(&workspace_dir).await?;
    tokio::fs::write(
        workspace_dir.join("SYSTEM.md"),
        "# System Notes\n\nFor integration testing.",
    )
    .await?;

    Ok(())
}

/// Build a `.agent` package from a test directory using the canonical Packager.
async fn build_agent_package_from_dir(
    agent_dir: &Path,
    output_path: &Path,
) -> anyhow::Result<AgentManifest> {
    let config_path = agent_dir.join("config").join("agent.toml");
    let config_toml = tokio::fs::read_to_string(&config_path).await?;
    let config: AgentConfig = toml::from_str(&config_toml)?;

    let identity_dir = agent_dir.join("identity");
    let did_json = tokio::fs::read_to_string(identity_dir.join("did.json")).await?;
    let did_doc: pekobot::identity::DIDDocument = serde_json::from_str(&did_json)?;
    let keys_enc = tokio::fs::read(identity_dir.join("keys.enc")).await?;
    let key_export: pekobot::identity::KeyPairExport = serde_json::from_slice(&keys_enc)?;
    let identity = Identity::from_did_document_and_key(did_doc, key_export)?;

    let skills_dir = agent_dir.join("skills");
    let workspace_dir = agent_dir.join("workspace");

    let packager = Packager::new(config, identity, None)
        .with_skills_dir(&skills_dir)
        .with_workspace_dir(&workspace_dir);

    let export_opts = ExportOptions {
        output_path: Some(output_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    let _path = packager.export(export_opts).await?;

    // Inspect to get the manifest (with layers computed)
    let (manifest, _validation) = pekobot::portable::inspect_agent(output_path, None).await?;
    Ok(manifest)
}

/// Create a test registry config pointing at the mock server
fn test_registry_config(host: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: host.to_string(),
        priority: 1,
        auth: None,
    });
    config
}

/// Build a RegistryManifest JSON from an AgentManifest and its layers so that
/// RegistryClient::push() can load it from local storage.
fn build_registry_manifest(
    agent_manifest: &AgentManifest,
    host: &str,
    tag: &str,
) -> anyhow::Result<RegistryManifest> {
    let mut reg_manifest =
        RegistryManifest::new(&agent_manifest.agent.name, &agent_manifest.agent.version)
            .with_digest(
                agent_manifest
                    .layers
                    .as_ref()
                    .and_then(|l| l.config.as_ref())
                    .unwrap_or(&"sha256:unknown".to_string()),
            )
            .with_ref(format!("{host}/{tag}", tag = tag.replace(':', "_")));

    if let Some(layers) = &agent_manifest.layers {
        if let Some(digest) = &layers.config {
            reg_manifest.add_layer(pekobot::portable::Layer::new(
                digest.clone(),
                pekobot::portable::LayerType::Config,
                0,
            ));
        }
        if let Some(digest) = &layers.identity {
            reg_manifest.add_layer(pekobot::portable::Layer::new(
                digest.clone(),
                pekobot::portable::LayerType::Identity,
                0,
            ));
        }
        if let Some(digest) = &layers.skills {
            reg_manifest.add_layer(pekobot::portable::Layer::new(
                digest.clone(),
                pekobot::portable::LayerType::Skills,
                0,
            ));
        }
        if let Some(digest) = &layers.workspace {
            reg_manifest.add_layer(pekobot::portable::Layer::new(
                digest.clone(),
                pekobot::portable::LayerType::Workspace,
                0,
            ));
        }
    }

    Ok(reg_manifest)
}

/// Store a RegistryManifest JSON in the local registry so push() can find it.
async fn store_registry_manifest_local(
    registry: &AgentRegistry,
    manifest: &RegistryManifest,
) -> anyhow::Result<()> {
    let digest = pekobot::portable::ImageDigest::new(&manifest.digest)?;
    let reg_manifests_dir = registry
        .root_path()
        .join("registry_manifests")
        .join(digest.dir_name());
    tokio::fs::create_dir_all(&reg_manifests_dir).await?;
    tokio::fs::write(reg_manifests_dir.join("manifest.json"), manifest.to_json()?).await?;
    Ok(())
}

// ── The full integration test ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Python mock registry server on port 18765"]
async fn test_full_packaging_pipeline() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // ═════════════════════════════════════════════════════════════════
    // 1. EXPORT .agent from directory (canonical Packager path)
    // ═════════════════════════════════════════════════════════════════
    let agent_dir = base_dir.join("integration-agent");
    create_test_agent_dir(&agent_dir).await.unwrap();

    let package_path = base_dir.join("integration-agent.agent");
    let manifest = build_agent_package_from_dir(&agent_dir, &package_path)
        .await
        .unwrap();

    assert!(package_path.exists(), ".agent package should exist");
    assert!(manifest.layers.is_some());
    let layers = manifest.layers.clone().unwrap();
    assert!(layers.config.is_some());
    assert!(layers.identity.is_some());
    assert!(layers.skills.is_some());
    assert!(layers.workspace.is_some());

    // Store manifest in a local registry so push can resolve layers
    let build_registry_dir = base_dir.join("build_registry");
    let build_registry = AgentRegistry::new(&build_registry_dir);
    build_registry.init().await.unwrap();
    build_registry
        .store_manifest(&manifest, Some("integration-agent:v1.0"))
        .await
        .unwrap();

    // ═════════════════════════════════════════════════════════════════
    // 2. PUSH to mock registry
    // ═════════════════════════════════════════════════════════════════
    let host = "127.0.0.1:18765";
    let reg_manifest =
        build_registry_manifest(&manifest, host, "integration-agent:v1.0").unwrap();

    // Store the RegistryManifest JSON where push() expects it
    store_registry_manifest_local(&build_registry, &reg_manifest)
        .await
        .unwrap();

    let push_config = test_registry_config(host);
    let push_client = RegistryClient::new(push_config, build_registry.clone());

    let manifest_digest =
        pekobot::portable::ImageDigest::new(layers.config.as_ref().unwrap()).unwrap();

    let mut push_events = Vec::new();
    let push_result = push_client
        .push(
            &manifest_digest,
            &format!("{host}/integration-agent:v1.0"),
            |event| push_events.push(event),
        )
        .await;

    assert!(push_result.is_ok(), "Push failed: {:?}", push_result.err());
    let has_done = push_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Push should complete with Done event");

    // ═════════════════════════════════════════════════════════════════
    // 3. PULL from mock registry to a fresh local registry
    // ═════════════════════════════════════════════════════════════════
    let pull_registry_dir = base_dir.join("pull_registry");
    let pull_registry = AgentRegistry::new(&pull_registry_dir);
    pull_registry.init().await.unwrap();

    let pull_config = test_registry_config(host);
    let pull_client = RegistryClient::new(pull_config, pull_registry.clone());

    let mut pull_events = Vec::new();
    let pull_result = pull_client
        .pull(&format!("{host}/integration-agent:v1.0"), |event| {
            pull_events.push(event)
        })
        .await;

    assert!(pull_result.is_ok(), "Pull failed: {:?}", pull_result.err());
    let has_done = pull_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Pull should complete with Done event");

    // Verify layers were pulled
    assert!(pull_registry.has_layer(layers.config.as_ref().unwrap()));
    assert!(pull_registry.has_layer(layers.identity.as_ref().unwrap()));
    assert!(pull_registry.has_layer(layers.skills.as_ref().unwrap()));
    assert!(pull_registry.has_layer(layers.workspace.as_ref().unwrap()));

    // ═════════════════════════════════════════════════════════════════
    // 4. IMPORT .agent package
    // ═════════════════════════════════════════════════════════════════
    let import_base = base_dir.join("imported_agents");
    tokio::fs::create_dir_all(&import_base).await.unwrap();

    let unpackager =
        pekobot::portable::Unpackager::new(&package_path).with_base_dir(&import_base);

    let import_options = ImportOptions {
        new_name: Some("imported-agent".to_string()),
        rotate_keys: false,
        import_workspace: true,
        import_sessions: false,
        skip_validation: false,
        force: false,
        passphrase: None,
        team: None,
    };

    let import_result = unpackager.import(import_options).await.unwrap();
    assert_eq!(import_result.name, "imported-agent");
    assert!(import_result.config_path.exists());

    // ═════════════════════════════════════════════════════════════════
    // 5. CREATE TEAM with imported agent
    // ═════════════════════════════════════════════════════════════════
    let team_name = "integration-team";
    let team_dir = base_dir.join("teams").join(team_name);
    tokio::fs::create_dir_all(&team_dir).await.unwrap();

    let team_toml = format!(
        r#"
[team]
name = "{team_name}"
description = "Integration test team"

[[agents]]
name = "imported-agent"
image = "./imported-agent"
instances = 1
"#
    );
    tokio::fs::write(team_dir.join("team.toml"), team_toml)
        .await
        .unwrap();

    // Load the imported agent's config and create an identity for team export
    let imported_config_path = import_result.config_path.clone();
    let imported_config_toml = tokio::fs::read_to_string(&imported_config_path)
        .await
        .unwrap();
    let imported_config: AgentConfig = toml::from_str(&imported_config_toml).unwrap();

    let imported_identity = Identity::new("imported-agent", DIDScope::Local)
        .await
        .unwrap();

    let agents = vec![(
        "imported-agent".to_string(),
        imported_config,
        imported_identity,
    )];

    // ═════════════════════════════════════════════════════════════════
    // 6. EXPORT team to .team
    // ═════════════════════════════════════════════════════════════════
    let export_options = TeamExportOptions {
        output_path: Some(
            base_dir
                .join("integration-team.team")
                .to_string_lossy()
                .to_string(),
        ),
        include_sessions: false,
        include_workspace: true,
        include_mcp: false,
        description: Some("Full integration test team".to_string()),
    };

    let team_package_path = export_team(team_name, None, base_dir, agents, export_options)
        .await
        .unwrap();

    assert!(team_package_path.exists(), "Team package should exist");

    // Inspect team package
    let team_manifest = inspect_team(&team_package_path).await.unwrap();
    assert_eq!(team_manifest.team.name, team_name);
    assert_eq!(team_manifest.team.agent_count, 1);

    // Verify packaging metadata with checksums
    let packaging = team_manifest
        .packaging
        .expect("team manifest should have packaging metadata");
    assert!(!packaging.files.is_empty());
    assert!(!packaging.checksums.is_empty());
    assert!(packaging.files.contains(&"team/team.toml".to_string()));

    // ═════════════════════════════════════════════════════════════════
    // 7. IMPORT team to new location
    // ═════════════════════════════════════════════════════════════════
    let import_team_base = base_dir.join("imported_teams");
    tokio::fs::create_dir_all(&import_team_base).await.unwrap();

    let team_import_options = TeamImportOptions {
        new_name: Some("imported-team".to_string()),
        import_sessions: false,
        import_workspace: true,
        import_mcp: false,
        rotate_keys: false,
        force: false,
    };

    let team_import_result =
        import_team_with_base_dir(&team_package_path, &import_team_base, team_import_options)
            .await
            .unwrap();

    assert_eq!(team_import_result.name, "imported-team");
    assert_eq!(team_import_result.agent_count, 1);

    // Verify team.toml restored
    let restored_team_toml = import_team_base
        .join("teams")
        .join("imported-team")
        .join("team.toml");
    assert!(restored_team_toml.exists(), "team.toml should be restored");
    let restored_content = tokio::fs::read_to_string(&restored_team_toml)
        .await
        .unwrap();
    assert!(restored_content.contains("integration-team"));
    assert!(restored_content.contains("imported-agent"));

    // Verify agent config restored (Unpackager::save_config stores as agents/{name}/config.toml)
    // Note: identity is stored via KeyStorage (global), not in the agent directory
    let restored_agent_dir = import_team_base
        .join("teams")
        .join("imported-team")
        .join("agents")
        .join("imported-agent");
    assert!(
        restored_agent_dir.join("config.toml").exists(),
        "agent config.toml should be restored"
    );
}

// ── Additional Phase 7 integration tests ─────────────────────────────

/// Test that an exported .agent can be inspected and imported without registry
#[tokio::test]
async fn test_export_then_import_roundtrip() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // Create source agent directory
    let agent_dir = base_dir.join("roundtrip-agent");
    create_test_agent_dir(&agent_dir).await.unwrap();

    // Export using Packager
    let package_path = base_dir.join("roundtrip-agent.agent");
    let manifest = build_agent_package_from_dir(&agent_dir, &package_path)
        .await
        .unwrap();

    // Inspect
    let info = pekobot::portable::get_package_info(&package_path)
        .await
        .unwrap();
    assert_eq!(info.name, "integration-agent");
    assert!(info.valid);

    // Import
    let import_base = base_dir.join("imported");
    tokio::fs::create_dir_all(&import_base).await.unwrap();

    let unpackager =
        pekobot::portable::Unpackager::new(&package_path).with_base_dir(&import_base);

    let import_result = unpackager
        .import(ImportOptions {
            new_name: Some("roundtrip-imported".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(import_result.name, "roundtrip-imported");
    assert!(import_result.config_path.exists());
}

/// Test that AgentManifest has no dead fields (clean manifest verification)
#[tokio::test]
async fn test_clean_manifest_has_no_capabilities_tools_mcp() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    let agent_dir = base_dir.join("clean-agent");
    create_test_agent_dir(&agent_dir).await.unwrap();

    let package_path = base_dir.join("clean-agent.agent");
    let manifest = build_agent_package_from_dir(&agent_dir, &package_path)
        .await
        .unwrap();

    // Clean manifest: these fields should not exist on AgentManifest
    // We verify by serializing to TOML and checking the output
    let toml_str = manifest.to_toml().unwrap();
    assert!(
        !toml_str.contains("capabilities"),
        "Manifest should not contain 'capabilities'"
    );
    assert!(
        !toml_str.contains("tool_sources"),
        "Manifest should not contain 'tool_sources'"
    );
    assert!(
        !toml_str.contains("\ntools ="),
        "Manifest should not contain a top-level 'tools' field"
    );
    assert!(
        !toml_str.contains("\nmcp ="),
        "Manifest should not contain a top-level 'mcp' field"
    );

    // But it SHOULD have layers
    assert!(
        toml_str.contains("layers"),
        "Manifest should contain 'layers'"
    );
}
