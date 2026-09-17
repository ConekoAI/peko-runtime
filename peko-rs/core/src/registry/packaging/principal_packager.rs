//! Packager for creating portable Principal packages
//!
//! Exports Principals to `.peko` files (tar.gz archives with manifest).

use crate::principal::config::PrincipalConfig;
use crate::registry::packaging::principal_manifest::{PrincipalLayers, PrincipalManifest};
use crate::registry::packaging::types::{compute_digest, Layer};
use anyhow::Context;
use peko_identity::Identity;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Export options for a Principal package.
///
/// ADR-056: there is exactly ONE export shape — the full-existence
/// snapshot (definition + identity-bearing local state + workspace
/// tooling). The pre-ADR-056 "definition only" mode and its
/// `include_sessions` hybrid flag are removed: cloning is
/// `peko create -s <seed.toml>` (fresh DID, fresh genesis),
/// and transporting a live principal is export → import of a
/// snapshot (same DID, state verbatim). Registry distribution uses
/// [`PrincipalPackager::export_for_registry`], which emits a
/// DID-free, key-free template payload — never a snapshot.
#[derive(Debug, Clone)]
pub struct PrincipalExportOptions {
    /// Output path (defaults to `<name>.principal`)
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

/// Registry-specific push descriptor produced alongside the local package.
#[derive(Debug, Clone)]
pub struct PrincipalRegistryDescriptor {
    /// Package file path
    pub package_path: PathBuf,
    /// Principal manifest TOML bytes (used as the OCI config blob)
    pub manifest_toml: Vec<u8>,
    /// Layer descriptors for the registry manifest
    pub layers: Vec<Layer>,
    /// Layer content indexed by digest (includes the config blob)
    pub layer_data: HashMap<String, Vec<u8>>,
    /// Raw `identity/did.json` bytes, used to pre-validate the manifest
    /// signature before pushing to a registry.
    pub did_doc: Vec<u8>,
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
/// **Grounding semantics (ADR-056):** there are exactly two ways to
/// ground a principal at a runtime. `peko create -s` grows one
/// from a template (fresh DID, genesis runs); importing a snapshot
/// wakes a transported one (same DID, boot state and schedule
/// verbatim, no genesis re-seed). A DID is therefore never forked
/// across live runtimes — cloning goes through a template with a
/// fresh identity.
///
/// **Registry artifact:** [`PrincipalPackager::export_for_registry`]
/// emits a *template* payload — config with `id`/`did`/`boot_state`
/// stripped, agent prompts, and workspace tooling, signed by the
/// source DID as endorsement. No keys, no sessions, no local state
/// ever leaves the host through the registry; a pulled seed
/// clones via a freshly minted identity.
///
/// **Phase A/5/7 history:** the legacy `with_memory_dir` knob and the
/// embedded-extension paths are gone; workspace tooling lives in the
/// principal's workspace; the `plugins/` layer convention (always
/// empty today) supersedes the legacy `extensions/` layer, which the
/// unpackager still accepts on import.
pub struct PrincipalPackager {
    config: PrincipalConfig,
    identity: Identity,
    agents_dir: Option<PathBuf>,
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
            agents_dir: None,
            sessions_dir: None,
            workspace_dir: None,
            local_root: None,
        }
    }

    /// Set the agents (prompts) directory.
    pub fn with_agents_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.agents_dir = Some(dir.as_ref().to_path_buf());
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

