//! Full packaging integration test (Phase 7 — PKG-I1, Principal-era).
//!
//! End-to-end pipeline:
//!   export .principal → push → pull → import
//!
//! This test is marked `#[ignore]` because it requires:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!
//! Run:
//!   cd peko-runtime
//!   cargo test --test packaging_integration -- --ignored
//!
//! ## Principal-era translation
//!
//! After the "Principal as the single actor" migration, the `.agent`
//! packaging surface was replaced with `.principal` packaging:
//!
//! - `peko::registry::packaging::Packager` / `AgentManifest` /
//!   `ExportOptions` / `Unpackager` → `PrincipalPackager` /
//!   `PrincipalManifest` / `PrincipalExportOptions` /
//!   `PrincipalUnpackager`.
//! - `RegistryClient::push(ref)` / `pull(ref)` → `push_principal(...)` /
//!   `pull_principal(...)` (the underlying OCI/JSON store is generic;
//!   these wrappers rebuild the right config blob / manifest kind).
//!
//! The legacy `AgentRegistry` is still used as the local OCI store
//! (`registry.has_layer`, `store_layer`, etc.) — it's a generic content-
//! addressed registry now, not agent-specific. PekoHub-side, the
//! `RegistryManifest::kind = "principal"` discriminator distinguishes a
//! Principal push from a legacy `.agent` push.
//!
//! The legacy test exercised `export .agent → push → pull → import
//! → create team → export .team → import team`. The team export/import
//! half is covered separately by `s4_publish_running_agent_with_permission`
//! and the team surface is otherwise unchanged; this file drops the
//! team half (with the corresponding team-`.toml` fixture) and keeps
//! the full `.principal` packaging pipeline.
//!
//! ## Principal package shape
//!
//! A `.principal` package carries:
//!   - `manifest.toml` (signed, ed25519) — points at `config/`,
//!     `identity/`, `agents/`, and optional `memory/` / `sessions/`
//!     layers.
//!   - `config/principal.toml` — the PrincipalConfig payload.
//!   - `identity/did.json` + `identity/keys.enc` — the principal's
//!     DID + private key (the same shape the agent-era package used).
//!   - `agents/<prompt>.md` — one or more AGENT.md prompts that
//!     `peko principal import` restores into the principal's
//!     workspace.

use peko::identity::{did::DIDScope, Identity};
use peko::principal::config::PrincipalConfig;
use peko::registry::packaging::{
    PrincipalExportOptions, PrincipalImportOptions, PrincipalManifest, PrincipalPackager,
    PrincipalUnpackager,
};
use peko::registry::{AgentRegistry, RegistryClient, RegistryConfig, RegistrySource};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

mod common;
use common::{create_test_user, reset_pekohub, PekohubBackend};
use serial_test::serial;

// ── Helpers ──────────────────────────────────────────────────────────

/// Create a minimal Principal directory structure for testing.
async fn create_test_principal_dir(base: &Path) -> anyhow::Result<()> {
    // config/principal.toml — a minimal PrincipalConfig-shaped body.
    // The unpackager's validation doesn't pin a specific schema; it
    // just checks that the manifest's declared files exist in the
    // package and match their declared checksums. We supply a
    // representative PrincipalConfig payload.
    let config_dir = base.join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    let principal_toml = r#"
name = "integration-principal"
description = "A test principal for full integration"
display_name = "Integration Test Principal"

[capabilities]
tools = []
skills = []
mcps = []
agents = []
"#;
    tokio::fs::write(config_dir.join("principal.toml"), principal_toml).await?;

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

    // agents/primary.md — the Principal's default agent prompt.
    let agents_dir = base.join("agents");
    tokio::fs::create_dir_all(&agents_dir).await?;
    tokio::fs::write(
        agents_dir.join("primary.md"),
        "---\n\
         name: primary\n\
         description: Integration test primary agent\n\
         ---\n\
         # Primary Agent\n\n\
         Integration test principal's primary agent.\n",
    )
    .await?;

    Ok(())
}

