//! Unpackager for importing portable Principal packages
//!
//! Extracts `.principal` files into the local peko runtime.
#![allow(dead_code)]

use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::identity::{storage::KeyStorage, Identity, KeyPairExport};
use crate::principal::config::{PrincipalConfig, PrincipalDID};
use crate::registry::packaging::principal_manifest::PrincipalManifest;
use crate::registry::packaging::trust_store::{TrustPolicy, TrustStatus, TrustStore};
use crate::registry::packaging::validation::ValidationResult;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Import options for a Principal package.
#[derive(Debug, Clone)]
pub struct PrincipalImportOptions {
    /// Rename the imported Principal
    pub new_name: Option<String>,
    /// Rotate keys (generate a new DID)
    pub rotate_keys: bool,
    /// Import session history
    pub import_sessions: bool,
    /// Allow importing an unsigned package
    pub allow_unsigned: bool,
    /// Force overwrite an existing Principal
    pub force: bool,
    /// Daemon-wide trust store used for TOFU pinning.
    pub trust_store: Option<Arc<RwLock<TrustStore>>>,
    /// How to handle trust pinning conflicts.
    pub trust_policy: TrustPolicy,
}

impl Default for PrincipalImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            rotate_keys: false,
            import_sessions: true,
            allow_unsigned: false,
            force: false,
            trust_store: None,
            trust_policy: TrustPolicy::Tofu,
        }
    }
}

/// Import result for a Principal package.
#[derive(Debug, Clone)]
pub struct PrincipalImportResult {
    /// Principal name
    pub name: String,
    /// Principal DID
    pub did: String,
    /// Path to imported config
    pub config_path: PathBuf,
    /// Whether keys were rotated
    pub keys_rotated: bool,
    /// Validation result
    pub validation: ValidationResult,
}

/// Unpackager for importing `.principal` packages.
pub struct PrincipalUnpackager {
    package_path: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl PrincipalUnpackager {
    /// Create a new Principal unpackager.
    pub fn new(package_path: impl AsRef<Path>, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            package_path: package_path.as_ref().to_path_buf(),
            config_dir,
            data_dir,
        }
    }

    /// Inspect a package without importing.
    pub async fn inspect(&self,
    ) -> anyhow::Result<(PrincipalManifest, ValidationResult)> {
        let files = self.extract_package().await?;
        let manifest = self.parse_manifest(&files)?;
        let validation = validate_package_for_principal(&manifest, &files);
        Ok((manifest, validation))
    }

    /// Import the package from a file.
    pub async fn import(
        &self,
        options: PrincipalImportOptions,
    ) -> anyhow::Result<PrincipalImportResult> {
        let files = self.extract_package().await?;
        self.import_from_files(files, options).await
    }

