//! Team Layer Builder — decompose `.team` archives into content-addressable layers
//!
//! Provides utilities to break down a `.team` package into:
//! - One `TeamConfig` layer (team.toml + manifest.toml with agent index)
//! - Per-agent standard layers (Config, Identity, Skills, Workspace, Sessions, Mcp)
//!
//! This enables cross-team deduplication: shared agents between teams reuse
//! the same layer digests and are skipped during registry push.

use crate::portable::team_packager::{
    AgentLayerRef, ExtensionRef, TeamAgentIndex, TeamInfo, TeamManifest,
};
use crate::portable::types::{compute_digest, LayerType};
use anyhow::Context;
use std::collections::{BTreeMap, HashMap};

/// Result of decomposing a `.team` archive into layers.
#[derive(Debug, Clone)]
pub struct DecomposedTeamLayers {
    /// TeamConfig layer bytes (gzipped tarball of team.toml + manifest.toml)
    pub team_config_layer: LayerBytes,
    /// Per-agent layers: agent_name → layer_type → (digest, bytes)
    pub agent_layers: HashMap<String, HashMap<LayerType, LayerBytes>>,
    /// Agent index for the TeamConfig manifest
    pub agent_index: HashMap<String, AgentLayerRef>,
    /// Extension references for auto-pull
    pub extensions: Vec<ExtensionRef>,
}

/// A layer with its digest pre-computed and bytes ready for storage.
#[derive(Debug, Clone)]
pub struct LayerBytes {
    /// SHA-256 digest (sha256:...)
    pub digest: String,
    /// Gzipped tarball bytes
    pub bytes: Vec<u8>,
    /// Size in bytes
    pub size: u64,
}

/// Decompose a `.team` archive (already extracted as a file map) into layers.
///
/// # Arguments
///
/// * `files` — Map of file paths (as stored in the .team archive) to their contents.
///   Expected paths: `team/manifest.toml`, `team/team.toml` (optional),
///   `agents/{name}/{layer}/{file}`.
///
/// # Returns
///
/// `DecomposedTeamLayers` containing the TeamConfig layer and per-agent layers.
pub fn decompose_team_archive(
    files: &HashMap<String, Vec<u8>>,
) -> anyhow::Result<DecomposedTeamLayers> {
    // Parse the team manifest to get metadata
    let team_manifest = extract_team_manifest(files)?;

    // Extract team.toml if present in the archive
    let team_toml = files.get("team/team.toml").cloned();

    // Group files by agent
    let agent_files = group_agent_files(files);

    // Build per-agent layers
    let mut agent_layers: HashMap<String, HashMap<LayerType, LayerBytes>> = HashMap::new();
    let mut agent_index: HashMap<String, AgentLayerRef> = HashMap::new();

    for (agent_name, files) in agent_files {
        let layers = build_agent_layers(&files)?;
        let layer_ref = agent_layers_to_ref(&layers);
        agent_layers.insert(agent_name.clone(), layers);
        agent_index.insert(agent_name, layer_ref);
    }

    // Build TeamConfig layer
    let team_config_layer =
        build_team_config_layer(&team_manifest, &agent_index, team_toml.as_ref(), &[])?;

    Ok(DecomposedTeamLayers {
        team_config_layer,
        agent_layers,
        agent_index,
        extensions: Vec::new(),
    })
}

/// Extract the team manifest from the extracted archive files.
fn extract_team_manifest(files: &HashMap<String, Vec<u8>>) -> anyhow::Result<TeamManifest> {
    let manifest_bytes = files
        .get("team/manifest.toml")
        .ok_or_else(|| anyhow::anyhow!("Missing team/manifest.toml in package"))?;
    let manifest_str = std::str::from_utf8(manifest_bytes)?;
    TeamManifest::from_toml(manifest_str).context("Failed to parse team manifest.toml")
}

