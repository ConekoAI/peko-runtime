//! Full packaging integration test (Phase 7 — PKG-I1)
//!
//! End-to-end pipeline:
//!   export .agent → push → pull → import → create team → export .team → import team
//!
//! This test is marked `#[ignore]` because it requires:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!
//! Run:
//!   cd peko-runtime
//!   cargo test --test packaging_integration -- --ignored

use pekobot::identity::{did::DIDScope, Identity};
use pekobot::portable::manifest::AgentLayers;
use pekobot::portable::{
    export_team, import_team_with_base_dir, inspect_team, AgentManifest, ExportOptions,
    ImportOptions, Packager, TeamExportOptions, TeamImportOptions,
};
use pekobot::registry::{
    AgentRegistry, RegistryClient, RegistryConfig, RegistryManifest, RegistrySource,
};
use pekobot::types::agent::AgentConfig;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

mod common;
use common::{create_test_user, reset_pekohub, PekohubBackend};
use serial_test::serial;

// ── Helpers ──────────────────────────────────────────────────────────

/// Create a minimal agent directory structure for testing.
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
provider_type = "openai"
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

/// Create a test registry config pointing at the hub server
fn test_registry_config(url: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: url.to_string(),
        priority: 1,
        auth: None,
        token: None,
    });
    config
}

/// Build a RegistryManifest JSON from an AgentManifest and its layers so that
/// RegistryClient::push() can load it from local storage.
///
/// The `ref_str` should be the full registry ref (e.g., "host/ns/name:tag").
fn build_registry_manifest(
    agent_manifest: &AgentManifest,
    manifest_digest: &str,
    ref_str: &str,
) -> anyhow::Result<RegistryManifest> {
    let mut reg_manifest =
        RegistryManifest::new(&agent_manifest.agent.name, &agent_manifest.agent.version)
            .with_digest(manifest_digest)
            .with_ref(ref_str.to_string());

    if let Some(layers) = &agent_manifest.layers {
        if let Some(digest) = &layers.config {
            // The OCI Image Manifest spec requires the top-level `config`
            // descriptor (digest + size + mediaType) to describe the
            // config blob. Without this, pekohub rejects the manifest
            // with `MANIFEST_INVALID / config.digest: Invalid digest
            // format` (because the descriptor is left at its default
            // empty digest). The size is the byte length of the gzipped
            // tarball we stored under that digest — see
            // `store_agent_layers_in_registry` for the encoder.
            reg_manifest = reg_manifest.with_config(digest.clone(), 0_u64, None::<String>);
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

/// Extract a `.agent` package and store its layer tarballs in the registry.
async fn store_agent_layers_in_registry(
    package_path: &Path,
    registry: &AgentRegistry,
    layers: &AgentLayers,
) -> anyhow::Result<()> {
    // Extract the .agent package (tar.gz)
    let file = std::fs::File::open(package_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut files: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let path_str = path.to_string_lossy().to_string();
        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;
        files.insert(path_str, content);
    }

    // Layer prefixes matching Packager::compute_layers
    let layer_prefixes = [
        (layers.config.as_ref(), "config"),
        (layers.identity.as_ref(), "identity"),
        (layers.skills.as_ref(), "skills"),
        (layers.workspace.as_ref(), "workspace"),
        (layers.sessions.as_ref(), "sessions"),
        (layers.mcp.as_ref(), "mcp"),
    ];

    for (expected_digest, prefix) in layer_prefixes {
        let Some(expected_digest) = expected_digest else {
            continue;
        };

        // Collect files for this layer
        let mut layer_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for (path, content) in &files {
            if path.starts_with(&format!("{prefix}/")) {
                let layer_path = path.strip_prefix(&format!("{prefix}/")).unwrap_or(path);
                layer_files.insert(layer_path.to_string(), content.clone());
            }
        }

        if layer_files.is_empty() {
            continue;
        }

        // Build gzipped tarball matching Packager::build_layer_digest
        let mut buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            for (path, content) in &layer_files {
                let mut header = tar::Header::new_gnu();
                header.set_path(path)?;
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append(&header, content.as_slice())?;
            }
            tar.finish()?;
        }

        // Verify digest matches
        let computed_digest = pekobot::portable::types::compute_digest(&buf);
        if computed_digest != *expected_digest {
            anyhow::bail!(
                "Layer digest mismatch for {prefix}: expected {expected_digest}, got {computed_digest}"
            );
        }

        // Store in registry
        registry.store_layer(expected_digest, &buf).await?;
    }

    Ok(())
}

// ── The full integration test ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_full_packaging_pipeline() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create user with namespace "ns" — PekoHub requires namespace ownership for pushes
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    // Full registry ref with namespace (PekoHub format: {host}/{namespace}/{name}:{tag})
    let registry_ref = format!("{}/ns/integration-agent:v1.0", backend.url);

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

    // Store manifest and layers in a local registry so push can resolve them
    let build_registry_dir = base_dir.join("build_registry");
    let build_registry = AgentRegistry::new(&build_registry_dir);
    build_registry.init().await.unwrap();
    let manifest_digest = build_registry
        .store_manifest(&manifest, Some("integration-agent:v1.0"))
        .await
        .unwrap();

    // Reconstruct and store layer tarballs so push_layer can read them
    store_agent_layers_in_registry(&package_path, &build_registry, &layers)
        .await
        .unwrap();

    // ═════════════════════════════════════════════════════════════════
    // 2. PUSH to registry
    // ═════════════════════════════════════════════════════════════════
    let reg_manifest =
        build_registry_manifest(&manifest, manifest_digest.as_str(), &registry_ref).unwrap();

    // Store the RegistryManifest JSON where push() expects it
    store_registry_manifest_local(&build_registry, &reg_manifest)
        .await
        .unwrap();

    let push_config = test_registry_config(&backend.url);
    let push_client = RegistryClient::new(push_config, build_registry.clone());

    let mut push_events = Vec::new();
    let push_result = push_client
        .push(&manifest_digest, &registry_ref, |event| push_events.push(event))
        .await;

    assert!(push_result.is_ok(), "Push failed: {:?}", push_result.err());
    let has_done = push_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Push should complete with Done event");

    // ═════════════════════════════════════════════════════════════════
    // 3. PULL from registry to a fresh local registry
    // ═════════════════════════════════════════════════════════════════
    let pull_registry_dir = base_dir.join("pull_registry");
    let pull_registry = AgentRegistry::new(&pull_registry_dir);
    pull_registry.init().await.unwrap();

    let pull_config = test_registry_config(&backend.url);
    let pull_client = RegistryClient::new(pull_config, pull_registry.clone());

    let mut pull_events = Vec::new();
    let pull_result = pull_client
        .pull(&registry_ref, |event| pull_events.push(event))
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

    let unpackager = pekobot::portable::Unpackager::new(&package_path).with_base_dir(&import_base);

    let import_options = ImportOptions {
        new_name: Some("imported-agent".to_string()),
        rotate_keys: false,
        import_workspace: true,
        import_sessions: false,
        skip_validation: false,
        force: false,
        passphrase: None,
        team: None,
        // Issue #14: signature verification is now enforced on import.
        // The integration pipeline produces packages via the canonical
        // Packager, which signs the manifest, so verification passes
        // without an opt-in.
        allow_unsigned: false,
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
        // Issue #14: signature verification is now enforced. Packages
        // built by the canonical Packager are signed, so this stays
        // at the secure default.
        allow_unsigned: false,
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

    // Verify agent config restored
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