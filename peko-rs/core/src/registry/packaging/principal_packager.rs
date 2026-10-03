//! Packager for creating portable Principal packages
//!
//! Exports Principals to `.peko` files (tar.gz archives with manifest).

use crate::principal::config::PrincipalConfig;
use crate::registry::packaging::principal_manifest::PrincipalManifest;
use anyhow::Context;
use peko_identity::Identity;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Export options for a Principal package.
///
/// ADR-056: there is exactly ONE export shape — the full-existence
/// snapshot (definition + identity-bearing local state + workspace
/// tooling). The pre-ADR-056 "definition only" mode and its
/// `include_sessions` hybrid flag are removed: cloning is
/// `peko create -s <seed.toml>` (fresh DID, fresh genesis),
/// and transporting a live principal is export → import of a
/// snapshot (same DID, state verbatim). Seeds are plain TOML files.
#[derive(Debug, Clone)]
pub struct PrincipalExportOptions {
    /// Output path (defaults to `<name>.peko`)
    pub output_path: Option<String>,
    /// Optional description
    pub description: Option<String>,
}

impl Default for PrincipalExportOptions {
    fn default() -> Self {
        Self {
            output_path: None,
            description: None,
        }
    }
}

/// Packager for creating `.peko` packages.
///
/// **ADR-056 (full-existence snapshot):** an export is the
/// principal's live existence — `config/`, `identity/` (DID doc +
/// keys), `agents/`, plus the identity-bearing Local-tier artifacts
/// (`sessions/`, `cron/`, `plans/`) and the workspace tooling the
/// principal installed (`tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`).
/// An export → import round-trip restores working context,
/// self-authored cadence, and tooling — not just the definition.
/// Derived Local state (`cache/`, `locks/`, `memory_index.json`) is
/// excluded and rebuilt at import.
///
/// **Grounding semantics (ADR-056/ADR-060):** there are exactly two ways to
/// ground a principal at a runtime. `peko create -s` grows one
/// from a seed (fresh DID, genesis runs); importing a snapshot
/// wakes a transported one (same DID, boot state and schedule
/// verbatim, no genesis re-seed). A DID is therefore never forked
/// across live runtimes — cloning goes through a seed with a
/// fresh identity.
///
pub struct PrincipalPackager {
    config: PrincipalConfig,
    identity: Identity,
    roles_dir: Option<PathBuf>,
    sessions_dir: Option<PathBuf>,
    workspace_dir: Option<PathBuf>,
    local_root: Option<PathBuf>,
}

impl PrincipalPackager {
    /// Create a new Principal packager.
    pub fn new(config: PrincipalConfig, identity: Identity) -> Self {
        Self {
            config,
            identity,
            roles_dir: None,
            sessions_dir: None,
            workspace_dir: None,
            local_root: None,
        }
    }