    async fn import_from_files(
        &self,
        files: HashMap<String, Vec<u8>>,
        options: PrincipalImportOptions,
    ) -> anyhow::Result<PrincipalImportResult> {
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml"))?
            .clone();
        let manifest = self.parse_manifest(&files)?;

        // Signature verification
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?;
        let (signature_status, public_key_multibase) = match verify_principal_signature(
            &manifest_bytes,
            did_doc_bytes,
            options.allow_unsigned,
            &manifest.principal.name,
        ) {
            Ok((SignatureStatus::Verified, pk)) => {
                tracing::debug!(
                    "principal manifest signature verified for '{}'",
                    manifest.principal.name
                );
                (SignatureStatus::Verified, pk)
            }
            Ok((SignatureStatus::AllowedUnsigned, _)) => (SignatureStatus::AllowedUnsigned, String::new()),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "[signature_verification_failed] Manifest signature check failed: {e}"
                ));
            }
        };

        let trust_name = options
            .new_name
            .as_ref()
            .unwrap_or(&manifest.principal.name)
            .clone();
        if signature_status == SignatureStatus::Verified {
            let resolver = PathResolver::with_dirs(
                self.config_dir.clone(),
                self.data_dir.clone(),
                self.data_dir.clone(),
            );
            enforce_trust_pinning(
                options.trust_store.as_ref(),
                options.trust_policy,
                &resolver,
                &trust_name,
                &manifest.principal.did,
                &public_key_multibase,
            )
            .await?;
        }

        let validation = validate_package_for_principal(&manifest, &files);
        if !validation.is_valid() && !options.force {
            return Err(anyhow::anyhow!(
                "Package validation failed. Use --force to import anyway.\n{}",
                validation.error_report()
            ));
        }

        let name = options
            .new_name
            .clone()
            .unwrap_or_else(|| manifest.principal.name.clone());

        let identity = self.import_identity(&files, &manifest, &options, &name).await?;
        let mut config = self.import_config(&files, &name, &identity)?;

        // Update DID in config to match the imported/rotated identity
        config.did = Some(PrincipalDID(identity.did.clone()));
        config.name = name.clone();

        // Default owner to local user unless already set
        if matches!(config.owner, Subject::User(ref u) if u == "default") {
            config.owner = Subject::User("local".to_string());
        }

        self.import_agents(&files, &name).await?;
        self.import_memory(&files, &name).await?;

        if options.import_sessions {
            self.import_sessions(&files, &name).await?;
        }

        let config_path = self.save_config(&config, &name).await?;

        Ok(PrincipalImportResult {
            name,
            did: identity.did,
            config_path,
            keys_rotated: options.rotate_keys,
            validation,
        })
    }

    async fn extract_package(&self,
    ) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let file = std::fs::File::open(&self.package_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut files = HashMap::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let path_str = path.to_string_lossy().to_string();
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            files.insert(path_str, content);
        }
        Ok(files)
    }

    fn parse_manifest(
        &self,
        files: &HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<PrincipalManifest> {
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        PrincipalManifest::from_toml(manifest_str)
    }

    async fn import_identity(
        &self,
        files: &HashMap<String, Vec<u8>>,
        manifest: &PrincipalManifest,
        options: &PrincipalImportOptions,
        principal_name: &str,
    ) -> anyhow::Result<Identity> {
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?;
        let did_doc: crate::identity::DIDDocument = serde_json::from_slice(did_doc_bytes)?;

        let identity_dir = self
            .data_dir
            .join("principals")
            .join(principal_name)
            .join("identity");

        if options.rotate_keys {
            let new_identity =
                Identity::new(&manifest.principal.name, crate::identity::did::DIDScope::Local)
                    .await?;
            let key_storage = KeyStorage::with_path(identity_dir)?;
            key_storage.store_identity(&new_identity).await?;
            return Ok(new_identity);
        }

        let encrypted_keys = files
            .get("identity/keys.enc")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/keys.enc"))?;
        let key_data = if manifest.identity.encrypted {
            anyhow::bail!("Encrypted principal packages are not yet supported")
        } else {
            encrypted_keys.clone()
        };

        let key_export: KeyPairExport = serde_json::from_slice(&key_data)?;
        let identity = Identity::from_did_document_and_key(did_doc, key_export)?;

        let key_storage = KeyStorage::with_path(identity_dir)?;
        if key_storage.exists(&identity.did) && !options.force {
            anyhow::bail!(
                "DID {} already exists locally. Use --force to overwrite or --rotate-keys to generate a new identity.",
                identity.did
            );
        }
        key_storage.store_identity(&identity).await?;

        Ok(identity)
    }

    fn import_config(
        &self,
        files: &HashMap<String, Vec<u8>>,
        new_name: &str,
        identity: &Identity,
    ) -> anyhow::Result<PrincipalConfig> {
        let config_bytes = files
            .get("config/principal.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing config/principal.toml"))?;
        let config_str = std::str::from_utf8(config_bytes)?;
        let mut config: PrincipalConfig = toml::from_str(config_str)?;

        config.name = new_name.to_string();
        config.did = Some(PrincipalDID(identity.did.clone()));
        config.owner = Subject::User("local".to_string());

        Ok(config)
    }

    async fn import_agents(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        let agents_dir = self
            .config_dir
            .join("principals")
            .join(principal_name)
            .join("agents");

        for (path, content) in files {
            if path.starts_with("agents/") {
                let file_name = path.strip_prefix("agents/").unwrap_or(path);
                let dest_path = agents_dir.join(file_name);
                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    async fn import_memory(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        let memory_dir = self
            .data_dir
            .join("principals")
            .join(principal_name)
            .join("memory");

        for (path, content) in files {
            if path.starts_with("memory/") {
                let file_name = path.strip_prefix("memory/").unwrap_or(path);
                let dest_path = memory_dir.join(file_name);
                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    async fn import_sessions(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        let sessions_dir = self
            .data_dir
            .join("principals")
            .join(principal_name)
            .join("memory")
            .join("sessions");

        for (path, content) in files {
            if path.starts_with("sessions/") {
                let file_name = path.strip_prefix("sessions/").unwrap_or(path);
                let dest_path = sessions_dir.join(file_name);
                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    async fn save_config(
        &self,
        config: &PrincipalConfig,
        name: &str,
    ) -> anyhow::Result<PathBuf> {
        let principal_dir = self.config_dir.join("principals").join(name);
        tokio::fs::create_dir_all(&principal_dir).await?;
        let config_path = principal_dir.join("principal.toml");
        let config_toml = toml::to_string_pretty(config)?;
        tokio::fs::write(&config_path, config_toml).await?;
        Ok(config_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SignatureStatus {
    Verified,
    AllowedUnsigned,
}

fn verify_principal_signature(
    manifest_bytes: &[u8],
    did_doc_bytes: &[u8],
    allow_unsigned: bool,
    name: &str,
) -> anyhow::Result<(SignatureStatus, String)> {
    use crate::identity::DIDDocument;
    use base64::Engine;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let manifest_str = std::str::from_utf8(manifest_bytes)
        .map_err(|e| anyhow::anyhow!("manifest is not utf-8: {e}"))?;
    let manifest = PrincipalManifest::from_toml(manifest_str)
        .map_err(|e| anyhow::anyhow!("failed to parse manifest for verification: {e}"))?;

    let signature_b64 = manifest.signatures.manifest.trim();
    if signature_b64.is_empty() {
        if allow_unsigned {
            tracing::warn!(
                "principal package '{}' is not signed; importing anyway because allow_unsigned is set",
                name
            );
            return Ok((SignatureStatus::AllowedUnsigned, String::new()));
        }
        anyhow::bail!("manifest is not signed");
    }

    if manifest.signatures.algorithm != "ed25519" {
        anyhow::bail!(
            "unsupported signature algorithm: {}",
            manifest.signatures.algorithm
        );
    }

    let manifest_for_verification = PrincipalManifest {
        signatures: crate::registry::packaging::manifest::Signatures {
            manifest: String::new(),
            algorithm: "ed25519".to_string(),
        },
        ..manifest.clone()
    };
    let signed_bytes = manifest_for_verification
        .to_toml()
        .map_err(|e| anyhow::anyhow!("failed to reconstruct signed manifest bytes: {e}"))?
        .into_bytes();

    let signature_vec = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("signature is not valid base64url: {e}"))?;
    if signature_vec.len() != 64 {
        anyhow::bail!("signature has wrong length: expected 64, got {}", signature_vec.len());
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&signature_vec);

    let did_doc: DIDDocument = serde_json::from_slice(did_doc_bytes)
        .map_err(|e| anyhow::anyhow!("identity/did.json is malformed: {e}"))?;
    let vm = did_doc
        .verification_method
        .first()
        .ok_or_else(|| anyhow::anyhow!("DID document has no verification methods"))?;
    let multibase = &vm.public_key_multibase;
    if !multibase.starts_with('z') {
        anyhow::bail!("public key is not multibase z-base58");
    }
    let public_key = bs58::decode(&multibase[1..])
        .into_vec()
        .map_err(|e| anyhow::anyhow!("public key is not multibase z-base58: {e}"))?;
    if public_key.len() != 32 {
        anyhow::bail!("public key has wrong length: expected 32, got {}", public_key.len());
    }
    let mut public_key_arr = [0u8; 32];
    public_key_arr.copy_from_slice(&public_key);

    // Binding check: the DID in the manifest must identify the public key
    // shipped in the DID document. Without this, a replaced package can
    // still self-verify by embedding any new keypair.
    if did_doc.id != manifest.principal.did {
        anyhow::bail!(
            "[identity_binding_failed] DID document id '{}' does not match manifest principal DID '{}'",
            did_doc.id,
            manifest.principal.did
        );
    }
    let parsed_did = Identity::parse_did(&manifest.principal.did)
        .map_err(|e| anyhow::anyhow!("[identity_binding_failed] invalid manifest DID: {e}"))?;
    let expected_key_hash = blake3::hash(&public_key).to_hex().to_string()[..16].to_string();
    if parsed_did.key_hash != expected_key_hash {
        anyhow::bail!(
            "[identity_binding_failed] manifest DID key hash does not match the public key in identity/did.json"
        );
    }

    let verifying_key = VerifyingKey::from_bytes(&public_key_arr)
        .map_err(|e| anyhow::anyhow!("ed25519 signature verification failed: {e}"))?;
    let sig = Signature::from_bytes(&signature);
    verifying_key
        .verify(&signed_bytes, &sig)
        .map_err(|e| anyhow::anyhow!("ed25519 signature verification failed: {e}"))?;

    Ok((SignatureStatus::Verified, multibase.clone()))
}

async fn enforce_trust_pinning(
    trust_store: Option<&Arc<RwLock<TrustStore>>>,
    trust_policy: TrustPolicy,
    resolver: &PathResolver,
    name: &str,
    did: &str,
    public_key_multibase: &str,
) -> anyhow::Result<()> {
    let Some(store) = trust_store else {
        tracing::debug!("no trust store configured; skipping TOFU pinning");
        return Ok(());
    };

    let mut store = store.write().await;
    match store.is_trusted(name, did) {
        TrustStatus::Unknown => {
            store.pin(name.to_string(), did.to_string(), Some(public_key_multibase.to_string()));
            store.save(resolver)?;
            tracing::info!("Pinned principal '{}' to DID {} on first import", name, did);
        }
        TrustStatus::Trusted => {
            tracing::debug!("principal '{}' is already pinned to DID {}", name, did);
        }
        TrustStatus::Mismatch { expected, actual } => {
            if trust_policy == TrustPolicy::AllowUntrusted {
                store.pin(name.to_string(), actual.clone(), Some(public_key_multibase.to_string()));
                store.save(resolver)?;
                tracing::warn!(
                    "Overriding trust pin for principal '{}' from {} to {}",
                    name,
                    expected,
                    actual
                );
            } else {
                anyhow::bail!(
                    "[trust_pinning_failed] principal '{}' was previously imported with DID {expected}, but this package is signed by DID {actual}. Use --force to accept the new identity.",
                    name
                );
            }
        }
    }

    Ok(())
}

fn validate_package_for_principal(
    manifest: &PrincipalManifest,
    files: &HashMap<String, Vec<u8>>,
) -> ValidationResult {
    use crate::registry::packaging::validation::{ValidationError, ValidationWarning};

    let mut result = ValidationResult::success();

    // Required files for a `.principal` package.
    let required_files = ["manifest.toml", "identity/did.json", "config/principal.toml"];
    for file in required_files {
        match files.get(file) {
            None => result.add_error(ValidationError::MissingFile(file.to_string())),
            Some(content) if content.is_empty() => {
                result.add_error(ValidationError::EmptyFile(file.to_string()));
            }
            Some(_) => {}
        }
    }

    // Validate checksums for all manifest-listed files.
    for (file_path, expected) in &manifest.packaging.checksums {
        match files.get(file_path) {
            Some(content) => {
                let actual = PrincipalManifest::compute_checksum(content);
                if &actual != expected {
                    result.add_error(ValidationError::ChecksumMismatch {
                        file: file_path.clone(),
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
            None => result.add_error(ValidationError::MissingFile(file_path.clone())),
        }
    }

    // Warn about files present but not declared in the manifest.
    for file_path in files.keys() {
        if file_path != "manifest.toml" && !manifest.packaging.files.contains(file_path) {
            result.add_warning(ValidationWarning::UnknownFile(file_path.clone()));
        }
    }

    if !manifest.identity.encrypted {
        result.add_warning(ValidationWarning::UnencryptedKeys);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::did::DIDScope;
    use crate::identity::Identity;
    use crate::principal::config::PrincipalConfig;
    use crate::registry::packaging::principal_packager::{
        PrincipalExportOptions, PrincipalPackager,
    };

    #[test]
    fn test_import_options_default() {
        let opts = PrincipalImportOptions::default();
        assert!(opts.new_name.is_none());
        assert!(!opts.rotate_keys);
        assert!(opts.import_sessions);
        assert!(!opts.allow_unsigned);
    }

    fn sample_config(name: &str, did: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            did: Some(PrincipalDID(did.to_string())),
            owner: Subject::User("local".to_string()),
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
        }
    }

    #[tokio::test]
    async fn import_principal_restores_identity_and_agents() {
        let identity = Identity::new("importme", DIDScope::Local).await.unwrap();
        let original_did = identity.did.clone();
        let config = sample_config("importme", &original_did);

        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("src-agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("planner.md"), b"# Planner").unwrap();

        let out = tmp.path().join("importme.principal");
        let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);
        packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // Import into fresh config/data dirs.
        let config_dir = tmp.path().join("cfg");
        let data_dir = tmp.path().join("data");
        let unpackager =
            PrincipalUnpackager::new(&out, config_dir.clone(), data_dir.clone());
        let result = unpackager
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();

        assert_eq!(result.name, "importme");
        assert_eq!(result.did, original_did);
        assert!(result.config_path.exists());

        // Agent prompt restored.
        let agent_path = config_dir
            .join("principals")
            .join("importme")
            .join("agents")
            .join("planner.md");
        assert!(agent_path.exists(), "agent prompt restored");

        // Identity persisted under data_dir.
        let identity_dir = data_dir
            .join("principals")
            .join("importme")
            .join("identity");
        let storage = KeyStorage::with_path(identity_dir).unwrap();
        assert!(storage.exists(&original_did), "identity persisted");
    }

    #[tokio::test]
    async fn import_with_rename_uses_new_name() {
        let identity = Identity::new("orig", DIDScope::Local).await.unwrap();
        let config = sample_config("orig", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("orig.principal");
        let packager = PrincipalPackager::new(config, identity);
        packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let config_dir = tmp.path().join("cfg");
        let data_dir = tmp.path().join("data");
        let unpackager = PrincipalUnpackager::new(&out, config_dir.clone(), data_dir);
        let result = unpackager
            .import(PrincipalImportOptions {
                new_name: Some("renamed".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.name, "renamed");
        assert!(config_dir
            .join("principals")
            .join("renamed")
            .join("principal.toml")
            .exists());
    }
}
