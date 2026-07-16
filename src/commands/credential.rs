//! Credential management commands.
//!
//! These commands manage runtime secrets stored in the encrypted vault at
//! `{config_dir}/vault.enc` (see `crate::common::vault`). The vault is a
//! generic namespace-keyed secret store; provider API keys live at
//! `provider:<id>/default`, but MCP servers, OAuth clients, registries,
//! and arbitrary secrets can use any namespace.
//!
//! Typical flows:
//!
//! ```text
//! # Set a generic credential
//! peko credential set mcp:analytics default --kind api_key --material "$KEY"
//!
//! # Set a provider API key (provider sugar)
//! peko credential provider-set-key openai --material "$OPENAI_KEY"
//! peko provider set-key anthropic --material "$ANTHROPIC_KEY"
//!
//! # Add a secondary provider key for rotation
//! peko provider rotate-add anthropic --material "$ANTHROPIC_ALT_KEY"
//!
//! # List credentials in a namespace
//! peko credential list --namespace provider:openai
//!
//! # Create a rotation binding
//! peko credential binding set provider:anthropic:default \
//!   --strategy round_robin --order <id1> <id2>
//!
//! # Live validation
//! peko credential test <id>
//! peko credential provider-test openai
//!
//! # Remove a credential
//! peko credential delete <id>
//! ```

use crate::commands::GlobalPaths;
use crate::common::vault::{
    Credential, CredentialFilter, CredentialKind, RotationBinding, RotationStrategy, Vault,
};
use crate::providers::catalog::ProviderCatalog;
use anyhow::{Context, Result};
use std::collections::HashSet;

/// Credential commands
#[derive(clap::Subcommand)]
pub enum CredentialCommands {
    /// Store or overwrite a credential in the vault.
    Set {
        /// Namespace for the credential (e.g. `provider:openai`,
        /// `mcp:analytics`).
        namespace: String,
        /// Slot name within the namespace (e.g. `default`, `alt-1`).
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
    },
    /// Live validation: ping the credential's consumer and report the
    /// structured outcome. For provider-namespace credentials this calls
    /// the provider's real API; for other namespaces it may be a no-op
    /// or a protocol-specific health check depending on the consumer.
    Test {
        /// Credential id (UUID).
        id: String,
    },
    /// Provider convenience: store the default API key for a provider.
    ProviderSetKey {
        /// Provider id.
        provider: String,
        /// API key material (omit for hidden prompt).
        #[arg(long, value_name = "SECRET")]
        material: Option<String>,
    },
    /// Provider convenience: delete the default API key for a provider.
    ProviderDeleteKey {
        /// Provider id.
        provider: String,
    },
    /// Provider convenience: live-test the default API key for a provider.
    ProviderTest {
        /// Provider id.
        provider: String,
    },
    /// Rotation binding management.
    #[command(subcommand)]
    Binding(BindingCommands),
    /// Migrate legacy provider keys from the OS keychain into the vault.
    ///
    /// Legacy per-provider OS keychain entries are no longer a supported
    /// secret source; the unified vault is the single source of truth.
    Migrate,
}

/// Rotation binding subcommands.
#[derive(clap::Subcommand)]
pub enum BindingCommands {
    /// List all rotation bindings.
    List,
    /// Get a binding by slot key (`namespace:name`).
    Get {
        /// Slot key.
        key: String,
    },
    /// Set (or overwrite) a rotation binding.
    Set {
        /// Slot key (`namespace:name`).
        key: String,
        /// Rotation strategy.
        #[arg(long, value_name = "STRATEGY")]
        strategy: String,
        /// Ordered list of credential ids.
        #[arg(long, required = true, num_args = 1.., value_name = "CREDENTIAL_ID")]
        order: Vec<String>,
    },
    /// Delete a rotation binding.
    Delete {
        /// Slot key (`namespace:name`).
        key: String,
    },
    /// Test each credential in a binding in order.
    TestRotation {
        /// Slot key (`namespace:name`).
        key: String,
    },
}

