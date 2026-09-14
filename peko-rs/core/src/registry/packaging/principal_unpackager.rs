//! Unpackager for importing portable Principal packages
//!
//! Extracts `.peko` files into the local peko runtime.
#![allow(dead_code)]

use crate::common::authority::{RuntimeAuthority, TierPath};
use crate::common::paths::PathResolver;
use crate::extensions::framework::store::ExtensionStore;
use crate::extensions::framework::types::ExtensionId;
use crate::principal::config::PrincipalConfig;
use crate::registry::packaging::path_safety::safe_join;
use crate::registry::packaging::principal_manifest::PrincipalManifest;
use crate::registry::packaging::trust_store::{TrustPolicy, TrustStatus, TrustStore};
use crate::registry::packaging::validation::ValidationResult;
use peko_auth::Subject;
use peko_extension_api::Capabilities;
use peko_identity::{storage::KeyStorage, Identity, KeyPairExport};
use peko_subject::PrincipalDID;
use std::collections::{BTreeSet, HashMap};
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
    /// Import the identity-bearing Local-tier state from a
    /// full-snapshot package (cron schedule with principal-id
    /// rebinding, plan DAGs). Sessions are governed by
    /// `import_sessions` independently (ADR-056).
    pub import_local_state: bool,
    /// Allow importing an unsigned package
    pub allow_unsigned: bool,
    /// Force overwrite an existing Principal
    pub force: bool,
    /// Daemon-wide trust store used for TOFU pinning.
    pub trust_store: Option<Arc<RwLock<TrustStore>>>,
    /// How to handle trust pinning conflicts.
    pub trust_policy: TrustPolicy,
    /// Capability grants to add to the imported Principal's
    /// `[capabilities] grants` list, deduplicated against existing grants.
    pub selected_capabilities: Vec<String>,
    /// Caller subject for the WriteSide gate. The IPC handler
    /// passes `caller.subject().clone(); defaults to
    /// `Subject::User("local")` so library callers (tests, the CLI
    /// `import` subcommand if it ever bypasses the IPC layer) clear
    /// the Shared tier actor gate.
    pub caller_subject: Subject,
    /// Caller capability snapshot for the WriteSide gate. The IPC
    /// handler passes the post-merge `Capabilities` (starter bundle
    /// plus any caller-selected grants) so the gate reflects what
    /// the new principal will actually carry. Defaults to
    /// `Capabilities::starter_bundle()` so existing tests don't
    /// need to thread this through.
    pub caller_capabilities: Capabilities,
}

