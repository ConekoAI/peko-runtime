//! Team Layer Reconstructor — reconstruct agents from registry layers
//!
//! Provides utilities to pull a team's `RegistryManifest` and reconstruct
//! each agent's files from its content-addressable layers, enabling direct
//! in-memory import without creating a temporary `.team` file.

use crate::registry::AgentRegistry;
use crate::registry::packaging::team_packager::{AgentLayerRef, TeamAgentIndex};
use crate::registry::packaging::types::LayerType;
use anyhow::Context;
use std::collections::HashMap;
use std::io::Read;

/// Result of reconstructing a team from registry layers.
#[derive(Debug, Clone)]
pub struct ReconstructedTeam {
    /// Team metadata from the TeamConfig layer
    pub team_info: TeamAgentIndex,
    /// Per-agent files: agent_name → relative_path → bytes
    /// Paths follow the `.team` archive convention: `{layer}/{file}`
    pub agent_files: HashMap<String, HashMap<String, Vec<u8>>>,
    /// team.toml content if present in TeamConfig layer
    pub team_toml: Option<Vec<u8>>,
}

/// Extract the `TeamAgentIndex` from a TeamConfig layer tarball.
///
/// The TeamConfig layer is a gzipped tarball containing `manifest.toml`
/// (which holds the `TeamAgentIndex`).
pub fn extract_team_config_index(layer_bytes: &[u8]) -> anyhow::Result<TeamAgentIndex> {
    let decoder = flate2::read::GzDecoder::new(layer_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        if path == "manifest.toml" {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            let index: TeamAgentIndex = toml::from_str(&content)
                .context("Failed to parse TeamAgentIndex from manifest.toml")?;
            return Ok(index);
        }
    }

    anyhow::bail!("manifest.toml not found in TeamConfig layer")
}

/// Reconstruct a single agent's files from its layer tarballs.
///
/// # Arguments
///
/// * `registry` — Local `AgentRegistry` containing downloaded layers
/// * `agent_ref` — `AgentLayerRef` with digests for each layer type
///
/// # Returns
///
/// Map of `{layer_type_dir}/{file_path} → bytes` for the agent,
/// e.g. `config/agent.toml`, `identity/did.json`, `skills/rust/SKILL.md`.
pub async fn reconstruct_agent_files(
    registry: &AgentRegistry,
    agent_ref: &AgentLayerRef,
) -> anyhow::Result<HashMap<String, Vec<u8>>> {
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();

    let layer_refs = [
        (LayerType::Config, Some(&agent_ref.config)),
        (LayerType::Identity, Some(&agent_ref.identity)),
        (LayerType::Skills, agent_ref.skills.as_ref()),
        (LayerType::Workspace, agent_ref.workspace.as_ref()),
        (LayerType::Sessions, agent_ref.sessions.as_ref()),
        (LayerType::Mcp, agent_ref.mcp.as_ref()),
    ];

    for (layer_type, digest_opt) in layer_refs {
        let Some(digest) = digest_opt else {
            continue;
        };

        let layer_bytes = registry
            .get_layer(digest)
            .await
            .with_context(|| format!("Failed to get layer {digest} from registry"))?;

        let prefix = layer_type.dir_name();
        extract_tarball_files(&layer_bytes, prefix, &mut files)
            .with_context(|| format!("Failed to extract {prefix} layer ({digest})"))?;
    }

    Ok(files)
}