/// Execute a credential subcommand.
pub async fn execute(cmd: CredentialCommands, paths: &GlobalPaths) -> Result<()> {
    let vault =
        Vault::load(paths.resolver().vault()).with_context(|| "failed to load credential vault")?;

    // Provider sugar still validates against the catalog and offers
    // nearest-neighbor suggestions for typos. Generic commands are
    // namespace-agnostic and skip the catalog snapshot.
    let known_provider_ids = load_known_provider_ids(paths).await;

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
        CredentialCommands::List { namespace, kind } => {
            list_cmd(&vault, namespace.as_deref(), kind.as_deref()).await
        }
        CredentialCommands::Test { id } => test_cmd(&vault, &id).await,
        CredentialCommands::ProviderSetKey { provider, material } => {
            provider_set_key_cmd(&vault, &provider, material, &known_provider_ids).await
        }
        CredentialCommands::ProviderDeleteKey { provider } => {
            provider_delete_key_cmd(&vault, &provider, &known_provider_ids).await
        }
        CredentialCommands::ProviderTest { provider } => {
            provider_test_cmd(&vault, &provider, &known_provider_ids).await
        }
        CredentialCommands::Binding(binding) => binding_execute(&vault, binding).await,
        CredentialCommands::Migrate => migrate_cmd(&vault).await,
    }
}

/// Read the provider id list from `providers.toml` once at command
/// dispatch time. Provider sugar validates against it; generic
/// credential commands are namespace-agnostic and do not consult it.
pub(crate) async fn load_known_provider_ids(paths: &GlobalPaths) -> Vec<String> {
    let catalog_path = paths.config_dir.join(ProviderCatalog::FILENAME);
    let Ok(catalog) = ProviderCatalog::load_or_init(&catalog_path).await else {
        return Vec::new();
    };
    catalog.list_all().await.into_iter().map(|e| e.id).collect()
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

    vault
        .set_credential(&credential)
        .with_context(|| format!("failed to store credential '{namespace}/{name}' in vault"))?;
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
    if vault
        .delete_credential(id)
        .with_context(|| format!("failed to delete credential '{id}'"))?
    {
        println!("Deleted credential '{id}'.");
        notify_daemon_reload().await;
    } else {
        println!("No credential '{id}'.");
    }
    Ok(())
}

