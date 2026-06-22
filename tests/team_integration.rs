//! Integration tests for team packaging (Phase 5)
//!
//! End-to-end: export team → verify checksums → import → verify data

use pekobot::registry::packaging::{
    export_team, import_team_with_base_dir, inspect_team, TeamExportOptions, TeamImportOptions,
};
use pekobot::agents::agent_config::AgentConfig;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

/// Create a minimal agent config for testing
fn create_test_agent_config(name: &str) -> AgentConfig {
    let toml = format!(
        r#"
name = "{name}"
description = "Test agent {name}"
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
"#
    );
    toml::from_str(&toml).unwrap()
}

/// Create a mock identity for testing
fn create_test_identity(name: &str) -> pekobot::identity::Identity {
    // Use a blocking approach since we're in a test context
    let rt = tokio::runtime::Handle::try_current();
    match rt {
        Ok(handle) => {
            // We're in an async context, use block_in_place
            tokio::task::block_in_place(|| {
                handle.block_on(async {
                    pekobot::identity::Identity::new(name, pekobot::identity::did::DIDScope::Local)
                        .await
                        .unwrap()
                })
            })
        }
        Err(_) => {
            // No runtime, create one
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                pekobot::identity::Identity::new(name, pekobot::identity::did::DIDScope::Local)
                    .await
                    .unwrap()
            })
        }
    }
}