    /// Set the roles (prompts) directory.
    pub fn with_roles_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.roles_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the sessions directory.
    pub fn with_sessions_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.sessions_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the principal workspace root (Shared tier) — scanned for
    /// tooling directories (`tools/`, `skills/`, `mcp/`, `hooks/`,
    /// `kb/`).
    pub fn with_workspace_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.workspace_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the Local tier root — scanned for the identity-bearing
    /// state directories (`cron/`, `plans/`).
    pub fn with_local_root(mut self, dir: impl AsRef<Path>) -> Self {
        self.local_root = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Export the Principal to a `.peko` package — a
    /// full-existence snapshot (ADR-056).
    pub async fn export(&self, options: PrincipalExportOptions) -> anyhow::Result<PathBuf> {
        let (files, _manifest) = self.collect_files(options.clone()).await?;
        self.create_archive(&files, &options).await
    }

    /// The stripped seed TOML (ADR-056): a `principal.toml` whose
    /// `id`, `did`, and `boot_state` are removed, so `peko create -s`
    /// mints a fresh identity and infers the boot state.
    pub fn seed_toml(&self) -> anyhow::Result<String> {
        let mut seed_config = self.config.clone();
        seed_config.id = None;
        seed_config.did = None;
        seed_config.boot_state = None;
        toml::to_string_pretty(&seed_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize template config: {e}"))
    }

    /// Collect all files for a full-existence snapshot package without
    /// creating the archive (ADR-056).
    pub async fn collect_files(
        &self,
        options: PrincipalExportOptions,
    ) -> anyhow::Result<(HashMap<String, Vec<u8>>, PrincipalManifest)> {
        let did = self
            .config
            .did
            .as_ref()
            .map(|d| d.0.clone())
            .unwrap_or_else(|| self.identity.did.clone());

        let mut manifest = PrincipalManifest::new(&self.config.name, &did);

        if let Some(ref desc) = options.description {
            manifest.description = Some(desc.clone());
        } else if let Some(ref desc) = self.config.identity.description {
            manifest.description = Some(desc.clone());
        }

        let mut files: HashMap<String, Vec<u8>> = HashMap::new();

        self.export_identity(&mut files, &mut manifest)
            .await
            .context("Failed to export identity")?;
        self.export_config(&mut files, &mut manifest)
            .context("Failed to export config")?;

        self.export_roles(&mut files, &mut manifest)
            .await
            .context("Failed to export roles")?;

        // Identity-bearing Local-tier state (ADR-056): the experiential
        // record (`sessions/`), the trunk's self-authored cadence
        // (`cron/`) and plan DAGs (`plans/`). `cache/` and `locks/`
        // are deliberately NOT scanned — they are ephemeral, never
        // identity-bearing.
        self.export_sessions(&mut files, &mut manifest)
            .await
            .context("Failed to export sessions")?;
        self.export_local_authored(&mut files, &mut manifest)
            .await
            .context("Failed to export local authored state")?;
        self.export_workspace_tooling(&mut files, &mut manifest)
            .await
            .context("Failed to export workspace tooling")?;

        let manifest_toml = manifest.to_toml().context("Failed to serialize manifest")?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        Ok((files, manifest))
    }

    async fn export_identity(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        let did_doc = serde_json::to_vec_pretty(&self.identity.to_did_document()?)?;
        files.insert("identity/did.json".to_string(), did_doc);
        manifest.add_file("identity/did.json", &files["identity/did.json"]);

        let keypair = self
            .identity
            .keypair
            .as_ref()
            .context("Identity has no keypair")?;
        let key_export = keypair.export();
        let key_data = serde_json::to_vec(&key_export)?;

        files.insert("identity/keys.enc".to_string(), key_data);
        manifest.add_file("identity/keys.enc", &files["identity/keys.enc"]);

        Ok(())
    }

    fn export_config(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        let config_toml = toml::to_string_pretty(&self.config)?;
        files.insert(
            "config/principal.toml".to_string(),
            config_toml.into_bytes(),
        );
        manifest.add_file("config/principal.toml", &files["config/principal.toml"]);
        Ok(())
    }

    async fn export_roles(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(dir) = &self.roles_dir {
            if dir.exists() {
                self.export_dir_recursive(dir, "roles", files, manifest)
                    .await?;
            }
        }
        Ok(())
    }

    async fn export_sessions(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(dir) = &self.sessions_dir {
            if dir.exists() {
                self.export_dir_recursive(dir, "sessions", files, manifest)
                    .await?;
            }
        }
        Ok(())
    }

    /// ADR-056: pack the identity-bearing Local-tier directories —
    /// `cron/` (authored schedule + run history) and `plans/` (Plan
    /// DAG storage). Ephemeral local state (`cache/`, `locks/`,
    /// `memory_index.json`) is intentionally skipped.
    async fn export_local_authored(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(local_root) = &self.local_root {
            let cron_dir = local_root.join("cron");
            if cron_dir.exists() {
                self.export_dir_recursive(&cron_dir, "cron", files, manifest)
                    .await?;
            }
            let plans_dir = local_root.join("plans");
            if plans_dir.exists() {
                self.export_dir_recursive(&plans_dir, "plans", files, manifest)
                    .await?;
            }
        }
        Ok(())
    }

    /// ADR-056: pack the workspace tooling directories (Shared tier
    /// root) — `tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`. Presence
    /// in the workspace = visibility (ADR-050), so a snapshot that
    /// drops them would import a principal that silently lost its
    /// tooling.
    async fn export_workspace_tooling(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(workspace_dir) = &self.workspace_dir {
            for dir_name in ["tools", "skills", "mcp", "hooks", "kb"] {
                let dir = workspace_dir.join(dir_name);
                if dir.exists() {
                    self.export_dir_recursive(&dir, dir_name, files, manifest)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn export_dir_recursive(
        &self,
        src_dir: &Path,
        package_prefix: &str,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        self.export_dir_recursive_skipping(src_dir, package_prefix, &[], files, manifest)
            .await
    }

    async fn export_dir_recursive_skipping(
        &self,
        src_dir: &Path,
        package_prefix: &str,
        skip_top_level: &[&str],
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        let mut entries = tokio::fs::read_dir(src_dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let src_path = entry.path();
            let file_name = entry.file_name().to_string_lossy().to_string();
            if skip_top_level.contains(&file_name.as_str()) {
                continue;
            }
            let package_path = format!("{package_prefix}/{file_name}");

            if src_path.is_dir() {
                Box::pin(self.export_dir_recursive(&src_path, &package_path, files, manifest))
                    .await?;
            } else {
                let content = tokio::fs::read(&src_path).await?;
                files.insert(package_path.clone(), content);
                manifest.add_file(&package_path, &files[&package_path]);
            }
        }

        Ok(())
    }

    async fn create_archive(
        &self,
        files: &HashMap<String, Vec<u8>>,
        options: &PrincipalExportOptions,
    ) -> anyhow::Result<PathBuf> {
        let output_path = if let Some(path) = &options.output_path {
            PathBuf::from(path)
        } else {
            PathBuf::from(format!("{}.peko", self.config.name))
        };

        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("Failed to create output directory: {}", parent.display())
                })?;
            }
        }

        let tar_gz = std::fs::File::create(&output_path)?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        for (path, content) in files.iter().collect::<std::collections::BTreeMap<_, _>>() {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_slice())?;
        }

        tar.into_inner()?.finish()?;
        Ok(output_path)
    }
}

/// Convenience function to export a Principal.
pub async fn export_principal(
    config: PrincipalConfig,
    identity: Identity,
    options: PrincipalExportOptions,
) -> anyhow::Result<PathBuf> {
    let packager = PrincipalPackager::new(config, identity);
    packager.export(options).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::PrincipalConfig;
    use peko_identity::did::DIDScope;
    use peko_identity::Identity;
    use peko_subject::PrincipalDID;

    #[test]
    fn test_export_options_default() {
        let opts = PrincipalExportOptions::default();
        assert!(opts.output_path.is_none());
        assert!(opts.description.is_none());
    }

    fn sample_config(name: &str, did: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            id: None,
            did: Some(PrincipalDID(did.to_string())),
            owner: peko_auth::Subject::User("local".to_string()),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            exposure: Default::default(),
            status: None,
            boot_state: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            quota: None,
            children: Default::default(),
        }
    }

