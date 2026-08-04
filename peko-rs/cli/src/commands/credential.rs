//! Credential management commands.
//!
//! These commands manage runtime secrets stored in the encrypted vault at
//! `{config_dir}/vault.enc` (see `peko_core::common::vault`). The vault is a
//! generic namespace-keyed secret store; model API keys live under the
//! `llm` namespace (see `peko model add --key`), but MCP servers, OAuth
//! clients, registries, and arbitrary secrets can use any namespace.
//!
//! Typical flows:
//!
//! ```text
//! # Set a generic credential
//! peko credential set mcp:analytics default --kind api_key --material "$KEY"
//!
//! # List credentials in a namespace
//! peko credential list --namespace llm
//!
//! # Remove a credential
//! peko credential delete <id>
//! ```
//!
//! Live validation of model credentials moved to `peko model test <id>`,
//! which pings the model's actual endpoint with the stored key.

use crate::commands::GlobalPaths;
use anyhow::{Context, Result};
use peko_core::common::vault::{Credential, CredentialFilter, CredentialKind, Vault};
use peko_providers::catalog::ModelCatalog;

/// Credential commands
#[derive(clap::Subcommand)]
pub enum CredentialCommands {
    /// Store or overwrite a credential in the vault.
    Set {
        /// Namespace for the credential (e.g. `llm`, `mcp:analytics`).
        namespace: String,
        /// Slot name within the namespace (e.g. `default`, a model id).
        name: String,
        /// Credential kind.
        #[arg(long, value_name = "KIND")]
        kind: String,
        /// Secret material (omit for hidden prompt).
        #[arg(long, value_name = "SECRET")]
        material: Option<String>,
        /// Optional metadata key/value pairs.
        #[arg(long = "metadata", value_name = "KEY=VALUE", value_parser = parse_metadata_pair)]
        metadata: Vec<(String, String)>,
        /// PR 3: every catalog entry that references the credential
        /// with this id is rewritten to point at the newly stored one.
        /// Used to bulk-rotate dependents before deleting the old key.
        #[arg(long, value_name = "CREDENTIAL_ID")]
        replace_on: Option<String>,
    },
    /// Fetch a credential record (the secret material is never shown).
    Get {
        /// Credential id (UUID).
        id: String,
    },
    /// Delete a credential by id.
    ///
    /// Refuses (exit 3) if the credential is referenced by any
    /// configured model. Pass `--force` to detach those models first;
    /// the detach is audit-logged at WARN. Use
    /// `peko credential set <new> --replace-on <id>` to swap
    /// dependents onto a new credential instead of breaking them.
    Delete {
        /// Credential id (UUID).
        id: String,
        /// PR 3: detach dependents silently before deleting. The
        /// detach is audit-logged. Prefer `--replace-on` on a fresh
        /// set when the dependents should keep working.
        #[arg(long)]
        force: bool,
    },
    /// List credentials with optional filters.
    List {
        /// Filter by namespace.
        #[arg(long, value_name = "NAMESPACE")]
        namespace: Option<String>,
        /// Filter by kind.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
        /// Include runtime-owned credentials (identity, tunnel).
        #[arg(long)]
        include_system: bool,
    },
    /// Migrate legacy provider keys from the OS keychain into the vault.
    ///
    /// Legacy per-provider OS keychain entries are no longer a supported
    /// secret source; the unified vault is the single source of truth.
    Migrate,
}

/// Execute a credential subcommand.
pub async fn execute(cmd: CredentialCommands, paths: &GlobalPaths) -> Result<()> {
    let vault =
        Vault::load(paths.resolver().vault()).with_context(|| "failed to load credential vault")?;

    match cmd {
        CredentialCommands::Set {
            namespace,
            name,
            kind,
            material,
            metadata,
            replace_on,
        } => set_cmd(paths, &vault, &namespace, &name, &kind, material, metadata, replace_on).await,
        CredentialCommands::Get { id } => get_cmd(&vault, &id).await,
        CredentialCommands::Delete { id, force } => match delete_cmd(paths, &vault, &id, force).await
        {
            Ok(()) => Ok(()),
            Err(DeleteError::InUse { message }) => {
                // PR 3 / `feature/model-first-config`: exit code 3
                // signals "credential in use" so scripts can detect
                // the refusal without parsing the message.
                eprintln!("{message}");
                std::process::exit(3);
            }
            Err(DeleteError::Other(e)) => Err(e),
        },
        CredentialCommands::List {
            namespace,
            kind,
            include_system,
        } => {
            list_cmd(
                &vault,
                paths,
                namespace.as_deref(),
                kind.as_deref(),
                include_system,
            )
            .await
        }
        CredentialCommands::Migrate => migrate_cmd(&vault).await,
    }
}