/// Build a `.principal` package from a test directory using the
/// canonical PrincipalPackager.
async fn build_principal_package_from_dir(
    principal_dir: &Path,
    output_path: &Path,
) -> anyhow::Result<PrincipalManifest> {
    let config_path = principal_dir.join("config").join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path).await?;
    let config: PrincipalConfig = toml::from_str(&config_toml)?;

    let identity_dir = principal_dir.join("identity");
    let did_json = tokio::fs::read_to_string(identity_dir.join("did.json")).await?;
    let did_doc: peko::identity::DIDDocument = serde_json::from_str(&did_json)?;
    let keys_enc = tokio::fs::read(identity_dir.join("keys.enc")).await?;
    let key_export: peko::identity::KeyPairExport = serde_json::from_slice(&keys_enc)?;
    let identity = Identity::from_did_document_and_key(did_doc, key_export)?;

    let agents_dir = principal_dir.join("agents");

    let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);

    let export_opts = PrincipalExportOptions {
        output_path: Some(output_path.to_string_lossy().to_string()),
        ..Default::default()
    };

    let _path = packager.export(export_opts).await?;

    // Extract the manifest from the produced archive so the test can
    // assert on layer structure (the packager's `export` returns the
    // path but not the manifest).
    let file = std::fs::File::open(output_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut manifest_bytes = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.to_string_lossy() == "manifest.toml" {
            entry.read_to_end(&mut manifest_bytes)?;
            break;
        }
    }
    let manifest: PrincipalManifest =
        PrincipalManifest::from_toml(std::str::from_utf8(&manifest_bytes)?)?;
    Ok(manifest)
}

/// Create a test registry config pointing at the hub server.
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