/// Reconstruct an entire team from a pulled `RegistryManifest`.
///
/// # Arguments
///
/// * `registry` — Local `AgentRegistry` with all layers downloaded
/// * `team_config_digest` — Digest of the `TeamConfig` layer
///
/// # Returns
///
/// `ReconstructedTeam` containing the agent index and per-agent file maps.
pub async fn reconstruct_team(
    registry: &AgentRegistry,
    team_config_digest: &str,
) -> anyhow::Result<ReconstructedTeam> {
    // Get TeamConfig layer
    let team_config_bytes = registry
        .get_layer(team_config_digest)
        .await
        .with_context(|| format!("Failed to get TeamConfig layer {team_config_digest}"))?;

    // Extract agent index
    let team_info = extract_team_config_index(&team_config_bytes)?;

    // Check for team.toml in TeamConfig layer
    let team_toml = extract_team_toml(&team_config_bytes)?;

    // Reconstruct each agent's files
    let mut agent_files: HashMap<String, HashMap<String, Vec<u8>>> = HashMap::new();

    for (agent_name, agent_ref) in &team_info.agents {
        let files = reconstruct_agent_files(registry, agent_ref)
            .await
            .with_context(|| format!("Failed to reconstruct agent: {agent_name}"))?;
        agent_files.insert(agent_name.clone(), files);
    }

    Ok(ReconstructedTeam {
        team_info,
        agent_files,
        team_toml,
    })
}

/// Extract files from a gzipped tarball into a map with a path prefix.
fn extract_tarball_files(
    tarball: &[u8],
    prefix: &str,
    files: &mut HashMap<String, Vec<u8>>,
) -> anyhow::Result<()> {
    let decoder = flate2::read::GzDecoder::new(tarball);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        let mut content = Vec::new();
        entry.read_to_end(&mut content)?;

        let full_path = format!("{prefix}/{path}");
        files.insert(full_path, content);
    }

    Ok(())
}