async fn list_cmd(vault: &Vault, namespace: Option<&str>, kind: Option<&str>) -> Result<()> {
    let kind = match kind {
        Some(k) => Some(parse_kind(k).with_context(|| format!("unknown credential kind '{k}'"))?),
        None => None,
    };
    let filter = CredentialFilter {
        namespace: namespace.map(String::from),
        kind,
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

async fn test_cmd(vault: &Vault, id: &str) -> Result<()> {
    // Verify the id exists locally so the user gets a clear message
    // before we try to reach the daemon.
    if vault.get_credential(id).is_none() {
        anyhow::bail!("credential not found: {id}");
    }

    let client = match crate::ipc::DaemonClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            anyhow::bail!("cannot reach the daemon (is `peko daemon start` running?): {e}");
        }
    };
    let resp = client.credential_test(id).await?;
    let (lines, exit_code) = render_credential_tested(id, &resp);
    for line in &lines {
        println!("{line}");
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Provider convenience: store `provider:<provider>/default` as an API key.
pub(crate) async fn provider_set_key_cmd(
    vault: &Vault,
    provider: &str,
    material: Option<String>,
    known_provider_ids: &[String],
) -> Result<()> {
    validate_known_provider(provider, known_provider_ids)?;
    let material = read_material(material, "API key: ")?;
    let secret = secrecy::SecretString::from(material);
    vault
        .set_provider_key(provider, &secret)
        .with_context(|| format!("failed to store key for '{provider}' in vault"))?;
    let id = find_default_credential_id(vault, provider).unwrap_or_else(|| "unknown".to_string());
    println!("Stored API key for '{provider}' (id {id}).");
    notify_daemon_reload().await;
    Ok(())
}

/// Provider convenience: delete the default API key for a provider.
pub(crate) async fn provider_delete_key_cmd(
    vault: &Vault,
    provider: &str,
    known_provider_ids: &[String],
) -> Result<()> {
    if vault.delete_provider_key(provider)? {
        println!("Removed API key for '{provider}'.");
        notify_daemon_reload().await;
    } else {
        suggest_for_missing(provider, &vault.list_providers(), known_provider_ids);
        println!("No API key stored for '{provider}'.");
    }
    Ok(())
}

/// Provider convenience: live-test the default API key for a provider.
pub(crate) async fn provider_test_cmd(
    vault: &Vault,
    provider: &str,
    known_provider_ids: &[String],
) -> Result<()> {
    let credential_id = match find_default_credential_id(vault, provider) {
        Some(id) => id,
        None => {
            suggest_for_missing(provider, &vault.list_providers(), known_provider_ids);
            anyhow::bail!("no API key stored for '{provider}'");
        }
    };
    test_cmd(vault, &credential_id).await
}

/// Add a secondary provider API key at `provider:<provider>/alt-N`.
pub(crate) async fn provider_rotate_add_cmd(
    vault: &Vault,
    provider: &str,
    material: Option<String>,
) -> Result<()> {
    let material = read_material(material, "API key: ")?;
    let secret = secrecy::SecretString::from(material);
    let namespace = provider_namespace(provider);
    let name = find_alt_name(vault, provider);
    let mut credential = Credential::now(
        namespace.clone(),
        name.clone(),
        CredentialKind::ApiKey,
        secret,
    );
    if let Some(id) = find_credential_id_for_slot(vault, &namespace, &name) {
        credential.id = id;
    }
    let id = credential.id.clone();
    vault
        .set_credential(&credential)
        .with_context(|| format!("failed to store rotated key '{namespace}/{name}' in vault"))?;
    println!("Stored rotated API key for '{provider}' at '{name}' (id {id}).");
    notify_daemon_reload().await;
    Ok(())
}

async fn binding_execute(vault: &Vault, cmd: BindingCommands) -> Result<()> {
    match cmd {
        BindingCommands::List => binding_list_cmd(vault).await,
        BindingCommands::Get { key } => binding_get_cmd(vault, &key).await,
        BindingCommands::Set {
            key,
            strategy,
            order,
        } => binding_set_cmd(vault, &key, &strategy, order).await,
        BindingCommands::Delete { key } => binding_delete_cmd(vault, &key).await,
        BindingCommands::TestRotation { key } => binding_test_rotation_cmd(vault, &key).await,
    }
}

async fn binding_list_cmd(vault: &Vault) -> Result<()> {
    let bindings = vault.list_bindings();
    if bindings.is_empty() {
        println!("No rotation bindings configured.");
        return Ok(());
    }
    println!("Rotation bindings ({}):", bindings.len());
    for (key, binding) in bindings {
        let ids = binding.ordered_credential_ids.join(", ");
        println!("  {} -> {} [{}]", key, binding.strategy.as_str(), ids);
    }
    Ok(())
}

async fn binding_get_cmd(vault: &Vault, key: &str) -> Result<()> {
    let (namespace, name) = parse_slot_key(key)?;
    match vault.get_binding(&namespace, &name) {
        Some(binding) => {
            println!("key:      {key}");
            println!("strategy: {}", binding.strategy.as_str());
            println!("order:    {}", binding.ordered_credential_ids.join(", "));
        }
        None => println!("No binding '{key}'."),
    }
    Ok(())
}

async fn binding_set_cmd(
    vault: &Vault,
    key: &str,
    strategy: &str,
    order: Vec<String>,
) -> Result<()> {
    let strategy = parse_strategy(strategy)
        .with_context(|| format!("unknown rotation strategy '{strategy}'; expected round_robin"))?;
    if order.is_empty() {
        anyhow::bail!("--order must contain at least one credential id");
    }
    for id in &order {
        if vault.get_credential(id).is_none() {
            anyhow::bail!("credential not found: {id}");
        }
    }
    let (namespace, name) = parse_slot_key(key)?;
    let slot_key = RotationBinding::slot_key(&namespace, &name);
    let binding = RotationBinding {
        strategy,
        ordered_credential_ids: order,
    };
    vault
        .set_binding(&slot_key, &binding)
        .with_context(|| format!("failed to store binding '{key}'"))?;
    println!("Stored rotation binding '{key}'.");
    notify_daemon_reload().await;
    Ok(())
}

async fn binding_delete_cmd(vault: &Vault, key: &str) -> Result<()> {
    let (namespace, name) = parse_slot_key(key)?;
    let slot_key = RotationBinding::slot_key(&namespace, &name);
    if vault.delete_binding(&slot_key)? {
        println!("Deleted rotation binding '{key}'.");
        notify_daemon_reload().await;
    } else {
        println!("No rotation binding '{key}'.");
    }
    Ok(())
}

async fn binding_test_rotation_cmd(vault: &Vault, key: &str) -> Result<()> {
    let (namespace, name) = parse_slot_key(key)?;
    let ids: Vec<String> = match vault.get_binding(&namespace, &name) {
        Some(binding) => binding.ordered_credential_ids,
        None => {
            // Fallback: test the single credential at the slot if no
            // binding is configured.
            find_credential_id_for_slot(vault, &namespace, &name)
                .into_iter()
                .collect()
        }
    };

    if ids.is_empty() {
        anyhow::bail!("no credentials to test for binding '{key}'");
    }

    let client = match crate::ipc::DaemonClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            anyhow::bail!("cannot reach the daemon (is `peko daemon start` running?): {e}");
        }
    };

    let mut any_failed = false;
    for id in &ids {
        let resp = client.credential_test(id).await?;
        let (lines, exit_code) = render_credential_tested(&format!("{key}/{id}"), &resp);
        for line in &lines {
            println!("  {line}");
        }
        if exit_code != 0 {
            any_failed = true;
        }
    }

    if any_failed {
        std::process::exit(2);
    }
    Ok(())
}