// ── The full integration test ────────────────────────────────────────

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_full_packaging_pipeline() -> anyhow::Result<()> {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create user with namespace "ns" — PekoHub requires namespace ownership for pushes
    let (_id, _ns) = create_test_user(&client, &backend.url, "ns").await;

    // Full registry ref with namespace (PekoHub format: {host}/{namespace}/{name}:{tag})
    let registry_ref_str = format!("{}/ns/integration-principal:v1.0", backend.url);

    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // ═════════════════════════════════════════════════════════════════
    // 1. EXPORT .principal from directory (canonical PrincipalPackager path)
    // ═════════════════════════════════════════════════════════════════
    let principal_dir = base_dir.join("integration-principal");
    create_test_principal_dir(&principal_dir).await.unwrap();

    let package_path = base_dir.join("integration-principal.principal");
    let manifest = build_principal_package_from_dir(&principal_dir, &package_path)
        .await
        .unwrap();

    assert!(package_path.exists(), ".principal package should exist");
    // The Principal packager should have emitted at least the
    // `config/` and `identity/` layers (the `agents/` layer is
    // populated when the principal carries agent prompts, which
    // our fixture does).
    assert!(
        manifest
            .layers
            .as_ref()
            .and_then(|l| l.config.as_ref())
            .is_some(),
        "manifest should declare a config layer"
    );
    assert!(
        manifest
            .layers
            .as_ref()
            .and_then(|l| l.identity.as_ref())
            .is_some(),
        "manifest should declare an identity layer"
    );

    // ═════════════════════════════════════════════════════════════════
    // 2. PUSH to registry
    // ═════════════════════════════════════════════════════════════════
    // The Principal push path goes through `PrincipalPackager::export_for_registry`,
    // which produces a `PrincipalRegistryDescriptor` carrying the
    // signed manifest + per-prefix layer blobs (config, identity,
    // agents, …). The RegistryClient's `push_principal` stores those
    // layers locally, builds a `RegistryManifest` with
    // `kind = "principal"`, and pushes via the underlying OCI client.
    let build_registry_dir = base_dir.join("build_registry");
    let build_registry = AgentRegistry::new(&build_registry_dir);
    build_registry.init().await.unwrap();

    let config_path = principal_dir.join("config").join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path).await?;
    let config: PrincipalConfig = toml::from_str(&config_toml)?;
    let identity_dir = principal_dir.join("identity");
    let did_json = tokio::fs::read_to_string(identity_dir.join("did.json")).await?;
    let did_doc: peko::identity::DIDDocument = serde_json::from_str(&did_json)?;
    let keys_enc = tokio::fs::read(identity_dir.join("keys.enc")).await?;
    let key_export: peko::identity::KeyPairExport = serde_json::from_slice(&keys_enc)?;
    let identity = Identity::from_did_document_and_key(did_doc, key_export)?;
    let agents_dir = principal_dir.join("agents");

    let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);

    let export_opts = PrincipalExportOptions {
        output_path: Some(package_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let descriptor = packager
        .export_for_registry(export_opts)
        .await
        .expect("export_for_registry");

    let push_config = test_registry_config(&backend.url);
    let push_client = RegistryClient::new(push_config, build_registry.clone());

    let mut push_events = Vec::new();
    let push_result = push_client
        .push_principal(
            &descriptor,
            "integration-principal",
            "1.0.0",
            &registry_ref_str,
            |event| push_events.push(event),
        )
        .await;

    assert!(push_result.is_ok(), "Push failed: {:?}", push_result.err());
    let has_done = push_events
        .iter()
        .any(|e| matches!(e, peko::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Push should complete with Done event");

    // ═════════════════════════════════════════════════════════════════
    // 3. PULL from registry to a fresh local registry
    // ═════════════════════════════════════════════════════════════════
    let pull_registry_dir = base_dir.join("pull_registry");
    let pull_registry = AgentRegistry::new(&pull_registry_dir);
    pull_registry.init().await.unwrap();

    let pull_config = test_registry_config(&backend.url);
    let pull_client = RegistryClient::new(pull_config, pull_registry.clone());

    let pull_output = base_dir.join("pulled.principal");
    let mut pull_events = Vec::new();
    let pull_result = pull_client
        .pull_principal(&registry_ref_str, &pull_output, |event| {
            pull_events.push(event)
        })
        .await;

    assert!(pull_result.is_ok(), "Pull failed: {:?}", pull_result.err());
    let has_done = pull_events
        .iter()
        .any(|e| matches!(e, peko::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Pull should complete with Done event");

    assert!(
        pull_output.exists(),
        "pulled .principal archive should exist at {}",
        pull_output.display()
    );

    // ═════════════════════════════════════════════════════════════════
    // 4. IMPORT .principal package
    // ═════════════════════════════════════════════════════════════════
    let import_config_dir = base_dir.join("imported_principals_config");
    let import_data_dir = base_dir.join("imported_principals_data");
    tokio::fs::create_dir_all(&import_config_dir).await.unwrap();
    tokio::fs::create_dir_all(&import_data_dir).await.unwrap();

    let unpackager = PrincipalUnpackager::new(
        &pull_output,
        import_config_dir.clone(),
        import_data_dir.clone(),
    );

    let import_options = PrincipalImportOptions {
        new_name: Some("imported-principal".to_string()),
        rotate_keys: false,
        import_sessions: false,
        // Issue #14: signature verification is now enforced on import.
        // The integration pipeline produces packages via the canonical
        // PrincipalPackager, which signs the manifest, so verification
        // passes without an opt-in.
        allow_unsigned: false,
        force: false,
    };

    let import_result = unpackager.import(import_options).await.unwrap();
    assert_eq!(import_result.name, "imported-principal");
    assert!(import_result.config_path.exists());

    // Sanity: the imported principal.toml on disk is a valid
    // PrincipalConfig and carries the original name.
    let imported_config_toml = tokio::fs::read_to_string(&import_result.config_path)
        .await
        .unwrap();
    let imported_config: PrincipalConfig = toml::from_str(&imported_config_toml).unwrap();
    assert_eq!(
        imported_config.name, "imported-principal",
        "imported principal.toml should carry the renamed principal name"
    );

    Ok(())
}