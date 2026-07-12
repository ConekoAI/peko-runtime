//! Packager for creating portable Principal packages
//!
//! Exports Principals to `.principal` files (tar.gz archives with manifest).

use crate::extensions::framework::manager::packaging::ExtensionPackager;
use crate::extensions::framework::store::ExtensionStore;
use crate::extensions::framework::types::ExtensionId;
use crate::identity::Identity;
use crate::principal::config::PrincipalConfig;
use crate::registry::packaging::principal_manifest::{PrincipalLayers, PrincipalManifest};
use crate::registry::packaging::types::{compute_digest, ExtensionRef, Layer, LayerType};
use anyhow::Context;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Export options for a Principal package.
#[derive(Debug, Clone)]
pub struct PrincipalExportOptions {
    /// Output path (defaults to `<name>.principal`)
    pub output_path: Option<String>,
    /// Include session history (can be large)
    pub include_sessions: bool,
    /// Embed extension packages in an `extensions/` layer
    pub with_extensions: bool,
    /// Optional description
    pub description: Option<String>,
}

impl Default for PrincipalExportOptions {
    fn default() -> Self {
        Self {
            output_path: None,
            include_sessions: false,
            with_extensions: false,
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

/// Packager for creating `.principal` packages.
pub struct PrincipalPackager {
    config: PrincipalConfig,
    identity: Identity,
    agents_dir: Option<PathBuf>,
    memory_dir: Option<PathBuf>,
    sessions_dir: Option<PathBuf>,
    extension_refs: Vec<ExtensionRef>,
    embedded_extensions: HashMap<String, Vec<u8>>,
}

impl PrincipalPackager {
    /// Create a new Principal packager.
    pub fn new(config: PrincipalConfig, identity: Identity) -> Self {
        Self {
            config,
            identity,
            agents_dir: None,
            memory_dir: None,
            sessions_dir: None,
            extension_refs: Vec::new(),
            embedded_extensions: HashMap::new(),
        }
    }

    /// Set the agents (prompts) directory.
    pub fn with_agents_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.agents_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the memory directory.
    pub fn with_memory_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.memory_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set the sessions directory.
    pub fn with_sessions_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.sessions_dir = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set extension references to record in the manifest.
    pub fn with_extension_refs(mut self, refs: Vec<ExtensionRef>) -> Self {
        self.extension_refs = refs;
        self
    }

    /// Set embedded extension packages to include in the `extensions/` layer.
    pub fn with_embedded_extensions(mut self, extensions: HashMap<String, Vec<u8>>) -> Self {
        self.embedded_extensions = extensions;
        self
    }

    /// Resolve all extensions referenced by `config.capabilities` through the
    /// loaded `ExtensionStore`, export each installed extension to a `.ext`
    /// package, and attach the resulting `ExtensionRef`s and embedded bytes.
    ///
    /// Names that cannot be resolved to an installed extension are skipped with
    /// a warning; this avoids failing a push because an extension name points
    /// to a not-yet-installed extension.
    pub async fn with_extensions_from_store(
        mut self,
        store: &ExtensionStore,
        config: &PrincipalConfig,
    ) -> anyhow::Result<Self> {
        let mut refs = Vec::new();
        let mut embedded = HashMap::new();
        let mut seen = HashSet::new();

        let names: Vec<&str> = config.capabilities.iter().map(|c| c.as_str()).collect();

        if names.is_empty() {
            return Ok(self);
        }

        // tempfile is a dev-dependency only, so build a unique temp directory
        // manually for production code.
        let temp_dir = std::env::temp_dir().join(format!(
            "peko-principal-ext-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        ));
        std::fs::create_dir_all(&temp_dir)?;

        for name in names {
            // Capabilities are typed strings like `skill:github_skill` or
            // `agent:researcher`. Strip the capability prefix so we can
            // resolve the underlying extension name/ID; skip wildcards and
            // built-in tool grants that do not reference an installed
            // extension package.
            let normalized = name
                .strip_prefix("skill:")
                .or_else(|| name.strip_prefix("agent:"))
                .or_else(|| name.strip_prefix("tool:"))
                .or_else(|| name.strip_prefix("mcp:"))
                .unwrap_or(name);
            if normalized.contains('*') {
                continue;
            }

            let Some(resolution) = store.resolve_tool_name(normalized).await else {
                tracing::warn!(
                    "Principal extension '{}' does not resolve to an installed extension; skipping embed",
                    name
                );
                continue;
            };
            if !seen.insert(resolution.id.clone()) {
                continue;
            }

            let ext_id = ExtensionId::new(&resolution.id);
            let temp_path = temp_dir.join(format!("{}.ext", resolution.id));
            ExtensionPackager::export(store, &ext_id, &temp_path)
                .await
                .with_context(|| format!("Failed to export extension {}", resolution.id))?;
            let bytes = std::fs::read(&temp_path)
                .with_context(|| format!("Failed to read exported extension {}", resolution.id))?;

            let registry_ref = resolution
                .registry_ref
                .unwrap_or_else(|| resolution.id.clone());
            refs.push(ExtensionRef {
                id: resolution.id.clone(),
                registry_ref,
            });
            embedded.insert(format!("extensions/{}.ext", resolution.id), bytes);
        }

        // Best-effort cleanup of the temp directory.
        let _ = std::fs::remove_dir_all(&temp_dir);

        self.extension_refs = refs;
        self.embedded_extensions = embedded;
        Ok(self)
    }

    /// Export the Principal to a `.principal` package.
    pub async fn export(&self, options: PrincipalExportOptions) -> anyhow::Result<PathBuf> {
        let (files, _manifest) = self.collect_files(options.clone()).await?;
        self.create_archive(&files, &options).await
    }

    /// Export and produce the descriptors needed for registry push.
    pub async fn export_for_registry(
        self,
        options: PrincipalExportOptions,
    ) -> anyhow::Result<PrincipalRegistryDescriptor> {
        let (files, manifest) = self.collect_files(options.clone()).await?;
        let package_path = self.create_archive(&files, &options).await?;

        let manifest_toml = manifest.to_toml()?.into_bytes();
        let mut layer_data: HashMap<String, Vec<u8>> = HashMap::new();
        let mut layers = Vec::new();

        // OCI config blob is the signed principal manifest.
        let config_digest = compute_digest(&manifest_toml);
        layer_data.insert(config_digest.clone(), manifest_toml.clone());

        let prefix_layers = [
            ("config", LayerType::Config),
            ("identity", LayerType::Identity),
            ("agents", LayerType::Workspace),
            ("memory", LayerType::Sessions),
            ("sessions", LayerType::Sessions),
            ("extensions", LayerType::Extensions),
        ];

        for (prefix, layer_type) in prefix_layers {
            let mut layer_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
            for (path, content) in &files {
                if let Some(rest) = path.strip_prefix(&format!("{prefix}/")) {
                    layer_files.insert(rest.to_string(), content.clone());
                }
            }

            if !layer_files.is_empty() {
                let (digest, bytes) = Self::build_layer_digest_and_bytes(&layer_files)?;
                let size = bytes.len() as u64;
                layer_data.insert(digest.clone(), bytes);
                layers.push(Layer::new(digest, layer_type, size));
            }
        }

        Ok(PrincipalRegistryDescriptor {
            package_path,
            manifest_toml,
            layers,
            layer_data,
            did_doc: files.get("identity/did.json").cloned().unwrap_or_default(),
        })
    }

    /// Collect all files for the package without creating the archive.
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

        manifest.extensions = self.extension_refs.clone();

        if options.with_extensions {
            for (path, content) in &self.embedded_extensions {
                files.insert(path.clone(), content.clone());
                manifest.add_file(path, content);
            }
        }

        self.export_agents(&mut files, &mut manifest)
            .await
            .context("Failed to export agents")?;
        self.export_memory(&mut files, &mut manifest)
            .await
            .context("Failed to export memory")?;

        if options.include_sessions {
            self.export_sessions(&mut files, &mut manifest)
                .await
                .context("Failed to export sessions")?;
        }

        manifest.layers = Some(Self::compute_layers(&files)?);
        self.sign_manifest(&mut manifest)
            .context("Failed to sign manifest")?;

        let manifest_toml = manifest.to_toml().context("Failed to serialize manifest")?;
        files.insert("manifest.toml".to_string(), manifest_toml.into_bytes());

        Ok((files, manifest))
    }

    fn compute_layers(files: &HashMap<String, Vec<u8>>) -> anyhow::Result<PrincipalLayers> {
        let mut layers = PrincipalLayers::default();

        let layer_prefixes = [
            "config",
            "identity",
            "agents",
            "memory",
            "sessions",
            "extensions",
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
                    "extensions" => layers.extensions = Some(digest),
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

    async fn export_memory(
        &self,
        files: &mut HashMap<String, Vec<u8>>,
        manifest: &mut PrincipalManifest,
    ) -> anyhow::Result<()> {
        if let Some(dir) = &self.memory_dir {
            if dir.exists() {
                // Sessions live under `memory/sessions` on disk but are a
                // separate package layer; skip them here so the memory
                // layer stays free of (potentially large) session history.
                self.export_dir_recursive_skipping(dir, "memory", &["sessions"], files, manifest)
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
            PathBuf::from(format!("{}.principal", self.config.name))
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
    use crate::identity::did::DIDScope;
    use crate::identity::Identity;
    use crate::principal::config::PrincipalConfig;
    use crate::subject::PrincipalDID;

    #[test]
    fn test_export_options_default() {
        let opts = PrincipalExportOptions::default();
        assert!(opts.output_path.is_none());
        assert!(!opts.include_sessions);
        assert!(!opts.with_extensions);
    }

    fn sample_config(name: &str, did: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: Some(PrincipalDID(did.to_string())),
            owner: crate::auth::Subject::User("local".to_string()),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
            memory: Default::default(),
            routing: Default::default(),
            capabilities: Default::default(),
            exposure: Default::default(),
            status: None,
            permissions: Vec::new(),
            preferred_provider_id: None,
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
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

        let out = tmp.path().join("roundtrip.principal");
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

    #[tokio::test]
    async fn export_for_registry_includes_layer_bytes() {
        let identity = Identity::new("registry", DIDScope::Local).await.unwrap();
        let config = sample_config("registry", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("registry.principal");
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