/// Extract team.toml from a TeamConfig layer if present.
fn extract_team_toml(layer_bytes: &[u8]) -> anyhow::Result<Option<Vec<u8>>> {
    let decoder = flate2::read::GzDecoder::new(layer_bytes);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();

        if path == "team.toml" {
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            return Ok(Some(content));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::AgentRegistry;
    use crate::registry::packaging::team_layer_builder::{build_tarball, decompose_team_archive};
    use std::collections::HashMap;

    fn make_test_team_files() -> HashMap<String, Vec<u8>> {
        let mut files = HashMap::new();

        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "test-team"
version = "1.0.0"
agent_count = 1

[format]
version = "1.0"
peko_version = "0.1.0"

[export]
created_at = "2024-01-01T00:00:00Z"
include_sessions = false
include_workspace = false
include_mcp = false
"#
            .to_vec(),
        );

        files.insert(
            "agents/researcher/config/agent.toml".to_string(),
            b"name = \"researcher\"\n".to_vec(),
        );
        files.insert(
            "agents/researcher/identity/did.json".to_string(),
            br#"{"id":"did:peko:researcher"}"#.to_vec(),
        );
        files.insert(
            "agents/researcher/skills/rust/SKILL.md".to_string(),
            b"# Rust skill".to_vec(),
        );

        files
    }

    #[tokio::test]
    async fn test_reconstruct_team_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        // Decompose a test team
        let files = make_test_team_files();
        let decomposed = decompose_team_archive(&files).unwrap();

        // Store layers in registry (as if pulled)
        registry
            .store_layer(
                &decomposed.team_config_layer.digest,
                &decomposed.team_config_layer.bytes,
            )
            .await
            .unwrap();

        for (_, layers) in &decomposed.agent_layers {
            for (_, layer) in layers {
                registry
                    .store_layer(&layer.digest, &layer.bytes)
                    .await
                    .unwrap();
            }
        }

        // Reconstruct the team
        let reconstructed = reconstruct_team(&registry, &decomposed.team_config_layer.digest)
            .await
            .unwrap();

        assert_eq!(reconstructed.team_info.team.name, "test-team");
        assert_eq!(reconstructed.agent_files.len(), 1);
        assert!(reconstructed.agent_files.contains_key("researcher"));

        let researcher = reconstructed.agent_files.get("researcher").unwrap();
        assert!(researcher.contains_key("config/agent.toml"));
        assert!(researcher.contains_key("identity/did.json"));
        assert!(researcher.contains_key("skills/rust/SKILL.md"));

        // Extensions list should be empty (no extensions in test data)
        assert!(reconstructed.team_info.extensions.is_empty());
    }

    #[tokio::test]
    async fn test_extract_team_config_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        let files = make_test_team_files();
        let decomposed = decompose_team_archive(&files).unwrap();

        registry
            .store_layer(
                &decomposed.team_config_layer.digest,
                &decomposed.team_config_layer.bytes,
            )
            .await
            .unwrap();

        let index = extract_team_config_index(&decomposed.team_config_layer.bytes).unwrap();
        assert_eq!(index.team.name, "test-team");
        assert_eq!(index.agents.len(), 1);
        assert!(index.extensions.is_empty());
    }

    #[tokio::test]
    async fn test_team_config_index_with_extensions() {
        use crate::registry::packaging::team_layer_builder::build_team_config_layer;
        use crate::registry::packaging::team_packager::TeamManifest;
        use crate::registry::packaging::types::ExtensionRef;
        use std::collections::HashMap;

        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        // Build a TeamConfig layer directly with extensions
        let team_manifest = TeamManifest {
            team: crate::registry::packaging::team_packager::TeamInfo {
                name: "ext-team".to_string(),
                description: None,
                version: "1.0.0".to_string(),
                agent_count: 0,
            },
            format: crate::registry::packaging::team_packager::TeamFormat {
                version: "1.0".to_string(),
                peko_version: "0.1.0".to_string(),
            },
            export: crate::registry::packaging::team_packager::ExportMetadata {
                created_at: "2024-01-01T00:00:00Z".to_string(),
                include_sessions: false,
                include_workspace: false,
                include_mcp: false,
            },
            packaging: None,
        };

        let extensions = vec![ExtensionRef {
            id: "calc-ext".to_string(),
            registry_ref: "pekohub.com/extensions/calculator:latest".to_string(),
        }];

        let layer =
            build_team_config_layer(&team_manifest, &HashMap::new(), None, &extensions).unwrap();

        registry
            .store_layer(&layer.digest, &layer.bytes)
            .await
            .unwrap();

        let index = extract_team_config_index(&layer.bytes).unwrap();
        assert_eq!(index.extensions.len(), 1);
        assert_eq!(index.extensions[0].id, "calc-ext");
        assert_eq!(
            index.extensions[0].registry_ref,
            "pekohub.com/extensions/calculator:latest"
        );
    }

    #[tokio::test]
    async fn test_team_config_index_backward_compat_no_extensions_field() {
        // Old manifest.toml without [[extensions]] should deserialize with empty vec
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        let files = make_test_team_files();
        let decomposed = decompose_team_archive(&files).unwrap();

        registry
            .store_layer(
                &decomposed.team_config_layer.digest,
                &decomposed.team_config_layer.bytes,
            )
            .await
            .unwrap();

        let index = extract_team_config_index(&decomposed.team_config_layer.bytes).unwrap();
        assert_eq!(index.team.name, "test-team");
        assert!(index.extensions.is_empty());
    }

    #[tokio::test]
    async fn test_reconstruct_agent_files_determinism() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        let files = make_test_team_files();
        let decomposed = decompose_team_archive(&files).unwrap();

        // Store layers
        for (_, layers) in &decomposed.agent_layers {
            for (_, layer) in layers {
                registry
                    .store_layer(&layer.digest, &layer.bytes)
                    .await
                    .unwrap();
            }
        }

        let agent_ref = decomposed.agent_index.get("researcher").unwrap();

        // Reconstruct twice — should get identical results
        let files1 = reconstruct_agent_files(&registry, agent_ref).await.unwrap();
        let files2 = reconstruct_agent_files(&registry, agent_ref).await.unwrap();

        assert_eq!(files1.len(), files2.len());
        for (path, content1) in &files1 {
            let content2 = files2.get(path).unwrap();
            assert_eq!(content1, content2, "Mismatch for {path}");
        }
    }

    #[tokio::test]
    async fn test_reconstruct_agent_missing_optional_layers() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        // Build a team with an agent that only has config and identity
        let mut files = HashMap::new();
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "minimal-team"
version = "1.0.0"
agent_count = 1