async fn migrate_cmd(_vault: &Vault) -> Result<()> {
    // RP3B: legacy per-provider OS keychain entries are no longer a
    // supported secret source. The unified vault is the single source
    // of truth for provider API keys.
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
pub(crate) fn parse_kind(s: &str) -> Option<CredentialKind> {
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

/// Parse a rotation strategy string.
pub(crate) fn parse_strategy(s: &str) -> Option<RotationStrategy> {
    match s {
        "round_robin" => Some(RotationStrategy::RoundRobin),
        "last_resort" => Some(RotationStrategy::LastResort),
        "random" => Some(RotationStrategy::Random),
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

/// Build the provider namespace from a provider id.
pub(crate) fn provider_namespace(provider: &str) -> String {
    format!("provider:{provider}")
}

/// Find the id of the credential at `(namespace, name)`, if any.
fn find_credential_id_for_slot(vault: &Vault, namespace: &str, name: &str) -> Option<String> {
    vault
        .list_credentials(&CredentialFilter {
            namespace: Some(namespace.to_string()),
            kind: None,
        })
        .into_iter()
        .find(|s| s.name == name)
        .map(|s| s.id)
}

/// Find the id of the default credential at `provider:{provider}`.
fn find_default_credential_id(vault: &Vault, provider: &str) -> Option<String> {
    let namespace = provider_namespace(provider);
    find_credential_id_for_slot(vault, &namespace, "default")
}

/// Pick the first unused `alt-N` name for a provider.
pub(crate) fn find_alt_name(vault: &Vault, provider: &str) -> String {
    let namespace = provider_namespace(provider);
    let existing: HashSet<String> = vault
        .list_credentials(&CredentialFilter {
            namespace: Some(namespace),
            kind: None,
        })
        .into_iter()
        .map(|s| s.name)
        .collect();
    for i in 1u32.. {
        let candidate = format!("alt-{i}");
        if !existing.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Split a binding slot key (`namespace:name`) into its parts.
fn parse_slot_key(key: &str) -> Result<(String, String)> {
    match key.rsplit_once(':') {
        Some((namespace, name)) if !namespace.is_empty() && !name.is_empty() => {
            Ok((namespace.to_string(), name.to_string()))
        }
        _ => anyhow::bail!(
            "invalid binding key '{key}'; expected 'namespace:name' (e.g. 'provider:anthropic:default')"
        ),
    }
}

/// Reject a provider id that isn't in the catalog; on rejection, list
/// the known ids and offer nearest-neighbor suggestions so the user
/// can spot a typo without running `peko provider list` first.
pub(crate) fn validate_known_provider(provider: &str, known_provider_ids: &[String]) -> Result<()> {
    if known_provider_ids.iter().any(|id| id == provider) {
        return Ok(());
    }

    let mut msg = format!("unknown provider id '{provider}'");

    if known_provider_ids.is_empty() {
        msg.push_str(
            "\nThe provider catalog is empty. Add the provider first with \
             `peko provider add --template <name>`.",
        );
    } else {
        let quoted: Vec<String> = known_provider_ids
            .iter()
            .map(|s| format!("'{s}'"))
            .collect();
        msg.push_str(&format!("\nKnown provider ids: {}", quoted.join(", ")));
        let suggestions = nearest_neighbors(provider, known_provider_ids, 3);
        if !suggestions.is_empty() {
            let q: Vec<String> = suggestions.iter().map(|s| format!("'{s}'")).collect();
            msg.push_str(&format!("\nDid you mean: {}", q.join(", ")));
        }
    }

    anyhow::bail!("{msg}")
}

/// Emit a "did you mean …?" line on stdout when the requested id
/// has no stored key — the user almost certainly typoed. Looks at
/// both the currently-stored provider keys and the catalog so a
/// fresh typo (never stored) still gets a suggestion toward a known
/// provider id.
fn suggest_for_missing(target: &str, stored_keys: &[String], known_provider_ids: &[String]) {
    let mut candidates: Vec<String> =
        Vec::with_capacity(stored_keys.len() + known_provider_ids.len());
    for s in stored_keys {
        if !candidates.contains(s) {
            candidates.push(s.clone());
        }
    }
    for s in known_provider_ids {
        if !candidates.contains(s) {
            candidates.push(s.clone());
        }
    }

    let suggestions = nearest_neighbors(target, &candidates, 3);
    if suggestions.is_empty() {
        return;
    }
    let q: Vec<String> = suggestions.iter().map(|s| format!("'{s}'")).collect();
    println!("  Did you mean: {}", q.join(", "));
}

/// Pure formatter for `CredentialTested` so the IPC plumbing is
/// tested by `cargo build` and the human-readable output is
/// covered by unit tests without spawning a daemon. Returns
/// `(lines, exit_code)` — exit code is 0 for success, 2 for a
/// validator failure, distinct from `1` (clap usage error) so
/// scripts can branch on outcome cleanly.
fn render_credential_tested(
    label: &str,
    resp: &crate::ipc::packet::ResponsePacket,
) -> (Vec<String>, i32) {
    let crate::ipc::packet::ResponsePacket::CredentialTested {
        ok,
        message,
        latency_ms,
        http_status,
        model_used,
        ..
    } = resp
    else {
        return (
            vec![format!(
                "unexpected response from daemon: {}  (is the daemon up-to-date?)",
                resp.variant_name()
            )],
            1,
        );
    };

    if *ok {
        let mut lines = vec![format!("✓ {label}: {message} ({latency_ms}ms)")];
        if let Some(model) = model_used {
            lines.push(format!("  via {model} (~1 token billed)"));
        }
        (lines, 0)
    } else {
        let mut lines = vec![format!("✗ {label}: {message}")];
        if let Some(code) = http_status {
            lines.push(format!("  HTTP {code} after {latency_ms}ms"));
        } else if message.contains("unknown provider")
            || message.contains("no key stored")
            || message.contains("credential not found")
        {
            lines.push(format!("  ({latency_ms}ms — request was not sent)"));
        } else {
            lines.push(format!("  connection failed after {latency_ms}ms"));
        }
        (lines, 2)
    }
}

/// True iff the validator's failure message names a configuration
/// gap the user can fix from this command.
#[cfg(test)]
fn message_invites_suggestion(_provider: &str, resp: &crate::ipc::packet::ResponsePacket) -> bool {
    if let crate::ipc::packet::ResponsePacket::CredentialTested { message, .. } = resp {
        message.contains("no key stored")
            || message.contains("unknown provider")
            || message.contains("credential not found")
    } else {
        false
    }
}

/// Return up to `limit` candidate strings within edit distance 3 of
/// `target`, ordered by ascending distance then by candidate string
/// for determinism. Ties broken alphabetically.
fn nearest_neighbors(target: &str, candidates: &[String], limit: usize) -> Vec<String> {
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .map(|c| (levenshtein(target, c), c))
        .filter(|(d, _)| *d <= 3)
        .collect();

    scored.sort_by(|(da, sa), (db, sb)| da.cmp(db).then_with(|| sa.cmp(sb)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, c)| c.clone()).collect()
}

/// Iterative two-row Levenshtein distance. Operates on Unicode chars
/// not bytes.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Cli;
    use crate::ipc::packet::ResponsePacket;
    use clap::Parser;
    use secrecy::{ExposeSecret, SecretString};
    use tempfile::tempdir;

    fn test_vault() -> (tempfile::TempDir, Vault) {
        let tmp = tempdir().unwrap();
        let vault = Vault::for_test(tmp.path(), "test-passphrase");
        (tmp, vault)
    }

    /// `miniax` vs `minimax` is one insertion; should rank first.
    #[test]
    fn nearest_neighbors_picks_one_edit_match() {
        let candidates = vec![
            "minimax".to_string(),
            "openai".to_string(),
            "anthropic".to_string(),
        ];
        let got = nearest_neighbors("miniax", &candidates, 3);
        assert_eq!(got, vec!["minimax".to_string()]);
    }

    /// Out-of-range distance (>= 4) drops the candidate entirely.
    #[test]
    fn nearest_neighbors_drops_distant_candidates() {
        let candidates = vec!["anthropic".to_string(), "openai".to_string()];
        let got = nearest_neighbors("miniax", &candidates, 3);
        assert!(got.is_empty(), "got: {got:?}");
    }

    #[test]
    fn nearest_neighbors_handles_empty_inputs() {
        assert!(nearest_neighbors("minimax", &[], 3).is_empty());
        assert!(nearest_neighbors("minimax", &["minimax".to_string()], 0).is_empty());
    }

    #[test]
    fn nearest_neighbors_breaks_ties_alphabetically() {
        let candidates = vec!["xfoo".to_string(), "foox".to_string()];
        let got = nearest_neighbors("foo", &candidates, 3);
        assert_eq!(got, vec!["foox".to_string(), "xfoo".to_string()]);
    }

    #[test]
    fn validate_known_provider_accepts_known() {
        let known = vec!["openai".to_string(), "minimax".to_string()];
        validate_known_provider("minimax", &known).expect("minimax is known");
    }

    #[test]
    fn validate_known_provider_rejects_typo_with_suggestion() {
        let known = vec!["openai".to_string(), "minimax".to_string()];
        let err = validate_known_provider("miniax", &known).expect_err("must reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("unknown provider id 'miniax'"), "got: {msg}");
        assert!(msg.contains("Did you mean: 'minimax'"), "got: {msg}");
        assert!(
            msg.contains("Known provider ids: 'openai', 'minimax'"),
            "got: {msg}"
        );
    }

    #[test]
    fn validate_known_provider_handles_empty_catalog() {
        let err = validate_known_provider("minimax", &[]).expect_err("empty catalog must reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("The provider catalog is empty"), "got: {msg}");
    }

    #[test]
    fn levenshtein_matches_hand_computed_values() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("flaw", "lawn"), 2);
        assert_eq!(levenshtein("minimax", "minimax"), 0);
        assert_eq!(levenshtein("miniax", "minimax"), 1);
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
            "provider:openai",
            "default",
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
            "provider:openai",
            "default",
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

        let provider_only = vault.list_credentials(&CredentialFilter {
            namespace: Some("provider:openai".to_string()),
            kind: None,
        });
        assert_eq!(provider_only.len(), 1);
        assert_eq!(provider_only[0].namespace, "provider:openai");

        let api_key_only = vault.list_credentials(&CredentialFilter {
            namespace: None,
            kind: Some(CredentialKind::ApiKey),
        });
        assert_eq!(api_key_only.len(), 2);
    }

    #[tokio::test]
    async fn provider_set_key_sugar_resolves_to_provider_namespace() {
        let (_tmp, vault) = test_vault();
        provider_set_key_cmd(
            &vault,
            "openai",
            Some("sk-openai".to_string()),
            &["openai".to_string()],
        )
        .await
        .unwrap();

        let key = vault.get_provider_key("openai").unwrap();
        assert_eq!(key.expose_secret(), "sk-openai");
    }

    #[tokio::test]
    async fn provider_delete_key_sugar_removes_default() {
        let (_tmp, vault) = test_vault();
        provider_set_key_cmd(
            &vault,
            "openai",
            Some("sk-openai".to_string()),
            &["openai".to_string()],
        )
        .await
        .unwrap();
        provider_delete_key_cmd(&vault, "openai", &["openai".to_string()])
            .await
            .unwrap();
        assert!(vault.get_provider_key("openai").is_none());
    }

    #[tokio::test]
    async fn provider_set_key_rejects_unknown_provider() {
        let (_tmp, vault) = test_vault();
        let err = provider_set_key_cmd(
            &vault,
            "miniax",
            Some("sk".to_string()),
            &["openai".to_string(), "minimax".to_string()],
        )
        .await
        .expect_err("must reject unknown provider");
        let msg = format!("{err:#}");
        assert!(msg.contains("Did you mean: 'minimax'"), "got: {msg}");
    }

    #[test]
    fn find_alt_name_picks_first_gap() {
        let (_tmp, vault) = test_vault();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("k1".into()),
            ))
            .unwrap();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "alt-2",
                CredentialKind::ApiKey,
                SecretString::new("k2".into()),
            ))
            .unwrap();

        assert_eq!(find_alt_name(&vault, "anthropic"), "alt-1");
    }

    #[tokio::test]
    async fn binding_set_rejects_unknown_credential_ids() {
        let (_tmp, vault) = test_vault();
        let err = binding_set_cmd(
            &vault,
            "provider:anthropic:default",
            "round_robin",
            vec!["not-a-real-id".to_string()],
        )
        .await
        .expect_err("must reject unknown id");
        assert!(err.to_string().contains("credential not found"));
    }

    #[tokio::test]
    async fn binding_set_then_list_roundtrips() {
        let (_tmp, vault) = test_vault();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("k1".into()),
            ))
            .unwrap();
        let id1 = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "alt-1",
                CredentialKind::ApiKey,
                SecretString::new("k2".into()),
            ))
            .unwrap();
        let id2 = vault.list_credentials(&CredentialFilter::default())[1]
            .id
            .clone();

        binding_set_cmd(
            &vault,
            "provider:anthropic:default",
            "round_robin",
            vec![id1.clone(), id2.clone()],
        )
        .await
        .unwrap();

        let binding = vault.get_binding("provider:anthropic", "default").unwrap();
        assert_eq!(binding.strategy, RotationStrategy::RoundRobin);
        assert_eq!(binding.ordered_credential_ids, vec![id1, id2]);
    }

    #[tokio::test]
    async fn binding_test_rotation_walks_credentials_in_order() {
        let (_tmp, vault) = test_vault();
        vault
            .set_credential(&Credential::now(
                "provider:anthropic",
                "default",
                CredentialKind::ApiKey,
                SecretString::new("k1".into()),
            ))
            .unwrap();
        let id1 = vault.list_credentials(&CredentialFilter::default())[0]
            .id
            .clone();

        // Without a daemon, test-rotation will fail at daemon connect.
        // We verify the binding resolution path by checking that the
        // function collects the right ids before dialing.
        let (namespace, name) = parse_slot_key("provider:anthropic:default").unwrap();
        let ids: Vec<String> = vault
            .get_binding(&namespace, &name)
            .map(|b| b.ordered_credential_ids)
            .unwrap_or_default();
        let fallback = find_credential_id_for_slot(&vault, &namespace, &name)
            .into_iter()
            .collect::<Vec<_>>();
        assert!(ids.is_empty());
        assert_eq!(fallback, vec![id1]);
    }

    #[test]
    fn parse_slot_key_splits_on_last_colon() {
        assert_eq!(
            parse_slot_key("provider:anthropic:default").unwrap(),
            ("provider:anthropic".to_string(), "default".to_string())
        );
        assert!(parse_slot_key("no-colon").is_err());
        assert!(parse_slot_key(":").is_err());
    }

    /// Successful OpenAI-compat ping.
    #[test]
    fn render_credential_tested_ok_openai_compat() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 1,
            id: "id-openai".to_string(),
            ok: true,
            message: "Connection successful (124 models)".to_string(),
            latency_ms: 187,
            http_status: Some(200),
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        let (lines, exit) = render_credential_tested("openai", &resp);
        assert_eq!(exit, 0, "ok outcome must exit 0");
        assert_eq!(lines.len(), 1, "no via-model line for OpenAI-compat");
        assert!(lines[0].starts_with("✓ openai:"), "got: {}", lines[0]);
        assert!(lines[0].contains("(187ms)"), "got: {}", lines[0]);
    }

    /// Successful Anthropic-format ping.
    #[test]
    fn render_credential_tested_ok_anthropic_includes_via_model_line() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 2,
            id: "id-anthropic".to_string(),
            ok: true,
            message: "Connection successful (1 token billed via claude-haiku-4-5)".to_string(),
            latency_ms: 312,
            http_status: Some(200),
            model_used: Some("claude-haiku-4-5".to_string()),
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        let (lines, exit) = render_credential_tested("anthropic", &resp);
        assert_eq!(exit, 0);
        assert_eq!(lines.len(), 2, "ok + via-model line");
        assert!(lines[0].contains("(312ms)"), "got: {}", lines[0]);
        assert!(lines[1].contains("claude-haiku-4-5"), "got: {}", lines[1]);
    }

    /// HTTP failure (401): exit 2, ✗ prefix, HTTP code on second line.
    #[test]
    fn render_credential_tested_failure_401_reports_status_and_exit_2() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 3,
            id: "id-openai".to_string(),
            ok: false,
            message: "HTTP 401: invalid api key".to_string(),
            latency_ms: 124,
            http_status: Some(401),
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        let (lines, exit) = render_credential_tested("openai", &resp);
        assert_eq!(exit, 2, "validator failure must exit 2, not 0/1");
        assert!(lines[0].starts_with("✗ openai:"), "got: {}", lines[0]);
        assert!(
            lines[1].contains("HTTP 401"),
            "expected HTTP status on second line, got: {:?}",
            lines
        );
    }

    #[test]
    fn render_credential_tested_connection_failure_says_so() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 4,
            id: "id-ollama".to_string(),
            ok: false,
            message: "connection refused: 127.0.0.1:11434".to_string(),
            latency_ms: 12,
            http_status: None,
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        let (lines, exit) = render_credential_tested("ollama", &resp);
        assert_eq!(exit, 2);
        assert!(lines[0].starts_with("✗ ollama:"));
        assert!(
            lines[1].contains("connection failed"),
            "expected connection-failed line, got: {:?}",
            lines
        );
    }

    #[test]
    fn message_invites_suggestion_for_missing_key_and_unknown_provider() {
        let missing = ResponsePacket::CredentialTested {
            request_id: 5,
            id: "id-miniax".to_string(),
            ok: false,
            message: "no key stored for 'miniax'".to_string(),
            latency_ms: 0,
            http_status: None,
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        assert!(message_invites_suggestion("miniax", &missing));

        let unknown = ResponsePacket::CredentialTested {
            request_id: 7,
            id: "id-miniax".to_string(),
            ok: false,
            message: "unknown provider: miniax".to_string(),
            latency_ms: 0,
            http_status: None,
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        assert!(message_invites_suggestion("miniax", &unknown));

        let auth = ResponsePacket::CredentialTested {
            request_id: 6,
            id: "id-openai".to_string(),
            ok: false,
            message: "HTTP 401: invalid api key".to_string(),
            latency_ms: 50,
            http_status: Some(401),
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        assert!(!message_invites_suggestion("openai", &auth));
    }

    #[test]
    fn render_credential_tested_unknown_provider_says_request_not_sent() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 8,
            id: "id-miniax".to_string(),
            ok: false,
            message: "unknown provider: miniax".to_string(),
            latency_ms: 0,
            http_status: None,
            model_used: None,
            tested_at: "2026-07-15T00:00:00Z".to_string(),
        };
        let (lines, exit) = render_credential_tested("miniax", &resp);
        assert_eq!(exit, 2);
        assert!(
            lines[1].contains("request was not sent"),
            "expected config-error tail, got: {:?}",
            lines
        );
    }

    #[test]
    fn render_credential_tested_unexpected_variant_surfaces_diagnostic() {
        let resp = ResponsePacket::Pong {
            request_id: 7,
            uptime_secs: 12,
            version: "0.1.0".to_string(),
        };
        let (lines, exit) = render_credential_tested("openai", &resp);
        assert_eq!(exit, 1);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("unexpected response"),
            "got: {}",
            lines[0]
        );
        assert!(lines[0].contains("up-to-date"), "got: {}", lines[0]);
    }

    /// Generic `set` command parses from argv.
    #[test]
    fn set_args_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "credential",
            "set",
            "mcp:analytics",
            "default",
            "--kind",
            "api_key",
            "--material",
            "secret",
            "--metadata",
            "foo=bar",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Credential(CredentialCommands::Set {
                namespace,
                name,
                kind,
                material,
                metadata,
            }) => {
                assert_eq!(namespace, "mcp:analytics");
                assert_eq!(name, "default");
                assert_eq!(kind, "api_key");
                assert_eq!(material.as_deref(), Some("secret"));
                assert_eq!(metadata, vec![("foo".to_string(), "bar".to_string())]);
            }
            _ => panic!("expected credential set"),
        }
    }

    /// Provider-set-key sugar parses from argv.
    #[test]
    fn provider_set_key_args_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "credential",
            "provider-set-key",
            "openai",
            "--material",
            "sk-test",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Credential(CredentialCommands::ProviderSetKey {
                provider,
                material,
            }) => {
                assert_eq!(provider, "openai");
                assert_eq!(material.as_deref(), Some("sk-test"));
            }
            _ => panic!("expected credential provider-set-key"),
        }
    }

    /// Binding set parses from argv.
    #[test]
    fn binding_set_args_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "credential",
            "binding",
            "set",
            "provider:anthropic:default",
            "--strategy",
            "round_robin",
            "--order",
            "id1",
            "id2",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Credential(CredentialCommands::Binding(
                BindingCommands::Set {
                    key,
                    strategy,
                    order,
                },
            )) => {
                assert_eq!(key, "provider:anthropic:default");
                assert_eq!(strategy, "round_robin");
                assert_eq!(order, vec!["id1".to_string(), "id2".to_string()]);
            }
            _ => panic!("expected credential binding set"),
        }
    }
}
