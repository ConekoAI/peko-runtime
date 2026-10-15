//! Unpackager for importing portable Principal packages
//!
//! Extracts `.peko` files into the local peko runtime.

use crate::common::authority::{RuntimeAuthority, TierPath};
use crate::common::paths::PathResolver;
use crate::principal::config::PrincipalConfig;
use crate::registry::packaging::path_safety::{reject_destination_links, safe_join};
use crate::registry::packaging::principal_manifest::{is_workspace_payload, PrincipalManifest};
use crate::registry::packaging::validation::ValidationResult;
use peko_auth::Subject;
use peko_identity::{Identity, KeyPairExport};
use peko_subject::PrincipalDID;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Import options for a Principal package.
#[derive(Debug, Clone)]
pub struct PrincipalImportOptions {
    /// Rename the imported Principal
    pub new_name: Option<String>,
    /// Rotate keys (generate a new DID)
    pub rotate_keys: bool,
    /// Import session history
    pub import_sessions: bool,
    /// Import the identity-bearing Local-tier state from a
    /// full-snapshot package (cron schedule with principal-id
    /// rebinding, plan DAGs). Sessions are governed by
    /// `import_sessions` independently (ADR-056).
    pub import_local_state: bool,
    /// Force overwrite an existing Principal (never bypasses validation).
    pub force: bool,
    /// Bind a displayed preview to these exact manifest bytes.
    pub expected_manifest_checksum: Option<String>,
    /// Caller subject for the ownership write gate (ADR-066 D9). The
    /// IPC handler passes `caller.subject().clone()`; defaults to
    /// `Subject::User("local")` so library callers (tests, the CLI
    /// `import` subcommand if it ever bypasses the IPC layer) clear
    /// the Shared tier actor gate.
    pub caller_subject: Subject,
    /// Observability hub for the ADR-066 D9 `Security` audit event a
    /// denied ownership-crossing write emits. `None` skips the event.
    pub observability: Option<Arc<peko_observability::Observability>>,
}

impl Default for PrincipalImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            rotate_keys: false,
            import_sessions: true,
            import_local_state: true,
            force: false,
            expected_manifest_checksum: None,
            caller_subject: Subject::User("local".to_string()),
            observability: None,
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
    /// Whether the package carried identity-bearing Local-tier state
    /// (`sessions/`, `cron/`, `plans/`). ADR-056: `true` ⇒ the import
    /// is a *wake* — boot state carried verbatim (an `organized`
    /// principal keeps its schedule and is not re-genesis'd);
    /// `false` ⇒ the import is a *template/clone* — the boot state is
    /// inferred and genesis runs on next boot.
    pub carried_local_state: bool,
}

/// Unpackager for importing `.peko` packages.
pub struct PrincipalUnpackager {
    package_path: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    identity_vault: Option<Arc<crate::common::vault::Vault>>,
}

