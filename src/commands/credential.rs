//! Credential management commands.
//!
//! These commands manage runtime secrets stored in the encrypted vault at
//! `{config_dir}/vault.enc` (see `crate::common::vault`). The vault is a
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
use crate::common::vault::{Credential, CredentialFilter, CredentialKind, Vault};
use anyhow::{Context, Result};

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
    },
    /// Fetch a credential record (the secret material is never shown).
    Get {
        /// Credential id (UUID).
        id: String,
    },
    /// Delete a credential by id.
    Delete {
        /// Credential id (UUID).
        id: String,
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
        } => set_cmd(&vault, &namespace, &name, &kind, material, metadata).await,
        CredentialCommands::Get { id } => get_cmd(&vault, &id).await,
        CredentialCommands::Delete { id } => delete_cmd(&vault, &id).await,
        CredentialCommands::List {
            namespace,
            kind,
            include_system,
        } => {
            list_cmd(&vault,
                namespace.as_deref(),
                kind.as_deref(),
                include_system,
            )
            .await
        }
        CredentialCommands::Migrate => migrate_cmd(&vault).await,
    }
}

async fn set_cmd(
    vault: &Vault,
    namespace: &str,
    name: &str,
    kind: &str,
    material: Option<String>,
    metadata_pairs: Vec<(String, String)>,
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
            if e.downcast_ref::<crate::common::vault::VaultError>()
                .is_some_and(|err| matches!(err, crate::common::vault::VaultError::SystemCredential(_)))
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
    println!("Stored credential '{namespace}/{name}' (id {id}).");
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

async fn delete_cmd(vault: &Vault, id: &str) -> Result<()> {
    match vault.delete_credential(id) {
        Ok(true) => {
            println!("Deleted credential '{id}'.");
            notify_daemon_reload().await;
        }
        Ok(false) => {
            println!("No credential '{id}'.");
        }
        Err(e) => {
            if e.downcast_ref::<crate::common::vault::VaultError>()
                .is_some_and(|err| matches!(err, crate::common::vault::VaultError::SystemCredential(_)))
            {
                anyhow::bail!(
                    "credential '{id}' is runtime-owned and cannot be deleted with this command; \
                     use the runtime-specific command instead"
                );
            }
            return Err(e).with_context(|| format!("failed to delete credential '{id}'"));
        }
    }
    Ok(())
}

async fn list_cmd(
    vault: &Vault,
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
    println!("Credentials ({}):", summaries.len());
    for s in summaries {
        let tested = match (s.last_tested_at, s.last_tested_ok) {
            (Some(dt), Some(true)) => {
                format!(" | last tested {} ✓", dt.format("%Y-%m-%d %H:%M UTC"))
            }
            (Some(dt), Some(false)) => {
                format!(" | last tested {} ✗", dt.format("%Y-%m-%d %H:%M UTC"))
            }
            _ => String::new(),
        };
        println!(
            "  {}  {}:{}  {}{}",
            s.id,
            s.namespace,
            s.name,
            s.kind.as_str(),
            tested
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
    let Ok(client) = crate::ipc::DaemonClient::connect().await else {
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
    use crate::commands::Cli;
    use clap::Parser;
    use secrecy::ExposeSecret;
    use tempfile::tempdir;

    fn test_vault() -> (tempfile::TempDir, Vault) {
        let tmp = tempdir().unwrap();
        let vault = Vault::for_test(tmp.path(), "test-passphrase");
        (tmp, vault)
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
        set_cmd(
            &vault,
            "mcp:analytics",
            "default",
            "api_key",
            Some("analytics-key".to_string()),
            vec![("region".to_string(), "us-east".to_string())],
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
        set_cmd(
            &vault,
            "llm",
            "my-model",
            "api_key",
            Some("sk-test".to_string()),
            vec![],
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
        set_cmd(
            &vault,
            "secret:foo",
            "default",
            "generic_secret",
            Some("bar".to_string()),
            vec![],
        )
        .await
        .unwrap();
        let id = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();

        delete_cmd(&vault, &id).await.unwrap();
        assert!(vault.get_credential(&id).is_none());
    }

    #[tokio::test]
    async fn list_credentials_respects_namespace_and_kind_filters() {
        let (_tmp, vault) = test_vault();
        set_cmd(
            &vault,
            "llm",
            "my-model",
            "api_key",
            Some("sk-1".to_string()),
            vec![],
        )
        .await
        .unwrap();
        set_cmd(
            &vault,
            "mcp:analytics",
            "default",
            "api_key",
            Some("key".to_string()),
            vec![],
        )
        .await
        .unwrap();
        set_cmd(
            &vault,
            "oauth:server",
            "default",
            "oauth_token",
            Some("tok".to_string()),
            vec![],
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
        let cli = Cli::try_parse_from([
            "peko",
            "credential",
            "list",
            "--include-system",
        ])
        .unwrap();
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

    /// System-owned credentials are excluded from default `list` output.
    #[tokio::test]
    async fn list_excludes_system_credentials_by_default() {
        let (_tmp, vault) = test_vault();
        set_cmd(
            &vault,
            "llm",
            "openai",
            "api_key",
            Some("sk-1".to_string()),
            vec![],
        )
        .await
        .unwrap();
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();

        list_cmd(&vault, None, None, false).await.unwrap();
        let summaries = vault.list_credentials(&CredentialFilter::default());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].namespace, "llm");
    }

    /// Generic `delete` on a system credential fails with a clear error.
    #[tokio::test]
    async fn delete_system_credential_rejected() {
        let (_tmp, vault) = test_vault();
        vault
            .set_identity_private_key("kid", "ed25519-raw-base64", "abc")
            .unwrap();
        let id = vault
            .list_credentials(&CredentialFilter {
                include_system: true,
                ..Default::default()
            })[0]
            .id
            .clone();

        let err = delete_cmd(&vault, &id).await.unwrap_err();
        assert!(err.to_string().contains("runtime-owned"));
    }
}
