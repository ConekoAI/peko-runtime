//! Shared fixture builder for `.principal` integration tests.
//!
//! Builds signed (or unsigned) `.principal` packages with embedded skill
//! extensions that declare `requires`/`provides` capabilities. Reuses the
//! canonical `PrincipalPackager`/`PrincipalUnpackager` paths so the fixture
//! shape matches production packages.

#![allow(dead_code)]

use anyhow::Context;
use peko::extensions::framework::store::ExtensionStore;
use peko::extensions::skill::SkillAdapter;
use peko_principal::config::PrincipalConfig;
use peko::registry::packaging::{
    compute_digest, PrincipalExportOptions, PrincipalManifest, PrincipalPackager,
    PrincipalRegistryDescriptor,
};
use peko_identity::{did::DIDScope, Identity};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Fixture description for a skill embedded in the package.
pub struct SkillFixture {
    pub id: String,
    pub requires: Vec<String>,
    pub provides: Vec<String>,
}

/// Builder for a test `.principal` package.
pub struct PrincipalPackageBuilder {
    name: String,
    skills: Vec<SkillFixture>,
    capabilities: Vec<String>,
    unsigned: bool,
}

impl PrincipalPackageBuilder {
    /// Start building a package for a principal named `name`.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            skills: Vec::new(),
            capabilities: Vec::new(),
            unsigned: false,
        }
    }

    /// Embed a skill extension.
    pub fn with_skill(mut self, id: &str, requires: &[&str], provides: &[&str]) -> Self {
        self.skills.push(SkillFixture {
            id: id.to_string(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// Add a capability grant to the principal config.
    pub fn with_capability(mut self, cap: &str) -> Self {
        self.capabilities.push(cap.to_string());
        self
    }

    /// Produce an unsigned package (empty manifest signature).
    pub fn unsigned(mut self) -> Self {
        self.unsigned = true;
        self
    }

    /// Build the `.principal` archive and return its path.
    pub async fn build(self) -> anyhow::Result<PathBuf> {
        let descriptor = self.export().await?;
        if !self.unsigned {
            return Ok(descriptor.package_path);
        }
        strip_signature(&descriptor.package_path)
    }

    /// Build a registry push descriptor for the package.
    ///
    /// For unsigned packages the manifest signature is cleared in the
    /// descriptor so registry-based previews report `signed: false`.
    pub async fn build_descriptor(self) -> anyhow::Result<PrincipalRegistryDescriptor> {
        let mut descriptor = self.export().await?;
        if self.unsigned {
            // The OCI config blob is the signed `manifest.toml`. Clear the
            // signature and replace the config blob in `layer_data` so the
            // registry manifest's config descriptor stays consistent. The
            // content-layer digests in `descriptor.layers` and in the embedded
            // `PrincipalManifest` are unchanged.
            let old_config_digest = compute_digest(&descriptor.manifest_toml);
            let mut manifest =
                PrincipalManifest::from_toml(std::str::from_utf8(&descriptor.manifest_toml)?)?;
            manifest.signatures.manifest = String::new();
            descriptor.manifest_toml = manifest.to_toml()?.into_bytes();
            let new_config_digest = compute_digest(&descriptor.manifest_toml);

            let _ = descriptor.layer_data.remove(&old_config_digest);
            descriptor
                .layer_data
                .insert(new_config_digest, descriptor.manifest_toml.clone());
        }
        Ok(descriptor)
    }

    async fn export(&self) -> anyhow::Result<PrincipalRegistryDescriptor> {
        let temp = tempfile::tempdir()?;
        let base = temp.path();

        // ── Skill extensions ─────────────────────────────────────────────
        let extensions_dir = base.join("extensions");
        for skill in &self.skills {
            create_skill_extension(&extensions_dir, skill).await?;
        }

        let store = ExtensionStore::new();
        store.register_adapter(Box::new(SkillAdapter::new())).await;
        if !self.skills.is_empty() {
            store
                .load_from_directory(&extensions_dir)
                .await
                .context("load skill fixtures into ExtensionStore")?;
        }

        // Reference each embedded skill by `skill:<id>` so the packager
        // resolves and exports it into the extensions layer.
        let mut grants: Vec<String> = self
            .skills
            .iter()
            .map(|s| format!("skill:{}", s.id))
            .chain(self.capabilities.iter().cloned())
            .collect();
        grants.sort();
        grants.dedup();

        // ── Principal config ─────────────────────────────────────────────
        let grants_toml = grants
            .iter()
            .map(|g| format!("{g:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let config_toml = format!(
            r#"
name = {name:?}
description = "Test principal fixture"

[capabilities]
grants = [{grants_toml}]
"#,
            name = self.name,
        );
        let config_dir = base.join("config");
        tokio::fs::create_dir_all(&config_dir).await?;
        tokio::fs::write(config_dir.join("principal.toml"), &config_toml).await?;
        let config: PrincipalConfig =
            toml::from_str(&config_toml).context("parse generated principal.toml for packager")?;

        // ── Identity ─────────────────────────────────────────────────────
        let identity = Identity::generate(DIDScope::Local, Some("fixture"))?;
        let identity_dir = base.join("identity");
        tokio::fs::create_dir_all(&identity_dir).await?;
        let did_doc = identity.to_did_document()?;
        tokio::fs::write(
            identity_dir.join("did.json"),
            serde_json::to_vec_pretty(&did_doc)?,
        )
        .await?;
        let key_export = identity
            .keypair
            .as_ref()
            .context("generated identity has no keypair")?
            .export();
        tokio::fs::write(
            identity_dir.join("keys.enc"),
            serde_json::to_vec(&key_export)?,
        )
        .await?;

        // ── Agent prompt ─────────────────────────────────────────────────
        let agents_dir = base.join("agents");
        tokio::fs::create_dir_all(&agents_dir).await?;
        tokio::fs::write(
            agents_dir.join("primary.md"),
            "---\nname: primary\ndescription: Fixture agent\n---\n\n# Primary\n",
        )
        .await?;

        // ── Packager ─────────────────────────────────────────────────────
        let package_path = base.join(format!("{}.principal", self.name));
        let mut packager =
            PrincipalPackager::new(config.clone(), identity).with_agents_dir(&agents_dir);
        if !self.skills.is_empty() {
            packager = packager
                .with_extensions_from_store(&store, &config)
                .await
                .context("embed skill fixtures")?;
        }

        let export_opts = PrincipalExportOptions {
            output_path: Some(package_path.to_string_lossy().to_string()),
            with_extensions: !self.skills.is_empty(),
            ..Default::default()
        };
        let descriptor = packager.export_for_registry(export_opts).await?;

        // Leak the temp dir so the archive survives until the test finishes.
        let _ = temp.keep();
        Ok(descriptor)
    }
}

async fn create_skill_extension(base: &Path, skill: &SkillFixture) -> anyhow::Result<()> {
    let ext_dir = base.join(&skill.id);
    tokio::fs::create_dir_all(&ext_dir).await?;

    let requires = serde_yaml::to_string(&skill.requires)?;
    let provides = serde_yaml::to_string(&skill.provides)?;
    let manifest = format!(
        "id: {id}\nname: {id}\nextension_type: skill\nversion: 1.0.0\ndescription: Fixture skill\nrequires:\n{requires}provides:\n{provides}",
        id = skill.id,
        requires = indent_yaml_list(&requires),
        provides = indent_yaml_list(&provides),
    );
    tokio::fs::write(ext_dir.join("manifest.yaml"), manifest).await?;

    let skill_md = format!(
        "---\nname: {id}\ndescription: Fixture skill\n---\n\n# {id}\n",
        id = skill.id
    );
    tokio::fs::write(ext_dir.join("SKILL.md"), skill_md).await?;

    Ok(())
}

fn indent_yaml_list(yaml: &str) -> String {
    yaml.lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("  {line}\n")
            }
        })
        .collect()
}

/// Read a tar.gz archive, clear the manifest signature, and write a new
/// unsigned archive next to the original.
fn strip_signature(path: &Path) -> anyhow::Result<PathBuf> {
    let file = std::fs::File::open(path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut entries: BTreeMap<String, (Vec<u8>, u32)> = BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path_str = entry.path()?.to_string_lossy().to_string();
        let mode = entry.header().mode().unwrap_or(0o644);
        let mut data = Vec::new();
        entry.read_to_end(&mut data)?;
        entries.insert(path_str, (data, mode));
    }

    let (manifest_bytes, _) = entries
        .get_mut("manifest.toml")
        .context("manifest.toml not found in package")?;
    let mut manifest = PrincipalManifest::from_toml(std::str::from_utf8(manifest_bytes)?)?;
    manifest.signatures.manifest = String::new();
    *manifest_bytes = manifest.to_toml()?.into_bytes();

    let out_path = path.with_extension("unsigned.principal");
    let out_file = std::fs::File::create(&out_path)?;
    let enc = flate2::write::GzEncoder::new(out_file, flate2::Compression::default());
    let mut builder = tar::Builder::new(enc);
    for (path_str, (data, mode)) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(&path_str)?;
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_cksum();
        builder.append(&header, data.as_slice())?;
    }
    builder.finish()?;

    Ok(out_path)
}