/// Group files from the .team archive by agent name.
///
/// Input paths: `agents/{name}/config/agent.toml`, `agents/{name}/identity/did.json`, etc.
/// Output: agent_name → relative_path (without `agents/{name}/` prefix) → bytes
fn group_agent_files(
    files: &HashMap<String, Vec<u8>>,
) -> HashMap<String, HashMap<String, Vec<u8>>> {
    let mut agents: HashMap<String, HashMap<String, Vec<u8>>> = HashMap::new();

    for (path, content) in files {
        if let Some(agent_path) = path.strip_prefix("agents/") {
            if let Some((agent_name, file_path)) = agent_path.split_once('/') {
                agents
                    .entry(agent_name.to_string())
                    .or_default()
                    .insert(file_path.to_string(), content.clone());
            }
        }
    }

    agents
}

/// Build content-addressable layers for a single agent's files.
///
/// Categorizes files by layer type prefix (config/, identity/, skills/, etc.)
/// and builds a gzipped tarball for each non-empty layer.
fn build_agent_layers(
    files: &HashMap<String, Vec<u8>>,
) -> anyhow::Result<HashMap<LayerType, LayerBytes>> {
    let mut layers: HashMap<LayerType, LayerBytes> = HashMap::new();

    let layer_prefixes = [
        (LayerType::Config, "config"),
        (LayerType::Identity, "identity"),
        (LayerType::Skills, "skills"),
        (LayerType::Workspace, "workspace"),
        (LayerType::Sessions, "sessions"),
        (LayerType::Mcp, "mcp"),
    ];

    for (layer_type, prefix) in layer_prefixes {
        let mut layer_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();

        for (path, content) in files {
            if let Some(layer_path) = path.strip_prefix(&format!("{prefix}/")) {
                layer_files.insert(layer_path.to_string(), content.clone());
            }
        }

        if !layer_files.is_empty() {
            let bytes = build_tarball(&layer_files)?;
            let digest = compute_digest(&bytes);
            let size = bytes.len() as u64;
            layers.insert(
                layer_type,
                LayerBytes {
                    digest,
                    bytes,
                    size,
                },
            );
        }
    }

    Ok(layers)
}

/// Convert agent layer map to an `AgentLayerRef` for the team index.
fn agent_layers_to_ref(layers: &HashMap<LayerType, LayerBytes>) -> AgentLayerRef {
    AgentLayerRef {
        config: layers
            .get(&LayerType::Config)
            .map_or_else(String::new, |l| l.digest.clone()),
        identity: layers
            .get(&LayerType::Identity)
            .map_or_else(String::new, |l| l.digest.clone()),
        skills: layers.get(&LayerType::Skills).map(|l| l.digest.clone()),
        workspace: layers.get(&LayerType::Workspace).map(|l| l.digest.clone()),
        sessions: layers.get(&LayerType::Sessions).map(|l| l.digest.clone()),
        mcp: layers.get(&LayerType::Mcp).map(|l| l.digest.clone()),
    }
}

/// Build the TeamConfig layer (gzipped tarball containing team metadata + agent index).
pub fn build_team_config_layer(
    team_manifest: &TeamManifest,
    agent_index: &HashMap<String, AgentLayerRef>,
    team_toml: Option<&Vec<u8>>,
    extensions: &[ExtensionRef],
) -> anyhow::Result<LayerBytes> {
    let team_info = TeamInfo {
        name: team_manifest.team.name.clone(),
        description: team_manifest.team.description.clone(),
        version: team_manifest.team.version.clone(),
        agent_count: agent_index.len(),
    };

    let index = TeamAgentIndex {
        team: team_info,
        agents: agent_index.clone(),
        extensions: extensions.to_vec(),
    };

    // Serialize the agent index as TOML
    let index_toml =
        toml::to_string_pretty(&index).context("Failed to serialize team agent index to TOML")?;

    // Build tarball with manifest.toml (agent index) and optionally team.toml
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    files.insert("manifest.toml".to_string(), index_toml.into_bytes());

    if let Some(toml_bytes) = team_toml {
        files.insert("team.toml".to_string(), toml_bytes.clone());
    }

    let bytes = build_tarball(&files)?;
    let digest = compute_digest(&bytes);
    let size = bytes.len() as u64;

    Ok(LayerBytes {
        digest,
        bytes,
        size,
    })
}