impl PrincipalUnpackager {
    /// Create a new Principal unpackager.
    pub fn new(package_path: impl AsRef<Path>, config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            package_path: package_path.as_ref().to_path_buf(),
            config_dir,
            data_dir,
            identity_vault: None,
        }
    }

    /// Use the daemon's unlocked vault for imported signing keys.
    pub fn with_identity_vault(mut self, vault: Arc<crate::common::vault::Vault>) -> Self {
        self.identity_vault = Some(vault);
        self
    }

    /// Inspect a package without importing.
    pub async fn inspect(&self) -> anyhow::Result<(PrincipalManifest, ValidationResult)> {
        let (manifest, _, validation) = self.inspect_detailed().await?;
        Ok((manifest, validation))
    }

    /// Inspect a package and return the extracted files alongside the
    /// manifest and validation result. Used by the import preview path
    /// to list bundled agents without re-extracting the archive.
    pub async fn inspect_detailed(
        &self,
    ) -> anyhow::Result<(
        PrincipalManifest,
        HashMap<String, Vec<u8>>,
        ValidationResult,
    )> {
        let files = self.extract_package().await?;
        let manifest = self.parse_manifest(&files)?;
        let validation = validate_package_for_principal(&manifest, &files);
        Ok((manifest, files, validation))
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
        let manifest = self.parse_manifest(&files)?;
        if let Some(expected) = &options.expected_manifest_checksum {
            anyhow::ensure!(
                *expected == PrincipalManifest::compute_checksum(&files["manifest.toml"]),
                "Snapshot changed since preview; inspect it again before importing."
            );
        }

        // ADR-056: seeds are plain TOML files ground via
        // `peko create -s` — keyless packages are no longer a
        // supported artifact shape. Fail fast, before any crypto work.
        if !files.contains_key("identity/keys.enc") {
            anyhow::bail!(
                "This package carries no keys — it looks like a seed artifact. \
                 Seeds are plain TOML files: ground one with \
                 `peko create <name> -s <seed.toml>`."
            );
        }

        let validation = validate_package_for_principal(&manifest, &files);
        if !validation.is_valid() {
            return Err(anyhow::anyhow!(
                "Package validation failed.\n{}",
                validation.error_report()
            ));
        }

        for path in files.keys() {
            safe_join(Path::new("snapshot"), path)?;
            anyhow::ensure!(
                !path.contains('\\') && !path.split('/').any(|p| p == "." || p.is_empty()),
                "[unsafe_path] non-canonical snapshot path: {path}"
            );
        }
        let did_doc: peko_identity::DIDDocument =
            serde_json::from_slice(&files["identity/did.json"])?;
        let key_export: KeyPairExport = serde_json::from_slice(&files["identity/keys.enc"])?;
        let source_identity = Identity::from_did_document_and_key(did_doc, key_export)?;
        anyhow::ensure!(
            source_identity.did == manifest.did,
            "[identity_binding_failed] Snapshot DID does not match identity/did.json"
        );
        let public_key = source_identity
            .keypair
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Snapshot has no keypair"))?
            .public_key_bytes();
        let parsed_did = Identity::parse_did(&manifest.did)?;
        anyhow::ensure!(
            parsed_did.key_hash == blake3::hash(&public_key).to_hex().to_string()[..16],
            "[identity_binding_failed] Snapshot DID does not identify its keys"
        );
        let multibase = format!("z{}", bs58::encode(public_key).into_string());
        anyhow::ensure!(
            source_identity
                .document
                .verification_method
                .first()
                .is_some_and(|v| v.public_key_multibase == multibase),
            "[identity_binding_failed] Snapshot DID document does not match its keys"
        );
        // Parse configuration before any identity or tooling writes.
        let snapshot_config: PrincipalConfig =
            toml::from_str(std::str::from_utf8(&files["config/principal.toml"])?)?;

        let name = options
            .new_name
            .clone()
            .unwrap_or_else(|| manifest.name.clone());

        // Defense in depth: even though IPC handlers validate `name` early,
        // re-check here because anything reaching this point flows into
        // filesystem paths. Rejects `..`, `/`, `\`, leading/trailing `-`,
        // non-alnum, etc. (also see the explicit `..` rule introduced in
        // `common::identifiers::validate_agent_name`).
        crate::common::identifiers::validate_agent_name(&name)
            .map_err(|e| anyhow::anyhow!("[unsafe_name] {e}"))?;

        // ADR-081 D6 step 1 / D7: one actor is hosted at most once per
        // runtime. A second copy of a hosted key is refused before any write;
        // rotating keys makes a new actor instead.
        if !options.rotate_keys {
            if let Some(key) = snapshot_config.actor_key.as_deref() {
                if let Some(hosted) = self.hosted_name_for_key(key, &name)? {
                    anyhow::bail!(
                        "This principal is already hosted here as '{hosted}'. A runtime \
                         hosts one copy of an actor; import with --rotate-keys to make \
                         a new actor instead."
                    );
                }
            }
        }

        // Preflight before installing keys or writing any snapshot payload.
        let workspace = self.config_dir.join("principals").join(&name);
        for path in files.keys().filter(|path| is_workspace_payload(path)) {
            reject_destination_links(&workspace, &safe_join(&workspace, path)?)?;
        }

        // Build a per-call authority that projects the IPC
        // caller's subject. The agent prompt
        // and identity writes (Shared tier) gate on ownership via this
        // authority; sessions writes (Local tier) rely on the actor gate
        // alone (the actor's tier-entitlement is the only Layer 2
        // check).
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let authority = RuntimeAuthority::for_caller(resolver, options.caller_subject.clone())
            .with_audit_sink(options.observability.clone());

        anyhow::ensure!(
            options.force
                || !self
                    .config_dir
                    .join("principals")
                    .join(&name)
                    .join("principal.toml")
                    .exists(),
            "Principal already exists locally. Use --force to overwrite."
        );
        let inventory = super::inventory::ExecutableInventory::from_files(&manifest.did, &files);
        if let Some(audit) = &options.observability {
            audit
                .audit_security_with_caller(
                    Some(&options.caller_subject),
                    "principal.snapshot_import",
                    Some(&manifest.did),
                    serde_json::to_value(&inventory)?,
                )
                .await?;
        }
        tracing::info!(inventory = %inventory.render(), "Importing principal snapshot");
        let identity = self
            .import_identity(&files, &manifest, &options, &name, &authority)
            .await?;
        let mut config = self.import_config(&files, &name, &identity)?;

        // Update DID in config to match the imported/rotated identity
        config.did = Some(PrincipalDID(identity.did.clone()));
        config.name = name.clone();
        config.hub_id = None;
        config.exposure = peko_auth::Exposure::LocalOnly;
        let store = peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: PathResolver::with_dirs(
                self.config_dir.clone(),
                self.data_dir.clone(),
                self.data_dir.clone(),
            )
            .runtime_dir(),
            shared_dir: None,
        });
        // ADR-081 D6 step 1: the local ID is this runtime's to give. A
        // principal that was hosted here before gets its old ID back; any
        // other gets a fresh one. The snapshot's ID — the source runtime's
        // — is never adopted.
        if options.rotate_keys {
            config.actor_key = Some(uuid::Uuid::new_v4().to_string());
            config.id = Some(peko_subject::PrincipalId::generate());
        } else {
            let key = config
                .actor_key
                .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
                .clone();
            config.id = Some(
                store
                    .former_local_id(&key)?
                    .map(peko_subject::PrincipalId)
                    .unwrap_or_else(peko_subject::PrincipalId::generate),
            );
        }

        // ADR-081 D6 step 2–3: actor references arrive by the portable rule.
        // A snapshot from before portable identity names only this
        // runtime's actors; its owner becomes the local user as before.
        let destination = self.destination_runtime();
        let portable = manifest.source_runtime.is_some();
        let foreign_owner = portable
            .then(|| super::portable::foreign_local_owner(&config, &destination))
            .flatten();
        let rule: Box<super::portable::Rewrite<'_>> = if portable {
            Box::new(super::portable::import_rule(
                &destination,
                foreign_owner.as_ref(),
            ))
        } else {
            Box::new(|subject: &Subject| subject.clone())
        };
        if portable {
            super::portable::rewrite_config(&mut config, &*rule);
        } else {
            config.owner = Subject::User("local".to_string());
        }

        // ADR-056 grounding semantics: what the import *is* follows
        // from what the package *carries*, not from a mode flag.
        //
        // - Carries Local-tier state (`sessions/`, `cron/`, `plans/`)
        //   ⇒ a *wake*: the package is a verbatim snapshot of a live
        //   principal, boot state included — carry it through. An
        //   `organized` principal therefore imports as `organized`
        //   WITH its authored cron schedule intact, and the daemon's
        //   boot seeding pass abstains (D4 of ADR-054: the trunk owns
        //   its rhythm, and the snapshot just delivered that rhythm
        //   across hosts). A `defined` snapshot re-genesis's on next
        //   boot, which is exactly what the source was.
        // - Carries no Local state (template package, or a legacy
        //   definition-shaped one) ⇒ a *clone*: the source's boot
        //   state is meaningless on this host — reset it and let
        //   ADR-054's inference apply (`has_definition()` ⇒
        //   `defined`). This also keeps the pre-ADR-056 bug closed
        //   where a definition-only export of an `organized` principal
        //   imported as `organized` with no schedule at all.
        let carried_local_state = files.keys().any(|p| {
            p.starts_with("sessions/") || p.starts_with("cron/") || p.starts_with("plans/")
        });
        if !carried_local_state {
            config.boot_state = None;
        }

        self.import_roles(&files, &name, &authority).await?;
        // Phase A: the legacy `import_memory` is gone. The memory
        // index (`local/memory_index.json`) is derived state — never
        // packaged, rebuilt by the runtime (ADR-056).
        // Sessions flow in under `import_sessions`; cron + plans and
        // the workspace tooling flow in below when the package
        // carries them (ADR-056).

        if options.import_sessions {
            self.import_sessions(&files, &name).await?;
        }

        self.import_workspace_tooling(&files, &manifest, &name, &*rule)
            .await?;
        if carried_local_state && options.import_local_state {
            let effective_principal_id = config
                .id
                .clone()
                .map(|pid| pid.0)
                .or_else(|| config.did.clone().map(|d| d.0))
                .unwrap_or_else(|| name.clone());
            self.import_local_authored(&files, &name, &effective_principal_id, &*rule)
                .await?;
        }
        // ADR-081 D5: the principal's contact book and membership record.
        let local_root = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        )
        .principal_layout(&name)
        .local
        .root;
        for (path, file) in [
            (
                "local/contacts.json",
                crate::principal::contacts::CONTACTS_FILE,
            ),
            ("local/channels.json", "channels.json"),
        ] {
            if let Some(bytes) = files.get(path) {
                tokio::fs::create_dir_all(&local_root).await?;
                tokio::fs::write(local_root.join(file), bytes).await?;
            }
        }
        // ADR-081 D6 step 5: the owner's conversation follows a local owner
        // to this runtime's local user.
        if foreign_owner.is_some() && options.import_sessions {
            if let Some(channels) = files.get("local/channels.json") {
                self.rebind_owner_dm(&store, &config, &local_root, channels, &destination)
                    .await?;
            }
        }

        let config_path = self.save_config(&config, &name).await?;

        Ok(PrincipalImportResult {
            name,
            did: identity.did,
            config_path,
            keys_rotated: options.rotate_keys,
            validation,
            carried_local_state,
        })
    }

    async fn extract_package(&self) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let file = std::fs::File::open(&self.package_path)?;
        let decoder = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(decoder);

        let mut files = HashMap::new();
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.header().entry_type().is_dir() {
                continue;
            }
            anyhow::ensure!(
                entry.header().entry_type().is_file(),
                "Snapshot contains a non-file archive entry"
            );
            let path = entry.path()?;
            let path_str = path.to_string_lossy().to_string();
            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;
            anyhow::ensure!(
                !files.contains_key(&path_str),
                "Duplicate snapshot path: {path_str}"
            );
            safe_join(Path::new("snapshot"), &path_str)?;
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
        authority: &RuntimeAuthority,
    ) -> anyhow::Result<Identity> {
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?;
        let did_doc: peko_identity::DIDDocument = serde_json::from_slice(did_doc_bytes)?;

        // Gate the principal workspace before installing a signing credential.
        authority
            .shared_identity_dir_write_for_name(principal_name)
            .await?;
        let identity = if options.rotate_keys {
            Identity::new(&manifest.name, peko_identity::did::DIDScope::Local).await?
        } else {
            let key_export: KeyPairExport = serde_json::from_slice(&files["identity/keys.enc"])?;
            Identity::from_did_document_and_key(did_doc, key_export)?
        };
        let vault = match &self.identity_vault {
            Some(vault) => vault.clone(),
            None => {
                #[cfg(test)]
                let vault = if self.config_dir.join("vault.enc").exists() {
                    crate::common::vault::Vault::load_with_passphrase(
                        self.config_dir.join("vault.enc"),
                        &secrecy::SecretString::new("snapshot-fixture".into()),
                    )?
                } else {
                    crate::common::vault::Vault::for_test(&self.config_dir, "snapshot-fixture")
                };
                #[cfg(not(test))]
                let vault = crate::common::vault::Vault::load(self.config_dir.join("vault.enc"))?;
                Arc::new(vault)
            }
        };
        let keypair = identity
            .keypair
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Snapshot has no keypair"))?;
        vault.set_principal_identity_private_key(
            &format!("{}#keys-1", identity.did),
            "ed25519-raw-base64",
            &keypair.export().private_key,
        )?;
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

        Ok(config)
    }

    async fn import_roles(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
        authority: &RuntimeAuthority,
    ) -> anyhow::Result<()> {
        // Phase A: role files live under the Shared tier
        // (`{config_dir}/principals/{name}/roles/`) so they ship in
        // the principal bundle. ADR-064 renamed the layer from
        // `agents/` to `roles/`; legacy packages shipping the
        // `agents/` prefix are restored into `roles/` with their
        // directory-layout `AGENT.md` files renamed to `ROLE.md`.
        //
        // ADR-066 D9: gate the directory on ownership via the
        // caller-projected authority. Matches the
        // `PrincipalCreate` role-prompt write gate.
        let roles_dir = authority
            .shared_roles_dir_write_for_name(principal_name)
            .await?
            .to_path_buf();

        for (path, content) in files {
            let rel = if let Some(rest) = path.strip_prefix("roles/") {
                rest
            } else if let Some(rest) = path.strip_prefix("agents/") {
                rest
            } else {
                continue;
            };
            // Legacy directory-layout role file: AGENT.md → ROLE.md.
            let rel = rel.replace("/AGENT.md", "/ROLE.md");
            let dest_path = safe_join(&roles_dir, &rel)?;
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(dest_path, content).await?;
        }
        Ok(())
    }

    async fn import_sessions(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        // Phase A: sessions live under the Local tier
        // (`{data_dir}/principals/{name}/local/sessions/`); the
        // older `{data_dir}/principals/{name}/memory/sessions/` path
        // is no longer touched.
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let sessions_dir = resolver.principal_layout(principal_name).local.sessions_dir;

        for (path, content) in files {
            if path.starts_with("sessions/") {
                let file_name = path.strip_prefix("sessions/").unwrap_or(path);
                let dest_path = safe_join(&sessions_dir, file_name)?;
                if let Some(parent) = dest_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    /// ADR-056: restore the workspace tooling directories
    /// (`tools/`, `skills/`, `mcp/`, `hooks/`, `kb/`) into the
    /// principal's Shared-tier workspace root. Presence in the
    /// workspace = visibility (ADR-050), so a snapshot import that
    /// skipped this would hand the principal a silently gutted
    /// tool catalog on its next turn.
    ///
    /// ADR-066 D8: command manifests are audited before restore; the
    /// ADR-046 canary also covers executable-content drift at daemon boot.
    async fn import_workspace_tooling(
        &self,
        files: &HashMap<String, Vec<u8>>,
        manifest: &PrincipalManifest,
        principal_name: &str,
        rule: &super::portable::Rewrite<'_>,
    ) -> anyhow::Result<()> {
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let workspace_root = resolver.principal_layout(principal_name).shared.root;

        if let Some(policy) = files.get(crate::security::policy::POLICY_FILE) {
            let text = super::portable::rewrite_policy(&String::from_utf8_lossy(policy), rule);
            let dest = workspace_root.join(crate::security::policy::POLICY_FILE);
            reject_destination_links(&workspace_root, &dest)?;
            tokio::fs::create_dir_all(&workspace_root).await?;
            tokio::fs::write(&dest, text).await?;
        }
        for (path, content) in files {
            let Some((prefix, rest)) =
                split_snapshot_path(path, &super::principal_manifest::WORKSPACE_DIRS)
            else {
                continue;
            };
            let dest_dir = workspace_root.join(prefix);
            let dest_path = safe_join(&dest_dir, rest)?;
            reject_destination_links(&workspace_root, &dest_path)?;
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&dest_path, content).await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if manifest.executable_files.contains(path) {
                    0o755
                } else {
                    0o644
                };
                tokio::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode))
                    .await?;
            }
            #[cfg(not(unix))]
            let _ = manifest;
        }
        Ok(())
    }

    /// ADR-056: restore the identity-bearing Local-tier state —
    /// `cron/` (authored schedule + run history) and `plans/` (Plan
    /// DAG storage) — into the principal's Local tier.
    ///
    /// Cron jobs are keyed on the source principal's runtime
    /// `PrincipalId` (ADR-054 D3); the importer rebinds every job's
    /// `principal_id` to the imported principal's effective id
    /// (`config.id` → DID → name, the same resolution the cron tools
    /// use) so the trunk can see its restored heartbeat via
    /// `Cron action list` immediately.
    async fn import_local_authored(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
        new_principal_id: &str,
        rule: &super::portable::Rewrite<'_>,
    ) -> anyhow::Result<()> {
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let layout = resolver.principal_layout(principal_name);

        const LOCAL_PREFIXES: [&str; 2] = ["cron", "plans"];
        for (path, content) in files {
            let Some((prefix, rest)) = split_snapshot_path(path, &LOCAL_PREFIXES) else {
                continue;
            };
            let dest_dir = match prefix {
                "cron" => layout.local.cron_dir.clone(),
                "plans" => layout.local.plans_dir.clone(),
                _ => continue,
            };
            let dest_path = safe_join(&dest_dir, rest)?;
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if prefix == "cron" && rest == "schedule.toml" {
                let remapped = remap_cron_principal_ids(content, new_principal_id);
                let remapped = super::portable::rewrite_cron(&remapped, rule);
                tokio::fs::write(dest_path, remapped).await?;
            } else {
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    /// The hosted principal (other than `target`, which `--force` may
    /// replace) whose immutable actor key is `key`.
    fn hosted_name_for_key(&self, key: &str, target: &str) -> anyhow::Result<Option<String>> {
        let Ok(entries) = std::fs::read_dir(self.config_dir.join("principals")) else {
            return Ok(None);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == target {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path().join("principal.toml")) else {
                continue;
            };
            let Ok(config) = toml::from_str::<PrincipalConfig>(&text) else {
                continue;
            };
            if config.actor_key.as_deref() == Some(key) {
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    /// This runtime's identity DID (`runtime/identity.toml`); `"local"`
    /// before the runtime has an identity.
    fn destination_runtime(&self) -> String {
        let path = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        )
        .runtime_identity();
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<peko_identity::runtime::RuntimeIdentity>(&text).ok())
            .map_or_else(|| "local".to_string(), |identity| identity.runtime_did)
    }

    /// Move the owner's DM session to this runtime's local user (ADR-081
    /// D6 step 5): the pair ID is recomputed from the principal's key and
    /// the local user's here, and the imported sessions bound to the old
    /// pair are rebound, compartment included.
    async fn rebind_owner_dm(
        &self,
        store: &peko_channel::ChannelStore,
        config: &PrincipalConfig,
        local_root: &Path,
        channels: &[u8],
        destination: &str,
    ) -> anyhow::Result<()> {
        let Some(principal_key) = config.actor_key.as_deref() else {
            return Ok(());
        };
        let channels: Vec<serde_json::Value> = serde_json::from_slice(channels).unwrap_or_default();
        let Some(old) = channels.iter().find_map(|channel| {
            let participants = channel["participants"].as_array()?;
            let owner_dm = participants.len() == 2
                && participants
                    .iter()
                    .any(|p| p["address"] == "user:local:local");
            owner_dm.then(|| channel["channel"].as_str().map(str::to_owned))?
        }) else {
            return Ok(());
        };
        let local = store.ensure_local_user(destination).await?;
        let new = peko_channel::dm_channel_id(principal_key, &local.identity_key);
        if new.as_str() == old {
            return Ok(());
        }
        let manager = std::sync::Arc::new(tokio::sync::RwLock::new(
            peko_session::SessionManager::new()
                .with_sessions_dir_internal(local_root.join("sessions")),
        ));
        let mut manager = manager.write().await;
        for meta in manager.list_all_sessions(false).await? {
            if meta.channel_id.as_deref() != Some(old.as_str()) {
                continue;
            }
            let id = meta.session_id.to_string();
            manager.bind_session_channel(&id, new.as_str()).await?;
            manager
                .set_session_compartment(
                    &id,
                    peko_tools_core::Compartment::Conversation {
                        channel: new.as_str().to_string(),
                    },
                )
                .await?;
        }
        Ok(())
    }

    async fn save_config(&self, config: &PrincipalConfig, name: &str) -> anyhow::Result<PathBuf> {
        let principal_dir = self.config_dir.join("principals").join(name);
        tokio::fs::create_dir_all(&principal_dir).await?;
        let config_path = principal_dir.join("principal.toml");
        let config_toml = toml::to_string_pretty(config)?;
        tokio::fs::write(&config_path, config_toml).await?;
        Ok(config_path)
    }
}

/// Split a package path into `(directory_prefix, rest)` if it starts with
/// one of the given directory prefixes (e.g. `cron/schedule.toml` →
/// `("cron", "schedule.toml")`).
fn split_snapshot_path<'a>(path: &'a str, prefixes: &[&'a str]) -> Option<(&'a str, &'a str)> {
    for prefix in prefixes {
        let with_slash = format!("{prefix}/");
        if let Some(rest) = path.strip_prefix(with_slash.as_str()) {
            if !rest.is_empty() {
                return Some((prefix, rest));
            }
        }
    }
    None
}

/// Rebind every cron job's `principal_id` in a cron database file to
/// the imported principal's effective runtime id (ADR-056).
///
/// Jobs in a per-principal schedule file are all owned by that
/// principal, so an unconditional rewrite is correct and idempotent.
/// NOTE: `local/cron/schedule.toml` carries a legacy `.toml` name but
/// is serialized as JSON (`CronDatabase`, `serde_json`), so JSON is
/// tried first; the TOML path is kept defensively in case the file
/// name ever becomes truthful again. A malformed file is passed
/// through unchanged with a warning — the cron engine will surface it
/// on next load rather than the import hard-failing on a snapshot's
/// side artifact.
fn remap_cron_principal_ids(schedule_bytes: &[u8], new_principal_id: &str) -> Vec<u8> {
    let text = match std::str::from_utf8(schedule_bytes) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "cron schedule file is not utf-8 ({e}); skipping principal-id rebinding"
            );
            return schedule_bytes.to_vec();
        }
    };

    let parse_error = |json_err: serde_json::Error, toml_err: toml::de::Error| {
        tracing::warn!(
            "cron schedule file could not be parsed for principal-id rebinding \
             (json: {json_err}; toml: {toml_err}); writing it through unchanged"
        );
    };

    // Primary: the real on-disk format (JSON). Fallback: TOML.
    let rewritten = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(mut value) => {
            if let Some(jobs) = value.get_mut("jobs").and_then(|j| j.as_array_mut()) {
                for job in jobs {
                    job["principal_id"] = serde_json::Value::String(new_principal_id.to_string());
                }
            }
            serde_json::to_string_pretty(&value)
                .map_err(|e| {
                    tracing::warn!(
                        "cron schedule re-serialization failed ({e}); writing the original bytes"
                    );
                    e
                })
                .ok()
                .map(String::into_bytes)
        }
        Err(json_err) => match toml::from_str::<toml::Value>(text) {
            Ok(mut value) => {
                if let Some(jobs) = value.get_mut("jobs").and_then(|j| j.as_array_mut()) {
                    for job in jobs {
                        job["principal_id"] = toml::Value::String(new_principal_id.to_string());
                    }
                }
                toml::to_string_pretty(&value)
                    .map_err(|e| {
                        tracing::warn!(
                        "cron schedule re-serialization failed ({e}); writing the original bytes"
                    );
                        e
                    })
                    .ok()
                    .map(String::into_bytes)
            }
            Err(toml_err) => {
                parse_error(json_err, toml_err);
                None
            }
        },
    };

    rewritten.unwrap_or_else(|| schedule_bytes.to_vec())
}