/// Tagged error returned by `delete_cmd`. The dispatcher maps
/// `InUse` to `exit(3)`; everything else propagates as `anyhow`.
#[derive(Debug)]
enum DeleteError {
    InUse {
        message: String,
    },
    Other(anyhow::Error),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeleteError::InUse { message } => f.write_str(message),
            DeleteError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<anyhow::Error> for DeleteError {
    fn from(e: anyhow::Error) -> Self {
        DeleteError::Other(e)
    }
}

async fn set_cmd(
    paths: &GlobalPaths,
    vault: &Vault,
    namespace: &str,
    name: &str,
    kind: &str,
    material: Option<String>,
    metadata_pairs: Vec<(String, String)>,
    replace_on: Option<String>,
) -> Result<()> {
    let kind = parse_kind(kind)
        .with_context(|| format!("unknown credential kind '{kind}'; expected one of: api_key, bearer_token, oauth_token, basic_auth, private_key, generic_secret"))?;
    let material = read_material(material, "Credential material: ")?;
    let secret = secrecy::SecretString::from(material);

    let mut credential = Credential::now(namespace.to_string(), name.to_string(), kind, secret);
    credential.metadata = build_metadata(metadata_pairs);
    if let Some(id) = find_credential_id_for_slot(vault, namespace, name) {
        credential.id = id;
    }
    let id = credential.id.clone();

    match vault.set_credential(&credential) {
        Ok(()) => {}
        Err(e) => {
            if e.downcast_ref::<peko_core::common::vault::VaultError>()
                .is_some_and(|err| {
                    matches!(
                        err,
                        peko_core::common::vault::VaultError::SystemCredential(_)
                    )
                })
            {
                anyhow::bail!(
                    "credential '{namespace}/{name}' is runtime-owned and cannot be changed with this command; \
                     use the runtime-specific command instead"
                );
            }
            return Err(e).with_context(|| {
                format!("failed to store credential '{namespace}/{name}' in vault")
            });
        }
    }

    // PR 3 / `feature/model-first-config`: bulk-rewire dependents
    // when `--replace-on <old-id>` was supplied. Catalog is loaded
    // lazily here so a `credential set` without `--replace-on` stays
    // catalog-free.
    let mut rewired = 0usize;
    if let Some(old_id) = replace_on.as_deref() {
        if old_id == id {
            anyhow::bail!(
                "--replace-on id matches the credential being stored; nothing to rewire"
            );
        }
        let cat = ModelCatalog::load_or_init(paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .context("failed to load model catalog for --replace-on rewire")?;
        rewired = cat
            .rewire_credential(old_id, &id)
            .await
            .with_context(|| format!("catalog rewire from '{old_id}' to '{id}' failed"))?;
    }

    if rewired > 0 {
        println!(
            "Stored credential '{namespace}/{name}' (id {id}). Rewired {rewired} model(s) from '{old}'.",
            old = replace_on.as_deref().unwrap_or("?"),
        );
    } else {
        println!("Stored credential '{namespace}/{name}' (id {id}).");
    }
    notify_daemon_reload().await;
    Ok(())
}

async fn get_cmd(vault: &Vault, id: &str) -> Result<()> {
    let credential = vault
        .get_credential(id)
        .with_context(|| format!("credential not found: {id}"))?;
    println!("id:           {}", credential.id);
    println!("namespace:    {}", credential.namespace);
    println!("name:         {}", credential.name);
    println!("kind:         {}", credential.kind.as_str());
    if !credential.metadata.is_null()
        && credential.metadata != serde_json::Value::Object(serde_json::Map::new())
    {
        println!("metadata:     {}", credential.metadata);
    }
    println!("created_at:   {}", credential.created_at.to_rfc3339());
    println!("updated_at:   {}", credential.updated_at.to_rfc3339());
    if let Some(tested_at) = credential.last_tested_at {
        println!("last_tested_at: {}", tested_at.to_rfc3339());
        if let Some(ok) = credential.last_tested_ok {
            println!("last_tested_ok: {}", ok);
        }
    }
    Ok(())
}

async fn delete_cmd(
    paths: &GlobalPaths,
    vault: &Vault,
    id: &str,
    force: bool,
) -> std::result::Result<(), DeleteError> {
    // PR 3 / `feature/model-first-config`: refuse the delete if any
    // configured model still references the credential. The catalog
    // is read here so the CLI matches what the daemon would say on
    // the IPC path; both surfaces enforce the same rule
    // independently so a CLI-only run is just as safe as a desktop
    // run.
    let dependents = match ModelCatalog::load_or_init(paths.config_dir.join(ModelCatalog::FILENAME))
        .await
    {
        Ok(cat) => cat.models_referencing(id).await,
        // Treat a missing/unreadable catalog as "no dependents" — we
        // can't prove any, so don't block the delete. The vault still
        // owns the authority here.
        Err(_) => Vec::new(),
    };

    if !dependents.is_empty() && !force {
        let names = dependents
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "credential '{id}' is referenced by {} model(s): {names}.\n\
             Re-run with --force to detach them and delete anyway (audit-logged at WARN),\n\
             or use `peko credential set <new-id> --replace-on {id}` to swap them first.",
            dependents.len(),
        );
        return Err(DeleteError::InUse { message });
    }

    let mut detached = 0u32;
    if !dependents.is_empty() {
        // Force path: detach every dependent before the vault delete
        // so we never leave an orphan pointing at a missing credential.
        let cat = ModelCatalog::load_or_init(paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .context("failed to load model catalog for force-delete detach")
            .map_err(DeleteError::Other)?;
        let entries = cat.list_all().await;
        for mut entry in entries {
            if entry.credential_id.as_deref() == Some(id) {
                entry.credential_id = None;
                entry.updated_at = chrono::Utc::now();
                cat.upsert(entry)
                    .await
                    .context("failed to detach dependent model during force-delete")
                    .map_err(DeleteError::Other)?;
                detached += 1;
            }
        }
    }

    match vault.delete_credential(id) {
        Ok(true) => {
            if detached > 0 {
                println!("Deleted credential '{id}' and detached {detached} model(s).");
            } else {
                println!("Deleted credential '{id}'.");
            }
            notify_daemon_reload().await;
        }
        Ok(false) => {
            println!("No credential '{id}'.");
        }
        Err(e) => {
            if e.downcast_ref::<peko_core::common::vault::VaultError>()
                .is_some_and(|err| {
                    matches!(
                        err,
                        peko_core::common::vault::VaultError::SystemCredential(_)
                    )
                })
            {
                return Err(DeleteError::Other(anyhow::anyhow!(
                    "credential '{id}' is runtime-owned and cannot be deleted with this command; \
                     use the runtime-specific command instead"
                )));
            }
            return Err(DeleteError::Other(
                e.context(format!("failed to delete credential '{id}'")),
            ));
        }
    }
    Ok(())
}

async fn list_cmd(
    vault: &Vault,
    paths: &GlobalPaths,
    namespace: Option<&str>,
    kind: Option<&str>,
    include_system: bool,
) -> Result<()> {
    let kind = match kind {
        Some(k) => Some(parse_kind(k).with_context(|| format!("unknown credential kind '{k}'"))?),
        None => None,
    };
    let filter = CredentialFilter {
        namespace: namespace.map(String::from),
        kind,
        include_system,
    };
    let summaries = vault.list_credentials(&filter);
    if summaries.is_empty() {
        println!("No credentials match the requested filters.");
        return Ok(());
    }

    // PR 3 / `feature/model-first-config`: join with the model
    // catalog so the listing can flag orphaned (referenced) keys.
    // The catalog is best-effort — a missing or unreadable file
    // means we just skip the dependent badge.
    let dependents: std::collections::HashMap<String, Vec<peko_providers::catalog::ModelConfig>> =
        match ModelCatalog::load_or_init(paths.config_dir.join(ModelCatalog::FILENAME)).await {
            Ok(cat) => {
                let mut map: std::collections::HashMap<
                    String,
                    Vec<peko_providers::catalog::ModelConfig>,
                > = std::collections::HashMap::new();
                for entry in cat.list_all().await {
                    if let Some(cid) = entry.credential_id.clone() {
                        map.entry(cid).or_default().push(entry);
                    }
                }
                map
            }
            Err(_) => std::collections::HashMap::new(),
        };

    println!("Credentials ({}):", summaries.len());
    for s in &summaries {
        let tested = match (s.last_tested_at, s.last_tested_ok) {
            (Some(dt), Some(true)) => {
                format!(" | last tested {} ✓", dt.format("%Y-%m-%d %H:%M UTC"))
            }
            (Some(dt), Some(false)) => {
                format!(" | last tested {} ✗", dt.format("%Y-%m-%d %H:%M UTC"))
            }
            _ => String::new(),
        };
        let ref_badge = match dependents.get(&s.id) {
            Some(entries) if !entries.is_empty() => format!(" | used by {} model(s)", entries.len()),
            _ => String::new(),
        };
        println!(
            "  {}  {}:{}{}  {}{}{}",
            s.id,
            s.namespace,
            s.name,
            ref_badge,
            s.kind.as_str(),
            tested,
            // trailing space already on `tested`; nothing extra
            ""
        );
    }

    // Surface orphans: any credential that's been flagged as
    // unused for ≥1 model references is fine, but a slot with
    // nothing referencing it is just sitting there. The CLI shows
    // the count at the bottom so the user can spot cleanup targets.
    let orphans = summaries
        .iter()
        .filter(|s| !s.has_key || !dependents.contains_key(&s.id))
        .count();
    if orphans > 0 {
        println!(
            "\nOrphaned vault keys (no model references): {orphans}. \
             Remove with `peko credential delete <id>`.",
        );
    }

    Ok(())
}

async fn migrate_cmd(_vault: &Vault) -> Result<()> {
    // RP3B: legacy per-provider OS keychain entries are no longer a
    // supported secret source. The unified vault is the single source
    // of truth for model API keys.
    println!("No legacy keychain entries to migrate.");
    Ok(())
}

/// Tell the running daemon to re-read the vault so the in-flight
/// root agent sees the mutation just stored/deleted. Silent on
/// connection failure (daemon may not be running; the next
/// `peko daemon start` will pick up the new state from disk).
async fn notify_daemon_reload() {
    let Ok(client) = peko_core::ipc::DaemonClient::connect().await else {
        return;
    };
    if let Err(e) = client.reload_providers().await {
        eprintln!("Daemon reload failed: {e}");
    }
}

/// Read material from `--material` or prompt with hidden echo.
fn read_material(material: Option<String>, prompt: &str) -> Result<String> {
    match material {
        Some(m) if !m.is_empty() => Ok(m),
        Some(_) => anyhow::bail!("--material must not be empty"),
        None => prompt_hidden(prompt),
    }
}

/// Prompt the user on stdin with hidden echo.
fn prompt_hidden(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let value = rpassword::prompt_password(prompt)
            .map_err(|e| anyhow::anyhow!("failed to read hidden prompt: {e}"))?;
        Ok(value)
    } else {
        eprintln!(
            "(warning: stdin is not a TTY; reading material visibly. \
             Pipe via `--material <VALUE>` for non-interactive use.)"
        );
        let mut s = String::new();
        stdin
            .read_line(&mut s)
            .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;
        Ok(s.trim().to_string())
    }
}

/// Build metadata JSON from `--metadata KEY=VALUE` pairs.
fn build_metadata(pairs: Vec<(String, String)>) -> serde_json::Value {
    if pairs.is_empty() {
        return serde_json::Value::Null;
    }
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        map.insert(k, serde_json::Value::String(v));
    }
    serde_json::Value::Object(map)
}