/// Build a gzipped tarball from a map of `(relative_path, bytes)`.
pub(crate) fn build_tarball(files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_slice())?;
        }

        tar.finish()?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_team_files() -> HashMap<String, Vec<u8>> {
        let mut files = HashMap::new();

        // Team manifest
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "test-team"
version = "1.0.0"
agent_count = 2

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

        // Agent 1: researcher
        files.insert(
            "agents/researcher/config/agent.toml".to_string(),
            b"name = \"researcher\"\n".to_vec(),
        );
        files.insert(
            "agents/researcher/identity/did.json".to_string(),
            br#"{"id":"did:peko:researcher"}"#.to_vec(),
        );

        // Agent 2: coder (shares no files with researcher)
        files.insert(
            "agents/coder/config/agent.toml".to_string(),
            b"name = \"coder\"\n".to_vec(),
        );
        files.insert(
            "agents/coder/identity/did.json".to_string(),
            br#"{"id":"did:peko:coder"}"#.to_vec(),
        );
        files.insert(
            "agents/coder/skills/rust/SKILL.md".to_string(),
            b"# Rust skill".to_vec(),
        );

        files
    }

    #[test]
    fn test_decompose_team_archive_basic() {
        let files = make_test_team_files();
        let result = decompose_team_archive(&files).unwrap();

        // Should have TeamConfig layer
        assert!(!result.team_config_layer.digest.is_empty());
        assert!(result.team_config_layer.size > 0);

        // Should have 2 agents
        assert_eq!(result.agent_layers.len(), 2);
        assert!(result.agent_layers.contains_key("researcher"));
        assert!(result.agent_layers.contains_key("coder"));

        // Researcher: config + identity
        let researcher = result.agent_layers.get("researcher").unwrap();
        assert!(researcher.contains_key(&LayerType::Config));
        assert!(researcher.contains_key(&LayerType::Identity));
        assert!(!researcher.contains_key(&LayerType::Skills));

        // Coder: config + identity + skills
        let coder = result.agent_layers.get("coder").unwrap();
        assert!(coder.contains_key(&LayerType::Config));
        assert!(coder.contains_key(&LayerType::Identity));
        assert!(coder.contains_key(&LayerType::Skills));
    }

    #[test]
    fn test_agent_index_populated() {
        let files = make_test_team_files();
        let result = decompose_team_archive(&files).unwrap();

        assert_eq!(result.agent_index.len(), 2);

        let researcher = result.agent_index.get("researcher").unwrap();
        assert!(!researcher.config.is_empty());
        assert!(!researcher.identity.is_empty());
        assert!(researcher.skills.is_none());

        let coder = result.agent_index.get("coder").unwrap();
        assert!(!coder.config.is_empty());
        assert!(!coder.identity.is_empty());
        assert!(coder.skills.is_some());
    }

    #[test]
    fn test_different_agents_different_digests() {
        let files = make_test_team_files();
        let result = decompose_team_archive(&files).unwrap();

        let researcher = result.agent_layers.get("researcher").unwrap();
        let coder = result.agent_layers.get("coder").unwrap();

        // Same layer type but different content → different digests
        assert_ne!(
            researcher.get(&LayerType::Config).unwrap().digest,
            coder.get(&LayerType::Config).unwrap().digest
        );
    }

    #[test]
    fn test_same_agent_same_digest() {
        // Build the same team twice — digests should be identical
        let files1 = make_test_team_files();
        let files2 = make_test_team_files();

        let result1 = decompose_team_archive(&files1).unwrap();
        let result2 = decompose_team_archive(&files2).unwrap();

        let r1 = result1.agent_layers.get("researcher").unwrap();
        let r2 = result2.agent_layers.get("researcher").unwrap();

        assert_eq!(
            r1.get(&LayerType::Config).unwrap().digest,
            r2.get(&LayerType::Config).unwrap().digest
        );
        assert_eq!(
            r1.get(&LayerType::Identity).unwrap().digest,
            r2.get(&LayerType::Identity).unwrap().digest
        );
    }

    #[test]
    fn test_team_config_layer_contains_index() {
        let files = make_test_team_files();
        let result = decompose_team_archive(&files).unwrap();

        // The TeamConfig layer should be a valid gzipped tarball
        let decoder = flate2::read::GzDecoder::new(result.team_config_layer.bytes.as_slice());
        let mut archive = tar::Archive::new(decoder);

        let entries: Vec<_> = archive.entries().unwrap().collect();
        assert!(!entries.is_empty());
    }

    #[test]
    fn test_empty_team_no_agents() {
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

        let result = decompose_team_archive(&files).unwrap();

        // Should have a valid TeamConfig layer
        assert!(!result.team_config_layer.digest.is_empty());
        assert!(result.team_config_layer.size > 0);

        // No agents
        assert!(result.agent_layers.is_empty());
        assert!(result.agent_index.is_empty());
    }

    #[test]
    fn test_agent_with_only_config_and_identity() {
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

        let result = decompose_team_archive(&files).unwrap();

        assert_eq!(result.agent_layers.len(), 1);
        assert_eq!(result.agent_index.len(), 1);

        let layer_ref = result.agent_index.get("minimal").unwrap();
        assert!(!layer_ref.config.is_empty());
        assert!(!layer_ref.identity.is_empty());
        assert!(layer_ref.skills.is_none());
        assert!(layer_ref.workspace.is_none());
        assert!(layer_ref.sessions.is_none());
        assert!(layer_ref.mcp.is_none());

        let layers = result.agent_layers.get("minimal").unwrap();
        assert!(layers.contains_key(&LayerType::Config));
        assert!(layers.contains_key(&LayerType::Identity));
        assert!(!layers.contains_key(&LayerType::Skills));
        assert!(!layers.contains_key(&LayerType::Workspace));
        assert!(!layers.contains_key(&LayerType::Sessions));
        assert!(!layers.contains_key(&LayerType::Mcp));
    }

    #[test]
    fn test_agent_with_all_layer_types() {
        let mut files = HashMap::new();
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "full-team"
version = "1.0.0"
agent_count = 1

[format]
version = "1.0"
peko_version = "0.1.0"

[export]
created_at = "2024-01-01T00:00:00Z"
include_sessions = true
include_workspace = true
include_mcp = true
"#
            .to_vec(),
        );

        files.insert(
            "agents/full/config/agent.toml".to_string(),
            b"name = \"full\"\n".to_vec(),
        );
        files.insert(
            "agents/full/identity/did.json".to_string(),
            br#"{"id":"did:peko:full"}"#.to_vec(),
        );
        files.insert(
            "agents/full/skills/rust/SKILL.md".to_string(),
            b"# Rust skill".to_vec(),
        );
        files.insert(
            "agents/full/workspace/notes.txt".to_string(),
            b"some notes".to_vec(),
        );
        files.insert(
            "agents/full/sessions/session_1.jsonl".to_string(),
            b"{}\n".to_vec(),
        );
        files.insert(
            "agents/full/mcp/servers.json".to_string(),
            br#"{"servers":[]}"#.to_vec(),
        );

        let result = decompose_team_archive(&files).unwrap();

        assert_eq!(result.agent_layers.len(), 1);
        assert_eq!(result.agent_index.len(), 1);

        let layer_ref = result.agent_index.get("full").unwrap();
        assert!(!layer_ref.config.is_empty());
        assert!(!layer_ref.identity.is_empty());
        assert!(layer_ref.skills.is_some());
        assert!(layer_ref.workspace.is_some());
        assert!(layer_ref.sessions.is_some());
        assert!(layer_ref.mcp.is_some());

        let layers = result.agent_layers.get("full").unwrap();
        assert!(layers.contains_key(&LayerType::Config));
        assert!(layers.contains_key(&LayerType::Identity));
        assert!(layers.contains_key(&LayerType::Skills));
        assert!(layers.contains_key(&LayerType::Workspace));
        assert!(layers.contains_key(&LayerType::Sessions));
        assert!(layers.contains_key(&LayerType::Mcp));
    }

    #[test]
    fn test_shared_agent_content_different_names() {
        let mut files = HashMap::new();
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "shared-team"
version = "1.0.0"
agent_count = 2

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

        // Two agents with identical config and identity content
        files.insert(
            "agents/agent_a/config/agent.toml".to_string(),
            b"name = \"shared\"\n".to_vec(),
        );
        files.insert(
            "agents/agent_a/identity/did.json".to_string(),
            br#"{"id":"did:peko:shared"}"#.to_vec(),
        );
        files.insert(
            "agents/agent_b/config/agent.toml".to_string(),
            b"name = \"shared\"\n".to_vec(),
        );
        files.insert(
            "agents/agent_b/identity/did.json".to_string(),
            br#"{"id":"did:peko:shared"}"#.to_vec(),
        );

        let result = decompose_team_archive(&files).unwrap();

        assert_eq!(result.agent_layers.len(), 2);
        assert_eq!(result.agent_index.len(), 2);

        let agent_a = result.agent_layers.get("agent_a").unwrap();
        let agent_b = result.agent_layers.get("agent_b").unwrap();

        // Config layers should have identical digests (same content)
        assert_eq!(
            agent_a.get(&LayerType::Config).unwrap().digest,
            agent_b.get(&LayerType::Config).unwrap().digest
        );

        // Identity layers should have identical digests (same content)
        assert_eq!(
            agent_a.get(&LayerType::Identity).unwrap().digest,
            agent_b.get(&LayerType::Identity).unwrap().digest
        );

        // AgentLayerRef digests should also match
        let ref_a = result.agent_index.get("agent_a").unwrap();
        let ref_b = result.agent_index.get("agent_b").unwrap();
        assert_eq!(ref_a.config, ref_b.config);
        assert_eq!(ref_a.identity, ref_b.identity);
    }

    #[test]
    fn test_team_toml_included_in_team_config_layer() {
        let mut files = HashMap::new();
        files.insert(
            "team/manifest.toml".to_string(),
            br#"
[team]
name = "team-with-toml"
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

        // Include team.toml in the archive
        let team_toml_content = b"name = \"team-with-toml\"\ndescription = \"A test team\"\n";
        files.insert("team/team.toml".to_string(), team_toml_content.to_vec());

        files.insert(
            "agents/single/config/agent.toml".to_string(),
            b"name = \"single\"\n".to_vec(),
        );
        files.insert(
            "agents/single/identity/did.json".to_string(),
            br#"{"id":"did:peko:single"}"#.to_vec(),
        );

        let result = decompose_team_archive(&files).unwrap();

        // The TeamConfig layer should contain both manifest.toml AND team.toml
        let decoder = flate2::read::GzDecoder::new(result.team_config_layer.bytes.as_slice());
        let mut archive = tar::Archive::new(decoder);

        let mut found_manifest = false;
        let mut found_team_toml = false;
        let mut team_toml_extracted = Vec::new();

        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if path == "manifest.toml" {
                found_manifest = true;
            }
            if path == "team.toml" {
                found_team_toml = true;
                std::io::Read::read_to_end(&mut entry, &mut team_toml_extracted).unwrap();
            }
        }

        assert!(
            found_manifest,
            "manifest.toml should be in TeamConfig layer"
        );
        assert!(found_team_toml, "team.toml should be in TeamConfig layer");
        assert_eq!(team_toml_extracted, team_toml_content);
    }

    #[test]
    fn test_team_toml_absent_when_not_in_archive() {
        // make_test_team_files() does NOT include team/team.toml
        let files = make_test_team_files();
        let result = decompose_team_archive(&files).unwrap();

        let decoder = flate2::read::GzDecoder::new(result.team_config_layer.bytes.as_slice());
        let mut archive = tar::Archive::new(decoder);

        let mut found_team_toml = false;
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().to_string();
            if path == "team.toml" {
                found_team_toml = true;
            }
        }

        assert!(
            !found_team_toml,
            "team.toml should NOT be in TeamConfig layer when absent from archive"
        );
    }
}
