//! Flat snapshot inventory (ADR-066 D6). Checksums detect corruption;
//! snapshots are local artifacts and carry no signatures or registry layers.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The supported runtime-local snapshot container.
pub const SNAPSHOT_FORMAT: &str = "peko-snapshot-v1";

/// Snapshot metadata and every payload file's SHA-256 checksum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrincipalManifest {
    pub format: String,
    pub name: String,
    pub did: String,
    pub created_at: String,
    pub peko_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub files: BTreeMap<String, String>,
    /// Workspace payloads restored with executable permission on Unix.
    /// Old v1 snapshots omit this field and restore ordinary files.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub executable_files: BTreeSet<String>,
}

impl PrincipalManifest {
    pub fn new(name: impl Into<String>, did: impl Into<String>) -> Self {
        Self {
            format: SNAPSHOT_FORMAT.into(),
            name: name.into(),
            did: did.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            peko_version: crate::VERSION.into(),
            description: None,
            files: BTreeMap::new(),
            executable_files: BTreeSet::new(),
        }
    }

    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let value: toml::Value = toml::from_str(text)?;
        if value.get("principal").is_some() || value.get("layers").is_some() {
            anyhow::bail!("Legacy OCI snapshot format is unsupported; re-export from the source runtime using the current snapshot format.");
        }
        let manifest: Self = value.try_into()?;
        anyhow::ensure!(
            manifest.format == SNAPSHOT_FORMAT,
            "Unsupported snapshot format '{}'; re-export from the source runtime.",
            manifest.format
        );
        Ok(manifest)
    }

    pub fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("sha256:{:x}", Sha256::digest(data))
    }

    pub fn add_file(&mut self, path: impl Into<String>, data: &[u8]) {
        self.files.insert(path.into(), Self::compute_checksum(data));
    }
}

pub(crate) fn is_workspace_payload(path: &str) -> bool {
    path.split_once('/').is_some_and(|(prefix, rest)| {
        matches!(prefix, "tools" | "skills" | "mcp" | "hooks" | "kb") && !rest.is_empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_manifest_roundtrip() {
        let mut m = PrincipalManifest::new("test", "did:peko:test");
        m.add_file("config/principal.toml", b"config");
        m.add_file("skills/check/scripts/check.sh", b"script");
        m.executable_files
            .insert("skills/check/scripts/check.sh".into());
        let text = m.to_toml().unwrap();
        assert!(text.contains("[files]"));
        assert!(!text.contains("layers"));
        assert!(!text.contains("signatures"));
        let parsed = PrincipalManifest::from_toml(&text).unwrap();
        assert_eq!(parsed.files, m.files);
        assert_eq!(parsed.executable_files, m.executable_files);
        assert_eq!(parsed.name, "test");
    }

    #[test]
    fn old_v1_manifest_defaults_to_no_executable_files() {
        let manifest = PrincipalManifest::new("test", "did:peko:test");
        let text = manifest.to_toml().unwrap();
        assert!(!text.contains("executable_files"));
        assert!(PrincipalManifest::from_toml(&text)
            .unwrap()
            .executable_files
            .is_empty());
    }
    #[test]
    fn rejects_legacy_with_reexport_guidance() {
        let err = PrincipalManifest::from_toml(
            "[principal]\nname = 'old'\n[layers]\nconfig = 'sha256:old'",
        )
        .unwrap_err();
        assert!(err
            .to_string()
            .contains("re-export from the source runtime"));
    }
    #[test]
    fn rejects_unknown_format() {
        let mut m = PrincipalManifest::new("test", "did:peko:test");
        m.format = "future".into();
        assert!(PrincipalManifest::from_toml(&m.to_toml().unwrap()).is_err());
    }
}