/// Create a team directory with team.toml
async fn create_team_dir(base: &Path, team_name: &str) -> anyhow::Result<()> {
    let team_dir = base.join("teams").join(team_name);
    tokio::fs::create_dir_all(&team_dir).await?;

    let team_toml = format!(
        r#"
[team]
name = "{team_name}"
description = "A test team"

[[agents]]
name = "agent1"
image = "./agent1"
instances = 1

[[agents]]
name = "agent2"
image = "./agent2"
instances = 1
"#
    );
    tokio::fs::write(team_dir.join("team.toml"), team_toml).await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_team_export_with_checksums() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // Create team directory with team.toml
    create_team_dir(base_dir, "test-team").await.unwrap();

    // Create agents
    let config1 = create_test_agent_config("agent1");
    let config2 = create_test_agent_config("agent2");
    let identity1 = create_test_identity("agent1");
    let identity2 = create_test_identity("agent2");

    let agents = vec![
        ("agent1".to_string(), config1, identity1),
        ("agent2".to_string(), config2, identity2),
    ];

    let options = TeamExportOptions {
        output_path: Some(
            base_dir
                .join("test-team.team")
                .to_string_lossy()
                .to_string(),
        ),
        include_sessions: false,
        include_workspace: false,
        include_mcp: false,
        description: Some("Test team export".to_string()),
    };

    let result = export_team("test-team", None, base_dir, agents, options)
        .await
        .unwrap();

    // ── Package exists ───────────────────────────────────────────────
    assert!(result.exists(), "Team package should exist");

    // ── Inspect manifest ─────────────────────────────────────────────
    let manifest = inspect_team(&result).await.unwrap();
    assert_eq!(manifest.team.name, "test-team");
    assert_eq!(manifest.team.agent_count, 2);

    // ── Packaging metadata exists with checksums ─────────────────────
    let packaging = manifest
        .packaging
        .expect("manifest should have packaging metadata");
    assert!(!packaging.files.is_empty(), "should have file list");
    assert!(!packaging.checksums.is_empty(), "should have checksums");
    assert_eq!(packaging.compression, "gzip");
    assert_eq!(packaging.archive_format, "tar");

    // ── team.toml should be in file list ─────────────────────────────
    assert!(
        packaging.files.contains(&"team/team.toml".to_string()),
        "team.toml should be in package"
    );
    assert!(
        packaging.checksums.contains_key("team/team.toml"),
        "team.toml should have checksum"
    );

    // ── Agent files should be in file list ───────────────────────────
    assert!(
        packaging
            .files
            .iter()
            .any(|f| f.starts_with("agents/agent1/")),
        "agent1 files should be in package"
    );
    assert!(
        packaging
            .files
            .iter()
            .any(|f| f.starts_with("agents/agent2/")),
        "agent2 files should be in package"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_team_import_with_checksum_validation() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // Create team directory with team.toml
    create_team_dir(base_dir, "test-team").await.unwrap();

    // Create agents
    let config1 = create_test_agent_config("agent1");
    let config2 = create_test_agent_config("agent2");
    let identity1 = create_test_identity("agent1");
    let identity2 = create_test_identity("agent2");

    let agents = vec![
        ("agent1".to_string(), config1, identity1),
        ("agent2".to_string(), config2, identity2),
    ];

    let export_options = TeamExportOptions {
        output_path: Some(
            base_dir
                .join("test-team.team")
                .to_string_lossy()
                .to_string(),
        ),
        include_sessions: false,
        include_workspace: false,
        include_mcp: false,
        description: None,
    };

    // Export
    let package_path = export_team("test-team", None, base_dir, agents, export_options)
        .await
        .unwrap();

    // Import to a new base directory
    let import_base = temp_dir.path().join("imported");
    tokio::fs::create_dir_all(&import_base).await.unwrap();

    let import_options = TeamImportOptions {
        new_name: Some("imported-team".to_string()),
        import_sessions: false,
        import_workspace: false,
        import_mcp: false,
        rotate_keys: false,
        force: false,
        // Issue #14: signature verification is now enforced. Packages
        // built by the canonical TeamPackager are signed (one per
        // agent), so this stays at the secure default.
        allow_unsigned: false,
    };

    let result = import_team_with_base_dir(&package_path, &import_base, import_options)
        .await
        .unwrap();

    // ── Import succeeded ─────────────────────────────────────────────
    assert_eq!(result.name, "imported-team");
    assert_eq!(result.agent_count, 2);

    // ── team.toml restored ───────────────────────────────────────────
    let restored_team_toml = import_base
        .join("teams")
        .join("imported-team")
        .join("team.toml");
    assert!(restored_team_toml.exists(), "team.toml should be restored");

    let restored_content = tokio::fs::read_to_string(&restored_team_toml)
        .await
        .unwrap();
    assert!(restored_content.contains("name = \"test-team\""));
    assert!(restored_content.contains("agent1"));
    assert!(restored_content.contains("agent2"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_team_import_fails_on_checksum_mismatch() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // Create team directory with team.toml
    create_team_dir(base_dir, "test-team").await.unwrap();

    // Create one agent
    let config1 = create_test_agent_config("agent1");
    let identity1 = create_test_identity("agent1");
    let agents = vec![("agent1".to_string(), config1, identity1)];

    let export_options = TeamExportOptions {
        output_path: Some(
            base_dir
                .join("test-team.team")
                .to_string_lossy()
                .to_string(),
        ),
        include_sessions: false,
        include_workspace: false,
        include_mcp: false,
        description: None,
    };

    // Export
    let package_path = export_team("test-team", None, base_dir, agents, export_options)
        .await
        .unwrap();

    // Read original package
    let tar_gz = std::fs::File::open(&package_path).unwrap();
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);

    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().to_string();
        // Skip directory entries
        if path.ends_with('/') {
            continue;
        }
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        files.insert(path, content);
    }

    // Tamper with a file
    let manifest_bytes = files.get("team/manifest.toml").unwrap().clone();
    let _manifest: pekobot::registry::packaging::TeamManifest =
        toml::from_str(std::str::from_utf8(&manifest_bytes).unwrap()).unwrap();

    // Modify a file but keep the old checksum
    if let Some(content) = files.get_mut("agents/agent1/config/agent.toml") {
        content.extend_from_slice(b"\n# tampered");
    }

    // Repackage with the (now wrong) checksums from the manifest
    let tampered_path = temp_dir.path().join("tampered.team");
    {
        let tar_gz = std::fs::File::create(&tampered_path).unwrap();
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Write all files using append_data for proper tar formatting
        for (path, content) in &files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, content.as_slice())
                .unwrap();
        }
        tar.finish().unwrap();
    }

    // Import should fail due to checksum mismatch
    let import_base = temp_dir.path().join("imported");
    tokio::fs::create_dir_all(&import_base).await.unwrap();

    let import_options = TeamImportOptions::default();
    let result = import_team_with_base_dir(&tampered_path, &import_base, import_options).await;

    assert!(result.is_err(), "Import should fail on checksum mismatch");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Checksum mismatch"),
        "Error should mention checksum mismatch: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_team_import_warns_without_checksums() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_dir = temp_dir.path();

    // Create team directory (no team.toml needed for this test)
    let team_dir = base_dir.join("teams").join("legacy-team");
    tokio::fs::create_dir_all(&team_dir).await.unwrap();

    // Create one agent
    let config1 = create_test_agent_config("agent1");
    let identity1 = create_test_identity("agent1");
    let agents = vec![("agent1".to_string(), config1, identity1)];

    // Export
    let export_options = TeamExportOptions {
        output_path: Some(base_dir.join("legacy.team").to_string_lossy().to_string()),
        include_sessions: false,
        include_workspace: false,
        include_mcp: false,
        description: None,
    };

    let package_path = export_team("legacy-team", None, base_dir, agents, export_options)
        .await
        .unwrap();

    // Read original package, strip packaging metadata, repackage
    let tar_gz = std::fs::File::open(&package_path).unwrap();
    let tar = flate2::read::GzDecoder::new(tar_gz);
    let mut archive = tar::Archive::new(tar);

    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().to_string();
        // Skip directory entries
        if path.ends_with('/') {
            continue;
        }
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        files.insert(path, content);
    }

    // Modify manifest to remove packaging
    let manifest_bytes = files.get("team/manifest.toml").unwrap().clone();
    let mut manifest: pekobot::registry::packaging::TeamManifest =
        toml::from_str(std::str::from_utf8(&manifest_bytes).unwrap()).unwrap();
    manifest.packaging = None;

    // Repackage without checksums
    let legacy_path = temp_dir.path().join("legacy-no-checksums.team");
    {
        let tar_gz = std::fs::File::create(&legacy_path).unwrap();
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Write modified manifest
        let manifest_toml = toml::to_string_pretty(&manifest).unwrap();
        files.insert("team/manifest.toml".to_string(), manifest_toml.into_bytes());

        // Write all files using append_data for proper tar formatting
        for (path, content) in &files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, content.as_slice())
                .unwrap();
        }
        tar.finish().unwrap();
    }

    // Import should succeed (with a warning printed to stderr)
    let import_base = temp_dir.path().join("imported");
    tokio::fs::create_dir_all(&import_base).await.unwrap();

    let import_options = TeamImportOptions::default();
    let result = import_team_with_base_dir(&legacy_path, &import_base, import_options).await;

    assert!(
        result.is_ok(),
        "Import should succeed without checksums: {:?}",
        result.err()
    );
}