    /// Emit the seed payload for registry distribution (ADR-056).
    ///
    /// The registry distributes DNA, not creatures — and DNA is a
    /// **plain TOML file** (a `principal.toml` with `id`, `did`, and
    /// `boot_state` stripped), exactly the shape `peko create -s`
    /// consumes. No package wrapper, no keys, no sessions, no
    /// cron/plans ever leave the host through the registry: the TOML
    /// is inspectable with `cat`, diffable, and groundable via a
    /// freshly minted identity and genesis. The source principal's
    /// public DID document rides along in the OCI descriptor as
    /// publisher provenance; cryptographic endorsement of the file
    /// bytes is a follow-up (registry credentials establish the
    /// publisher for now).
    pub async fn export_for_registry(
        self,
        options: PrincipalExportOptions,
    ) -> anyhow::Result<PrincipalRegistryDescriptor> {
        let seed_toml = self.seed_toml()?;
        let seed_bytes = seed_toml.into_bytes();

        // Write the TOML artifact to disk (the caller treats it as the
        // pushed artifact and may clean it up).
        let artifact_path = match &options.output_path {
            Some(p) => PathBuf::from(p),
            None => PathBuf::from(format!("{}.seed.toml", self.config.name)),
        };
        if let Some(parent) = artifact_path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("Failed to create output directory: {}", parent.display())
            })?;
        }
        tokio::fs::write(&artifact_path, &seed_bytes).await?;

        // OCI transport: the seed TOML IS the config blob; there
        // are no content layers.
        let mut layer_data: HashMap<String, Vec<u8>> = HashMap::new();
        let config_digest = compute_digest(&seed_bytes);
        layer_data.insert(config_digest.clone(), seed_bytes.clone());

        let did_doc = serde_json::to_vec_pretty(&self.identity.to_did_document()?)?;

        Ok(PrincipalRegistryDescriptor {
            package_path: artifact_path,
            manifest_toml: seed_bytes,
            layers: Vec::new(),
            layer_data,
            did_doc,
        })
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

        let mut manifest = PrincipalManifest::new(&self.config.name, "1.0.0", &did);

        if let Some(ref desc) = options.description {
            manifest.principal.description = Some(desc.clone());
        } else if let Some(ref desc) = self.config.identity.description {
            manifest.principal.description = Some(desc.clone());
        }

        let mut files: HashMap<String, Vec<u8>> = HashMap::new();

        self.export_identity(&mut files, &mut manifest)
            .await
            .context("Failed to export identity")?;
        self.export_config(&mut files, &mut manifest)
            .context("Failed to export config")?;

        // Phase 5/7 (ADR-047): no embedded-extension paths; the
        // `plugins/` layer convention is emitted (always empty today)
        // and the legacy `extensions/` prefix is dropped entirely
        // from packager output.

        self.export_agents(&mut files, &mut manifest)
            .await
            .context("Failed to export agents")?;

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

        manifest.layers = Some(Self::compute_layers(&files)?);
        self.sign_manifest(&mut manifest)
            .context("Failed to sign manifest")?;

        let manifest_toml = manifest.to_toml().context("Failed to serialize manifest")?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        Ok((files, manifest))
    }

    fn compute_layers(files: &HashMap<String, Vec<u8>>) -> anyhow::Result<PrincipalLayers> {
        let mut layers = PrincipalLayers::default();

        // Phase 7 (ADR-047 §5): the canonical plugin layer is `plugins/`;
        // the legacy `extensions/` prefix is no longer emitted by the
        // packager. The unpackager still accepts it on import.
        // ADR-056: full-snapshot layers (`cron`, `plans`, `tools`,
        // `skills`, `mcp`, `hooks`, `kb`) are computed from whatever
        // the collector packed.
        let layer_prefixes = [
            "config", "identity", "agents", "memory", "sessions", "cron", "plans", "tools",
            "skills", "mcp", "hooks", "kb", "plugins",
        ];

        for prefix in layer_prefixes {
            let mut layer_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for (path, content) in files {
                if path.starts_with(&format!("{prefix}/")) {
                    let layer_path = path.strip_prefix(&format!("{prefix}/")).unwrap_or(path);
                    layer_files.insert(layer_path.to_string(), content.clone());
                }
            }

            if !layer_files.is_empty() {
                let digest = Self::build_layer_digest(&layer_files)?;
                match prefix {
                    "config" => layers.config = Some(digest),
                    "identity" => layers.identity = Some(digest),
                    "agents" => layers.agents = Some(digest),
                    "memory" => layers.memory = Some(digest),
                    "sessions" => layers.sessions = Some(digest),
                    "cron" => layers.cron = Some(digest),
                    "plans" => layers.plans = Some(digest),
                    "tools" => layers.tools = Some(digest),
                    "skills" => layers.skills = Some(digest),
                    "mcp" => layers.mcp = Some(digest),
                    "hooks" => layers.hooks = Some(digest),
                    "kb" => layers.kb = Some(digest),
                    "plugins" => layers.plugins = Some(digest),
                    _ => {}
                }
            }
        }

        Ok(layers)
    }

    fn build_layer_digest(files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<String> {
        let (digest, _bytes) = Self::build_layer_digest_and_bytes(files)?;
        Ok(digest)
    }

    /// Build a deterministic gzip tar layer and return both its digest and bytes.
    fn build_layer_digest_and_bytes(
        files: &BTreeMap<String, Vec<u8>>,
    ) -> anyhow::Result<(String, Vec<u8>)> {
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
        let digest = compute_digest(&buf);
        Ok((digest, buf))
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

    async fn export_agents(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(dir) = &self.agents_dir {
            if dir.exists() {
                self.export_dir_recursive(dir, "agents", files, manifest)
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

    fn sign_manifest(&self, manifest: &mut PrincipalManifest) -> anyhow::Result<()> {
        let manifest_for_signing = PrincipalManifest {
            signatures: crate::registry::packaging::manifest::Signatures {
                manifest: String::new(),
                algorithm: "ed25519".to_string(),
            },
            ..manifest.clone()
        };

        let manifest_toml = manifest_for_signing.to_toml()?;

        let keypair = self
            .identity
            .keypair
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Identity has no keypair"))?;
        let signature = keypair.sign(manifest_toml.as_bytes());

        use base64::Engine;
        manifest.signatures.manifest =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature.to_bytes());
        manifest.signatures.algorithm = "ed25519".to_string();

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

        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_slice())?;
        }

        tar.finish()?;
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
            capabilities: Default::default(),
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
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("researcher.md"),
            b"# Researcher\nPrompt body",
        )
        .unwrap();

        let out = tmp.path().join("roundtrip.peko");
        let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);
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
    async fn principal_manifest_layers_computed() {
        let identity = Identity::new("layers", DIDScope::Local).await.unwrap();
        let config = sample_config("layers", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("a.md"), b"prompt").unwrap();

        let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);
        let (_files, manifest) = packager
            .collect_files(PrincipalExportOptions::default())
            .await
            .unwrap();

        let layers = manifest.layers.expect("layers computed");
        assert!(layers.config.is_some(), "config layer present");
        assert!(layers.identity.is_some(), "identity layer present");
        assert!(layers.agents.is_some(), "agents layer present");
        assert!(!manifest.signatures.manifest.is_empty(), "manifest signed");
    }

    /// Build a realistic tier layout for snapshot tests: agents in the
    /// shared root, tooling dirs under the workspace root, sessions +
    /// cron + plans under the local root.
    fn seed_layout(tmp: &tempfile::TempDir, name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let shared_root = tmp.path().join("shared").join(name);
        let local_root = tmp.path().join("data").join(name).join("local");

        std::fs::create_dir_all(shared_root.join("agents")).unwrap();
        std::fs::write(shared_root.join("agents").join("root.md"), b"# root").unwrap();

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
    async fn snapshot_collects_local_and_tooling_layers() {
        let identity = Identity::new("snapshot", DIDScope::Local).await.unwrap();
        let config = sample_config("snapshot", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let (shared_root, local_root, _tmp_path) = seed_layout(&tmp, "snapshot");

        let packager = PrincipalPackager::new(config, identity)
            .with_agents_dir(shared_root.join("agents"))
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

        let layers = manifest.layers.expect("layers computed");
        assert!(layers.cron.is_some(), "cron layer digest");
        assert!(layers.plans.is_some(), "plans layer digest");
        assert!(layers.tools.is_some(), "tools layer digest");
        assert!(layers.kb.is_some(), "kb layer digest");
        assert!(layers.sessions.is_some(), "sessions layer digest");
    }

    /// ADR-056: the registry artifact is a plain TOML template — a
    /// `principal.toml` stripped of `id`/`did`/`boot_state`. It is
    /// not a package at all: inspectable with `cat`, groundable via
    /// `peko create -s`, and incapable of carrying keys or lived
    /// state.
    #[tokio::test]
    async fn registry_seed_is_a_plain_toml() {
        use crate::principal::config::BootState;

        let identity = Identity::new("template", DIDScope::Local).await.unwrap();
        let mut config = sample_config("template", &identity.did);
        config.id = Some(peko_subject::PrincipalId("prin_template_src".into()));
        config.set_boot_state(BootState::Organized);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("template.seed.toml");

        let packager = PrincipalPackager::new(config, identity);
        let descriptor = packager
            .export_for_registry(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // The artifact on disk is the TOML itself — no keys, no
        // source identity, no boot state.
        let artifact = std::fs::read_to_string(&out).unwrap();
        assert!(!artifact.contains("prin_template_src"), "{artifact}");
        assert!(!artifact.contains("boot_state"), "{artifact}");
        assert!(!artifact.contains("keys"), "{artifact}");

        // OCI transport: the TOML is the config blob; zero layers.
        assert!(descriptor.layers.is_empty());
        assert_eq!(descriptor.manifest_toml, artifact.as_bytes());
        assert_eq!(descriptor.layer_data.len(), 1);
    }

    #[tokio::test]
    async fn export_for_registry_includes_layer_bytes() {
        let identity = Identity::new("registry", DIDScope::Local).await.unwrap();
        let config = sample_config("registry", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("registry.peko");
        let packager = PrincipalPackager::new(config, identity);
        let descriptor = packager
            .export_for_registry(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // The config blob is the manifest TOML and is present in layer_data.
        let config_digest = compute_digest(&descriptor.manifest_toml);
        assert!(descriptor.layer_data.contains_key(&config_digest));
        // Every declared layer has its bytes available.
        for layer in &descriptor.layers {
            assert!(
                descriptor.layer_data.contains_key(&layer.digest),
                "missing bytes for layer {}",
                layer.digest
            );
            assert_eq!(
                layer.size_bytes,
                descriptor.layer_data[&layer.digest].len() as u64
            );
        }
    }
}