fn validate_package_for_principal(
    manifest: &PrincipalManifest,
    files: &HashMap<String, Vec<u8>>,
) -> ValidationResult {
    use crate::registry::packaging::validation::ValidationError;

    let mut result = ValidationResult::success();

    for path in &manifest.executable_files {
        if !manifest.files.contains_key(path) || !is_workspace_payload(path) {
            result.add_error(ValidationError::InvalidManifest(format!(
                "Executable file must be a declared workspace payload: {path}"
            )));
        }
    }

    // Required files for a `.peko` package.
    let required_files = [
        "manifest.toml",
        "identity/did.json",
        "config/principal.toml",
    ];
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
    for (file_path, expected) in &manifest.files {
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
        if file_path != "manifest.toml" && !manifest.files.contains_key(file_path) {
            result.add_error(ValidationError::InvalidManifest(format!(
                "Undeclared file: {file_path}"
            )));
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::{BootState, PrincipalConfig};
    use crate::registry::packaging::principal_packager::{
        PrincipalExportOptions, PrincipalPackager,
    };
    use peko_identity::did::DIDScope;
    use peko_identity::Identity;

    #[test]
    fn test_import_options_default() {
        let opts = PrincipalImportOptions::default();
        assert!(opts.new_name.is_none());
        assert!(!opts.rotate_keys);
        assert!(opts.import_sessions);
        // Defaults clear the Shared tier gate so library callers
        // (tests, future direct-call sites) don't have to thread
        // `caller_subject` through every constructor.
        assert!(matches!(opts.caller_subject, Subject::User(ref u) if u == "local"));
    }

    fn sample_config(name: &str, did: &str) -> PrincipalConfig {
        PrincipalConfig {
            hub_id: None,
            actor_key: None,
            name: name.to_string(),
            id: None,
            did: Some(PrincipalDID(did.to_string())),
            owner: Subject::User("local".to_string()),
            identity: Default::default(),
            intent: Default::default(),
            governance: Default::default(),
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
    async fn import_principal_restores_identity_and_agents() {
        let identity = Identity::new("importme", DIDScope::Local).await.unwrap();
        let original_did = identity.did.clone();
        let config = sample_config("importme", &original_did);

        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("src-agents");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("planner.md"), b"# Planner").unwrap();

        let out = tmp.path().join("importme.peko");
        let packager = PrincipalPackager::new(config, identity).with_roles_dir(&roles_dir);
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
        let unpackager = PrincipalUnpackager::new(&out, config_dir.clone(), data_dir.clone());
        let result = unpackager
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();

        assert_eq!(result.name, "importme");
        assert_eq!(result.did, original_did);
        assert!(result.config_path.exists());

        // Role file restored (Shared tier).
        let role_path = config_dir
            .join("principals")
            .join("importme")
            .join("roles")
            .join("planner.md");
        assert!(role_path.exists(), "role prompt restored");

        let vault = crate::common::vault::Vault::load_with_passphrase(
            config_dir.join("vault.enc"),
            &secrecy::SecretString::new("snapshot-fixture".into()),
        )
        .unwrap();
        assert!(vault
            .get_principal_identity_private_key(&format!("{original_did}#keys-1"))
            .is_some());
        let restored: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(&result.config_path).unwrap()).unwrap();
        assert!(restored.hub_id.is_none());
        assert!(restored.id.is_some());
        assert!(restored.actor_key.is_some());
    }

    #[tokio::test]
    async fn importing_remote_contact_allocates_a_local_id_without_adopting_foreign_id() {
        use peko_channel::{ChannelConfig, ChannelPort, ChannelStore};
        use peko_subject::PrincipalId;
        let identity = Identity::new("remote-import", DIDScope::Local)
            .await
            .unwrap();
        let mut config = sample_config("remote-import", &identity.did);
        config.id = Some(PrincipalId("prin_foreign".into()));
        config.actor_key = Some("portable-key".into());
        let signer = identity.did.clone();
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("remote.peko");
        PrincipalPackager::new(config, identity)
            .export(PrincipalExportOptions {
                output_path: Some(package.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let config_dir = temp.path().join("config");
        let port = ChannelStore::new(ChannelConfig {
            runtime_dir: config_dir.join("runtime"),
            shared_dir: None,
        });
        let remote: Subject = "principal:hub:portable".parse().unwrap();
        let known = port
            .register_actor(
                &remote,
                Some("portable"),
                Some(&signer),
                Some("portable-key"),
                None,
                "public",
            )
            .await
            .unwrap();
        // Another principal here learned the remote contact.
        let learned = crate::principal::contacts::ContactRecord::from_actor(&known, "local");
        let result = PrincipalUnpackager::new(&package, config_dir, temp.path().join("data"))
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();
        let restored: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(result.config_path).unwrap()).unwrap();
        let local_id = restored.id.unwrap();
        assert!(local_id.0.starts_with("prin_"));
        assert_ne!(local_id.0, "prin_foreign");
        assert_eq!(restored.actor_key.as_deref(), Some("portable-key"));
        let promoted = port
            .register_actor(
                &Subject::from(&local_id),
                Some("portable"),
                Some(&signer),
                Some("portable-key"),
                None,
                "private",
            )
            .await
            .unwrap();
        // Promotion keeps the learned relationship routable: same key.
        assert_eq!(promoted.identity_key, "portable-key");
        assert!(learned.reachable_on(&port, "local"));
    }

    #[tokio::test]
    async fn rotated_snapshot_is_a_new_local_actor_with_a_vault_signer() {
        let temp = tempfile::tempdir().unwrap();
        let identity = Identity::new("source", DIDScope::Local).await.unwrap();
        let source_did = identity.did.clone();
        let mut config = sample_config("source", &source_did);
        let source_id = peko_subject::PrincipalId::generate();
        config.id = Some(source_id.clone());
        config.actor_key = Some("original-actor".into());
        config.hub_id = Some(uuid::Uuid::new_v4().to_string());
        config.exposure = peko_auth::Exposure::Public;
        let output = temp.path().join("source.peko");
        PrincipalPackager::new(config, identity)
            .export(PrincipalExportOptions {
                output_path: Some(output.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let config_dir = temp.path().join("config");
        let vault = Arc::new(crate::common::vault::Vault::for_test(
            &config_dir,
            "snapshot-test",
        ));
        let result = PrincipalUnpackager::new(&output, config_dir, temp.path().join("data"))
            .with_identity_vault(vault.clone())
            .import(PrincipalImportOptions {
                rotate_keys: true,
                ..Default::default()
            })
            .await
            .unwrap();
        let config: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(result.config_path).unwrap()).unwrap();
        assert!(result.keys_rotated);
        assert_ne!(result.did, source_did);
        assert!(result.did.starts_with("did:key:"));
        assert_ne!(config.id.unwrap(), source_id);
        assert_ne!(config.actor_key.as_deref(), Some("original-actor"));
        assert!(config.hub_id.is_none());
        assert_eq!(config.exposure, peko_auth::Exposure::LocalOnly);
        assert!(vault
            .get_principal_identity_private_key(&format!("{}#keys-1", result.did))
            .is_some());
    }

    #[tokio::test]
    async fn import_with_rename_uses_new_name() {
        let identity = Identity::new("orig", DIDScope::Local).await.unwrap();
        let config = sample_config("orig", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("orig.peko");
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

    /// ADR-056: a full-snapshot round-trip restores the principal's
    /// live existence — sessions, the authored cron schedule (with
    /// principal-id rebinding), plans, and the installed workspace
    /// tooling land in the right tier directories, and the boot state
    /// carries through verbatim (an `organized` principal imports as
    /// `organized` and is not re-genesis'd).
    #[tokio::test]
    async fn full_snapshot_roundtrip_restores_live_state() {
        use crate::registry::packaging::principal_packager::PrincipalExportOptions;

        let identity = Identity::new("live", DIDScope::Local).await.unwrap();
        let original_did = identity.did.clone();
        let mut config = sample_config("live", &original_did);
        config.set_boot_state(BootState::Organized);

        let tmp = tempfile::tempdir().unwrap();
        let shared_root = tmp.path().join("shared").join("live");
        let local_root = tmp.path().join("data").join("live").join("local");

        std::fs::create_dir_all(shared_root.join("agents")).unwrap();
        std::fs::write(shared_root.join("agents").join("root.md"), b"# root").unwrap();
        std::fs::create_dir_all(shared_root.join("tools").join("my-tool")).unwrap();
        std::fs::write(
            shared_root
                .join("tools")
                .join("my-tool")
                .join("manifest.yaml"),
            b"id: my-tool\n",
        )
        .unwrap();
        std::fs::create_dir_all(shared_root.join("kb")).unwrap();
        std::fs::write(shared_root.join("kb").join("MEMORY.md"), b"hot").unwrap();

        std::fs::create_dir_all(local_root.join("sessions")).unwrap();
        std::fs::write(local_root.join("sessions").join("s1.jsonl"), b"{}\n").unwrap();
        std::fs::create_dir_all(local_root.join("cron")).unwrap();
        std::fs::write(
            local_root.join("cron").join("schedule.toml"),
            "version = 2\n[[jobs]]\nid = \"keepalive\"\nname = \"keepalive\"\nprincipal_id = \"prin_old\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(local_root.join("plans")).unwrap();
        std::fs::write(local_root.join("plans").join("p1.jsonl"), b"{}\n").unwrap();

        let out = tmp.path().join("live.peko");
        let packager = PrincipalPackager::new(config, identity)
            .with_roles_dir(shared_root.join("agents"))
            .with_sessions_dir(local_root.join("sessions"))
            .with_workspace_dir(&shared_root)
            .with_local_root(&local_root);
        packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // Transport survives removal of the source workspace and authored state.
        std::fs::remove_dir_all(&shared_root).unwrap();
        std::fs::remove_dir_all(&local_root).unwrap();

        let config_dir = tmp.path().join("cfg");
        let data_dir = tmp.path().join("new-data");
        let unpackager = PrincipalUnpackager::new(&out, config_dir.clone(), data_dir.clone());
        let result = unpackager
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();

        assert!(
            result.carried_local_state,
            "snapshot packages carry Local-tier state"
        );

        // Workspace tooling restored to the Shared tier root.
        assert!(
            config_dir
                .join("principals")
                .join("live")
                .join("tools")
                .join("my-tool")
                .join("manifest.yaml")
                .exists(),
            "tooling restored to workspace root"
        );
        assert!(
            config_dir
                .join("principals")
                .join("live")
                .join("kb")
                .join("MEMORY.md")
                .exists(),
            "kb restored to workspace root"
        );

        // Local authored state restored to the Local tier.
        assert!(
            data_dir
                .join("principals")
                .join("live")
                .join("local")
                .join("sessions")
                .join("s1.jsonl")
                .exists(),
            "sessions restored"
        );
        assert!(
            data_dir
                .join("principals")
                .join("live")
                .join("local")
                .join("plans")
                .join("p1.jsonl")
                .exists(),
            "plans restored"
        );

        // Cron state is rebound to the destination runtime's local actor ID.
        let restored: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(&result.config_path).unwrap()).unwrap();
        let local_id = restored.id.unwrap().0;
        let schedule_path = data_dir
            .join("principals")
            .join("live")
            .join("local")
            .join("cron")
            .join("schedule.toml");
        let schedule = std::fs::read_to_string(&schedule_path).unwrap();
        assert!(
            schedule.contains(&format!("principal_id = \"{local_id}\"")),
            "principal ids rebound to the effective runtime id: {schedule}"
        );
        assert!(
            !schedule.contains("prin_old"),
            "stale source ids must not survive: {schedule}"
        );

        // Boot state carried verbatim: no genesis re-seed.
        let imported = std::fs::read_to_string(
            config_dir
                .join("principals")
                .join("live")
                .join("principal.toml"),
        )
        .unwrap();
        assert!(
            imported.contains("boot_state = \"organized\""),
            "organized state survives the snapshot round-trip: {imported}"
        );
    }

    fn write_runtime_identity(config_dir: &Path, did: &str) {
        let identity = peko_identity::runtime::RuntimeIdentity {
            runtime_did: did.into(),
            key_id: format!("{did}#keys-1"),
            created_at: chrono::Utc::now(),
        };
        let path = config_dir.join("runtime/identity.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, toml::to_string_pretty(&identity).unwrap()).unwrap();
    }

    /// ADR-081 D6: a snapshot carries contacts, policy, grants, cron
    /// creators, and the owner. A local owner becomes the destination's
    /// local user (its DM session follows), local grants go dormant away
    /// from home and revive on return, and a hosted actor is never imported
    /// twice.
    #[tokio::test]
    async fn portable_identity_moves_and_returns() {
        use crate::registry::packaging::principal_packager::{
            PortableExport, PrincipalExportOptions,
        };
        use peko_subject::ActorKeys;
        const SRC: &str = "did:key:zSource";
        const DST: &str = "did:key:zDest";

        let identity = Identity::new("mover", DIDScope::Local).await.unwrap();
        let mut config = sample_config("mover", &identity.did);
        config.actor_key = Some("mover-key".into());
        config.permissions.push(peko_auth::PermissionGrant {
            subject: "principal:local:prin_peer".parse().unwrap(),
            permission: peko_auth::Permission::Trusted,
            granted_at: String::new(),
            granted_by: Subject::User("local".into()),
        });
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("src/shared");
        let local = tmp.path().join("src/local");
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(
            shared.join("POLICY.md"),
            "# Defaults\n\n## principal:local:prin_peer\n- peers\n",
        )
        .unwrap();
        std::fs::create_dir_all(shared.join("workflows")).unwrap();
        std::fs::write(shared.join("workflows/daily.yaml"), "steps: []\n").unwrap();
        std::fs::create_dir_all(local.join("cron")).unwrap();
        std::fs::write(
            local.join("cron/schedule.toml"),
            serde_json::json!({"version": 2, "jobs": [{"id": "brief", "principal_id": "prin_src",
                "created_by": {"actor": "user:local:local", "trust": "owner"}}]})
            .to_string(),
        )
        .unwrap();
        let contact = crate::principal::contacts::ContactRecord {
            kind: "principal".into(),
            local: Some(crate::principal::contacts::LocalRoute {
                runtime: SRC.into(),
                id: "prin_peer".into(),
            }),
            hub_id: None,
            identity_key: "peer-key".into(),
            signing_did: None,
            name: Some("peer".into()),
            registration: "local_only".into(),
        };
        crate::principal::contacts::ContactBook::in_local_root(&local)
            .remember(contact.clone())
            .await
            .unwrap();
        // The owner's DM session on the source runtime.
        let old_dm = peko_channel::dm_channel_id("mover-key", &format!("local@{SRC}"));
        let mut sessions =
            peko_session::SessionManager::new().with_sessions_dir_internal(local.join("sessions"));
        let session = sessions
            .create_session(
                "root",
                &Subject::Public,
                peko_session::SessionCreateOptions::new(),
            )
            .await
            .unwrap()
            .session_id()
            .to_string();
        sessions
            .bind_session_channel(&session, old_dm.as_str())
            .await
            .unwrap();
        let channels = serde_json::to_vec(&serde_json::json!([{
            "channel": old_dm.as_str(),
            "participants": [
                {"address": "principal:local:prin_src", "identity_key": "mover-key"},
                {"address": "user:local:local", "identity_key": format!("local@{SRC}")},
            ],
            "binding": "/local-user",
        }]))
        .unwrap();

        let out = tmp.path().join("mover.peko");
        PrincipalPackager::new(config, identity)
            .with_sessions_dir(local.join("sessions"))
            .with_workspace_dir(&shared)
            .with_local_root(&local)
            .with_portable(PortableExport {
                source_runtime: SRC.into(),
                hub_routes: Default::default(),
                channels: Some(channels),
            })
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // Move to another runtime.
        let dst_config = tmp.path().join("dst/config");
        let dst_data = tmp.path().join("dst/data");
        write_runtime_identity(&dst_config, DST);
        let moved = PrincipalUnpackager::new(&out, dst_config.clone(), dst_data.clone())
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();
        let restored: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(&moved.config_path).unwrap()).unwrap();
        let keys = peko_subject::CanonicalKeys;
        assert_eq!(
            restored.owner,
            Subject::User("local".into()),
            "local owner reassigned"
        );
        assert!(
            !keys.same_actor(
                &restored.permissions[0].subject,
                &"principal:local:prin_peer".parse().unwrap()
            ),
            "a local grant is dormant away from home"
        );
        assert_ne!(
            restored.id.as_ref().unwrap().0,
            "prin_src",
            "foreign local ID not adopted"
        );
        let workspace = dst_config.join("principals/mover");
        let policy = std::fs::read_to_string(workspace.join("POLICY.md")).unwrap();
        assert!(
            policy.contains("## principal:local:prin_peer@did:key:zSource"),
            "{policy}"
        );
        assert!(workspace.join("workflows/daily.yaml").exists());
        let dst_local = dst_data.join("principals/mover/local");
        let book = crate::principal::contacts::ContactBook::in_local_root(&dst_local);
        assert_eq!(
            book.list().unwrap(),
            vec![contact.clone()],
            "contacts survive the move"
        );
        let cron = std::fs::read_to_string(dst_local.join("cron/schedule.toml")).unwrap();
        assert!(cron.contains("\"actor\": \"user:local:local\""), "{cron}");
        let new_dm = peko_channel::dm_channel_id("mover-key", &format!("local@{DST}"));
        let rebound = peko_session::SessionManager::new()
            .with_sessions_dir_internal(dst_local.join("sessions"))
            .list_all_sessions(false)
            .await
            .unwrap();
        assert_eq!(
            rebound[0].channel_id.as_deref(),
            Some(new_dm.as_str()),
            "owner DM follows"
        );

        // A second copy of the same actor is refused on this runtime.
        let err = PrincipalUnpackager::new(&out, dst_config.clone(), dst_data.clone())
            .import(PrincipalImportOptions {
                new_name: Some("mover-two".into()),
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already hosted here"), "{err}");

        // Back home, the local grant and policy section revive.
        let home_config = tmp.path().join("home/config");
        let home_data = tmp.path().join("home/data");
        write_runtime_identity(&home_config, SRC);
        let back = PrincipalUnpackager::new(&out, home_config.clone(), home_data)
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();
        let home: PrincipalConfig =
            toml::from_str(&std::fs::read_to_string(&back.config_path).unwrap()).unwrap();
        assert!(keys.same_actor(&home.owner, &Subject::User("local".into())));
        assert!(keys.same_actor(
            &home.permissions[0].subject,
            &"principal:local:prin_peer".parse().unwrap()
        ));
        let policy =
            std::fs::read_to_string(home_config.join("principals/mover/POLICY.md")).unwrap();
        assert!(
            policy.contains("## principal:local:prin_peer\n"),
            "{policy}"
        );
    }

    /// ADR-056: a package that carries NO Local-tier state (legacy
    /// definition-shaped, or a keyless template) resets the boot state
    /// so ADR-054's inference applies. An `organized` source must not
    /// land the import as `organized` with no schedule to show for it.
    #[tokio::test]
    async fn import_without_local_state_does_not_inherit_organized_state() {
        use crate::registry::packaging::principal_packager::PrincipalExportOptions;

        let identity = Identity::new("org", DIDScope::Local).await.unwrap();
        let mut config = sample_config("org", &identity.did);
        config.set_boot_state(BootState::Organized);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("org.peko");
        // No sessions/cron/plans dirs wired in — the package carries
        // no Local state, so it clones rather than wakes.
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
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();

        assert!(!result.carried_local_state, "no local state in the package");
        let imported = std::fs::read_to_string(
            config_dir
                .join("principals")
                .join("org")
                .join("principal.toml"),
        )
        .unwrap();
        assert!(
            !imported.contains("boot_state"),
            "an import without local state must infer its boot state, not inherit it: {imported}"
        );
    }

    /// ADR-056: `remap_cron_principal_ids` rewrites every job's
    /// `principal_id`, is idempotent, and passes malformed files
    /// through unchanged.
    #[test]
    fn remap_cron_principal_ids_rewrites_all_jobs() {
        let schedule = r#"
version = 2

[[jobs]]
id = "keepalive"
name = "keepalive"
principal_id = "prin_old"

[[jobs]]
id = "genesis"
name = "genesis"
principal_id = "prin_old"
"#;
        let remapped =
            String::from_utf8(remap_cron_principal_ids(schedule.as_bytes(), "prin_new")).unwrap();
        assert_eq!(remapped.matches("prin_new").count(), 2);
        assert!(!remapped.contains("prin_old"));

        // Idempotent.
        let again =
            String::from_utf8(remap_cron_principal_ids(remapped.as_bytes(), "prin_new")).unwrap();
        assert_eq!(again.matches("prin_new").count(), 2);

        // Malformed input passes through unchanged.
        let garbage = b"\xff\xfe not toml";
        assert_eq!(
            remap_cron_principal_ids(garbage, "prin_new"),
            garbage.to_vec()
        );
    }

    /// ADR-056: the real on-disk cron database format is JSON despite
    /// the legacy `schedule.toml` file name (`CronDatabase`,
    /// `serde_json::to_string_pretty`). The rebinding must handle it.
    #[test]
    fn remap_cron_principal_ids_handles_real_json_format() {
        let schedule = serde_json::json!({
            "version": 2,
            "jobs": [
                {
                    "id": "keepalive",
                    "name": "keepalive",
                    "principal_id": "prin_old",
                    "schedule": { "every": { "every_ms": 600_000 } },
                    "action": { "send": { "message": "", "target": "trunk" } },
                    "delete_after_run": false,
                    "enabled": true
                },
                {
                    "id": "trunk-authored",
                    "name": "morning-brief",
                    "principal_id": "prin_old",
                    "schedule": { "at": { "at": "2026-09-15T01:00:00Z" } },
                    "action": { "send": { "message": "morning", "target": "trunk" } },
                    "delete_after_run": false,
                    "enabled": true
                }
            ],
            "runs": []
        })
        .to_string();

        let remapped =
            String::from_utf8(remap_cron_principal_ids(schedule.as_bytes(), "prin_new")).unwrap();
        assert_eq!(remapped.matches("prin_new").count(), 2, "{remapped}");
        assert!(!remapped.contains("prin_old"), "{remapped}");

        // The rest of the document survives the rewrite.
        let parsed: serde_json::Value = serde_json::from_str(&remapped).unwrap();
        assert_eq!(parsed["jobs"][0]["id"], "keepalive");
        assert_eq!(parsed["jobs"][1]["name"], "morning-brief");
        assert_eq!(parsed["version"], 2);
    }

    #[tokio::test]
    async fn import_denied_for_non_operator_caller() {
        let identity = Identity::new("denied", DIDScope::Local).await.unwrap();
        let config = sample_config("denied", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let roles_dir = tmp.path().join("src-agents");
        std::fs::create_dir_all(&roles_dir).unwrap();
        std::fs::write(roles_dir.join("planner.md"), b"# Planner").unwrap();

        let out = tmp.path().join("denied.peko");
        let packager = PrincipalPackager::new(config, identity).with_roles_dir(&roles_dir);
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

        let audit_dir = tmp.path().join("audit");
        let observability = std::sync::Arc::new(
            peko_observability::Observability::with_audit_dir("test", audit_dir.clone())
                .expect("audit dir"),
        );

        let opts = PrincipalImportOptions {
            caller_subject: peko_auth::Subject::Visitor("visitor-1".to_string()),
            observability: Some(observability),
            ..PrincipalImportOptions::default()
        };
        let err = unpackager
            .import(opts)
            .await
            .expect_err("import should be denied for a visitor caller");
        let msg = format!("{err}");
        assert!(
            msg.contains("cross-principal write denied"),
            "expected ownership denial, got: {msg}"
        );

        let today = chrono::Utc::now().date_naive();
        let log = std::fs::read_to_string(audit_dir.join(format!("audit-{today}.jsonl")))
            .expect("audit file written");
        assert!(
            log.contains("principal.cross_principal_write_denied"),
            "audit log must carry the crossing event: {log}"
        );
    }
    async fn valid_snapshot_files() -> HashMap<String, Vec<u8>> {
        let identity = Identity::new("checked", DIDScope::Local).await.unwrap();
        let config = sample_config("checked", &identity.did);
        PrincipalPackager::new(config, identity)
            .collect_files(PrincipalExportOptions::default())
            .await
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn executable_metadata_must_reference_declared_workspace_files() {
        let files = valid_snapshot_files().await;
        for path in ["skills/missing/script.sh", "identity/keys.enc"] {
            let mut files = files.clone();
            let mut manifest =
                PrincipalManifest::from_toml(std::str::from_utf8(&files["manifest.toml"]).unwrap())
                    .unwrap();
            manifest.executable_files.insert(path.into());
            files.insert(
                "manifest.toml".into(),
                manifest.to_toml().unwrap().into_bytes(),
            );
            let temp = tempfile::tempdir().unwrap();
            let config = temp.path().join("cfg");
            let unpackager =
                PrincipalUnpackager::new("unused", config.clone(), temp.path().join("data"));
            let error = unpackager
                .import_from_files(
                    files,
                    PrincipalImportOptions {
                        force: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("declared workspace payload"));
            assert!(!config.exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn force_import_rejects_linked_skill_destinations_before_writes() {
        let mut files = valid_snapshot_files().await;
        files.insert("skills/check/SKILL.md".into(), b"definition".to_vec());
        refresh_manifest(&mut files);
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("cfg");
        let workspace = config.join("principals/checked");
        std::fs::create_dir_all(&workspace).unwrap();
        let outside = temp.path().join("other-principal");
        std::fs::create_dir_all(outside.join("check")).unwrap();
        std::fs::write(outside.join("check/SKILL.md"), "original").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("skills")).unwrap();
        let unpackager =
            PrincipalUnpackager::new("unused", config.clone(), temp.path().join("data"));
        let error = unpackager
            .import_from_files(
                files,
                PrincipalImportOptions {
                    force: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(outside.join("check/SKILL.md")).unwrap(),
            "original"
        );
        assert!(!config.join("vault.enc").exists());
        assert!(!workspace.join("principal.toml").exists());
    }

    fn refresh_manifest(files: &mut HashMap<String, Vec<u8>>) {
        let mut manifest =
            PrincipalManifest::from_toml(std::str::from_utf8(&files["manifest.toml"]).unwrap())
                .unwrap();
        manifest.files.clear();
        for (path, data) in files.iter().filter(|(p, _)| p.as_str() != "manifest.toml") {
            manifest.add_file(path, data);
        }
        files.insert(
            "manifest.toml".into(),
            manifest.to_toml().unwrap().into_bytes(),
        );
    }

    #[tokio::test]
    async fn invalid_snapshots_fail_before_writes_even_with_force() {
        let files = valid_snapshot_files().await;
        let mut tampered = files.clone();
        tampered.insert("config/principal.toml".into(), b"name = 'changed'".to_vec());
        let mut undeclared = files.clone();
        undeclared.insert(
            "hooks/extra/hook.toml".into(),
            b"command = '/bin/echo'".to_vec(),
        );
        let mut missing = files.clone();
        missing.remove("config/principal.toml");
        let mut traversal = files.clone();
        traversal.insert("roles/../../escape.md".into(), b"escape".to_vec());
        refresh_manifest(&mut traversal);
        let mut mismatch = files.clone();
        let mut m =
            PrincipalManifest::from_toml(std::str::from_utf8(&mismatch["manifest.toml"]).unwrap())
                .unwrap();
        m.did = "did:peko:wrong".into();
        mismatch.insert("manifest.toml".into(), m.to_toml().unwrap().into_bytes());
        for files in [tampered, undeclared, missing, traversal, mismatch] {
            let temp = tempfile::tempdir().unwrap();
            let cfg = temp.path().join("cfg");
            let data = temp.path().join("data");
            let unpackager = PrincipalUnpackager::new("unused", cfg.clone(), data.clone());
            unpackager
                .import_from_files(
                    files,
                    PrincipalImportOptions {
                        force: true,
                        ..Default::default()
                    },
                )
                .await
                .expect_err("invalid snapshot must fail");
            assert!(!cfg.exists(), "no partial shared state");
            assert!(!data.exists(), "no partial local state");
        }
    }

    #[tokio::test]
    async fn keyless_snapshot_points_to_create_seed() {
        let mut files = valid_snapshot_files().await;
        files.remove("identity/keys.enc");
        refresh_manifest(&mut files);
        let temp = tempfile::tempdir().unwrap();
        let unpackager =
            PrincipalUnpackager::new("unused", temp.path().join("cfg"), temp.path().join("data"));
        let err = unpackager
            .import_from_files(files, PrincipalImportOptions::default())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("peko create") && err.to_string().contains("-s"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn snapshot_change_since_preview_fails_before_writes() {
        let files = valid_snapshot_files().await;
        let temp = tempfile::tempdir().unwrap();
        let cfg = temp.path().join("cfg");
        let unpackager = PrincipalUnpackager::new("unused", cfg.clone(), temp.path().join("data"));
        let err = unpackager
            .import_from_files(
                files,
                PrincipalImportOptions {
                    expected_manifest_checksum: Some("sha256:previous".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("changed since preview"));
        assert!(!cfg.exists());
    }

    #[tokio::test]
    async fn snapshot_import_audits_full_executable_inventory_at_security_severity() {
        let mut files = valid_snapshot_files().await;
        let hook =
            "command = '/bin/echo'\nargs = ['full command']\nbinds = ['Stop', 'PreToolUse:Bash']\n";
        let mcp = r#"{"name":"server","transport":{"type":"stdio","command":"node","args":["server.js"]}}"#;
        files.insert("hooks/notice/hook.toml".into(), hook.as_bytes().to_vec());
        files.insert("mcp/server/server.json".into(), mcp.as_bytes().to_vec());
        files.insert("skills/skill-one/SKILL.md".into(), b"instructions".to_vec());
        refresh_manifest(&mut files);
        let manifest =
            PrincipalManifest::from_toml(std::str::from_utf8(&files["manifest.toml"]).unwrap())
                .unwrap();
        let inventory =
            super::super::inventory::ExecutableInventory::from_files(&manifest.did, &files);
        assert!(inventory.render().contains(hook));
        assert!(inventory.render().contains(mcp));
        let temp = tempfile::tempdir().unwrap();
        let audit_dir = temp.path().join("audit");
        let observability = Arc::new(
            peko_observability::Observability::with_audit_dir("test", audit_dir.clone()).unwrap(),
        );
        let unpackager =
            PrincipalUnpackager::new("unused", temp.path().join("cfg"), temp.path().join("data"));
        unpackager
            .import_from_files(
                files,
                PrincipalImportOptions {
                    observability: Some(observability),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let today = chrono::Utc::now().date_naive();
        let log = std::fs::read_to_string(audit_dir.join(format!("audit-{today}.jsonl"))).unwrap();
        let events: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let event = events
            .iter()
            .find(|e| e["event_type"] == "principal.snapshot_import")
            .expect("import audit event");
        assert_eq!(event["severity"], "security");
        assert_eq!(event["details"], serde_json::to_value(&inventory).unwrap());
    }
    #[tokio::test]
    async fn archive_rejects_duplicate_paths_and_symlinks() {
        for symlink in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("invalid.peko");
            let encoder = flate2::write::GzEncoder::new(
                std::fs::File::create(&path).unwrap(),
                flate2::Compression::default(),
            );
            let mut tar = tar::Builder::new(encoder);
            for _ in 0..2 {
                let mut header = tar::Header::new_gnu();
                header.set_path("manifest.toml").unwrap();
                header.set_mode(0o644);
                if symlink {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_link_name("/tmp/outside").unwrap();
                    header.set_size(0);
                    header.set_cksum();
                    tar.append(&header, std::io::empty()).unwrap();
                } else {
                    header.set_size(1);
                    header.set_cksum();
                    tar.append(&header, &b"x"[..]).unwrap();
                }
            }
            tar.into_inner().unwrap().finish().unwrap();
            let unpackager =
                PrincipalUnpackager::new(&path, temp.path().join("cfg"), temp.path().join("data"));
            let error = unpackager.inspect().await.unwrap_err().to_string();
            assert!(
                error.contains(if symlink { "non-file" } else { "Duplicate" }),
                "{error}"
            );
            assert!(!temp.path().join("cfg").exists());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_roundtrip_preserves_assets_and_execution_after_rename() {
        use crate::extensions::skill::WorkspaceSkillRuntime;
        use crate::tools::builtin::skill::{SkillRuntime, SkillTool};
        use peko_tools_core::{Tool, ToolContext};
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let script_rel = "skills/check/scripts/check.sh";
        std::fs::create_dir_all(source.join("skills/check/scripts")).unwrap();
        let definition =
            "---\nname: check\ndescription: Check assets\n---\n!`skills/check/scripts/check.sh`";
        std::fs::write(source.join("skills/check/SKILL.md"), definition).unwrap();
        std::fs::write(source.join(script_rel), "#!/bin/sh\nprintf portable").unwrap();
        std::fs::set_permissions(
            source.join(script_rel),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::create_dir_all(source.join("skills/check/references")).unwrap();
        std::fs::write(
            source.join("skills/check/references/info.txt"),
            "reference data",
        )
        .unwrap();
        let identity = Identity::new("review-skill", DIDScope::Local)
            .await
            .unwrap();
        let config = sample_config("review-skill", &identity.did);
        let out = tmp.path().join("skill.peko");
        let packager = PrincipalPackager::new(config, identity).with_workspace_dir(&source);
        packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let config_dir = tmp.path().join("imported");
        let unpackager =
            PrincipalUnpackager::new(&out, config_dir.clone(), tmp.path().join("data"));
        let (manifest, files, validation) = unpackager.inspect_detailed().await.unwrap();
        assert!(validation.is_valid());
        assert!(manifest.executable_files.contains(script_rel));
        let inventory = crate::registry::packaging::inventory::ExecutableInventory::from_files(
            &manifest.did,
            &files,
        );
        assert_eq!(inventory.executable_files, [script_rel]);
        assert!(inventory.render().contains(script_rel));
        unpackager
            .import(PrincipalImportOptions {
                new_name: Some("renamed".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let dest = config_dir.join("principals/renamed");
        for rel in [
            "skills/check/SKILL.md",
            script_rel,
            "skills/check/references/info.txt",
        ] {
            assert_eq!(
                std::fs::read(source.join(rel)).unwrap(),
                std::fs::read(dest.join(rel)).unwrap()
            );
        }
        std::fs::remove_dir_all(&source).unwrap();
        let runtime = WorkspaceSkillRuntime::new(dest.join("skills"));
        assert!(runtime.resolve_skill("check").is_some());
        let tool = SkillTool::new(Arc::new(runtime));
        let mut ctx = ToolContext::default_for_tool("Skill");
        ctx.workspace = Some(dest.display().to_string());
        let actual = tool
            .execute_with_context(serde_json::json!({"name":"check"}), &ctx)
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(dest.join(script_rel))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            actual["body"], "portable",
            "script bytes survived but execution did not"
        );
    }
}