    #[tokio::test]
    async fn export_principal_roundtrip() {
        let identity = Identity::new("roundtrip", DIDScope::Local).await.unwrap();
        let config = sample_config("roundtrip", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(
            roles_dir.join("researcher.md"),
            b"# Researcher\nPrompt body",
        )
        .unwrap();

        let out = tmp.path().join("roundtrip.peko");
        let packager = PrincipalPackager::new(config, identity).with_roles_dir(&roles_dir);
        let path = packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(path.exists());
        // Gzip magic bytes
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
    }

    #[tokio::test]
    async fn principal_manifest_inventory_computed() {
        let identity = Identity::new("layers", DIDScope::Local).await.unwrap();
        let config = sample_config("layers", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("roles");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("a.md"), b"prompt").unwrap();

        let packager = PrincipalPackager::new(config, identity).with_roles_dir(&roles_dir);
        let (_files, manifest) = packager
            .collect_files(PrincipalExportOptions::default())
            .await
            .unwrap();

        assert!(manifest.files.contains_key("config/principal.toml"));
        assert!(manifest.files.contains_key("identity/keys.enc"));
        assert!(manifest.files.contains_key("roles/a.md"));
    }

    /// Build a realistic tier layout for snapshot tests: roles in the
    /// shared root, tooling dirs under the workspace root, sessions +
    /// cron + plans under the local root.
    fn seed_layout(tmp: &tempfile::TempDir, name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let shared_root = tmp.path().join("shared").join(name);
        let local_root = tmp.path().join("data").join(name).join("local");

        std::fs::create_dir_all(shared_root.join("roles")).unwrap();
        std::fs::write(shared_root.join("roles").join("root.md"), b"# root").unwrap();

        let tooling = [
            (
                "tools",
                "my-tool/manifest.yaml",
                b"id: my-tool\n".as_slice(),
            ),
            ("skills", "docker/SKILL.md", b"---\nname: docker\n---\nbody"),
            ("mcp", "weather/server.json", b"{\"command\":\"uvx\"}"),
            ("hooks", "notify/hook.toml", b"binds = [\"Stop\"]"),
            ("kb", "MEMORY.md", b"hot memory"),
        ];
        for (dir, file, content) in tooling {
            let path = shared_root.join(dir).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }

        std::fs::create_dir_all(local_root.join("sessions")).unwrap();
        std::fs::write(local_root.join("sessions").join("s1.jsonl"), b"{}\n").unwrap();
        std::fs::create_dir_all(local_root.join("cron")).unwrap();
        std::fs::write(
            local_root.join("cron").join("schedule.toml"),
            "version = 2\n[[jobs]]\nid = \"keepalive\"\nprincipal_id = \"prin_old\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(local_root.join("plans")).unwrap();
        std::fs::write(local_root.join("plans").join("p1.jsonl"), b"{}\n").unwrap();
        // Ephemeral local state — must never be packaged (ADR-056).
        std::fs::write(local_root.join("memory_index.json"), b"{}").unwrap();
        std::fs::create_dir_all(local_root.join("cache")).unwrap();
        std::fs::write(local_root.join("cache").join("scratch"), b"x").unwrap();
        std::fs::create_dir_all(local_root.join("locks")).unwrap();
        std::fs::write(local_root.join("locks").join("l.lock"), b"x").unwrap();

        (shared_root, local_root, tmp.path().to_path_buf())
    }

    #[tokio::test]
    async fn snapshot_collects_local_state_and_tooling() {
        let identity = Identity::new("snapshot", DIDScope::Local).await.unwrap();
        let config = sample_config("snapshot", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let (shared_root, local_root, _tmp_path) = seed_layout(&tmp, "snapshot");

        let packager = PrincipalPackager::new(config, identity)
            .with_roles_dir(shared_root.join("roles"))
            .with_sessions_dir(local_root.join("sessions"))
            .with_workspace_dir(&shared_root)
            .with_local_root(&local_root);

        let (files, manifest) = packager
            .collect_files(PrincipalExportOptions::default())
            .await
            .unwrap();

        assert!(files.contains_key("sessions/s1.jsonl"), "sessions packed");
        assert!(files.contains_key("cron/schedule.toml"), "cron packed");
        assert!(files.contains_key("plans/p1.jsonl"), "plans packed");
        assert_eq!(manifest.files.len() + 1, files.len());
        for (path, checksum) in &manifest.files {
            assert_eq!(checksum, &PrincipalManifest::compute_checksum(&files[path]));
        }
        assert!(files.contains_key("tools/my-tool/manifest.yaml"));
        assert!(files.contains_key("skills/docker/SKILL.md"));
        assert!(files.contains_key("mcp/weather/server.json"));
        assert!(files.contains_key("hooks/notify/hook.toml"));
        assert!(files.contains_key("kb/MEMORY.md"));

        assert!(
            !files.keys().any(|p| p.starts_with("cache/")),
            "cache must not be packaged"
        );
        assert!(
            !files.keys().any(|p| p.starts_with("locks/")),
            "locks must not be packaged"
        );
        assert!(
            !files.contains_key("memory_index.json"),
            "memory index is derived state — not packaged"
        );
    }

    /// ADR-056: the seed artifact is a plain TOML template — a
    /// `principal.toml` stripped of `id`/`did`/`boot_state`. It is
    /// not a package at all: inspectable with `cat`, groundable via
    /// `peko create -s`, and incapable of carrying keys or lived
    /// state.
    #[tokio::test]
    async fn seed_is_plain_toml() {
        use crate::principal::config::BootState;

        let identity = Identity::new("template", DIDScope::Local).await.unwrap();
        let mut config = sample_config("template", &identity.did);
        config.id = Some(peko_subject::PrincipalId("prin_template_src".into()));
        config.set_boot_state(BootState::Organized);

        let packager = PrincipalPackager::new(config, identity);
        let artifact = packager.seed_toml().unwrap();
        assert!(!artifact.contains("prin_template_src"), "{artifact}");
        assert!(!artifact.contains("boot_state"), "{artifact}");
        assert!(!artifact.contains("keys"), "{artifact}");
    }
}