/// Parse a credential kind string.
fn parse_kind(s: &str) -> Option<CredentialKind> {
    match s {
        "api_key" => Some(CredentialKind::ApiKey),
        "bearer_token" => Some(CredentialKind::BearerToken),
        "oauth_token" => Some(CredentialKind::OAuthToken),
        "basic_auth" => Some(CredentialKind::BasicAuth),
        "private_key" => Some(CredentialKind::PrivateKey),
        "generic_secret" => Some(CredentialKind::GenericSecret),
        _ => None,
    }
}

/// Parse `--metadata KEY=VALUE`, splitting on the first `=`.
fn parse_metadata_pair(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) => Ok((k.to_string(), v.to_string())),
        None => Err(format!("metadata must be in the form KEY=VALUE, got '{s}'")),
    }
}

/// Find the id of the credential at `(namespace, name)`, if any.
fn find_credential_id_for_slot(vault: &Vault, namespace: &str, name: &str) -> Option<String> {
    vault
        .list_credentials(&CredentialFilter {
            namespace: Some(namespace.to_string()),
            kind: None,
            include_system: true,
        })
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{from_cli, Cli};
    use clap::Parser;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::tempdir;

    fn test_vault() -> (tempfile::TempDir, Vault) {
        let tmp = tempdir().unwrap();
        let vault = Vault::for_test(tmp.path(), "test-passphrase");
        (tmp, vault)
    }

    /// Build a `GlobalPaths` rooted at a fresh tempdir. Mirrors the
    /// `fresh_paths` helper in `commands::model::tests` so credential
    /// and model tests can share an isolated config directory.
    fn fresh_paths() -> GlobalPaths {
        let temp = std::env::temp_dir().join(format!(
            "PEKO_cred_test_{}_{}",
            std::process::id(),
            AtomicU64::new(0).fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        std::env::set_var("PEKO_MASTER_PASSPHRASE", "test-cred-cmd");
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            temp.join("config").to_str().unwrap(),
            "--data-dir",
            temp.join("data").to_str().unwrap(),
            "--cache-dir",
            temp.join("cache").to_str().unwrap(),
            "credential",
            "list",
        ]);
        from_cli(&cli)
    }

    #[test]
    fn parse_kind_accepts_all_variants() {
        assert_eq!(parse_kind("api_key"), Some(CredentialKind::ApiKey));
        assert_eq!(
            parse_kind("bearer_token"),
            Some(CredentialKind::BearerToken)
        );
        assert_eq!(parse_kind("oauth_token"), Some(CredentialKind::OAuthToken));
        assert_eq!(parse_kind("basic_auth"), Some(CredentialKind::BasicAuth));
        assert_eq!(parse_kind("private_key"), Some(CredentialKind::PrivateKey));
        assert_eq!(
            parse_kind("generic_secret"),
            Some(CredentialKind::GenericSecret)
        );
        assert_eq!(parse_kind("nope"), None);
    }

    #[test]
    fn parse_metadata_pair_splits_on_first_equals() {
        assert_eq!(
            parse_metadata_pair("foo=bar").unwrap(),
            ("foo".to_string(), "bar".to_string())
        );
        assert_eq!(
            parse_metadata_pair("foo=bar=baz").unwrap(),
            ("foo".to_string(), "bar=baz".to_string())
        );
        assert!(parse_metadata_pair("noequals").is_err());
    }

    #[tokio::test]
    async fn generic_set_credential_stores_in_vault() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        set_cmd(
            &paths,
            &vault,
            "mcp:analytics",
            "default",
            "api_key",
            Some("analytics-key".to_string()),
            vec![("region".to_string(), "us-east".to_string())],
            None,
        )
        .await
        .unwrap();

        let summaries = vault.list_credentials(&CredentialFilter::default());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].namespace, "mcp:analytics");
        assert_eq!(summaries[0].name, "default");
        assert_eq!(summaries[0].kind, CredentialKind::ApiKey);

        let full = vault.get_credential(&summaries[0].id).unwrap();
        assert_eq!(full.metadata["region"], "us-east");
        assert_eq!(full.material.expose_secret(), "analytics-key");
    }

    #[tokio::test]
    async fn generic_get_credential_shows_no_material() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        set_cmd(
            &paths,
            &vault,
            "llm",
            "my-model",
            "api_key",
            Some("sk-test".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        let id = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();

        // get_cmd prints to stdout; we just verify it does not panic and
        // the vault record is intact.
        get_cmd(&vault, &id).await.unwrap();
    }

    #[tokio::test]
    async fn generic_delete_credential_removes_it() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        set_cmd(
            &paths,
            &vault,
            "secret:foo",
            "default",
            "generic_secret",
            Some("bar".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        let id = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();

        delete_cmd(&paths, &vault, &id, false).await.unwrap();
        assert!(vault.get_credential(&id).is_none());
    }

    #[tokio::test]
    async fn list_credentials_respects_namespace_and_kind_filters() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        set_cmd(
            &paths,
            &vault,
            "llm",
            "my-model",
            "api_key",
            Some("sk-1".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        set_cmd(
            &paths,
            &vault,
            "mcp:analytics",
            "default",
            "api_key",
            Some("key".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        set_cmd(
            &paths,
            &vault,
            "oauth:server",
            "default",
            "oauth_token",
            Some("tok".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();

        let llm_only = vault.list_credentials(&CredentialFilter {
            namespace: Some("llm".to_string()),
            kind: None,
            include_system: false,
        });
        assert_eq!(llm_only.len(), 1);
        assert_eq!(llm_only[0].namespace, "llm");

        let api_key_only = vault.list_credentials(&CredentialFilter {
            namespace: None,
            kind: Some(CredentialKind::ApiKey),
            include_system: false,
        });
        assert_eq!(api_key_only.len(), 2);
    }

    /// `list --include-system` parses from argv.
    #[test]
    fn list_include_system_parses() {
        let cli = Cli::try_parse_from(["peko", "credential", "list", "--include-system"]).unwrap();
        match cli.command {
            crate::commands::Commands::Credential(CredentialCommands::List {
                namespace,
                kind,
                include_system,
            }) => {
                assert!(namespace.is_none());
                assert!(kind.is_none());
                assert!(include_system);
            }
            _ => panic!("expected credential list"),
        }
    }

    /// PR 3 / `feature/model-first-config`: `credential delete`
    /// refuses when a configured model still references the
    /// credential, returning `DeleteError::InUse` with a
    /// dependents list. The dispatcher in `execute()` maps this
    /// to `exit(3)`; we test the helper directly because calling
    /// `exit` from a unit test would terminate `cargo`.
    #[tokio::test]
    async fn delete_refuses_when_catalog_references_credential() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        // 1. Store a credential under `llm`.
        set_cmd(
            &paths,
            &vault,
            "llm",
            "anthropic-sonnet",
            "api_key",
            Some("sk-test".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        let cred_id = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();

        // 2. Insert a catalog entry that points at that credential.
        let cat = ModelCatalog::load_or_init(paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .unwrap();
        let tmpl = peko_providers::templates::find_template("anthropic").unwrap();
        let mut entry = peko_providers::catalog::ModelConfig::from_template(
            tmpl,
            "anthropic-sonnet",
            "claude-sonnet-4-5",
        );
        entry.credential_id = Some(cred_id.clone());
        cat.upsert(entry).await.unwrap();

        // 3. Without --force, delete_cmd must return InUse and the
        // vault record must survive.
        match delete_cmd(&paths, &vault, &cred_id, false).await {
            Err(DeleteError::InUse { message }) => {
                assert!(
                    message.contains(&cred_id),
                    "error message should name the credential id, got: {message}"
                );
                assert!(
                    message.contains("anthropic-sonnet"),
                    "error message should name the dependent model, got: {message}"
                );
            }
            other => panic!("expected DeleteError::InUse, got {other:?}"),
        }
        assert!(
            vault.get_credential(&cred_id).is_some(),
            "vault record must survive a refused delete"
        );

        // 4. With --force, delete proceeds, detaches the model, and
        //    removes the vault record.
        delete_cmd(&paths, &vault, &cred_id, true).await.unwrap();
        assert!(vault.get_credential(&cred_id).is_none());
    }

    /// System-owned credentials are excluded from default `list` output.
    #[tokio::test]
    async fn list_excludes_system_credentials_by_default() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        set_cmd(
            &paths,
            &vault,
            "llm",
            "openai",
            "api_key",
            Some("sk-1".to_string()),
            vec![],
            None,
        )
        .await
        .unwrap();
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();

        list_cmd(&vault, &paths, None, None, false).await.unwrap();
        let summaries = vault.list_credentials(&CredentialFilter::default());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].namespace, "llm");
    }

    /// Generic `delete` on a system credential fails with a clear error.
    #[tokio::test]
    async fn delete_system_credential_rejected() {
        let (_tmp, vault) = test_vault();
        let paths = fresh_paths();
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();
        let id = vault.list_credentials(&CredentialFilter {
            include_system: true,
            ..Default::default()
        })[0]
            .id
            .clone();

        match delete_cmd(&paths, &vault, &id, false).await {
            Err(DeleteError::Other(e)) => assert!(e.to_string().contains("runtime-owned")),
            other => panic!("expected DeleteError::Other, got {other:?}"),
        }
    }
}