[format]
version = "1.0"
peko_version = "0.1.0"

[export]
created_at = "2024-01-01T00:00:00Z"
include_sessions = false
include_workspace = false
include_mcp = false
"#
            .to_vec(),
        );
        files.insert(
            "agents/minimal/config/agent.toml".to_string(),
            b"name = \"minimal\"\n".to_vec(),
        );
        files.insert(
            "agents/minimal/identity/did.json".to_string(),
            br#"{"id":"did:peko:minimal"}"#.to_vec(),
        );

        let decomposed = decompose_team_archive(&files).unwrap();

        // Store only config and identity layers
        let minimal_layers = decomposed.agent_layers.get("minimal").unwrap();
        registry
            .store_layer(
                &minimal_layers.get(&LayerType::Config).unwrap().digest,
                &minimal_layers.get(&LayerType::Config).unwrap().bytes,
            )
            .await
            .unwrap();
        registry
            .store_layer(
                &minimal_layers.get(&LayerType::Identity).unwrap().digest,
                &minimal_layers.get(&LayerType::Identity).unwrap().bytes,
            )
            .await
            .unwrap();

        let agent_ref = decomposed.agent_index.get("minimal").unwrap();
        let reconstructed = reconstruct_agent_files(&registry, agent_ref).await.unwrap();

        // Should only have config and identity entries
        assert_eq!(reconstructed.len(), 2);
        assert!(reconstructed.contains_key("config/agent.toml"));
        assert!(reconstructed.contains_key("identity/did.json"));
        assert!(!reconstructed.contains_key("skills/rust/SKILL.md"));
        assert!(!reconstructed.contains_key("workspace/notes.txt"));
        assert!(!reconstructed.contains_key("sessions/session_1.jsonl"));
        assert!(!reconstructed.contains_key("mcp/servers.json"));
    }

    #[tokio::test]
    async fn test_empty_agent_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let registry = AgentRegistry::new(temp_dir.path());
        registry.init().await.unwrap();

        // Build a team with no agents
        let mut files = HashMap::new();
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "empty-team"
version = "1.0.0"
agent_count = 0

[format]
version = "1.0"
peko_version = "0.1.0"

[export]
created_at = "2024-01-01T00:00:00Z"
include_sessions = false
include_workspace = false
include_mcp = false
"#
            .to_vec(),
        );

        let decomposed = decompose_team_archive(&files).unwrap();

        // Store the TeamConfig layer
        registry
            .store_layer(
                &decomposed.team_config_layer.digest,
                &decomposed.team_config_layer.bytes,
            )
            .await
            .unwrap();

        // Reconstruct the team
        let reconstructed = reconstruct_team(&registry, &decomposed.team_config_layer.digest)
            .await
            .unwrap();

        assert_eq!(reconstructed.team_info.team.name, "empty-team");
        assert_eq!(reconstructed.team_info.team.agent_count, 0);
        assert!(reconstructed.agent_files.is_empty());
    }

    #[tokio::test]
    async fn test_team_config_missing_manifest_toml() {
        // Build a tarball that does NOT contain manifest.toml
        let mut files: std::collections::BTreeMap<String, Vec<u8>> =
            std::collections::BTreeMap::new();
        files.insert("other_file.txt".to_string(), b"some content".to_vec());
        let bad_layer = build_tarball(&files).unwrap();

        let result = extract_team_config_index(&bad_layer);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("manifest.toml") || err_msg.contains("not found"),
            "Error should mention missing manifest.toml, got: {err_msg}"
        );
    }
}