impl Default for PrincipalImportOptions {
    fn default() -> Self {
        Self {
            new_name: None,
            rotate_keys: false,
            import_sessions: true,
            import_local_state: true,
            allow_unsigned: false,
            force: false,
            trust_store: None,
            trust_policy: TrustPolicy::Tofu,
            selected_capabilities: Vec::new(),
            caller_subject: Subject::User("local".to_string()),
            caller_capabilities: Capabilities::starter_bundle(),
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
    /// IDs of embedded extensions that were installed during import.
    pub installed_extensions: Vec<String>,
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
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml"))?
            .clone();
        let manifest = self.parse_manifest(&files)?;

        // ADR-056: templates are plain TOML files ground via
        // `principal create -f` — keyless packages are no longer a
        // supported artifact shape. Fail fast, before any crypto work.
        if !files.contains_key("identity/keys.enc") {
            anyhow::bail!(
                "This package carries no keys — it looks like a template artifact. \
                 Templates are plain TOML files: ground one with \
                 `peko principal create <name> -f <template.toml>`."
            );
        }

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
            Ok((SignatureStatus::AllowedUnsigned, _)) => {
                (SignatureStatus::AllowedUnsigned, String::new())
            }
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

        // Defense in depth: even though IPC handlers validate `name` early,
        // re-check here because anything reaching this point flows into
        // filesystem paths. Rejects `..`, `/`, `\`, leading/trailing `-`,
        // non-alnum, etc. (also see the explicit `..` rule introduced in
        // `common::identifiers::validate_agent_name`).
        crate::common::identifiers::validate_agent_name(&name)
            .map_err(|e| anyhow::anyhow!("[unsafe_name] {e}"))?;

        // Embedded extension ids flow into `temp_dir/{id}.ext` paths during
        // `import_extensions` (called separately by the IPC handler).
        // Validate them here too so a caller that reaches this method
        // directly — bypassing the IPC layer — also catches adversarial
        // ids before any temp-dir collision can occur.
        for ext_ref in &manifest.extensions {
            crate::common::identifiers::validate_agent_name(&ext_ref.id)
                .map_err(|e| anyhow::anyhow!("[unsafe_extension_id] {}: {e}", ext_ref.id))?;
        }

        // Phase C: Build a per-call authority that projects the IPC
        // caller's subject and capability snapshot. The agent prompt
        // and identity writes (Shared tier) gate on this authority;
        // sessions writes (Local tier) rely on the actor gate alone
        // (no per-resource capability exists for sessions — the
        // actor's tier-entitlement is the only Layer 2 check).
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let authority = RuntimeAuthority::for_caller(resolver, options.caller_subject.clone());

        let identity = self
            .import_identity(&files, &manifest, &options, &name, &authority)
            .await?;
        let mut config = self.import_config(&files, &name, &identity)?;

        // Apply any capabilities chosen during the preview/confirm flow.
        for cap in &options.selected_capabilities {
            if !config.capabilities.contains_str(cap) {
                config.capabilities.push(cap.clone());
            }
        }

        // Update DID in config to match the imported/rotated identity
        config.did = Some(PrincipalDID(identity.did.clone()));
        config.name = name.clone();

        // Default owner to local user unless already set
        if matches!(config.owner, Subject::User(ref u) if u == "default") {
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

        self.import_agents(&files, &name, &options, &authority)
            .await?;
        // Phase A: the legacy `import_memory` is gone. The memory
        // index (`local/memory_index.json`) is derived state — never
        // packaged, rebuilt by the runtime (ADR-056).
        // Sessions flow in under `import_sessions`; cron + plans and
        // the workspace tooling flow in below when the package
        // carries them (ADR-056).

        if options.import_sessions {
            self.import_sessions(&files, &name).await?;
        }

        self.import_workspace_tooling(&files, &name).await?;
        if carried_local_state && options.import_local_state {
            let effective_principal_id = config
                .id
                .clone()
                .map(|pid| pid.0)
                .or_else(|| config.did.clone().map(|d| d.0))
                .unwrap_or_else(|| name.clone());
            self.import_local_authored(&files, &name, &effective_principal_id)
                .await?;
        }

        let config_path = self.save_config(&config, &name).await?;

        Ok(PrincipalImportResult {
            name,
            did: identity.did,
            config_path,
            keys_rotated: options.rotate_keys,
            validation,
            installed_extensions: Vec::new(),
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
        authority: &RuntimeAuthority,
    ) -> anyhow::Result<Identity> {
        let did_doc_bytes = files
            .get("identity/did.json")
            .ok_or_else(|| anyhow::anyhow!("Missing identity/did.json"))?;
        let did_doc: peko_identity::DIDDocument = serde_json::from_slice(did_doc_bytes)?;

        // Phase A: identity lives in the Shared tier so it ships
        // in the portable bundle. The directory holds the
        // `identity.json` (public DID) and `keys.enc` (private key
        // export); `KeyStorage::with_path` expects the directory.
        //
        // Phase C: gate the directory on `principal:write_identity`
        // via the caller-projected authority. Sponsor's `[[permissions]]`
        // ACL is the lower-level PekoHub check; this is the per-resource
        // gate.
        let identity_dir = authority
            .shared_identity_dir_write_for_name(principal_name, Some(&options.caller_capabilities))?
            .to_path_buf();

        if options.rotate_keys {
            let new_identity = Identity::new(
                &manifest.principal.name,
                peko_identity::did::DIDScope::Local,
            )
            .await?;
            let key_storage = KeyStorage::with_path(identity_dir)?;
            key_storage.store_identity(&new_identity).await?;
            return Ok(new_identity);
        }

        let key_data = if manifest.identity.encrypted {
            anyhow::bail!("Encrypted principal packages are not yet supported")
        } else {
            files["identity/keys.enc"].clone()
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
        options: &PrincipalImportOptions,
        authority: &RuntimeAuthority,
    ) -> anyhow::Result<()> {
        // Phase A: agents live under the Shared tier
        // (`{config_dir}/principals/{name}/agents/`) so they ship in
        // the principal bundle.
        //
        // Phase C: gate the directory on `principal:write_agents` via
        // the caller-projected authority. Matches the
        // `PrincipalCreate` agent-prompt write gate.
        let agents_dir = authority
            .shared_agents_dir_write_for_name(principal_name, Some(&options.caller_capabilities))?
            .to_path_buf();

        for (path, content) in files {
            if path.starts_with("agents/") {
                let file_name = path.strip_prefix("agents/").unwrap_or(path);
                let dest_path = safe_join(&agents_dir, file_name)?;
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
    /// Prototype note: no per-directory capability gate yet. The
    /// ADR-046 audit canary still covers `tools/`, `hooks/`, and
    /// `mcp/` baseline drift on the next daemon boot, and the
    /// production pass should gate this alongside `principal:write_agents`.
    async fn import_workspace_tooling(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
    ) -> anyhow::Result<()> {
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let workspace_root = resolver.principal_layout(principal_name).shared.root;

        const TOOLING_PREFIXES: [&str; 5] = ["tools", "skills", "mcp", "hooks", "kb"];
        for (path, content) in files {
            let Some((prefix, rest)) = split_layer_path(path, &TOOLING_PREFIXES) else {
                continue;
            };
            let dest_dir = workspace_root.join(prefix);
            let dest_path = safe_join(&dest_dir, rest)?;
            if let Some(parent) = dest_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(dest_path, content).await?;
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
    /// `CronList` immediately.
    async fn import_local_authored(
        &self,
        files: &HashMap<String, Vec<u8>>,
        principal_name: &str,
        new_principal_id: &str,
    ) -> anyhow::Result<()> {
        let resolver = PathResolver::with_dirs(
            self.config_dir.clone(),
            self.data_dir.clone(),
            self.data_dir.clone(),
        );
        let layout = resolver.principal_layout(principal_name);

        const LOCAL_PREFIXES: [&str; 2] = ["cron", "plans"];
        for (path, content) in files {
            let Some((prefix, rest)) = split_layer_path(path, &LOCAL_PREFIXES) else {
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
                tokio::fs::write(dest_path, remapped).await?;
            } else {
                tokio::fs::write(dest_path, content).await?;
            }
        }
        Ok(())
    }

    /// Extract the embedded `plugins/` (or legacy `extensions/`) layer and
    /// route each bundle through `store`. Returns the IDs of installed
    /// plugins.
    ///
    /// **Phase 5 (ADR-047 §2.1):** workspace-resident tooling
    /// (tools/hooks/skills/MCP) lives in the principal's workspace and
    /// is not embedded in `.peko` packages (the `--with-extensions`
    /// flag was dropped in Phase 5c).
    ///
    /// **Phase 7 (ADR-047 §5):** new packages emit a `plugins/` layer
    /// instead of `extensions/`. This method accepts both prefixes and
    /// routes them through the same handler. As with Phase 5, no plugin
    /// content is actually installed today — the method is preserved for
    /// the IPC handler's call signature and future bundles.
    pub async fn import_extensions(
        &self,
        manifest: &PrincipalManifest,
        _store: &ExtensionStore,
    ) -> anyhow::Result<Vec<ExtensionId>> {
        if !manifest.extensions.is_empty() {
            tracing::warn!(
                "Principal package declares {} embedded extension(s) via the \
                 legacy `extensions` layer; this is mapped to the new `plugins/` \
                 convention (ADR-047 §5). Workspace plugins are not part of \
                 the portable bundle.",
                manifest.extensions.len()
            );
        }
        Ok(Vec::new())
    }

    /// Extract the union of capabilities declared by the embedded plugins
    /// (or legacy embedded extensions) in a `.peko` package.
    ///
    /// Returns `(required_capabilities, warnings)`. The required set is the
    /// union of each embedded plugin manifest's `requires` list.
    ///
    /// **Phase 7 (ADR-047 §5):** accepts both `plugins/<id>.plugin` (new)
    /// and `extensions/<id>.ext` (legacy) archive paths.
    pub fn extract_extension_capabilities(
        manifest: &PrincipalManifest,
        files: &HashMap<String, Vec<u8>>,
    ) -> (Vec<String>, Vec<String>) {
        let mut required = BTreeSet::new();
        let mut warnings = Vec::new();

        for ext_ref in &manifest.extensions {
            // Reject path-traversal spellings in the embedded extension id
            // before formatting any path-like string from it.
            if let Err(e) = crate::common::identifiers::validate_agent_name(&ext_ref.id) {
                warnings.push(format!(
                    "Unsafe extension id '{}' in manifest: {e}",
                    ext_ref.id
                ));
                continue;
            }

            // Phase 7 (ADR-047 §5): prefer `plugins/<id>.plugin` (new
            // exports); fall back to `extensions/<id>.ext` (legacy) so
            // pre-Phase-7 packages still surface their capability union.
            let plugin_path = format!("plugins/{}.plugin", ext_ref.id);
            let legacy_path = format!("extensions/{}.ext", ext_ref.id);
            let (resolved_path, bytes) = match files.get(&plugin_path) {
                Some(b) => (plugin_path.as_str(), Some(b)),
                None => match files.get(&legacy_path) {
                    Some(b) => (legacy_path.as_str(), Some(b)),
                    None => (legacy_path.as_str(), None),
                },
            };
            let Some(bytes) = bytes else {
                warnings.push(format!(
                    "Principal package declares extension '{}' but has no embedded {}",
                    ext_ref.id, resolved_path
                ));
                continue;
            };

            match Self::parse_embedded_extension_capabilities(bytes) {
                Ok((_, reqs)) => {
                    required.extend(reqs);
                }
                Err(e) => {
                    warnings.push(format!(
                        "Could not read capabilities for embedded extension '{}': {e}",
                        ext_ref.id
                    ));
                }
            }
        }

        (required.into_iter().collect(), warnings)
    }

    fn parse_embedded_extension_capabilities(
        embedded_bytes: &[u8],
    ) -> anyhow::Result<(Vec<String>, Vec<String>)> {
        let cursor = std::io::Cursor::new(embedded_bytes);
        let tar = flate2::read::GzDecoder::new(cursor);
        let mut archive = tar::Archive::new(tar);

        let mut manifest_yaml: Option<Vec<u8>> = None;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();
            if path == "extension/manifest.yaml" {
                let mut content = Vec::new();
                entry.read_to_end(&mut content)?;
                manifest_yaml = Some(content);
                break;
            }
        }

        let content = manifest_yaml.ok_or_else(|| {
            anyhow::anyhow!("extension/manifest.yaml not found in embedded .ext package")
        })?;
        let s = std::str::from_utf8(&content)?;
        let value: serde_yaml::Value = serde_yaml::from_str(s)?;

        let string_array = |value: &serde_yaml::Value, key: &str| -> Vec<String> {
            value
                .get(key)
                .and_then(|v| v.as_sequence())
                .map(|seq| {
                    seq.iter()
                        .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };

        let mut provides = string_array(&value, "provides");
        let mut requires = string_array(&value, "requires");
        provides.sort();
        requires.sort();
        Ok((provides, requires))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignatureStatus {
    Verified,
    AllowedUnsigned,
}

/// Split a package path into `(layer_prefix, rest)` if it starts with
/// one of the given layer prefixes (e.g. `cron/schedule.toml` →
/// `("cron", "schedule.toml")`).
fn split_layer_path<'a>(path: &'a str, prefixes: &[&'a str]) -> Option<(&'a str, &'a str)> {
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

pub(crate) fn verify_principal_signature(
    manifest_bytes: &[u8],
    did_doc_bytes: &[u8],
    allow_unsigned: bool,
    name: &str,
) -> anyhow::Result<(SignatureStatus, String)> {
    use base64::Engine;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    use peko_identity::DIDDocument;

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
        anyhow::bail!(
            "signature has wrong length: expected 64, got {}",
            signature_vec.len()
        );
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
        anyhow::bail!(
            "public key has wrong length: expected 32, got {}",
            public_key.len()
        );
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
            store.pin(
                name.to_string(),
                did.to_string(),
                Some(public_key_multibase.to_string()),
            );
            store.save(resolver)?;
            tracing::info!("Pinned principal '{}' to DID {} on first import", name, did);
        }
        TrustStatus::Trusted => {
            tracing::debug!("principal '{}' is already pinned to DID {}", name, did);
        }
        TrustStatus::Mismatch { expected, actual } => {
            if trust_policy == TrustPolicy::AllowUntrusted {
                store.pin(
                    name.to_string(),
                    actual.clone(),
                    Some(public_key_multibase.to_string()),
                );
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
        assert!(!opts.allow_unsigned);
        assert!(opts.selected_capabilities.is_empty());
        // Phase C bootstrap: defaults clear the Shared tier gate so
        // library callers (tests, future direct-call sites) don't
        // have to thread `caller_subject` / `caller_capabilities`
        // through every constructor.
        assert!(matches!(opts.caller_subject, Subject::User(ref u) if u == "local"));
        assert!(opts
            .caller_capabilities
            .is_granted(&peko_extension_api::Capability::new(
                "principal:write_agents"
            )));
        assert!(opts
            .caller_capabilities
            .is_granted(&peko_extension_api::Capability::new(
                "principal:write_identity"
            )));
    }

    fn sample_config(name: &str, did: &str) -> PrincipalConfig {
        PrincipalConfig {
            name: name.to_string(),
            id: None,
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
            boot_state: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
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
        let agents_dir = tmp.path().join("src-agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("planner.md"), b"# Planner").unwrap();

        let out = tmp.path().join("importme.peko");
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
        let unpackager = PrincipalUnpackager::new(&out, config_dir.clone(), data_dir.clone());
        let result = unpackager
            .import(PrincipalImportOptions::default())
            .await
            .unwrap();

        assert_eq!(result.name, "importme");
        assert_eq!(result.did, original_did);
        assert!(result.config_path.exists());

        // Agent prompt restored (Shared tier).
        let agent_path = config_dir
            .join("principals")
            .join("importme")
            .join("agents")
            .join("planner.md");
        assert!(agent_path.exists(), "agent prompt restored");

        // Identity persisted (Shared tier — Phase A moved it out of
        // `data_dir/principals/{name}/identity/` so it ships in the
        // portable bundle).
        let identity_dir = config_dir
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
            .with_agents_dir(shared_root.join("agents"))
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

        // Cron schedule restored AND rebound to the imported
        // principal's effective runtime id. `config.id` is unset in
        // this fixture, so the effective id resolves to the imported
        // DID (id → DID → name, the cron tools' resolution order).
        let schedule_path = data_dir
            .join("principals")
            .join("live")
            .join("local")
            .join("cron")
            .join("schedule.toml");
        let schedule = std::fs::read_to_string(&schedule_path).unwrap();
        assert!(
            schedule.contains(&format!("principal_id = \"{original_did}\"")),
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

    /// ADR-056: the registry artifact is a plain TOML template —
    /// `export_for_registry` emits a stripped `principal.toml`, not a
    /// package. Templates are ground via `principal create -f`, so a
    /// keyless *package* is rejected with actionable guidance rather
    /// than silently cloned.
    #[tokio::test]
    async fn registry_artifact_is_a_template_toml() {
        use crate::registry::packaging::principal_packager::PrincipalExportOptions;

        let identity = Identity::new("tmpl-src", DIDScope::Local).await.unwrap();
        let source_did = identity.did.clone();
        let mut config = sample_config("tmpl-src", &source_did);
        config.id = Some(peko_subject::PrincipalId("prin_tmpl_source".into()));
        config.set_boot_state(BootState::Organized);

        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("tmpl-src.template.toml");
        let packager = PrincipalPackager::new(config, identity);
        let descriptor = packager
            .export_for_registry(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        // The artifact on disk is the template TOML, not an archive.
        let artifact = std::fs::read_to_string(&out).unwrap();
        assert!(!artifact.contains("prin_tmpl_source"), "{artifact}");
        assert!(!artifact.contains(&source_did), "{artifact}");
        assert!(!artifact.contains("boot_state"), "{artifact}");
        assert!(
            !out.display().to_string().ends_with(".peko"),
            "templates are TOML files, not packages"
        );

        // The OCI config blob IS the template TOML; no content layers.
        assert!(descriptor.layers.is_empty());
        let blob = std::str::from_utf8(&descriptor.manifest_toml).unwrap();
        assert_eq!(blob, &artifact);

        // A keyless *package* (wrong shape) is rejected with guidance.
        let files: HashMap<String, Vec<u8>> = HashMap::from([
            (
                "manifest.toml".to_string(),
                PrincipalManifest::new("tmpl-src", "1.0.0", &source_did)
                    .to_toml()
                    .unwrap()
                    .into_bytes(),
            ),
            ("identity/did.json".to_string(), b"{}".to_vec()),
        ]);
        let unpackager =
            PrincipalUnpackager::new(&out, tmp.path().join("cfg"), tmp.path().join("data"));
        let err = unpackager
            .import_from_files(files, PrincipalImportOptions::default())
            .await
            .expect_err("keyless packages are not a supported artifact shape");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("principal create") && msg.contains("-f"),
            "expected create -f guidance, got: {msg}"
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

    /// Build a minimal `.ext` archive in memory containing an
    /// `extension/manifest.yaml` with the given `requires` list.
    fn fake_ext_bytes(requires: &[&str]) -> Vec<u8> {
        let mut manifest = String::from(
            "id: test-ext\n\
             name: Test Extension\n\
             version: 1.0.0\n\
             description: A test extension\n\
             extension_type: skill\n",
        );
        if !requires.is_empty() {
            manifest.push_str("requires:\n");
            for req in requires {
                manifest.push_str(&format!("  - {req}\n"));
            }
        }

        let buf = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_path("extension/manifest.yaml").unwrap();
        header.set_size(manifest.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest.as_bytes()).unwrap();
        tar.finish().unwrap();

        let enc = tar.into_inner().unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn parse_embedded_extension_capabilities_reads_requires() {
        let bytes = fake_ext_bytes(&["tool:Read", "network"]);
        let (provides, requires) =
            PrincipalUnpackager::parse_embedded_extension_capabilities(&bytes).unwrap();
        assert!(provides.is_empty());
        assert_eq!(
            requires,
            vec!["network".to_string(), "tool:Read".to_string()]
        );
    }

    #[test]
    fn extract_extension_capabilities_aggregates_required_caps() {
        let mut manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        manifest.extensions = vec![
            crate::registry::packaging::types::ExtensionRef {
                id: "ext-a".to_string(),
                registry_ref: "pekohub.com/ext/a".to_string(),
            },
            crate::registry::packaging::types::ExtensionRef {
                id: "ext-b".to_string(),
                registry_ref: "pekohub.com/ext/b".to_string(),
            },
        ];

        let mut files = HashMap::new();
        files.insert(
            "extensions/ext-a.ext".to_string(),
            fake_ext_bytes(&["tool:Read"]),
        );
        files.insert(
            "extensions/ext-b.ext".to_string(),
            fake_ext_bytes(&["tool:Write", "network"]),
        );

        let (required, warnings) =
            PrincipalUnpackager::extract_extension_capabilities(&manifest, &files);

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            required,
            vec![
                "network".to_string(),
                "tool:Read".to_string(),
                "tool:Write".to_string(),
            ]
        );
    }

    #[test]
    fn extract_extension_capabilities_warns_on_missing_archive() {
        let mut manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        manifest.extensions = vec![crate::registry::packaging::types::ExtensionRef {
            id: "missing".to_string(),
            registry_ref: "pekohub.com/ext/missing".to_string(),
        }];

        let (required, warnings) =
            PrincipalUnpackager::extract_extension_capabilities(&manifest, &HashMap::new());

        assert!(required.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing"));
    }

    /// Phase 7 (ADR-047 §5): new exports land plugin archives under
    /// `plugins/<id>.plugin` and the unpackager surfaces their capability
    /// union. The legacy `extensions/<id>.ext` path remains accepted but
    /// `plugins/` wins when both are present.
    #[test]
    fn extract_extension_capabilities_reads_plugins_path() {
        let mut manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        manifest.extensions = vec![
            crate::registry::packaging::types::ExtensionRef {
                id: "plugin-a".to_string(),
                registry_ref: "pekohub.com/ext/a".to_string(),
            },
            crate::registry::packaging::types::ExtensionRef {
                id: "plugin-b".to_string(),
                registry_ref: "pekohub.com/ext/b".to_string(),
            },
        ];

        let mut files = HashMap::new();
        files.insert(
            "plugins/plugin-a.plugin".to_string(),
            fake_ext_bytes(&["tool:Read"]),
        );
        files.insert(
            "plugins/plugin-b.plugin".to_string(),
            fake_ext_bytes(&["tool:Write", "network"]),
        );

        let (required, warnings) =
            PrincipalUnpackager::extract_extension_capabilities(&manifest, &files);

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(
            required,
            vec![
                "network".to_string(),
                "tool:Read".to_string(),
                "tool:Write".to_string(),
            ]
        );
    }

    /// Phase 7 (ADR-047 §5): legacy `extensions/<id>.ext` archives are
    /// still readable when no `plugins/<id>.plugin` companion is present.
    #[test]
    fn extract_extension_capabilities_falls_back_to_legacy_path() {
        let mut manifest = PrincipalManifest::new("test", "1.0.0", "did:peko:test");
        manifest.extensions = vec![crate::registry::packaging::types::ExtensionRef {
            id: "ext-a".to_string(),
            registry_ref: "pekohub.com/ext/a".to_string(),
        }];

        let mut files = HashMap::new();
        files.insert(
            "extensions/ext-a.ext".to_string(),
            fake_ext_bytes(&["tool:Read"]),
        );

        let (required, warnings) =
            PrincipalUnpackager::extract_extension_capabilities(&manifest, &files);

        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        assert_eq!(required, vec!["tool:Read".to_string()]);
    }

    /// PR 2: Phase C gate. A caller without `principal:write_agents`
    /// cannot import a `.peko` package — the gate fires inside
    /// `import_agents` before any agent prompt reaches disk.
    /// `import_identity` runs first and also requires
    /// `principal:write_identity`; either gate is acceptable here
    /// (they're checked in order: identity first, then agents).
    /// The test asserts the operation fails with a capability-denied
    /// message rather than letting agent prompts land on disk.
    #[tokio::test]
    async fn import_denied_when_caller_lacks_write_caps() {
        use peko_extension_api::Capabilities;

        let identity = Identity::new("denied", DIDScope::Local).await.unwrap();
        let config = sample_config("denied", &identity.did);

        let tmp = tempfile::tempdir().unwrap();
        let agents_dir = tmp.path().join("src-agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(agents_dir.join("planner.md"), b"# Planner").unwrap();

        let out = tmp.path().join("denied.peko");
        let packager = PrincipalPackager::new(config, identity).with_agents_dir(&agents_dir);
        packager
            .export(PrincipalExportOptions {
                output_path: Some(out.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        let config_dir = tmp.path().join("cfg");
        let data_dir = tmp.path().join("data");
        let unpackager = PrincipalUnpackager::new(&out, config_dir, data_dir);

        // Caller clears the Shared tier actor gate (Subject::User)
        // but carries no capability grants — the first gate that
        // fires (inside `import_identity` for
        // `principal:write_identity`) returns a
        // `CapabilityDenied{Shared}` wrapped in `anyhow::Error`.
        let opts = PrincipalImportOptions {
            caller_capabilities: Capabilities::new(), // empty
            ..PrincipalImportOptions::default()
        };
        let err = unpackager
            .import(opts)
            .await
            .expect_err("import should be denied without capability grants");
        let msg = format!("{err}");
        assert!(
            msg.contains("principal:write_identity")
                || msg.contains("principal:write_agents")
                || msg.contains("CapabilityDenied"),
            "expected capability denial, got: {msg}"
        );
    }
}
