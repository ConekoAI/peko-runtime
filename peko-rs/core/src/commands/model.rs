//! Model management commands.
//!
//! These commands operate on the runtime-owned model catalog
//! (`~/.peko/models.toml`). The catalog is model-first: each entry
//! bundles endpoint info (base URL, API format), the wire model id,
//! context-window metadata, and an optional `credential_id` pointing
//! into the vault. There is no separate provider layer.
//!
//! Every flow is fully non-interactive: agents and humans alike drive
//! it from a shell. A typical first-time setup is one command:
//!
//! ```text
//! peko model add --template anthropic \
//!                --model claude-sonnet-4-5 \
//!                --key "$ANTHROPIC_API_KEY"
//! ```
//!
//! Custom (non-template) models are supported too:
//!
//! ```text
//! peko model add --custom --id my-llama \
//!                --api-format openai_completions \
//!                --base-url http://localhost:8080/v1 \
//!                --model llama-3.1-8b
//! ```

use crate::commands::GlobalPaths;
use crate::common::vault::{Credential, CredentialKind, Vault};
use anyhow::{Context, Result};
use peko_providers::catalog::{ApiFormat, ModelCatalog, ModelConfig};
use peko_providers::templates;

/// Vault namespace for model API keys.
const LLM_NAMESPACE: &str = "llm";

/// Model commands
#[derive(clap::Subcommand)]
pub enum ModelCommands {
    /// List all configured models in the runtime catalog.
    List {
        /// Show detailed information including base URL, wire model id,
        /// and credential wiring.
        #[arg(long)]
        detailed: bool,
    },
    /// List the built-in preset templates available with `model add`.
    Templates,
    /// Add a model to the catalog. Either `--template` or `--custom`
    /// plus the relevant flags must be supplied.
    Add(AddArgs),
    /// Remove a model from the catalog (does not delete its credential).
    Remove {
        /// Configured model id to remove.
        id: String,
    },
    /// Live-test a configured model: ping its endpoint with the stored
    /// credential and report the outcome.
    Test {
        /// Configured model id to test.
        id: String,
    },
}

/// Arguments for `peko model add`.
#[derive(clap::Args)]
pub struct AddArgs {
    /// Seed from a built-in preset template (e.g. `anthropic`,
    /// `openai`, `ollama`). Mutually exclusive with `--custom`.
    #[arg(long, conflicts_with = "custom")]
    template: Option<String>,
    /// Configured model id to use in the catalog. If omitted with
    /// `--template`, a default of `{template}-{model}` is used.
    /// Required with `--custom`.
    #[arg(long)]
    id: Option<String>,
    /// Wire model id (the id the API expects on the wire, e.g.
    /// `gpt-4o`, `claude-sonnet-4-5`). Required.
    #[arg(long, value_name = "WIRE_MODEL_ID")]
    model: Option<String>,
    /// Override the display name (otherwise the template's curated
    /// name for the wire model is used, or the configured id for
    /// `--custom`).
    #[arg(long)]
    display_name: Option<String>,
    /// Add a fully custom model (OpenAI-compatible or
    /// Anthropic-compatible endpoint).
    #[arg(long, conflicts_with = "template")]
    custom: bool,
    /// API format for a custom model.
    /// One of `openai_completions`, `anthropic_messages`.
    #[arg(long, requires = "custom")]
    api_format: Option<String>,
    /// Base URL for a custom model.
    #[arg(long, requires = "custom")]
    base_url: Option<String>,
    /// Store an API key for this model in the vault immediately and
    /// wire it as the model's `credential_id`. Mutually exclusive
    /// with `--credential-id`.
    #[arg(long, value_name = "SECRET", conflicts_with = "credential_id")]
    key: Option<String>,
    /// Reference an existing vault credential id instead of storing a
    /// new key.
    #[arg(long, value_name = "CREDENTIAL_ID")]
    credential_id: Option<String>,
    /// Context window in tokens (custom models only; template models
    /// inherit the curated value).
    #[arg(long, requires = "custom", value_name = "TOKENS")]
    context_window: Option<u32>,
    /// Max output tokens (custom models only; template models inherit
    /// the curated value).
    #[arg(long, requires = "custom", value_name = "TOKENS")]
    max_output_tokens: Option<u32>,
}

/// Execute a model subcommand.
pub async fn execute(cmd: ModelCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        ModelCommands::List { detailed } => list_cmd(paths, detailed).await,
        ModelCommands::Templates => templates_cmd().await,
        ModelCommands::Add(args) => add_cmd(args, paths).await,
        ModelCommands::Remove { id } => remove_cmd(&id, paths).await,
        ModelCommands::Test { id } => test_cmd(&id, paths).await,
    }
}

/// Tell the running daemon to re-read `models.toml` from disk so the
/// in-flight root agent sees the mutation just persisted by the caller.
/// Silent on connection failure — the daemon may not be running (cold
/// start, dev workflow), in which case the next `peko daemon start`
/// will pick up the new state from disk anyway.
async fn notify_daemon_reload() {
    let Ok(client) = crate::ipc::DaemonClient::connect().await else {
        return;
    };
    match client.reload_providers().await {
        Ok(crate::ipc::ResponsePacket::ModelReloaded {
            models_count,
            keys_count,
            ..
        }) => {
            if models_count > 0 || keys_count > 0 {
                println!("Daemon reloaded: {models_count} model(s), {keys_count} key(s).");
            }
        }
        Ok(crate::ipc::ResponsePacket::Error { message, .. }) => {
            eprintln!("Daemon reload returned error: {message}");
        }
        Ok(other) => {
            eprintln!("Daemon reload returned unexpected packet: {other:?}");
        }
        Err(e) => {
            eprintln!("Daemon reload failed: {e}");
        }
    }
}

fn catalog_path(paths: &GlobalPaths) -> std::path::PathBuf {
    paths.config_dir.join(ModelCatalog::FILENAME)
}

async fn open_catalog(paths: &GlobalPaths) -> Result<std::sync::Arc<ModelCatalog>> {
    let path = catalog_path(paths);
    ModelCatalog::load_or_init(&path).await
}

async fn list_cmd(paths: &GlobalPaths, detailed: bool) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entries = cat.list_all().await;

    if entries.is_empty() {
        println!("No models in the catalog.");
        println!("Add one with: peko model add --template <anthropic|openai|ollama|...> --model <wire-id>");
        println!(
            "Or:           peko model add --custom --id <id> --api-format <fmt> --base-url <url> --model <wire-id>"
        );
        return Ok(());
    }

    println!("Model catalog ({} entries):\n", entries.len());

    for e in &entries {
        let status = if e.enabled { "✓" } else { "✗" };
        let from_tmpl = e
            .template_id
            .as_deref()
            .map(|t| format!(" [from {t}]"))
            .unwrap_or_default();

        println!("  [{status}] {} - {}{from_tmpl}", e.id, e.display_name);

        if detailed {
            println!("      model_id:      {}", e.model_id);
            println!("      format:        {}", e.api_format);
            println!("      base_url:      {}", e.base_url);
            if let Some(ctx) = e.context_window {
                println!("      context_window: {ctx}");
            }
            if let Some(mot) = e.max_output_tokens {
                println!("      max_output_tokens: {mot}");
            }
            println!("      requires_key:  {}", e.requires_key,);
            match &e.credential_id {
                Some(cid) => println!("      credential_id: {cid}"),
                None if e.requires_key => println!(
                    "      credential_id: (none — store one with `peko credential set llm <name> --kind api_key`)"
                ),
                None => {}
            }
            if !e.headers.is_empty() {
                println!("      headers:       {} item(s)", e.headers.len());
            }
            println!();
        }
    }

    Ok(())
}

async fn templates_cmd() -> Result<()> {
    println!("Available preset templates:\n");
    for t in templates::iter_templates() {
        let n_models = t.models.len();
        println!(
            "  {:<14} {:<28} ({} model{})",
            t.id,
            t.display_name,
            n_models,
            if n_models == 1 { "" } else { "s" }
        );
        for m in t.models {
            let dn = m
                .display_name
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            println!("      - {}{dn}", m.id);
        }
    }
    println!("\nUse: peko model add --template <id> --model <wire-id>");
    Ok(())
}

async fn add_cmd(args: AddArgs, paths: &GlobalPaths) -> Result<()> {
    // Bare invocation: refuse with a clear pointer rather than launching
    // an interactive wizard. Agents must always get a deterministic,
    // scriptable surface here.
    if args.template.is_none() && !args.custom {
        anyhow::bail!(
            "either --template <id> or --custom is required.\n\
             \n\
             Quick start:\n\
               peko model add --template anthropic --model claude-sonnet-4-5 --key \"$ANTHROPIC_API_KEY\"\n\
             \n\
             List templates:\n\
               peko model templates"
        );
    }

    let model_id = args
        .model
        .clone()
        .with_context(|| "--model <wire-id> is required")?;
    if model_id.is_empty() {
        anyhow::bail!("--model must not be empty");
    }

    let cat = open_catalog(paths).await?;

    let entry = if let Some(template_id) = args.template.as_deref() {
        let tmpl = templates::find_template(template_id).with_context(|| {
            format!(
                "unknown template '{template_id}'. Run `peko model templates` to list available ones."
            )
        })?;
        let id = args
            .id
            .clone()
            .unwrap_or_else(|| format!("{}-{model_id}", tmpl.id));
        let mut entry = ModelConfig::from_template(tmpl, id, model_id);
        if let Some(dn) = args.display_name.clone() {
            entry.display_name = dn;
        }
        entry
    } else if args.custom {
        let api_format_str = args.api_format.as_deref().with_context(|| {
            "--api-format is required with --custom (openai_completions | anthropic_messages)"
        })?;
        let api_format = ApiFormat::from_wire(api_format_str)
            .with_context(|| format!("unknown --api-format '{api_format_str}'"))?;
        let base_url = args
            .base_url
            .clone()
            .with_context(|| "--base-url is required with --custom")?;
        let id = args
            .id
            .clone()
            .with_context(|| "--id is required with --custom")?;
        if id.is_empty() {
            anyhow::bail!("--id must not be empty");
        }
        ModelConfig {
            id: id.clone(),
            display_name: args.display_name.clone().unwrap_or_else(|| id.clone()),
            template_id: None,
            api_format,
            base_url,
            model_id,
            context_window: args.context_window,
            max_output_tokens: args.max_output_tokens,
            headers: Default::default(),
            credential_id: None,
            requires_key: true,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
        }
    } else {
        unreachable!("guarded by the bare-invocation check above");
    };

    // Wire the credential: either reference an existing vault credential
    // by id, or store a new API key in the vault under `llm` and point
    // the entry at it.
    let mut entry = entry;
    if let Some(cid) = args.credential_id.as_deref() {
        if cid.is_empty() {
            anyhow::bail!("--credential-id must not be empty");
        }
        let vault =
            Vault::load(paths.resolver().vault()).context("failed to load credential vault")?;
        if vault.get_credential(cid).is_none() {
            anyhow::bail!("credential not found in vault: {cid}");
        }
        entry.credential_id = Some(cid.to_string());
    } else if let Some(key) = args.key.as_deref() {
        if key.is_empty() {
            anyhow::bail!("--key must not be empty");
        }
        if !entry.requires_key {
            anyhow::bail!(
                "--key supplied but model '{}' does not require a key",
                entry.id
            );
        }
        let vault =
            Vault::load(paths.resolver().vault()).context("failed to load credential vault")?;
        let credential = Credential::now(
            LLM_NAMESPACE.to_string(),
            entry.id.clone(),
            CredentialKind::ApiKey,
            secrecy::SecretString::from(key.to_string()),
        );
        let cid = credential.id.clone();
        vault
            .set_credential(&credential)
            .with_context(|| format!("failed to store key for '{}' in vault", entry.id))?;
        entry.credential_id = Some(cid.clone());
        println!(
            "Stored API key for '{}' in the vault (credential id {cid}).",
            entry.id
        );
    }

    let requires_key = entry.requires_key;
    let has_credential = entry.credential_id.is_some();
    let entry_id = entry.id.clone();
    let entry_display = entry.display_name.clone();

    if cat.get(&entry_id).await.is_some() {
        anyhow::bail!(
            "model id '{entry_id}' already exists. Run `peko model edit {entry_id}` (not yet implemented) or `peko model remove {entry_id}` and re-add."
        );
    }
    cat.upsert(entry).await?;
    println!("Added model '{entry_id}' ({entry_display}).");

    if requires_key && !has_credential {
        println!(
            "Next: store its API key with: peko credential set llm {entry_id} --kind api_key --material \"$YOUR_KEY\"\n\
             (or re-run `peko model add` with --key to store and wire it in one step)"
        );
    }

    notify_daemon_reload().await;
    Ok(())
}

async fn remove_cmd(id: &str, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    if cat.remove(id).await? {
        println!("Removed model '{id}'.");
        notify_daemon_reload().await;
    } else {
        println!("No model '{id}' in the catalog.");
    }
    Ok(())
}

async fn test_cmd(id: &str, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let config = cat
        .get(id)
        .await
        .with_context(|| format!("model not found in catalog: {id}"))?;

    // Resolve the credential material from the vault, if the entry
    // references one.
    let api_key = match &config.credential_id {
        Some(cid) => {
            let vault =
                Vault::load(paths.resolver().vault()).context("failed to load credential vault")?;
            let credential = vault
                .get_credential(cid)
                .with_context(|| format!("credential not found in vault: {cid}"))?;
            Some(credential.material)
        }
        None => None,
    };

    let outcome = peko_providers::validator::Validator::test(&config, api_key.as_ref()).await;

    // Record the outcome on the credential so `credential list` shows
    // the last-tested marker.
    if let Some(cid) = &config.credential_id {
        if let Ok(vault) = Vault::load(paths.resolver().vault()) {
            let _ = vault.record_test(cid, outcome.ok);
        }
    }

    if outcome.ok {
        println!("✓ {id}: {} ({}ms)", outcome.message, outcome.latency_ms);
        if let Some(model) = &outcome.model_used {
            println!("  via {model} (~1 token billed)");
        }
        Ok(())
    } else {
        println!("✗ {id}: {}", outcome.message);
        if let Some(code) = outcome.http_status {
            println!("  HTTP {code} after {}ms", outcome.latency_ms);
        } else {
            println!("  ({}ms)", outcome.latency_ms);
        }
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::Cli;
    use clap::Parser;

    /// Build a `GlobalPaths` rooted at a fresh tempdir, with a
    /// `PEKO_MASTER_PASSPHRASE` set so the vault can be written.
    fn fresh_paths() -> GlobalPaths {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let temp = std::env::temp_dir().join(format!(
            "PEKO_model_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        std::env::set_var("PEKO_MASTER_PASSPHRASE", "test-model-cmd");
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            temp.join("config").to_str().unwrap(),
            "--data-dir",
            temp.join("data").to_str().unwrap(),
            "--cache-dir",
            temp.join("cache").to_str().unwrap(),
            "model",
            "list",
        ]);
        GlobalPaths::from_cli(&cli)
    }

    /// `peko model add` with no flags must NOT launch an interactive
    /// flow — agents have to be able to detect "no input" and recover.
    #[test]
    fn add_args_bare_invocation_has_no_template_or_custom() {
        let cli = Cli::try_parse_from(["peko", "model", "add"]).unwrap();
        match cli.command {
            crate::commands::Commands::Model(ModelCommands::Add(args)) => {
                assert!(args.template.is_none());
                assert!(!args.custom);
                assert!(args.key.is_none());
                assert!(args.credential_id.is_none());
            }
            _ => panic!("expected model add"),
        }
    }

    /// Template + key flags parse.
    #[test]
    fn add_args_template_flags_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "model",
            "add",
            "--template",
            "anthropic",
            "--model",
            "claude-3-5-haiku-latest",
            "--key",
            "sk-test",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Model(ModelCommands::Add(args)) => {
                assert_eq!(args.template.as_deref(), Some("anthropic"));
                assert_eq!(args.model.as_deref(), Some("claude-3-5-haiku-latest"));
                assert_eq!(args.key.as_deref(), Some("sk-test"));
            }
            _ => panic!("expected model add"),
        }
    }

    /// `--key` and `--credential-id` conflict (clap guard).
    #[test]
    fn key_and_credential_id_conflict() {
        let result = Cli::try_parse_from([
            "peko",
            "model",
            "add",
            "--template",
            "anthropic",
            "--model",
            "claude-3-5-haiku-latest",
            "--key",
            "sk-test",
            "--credential-id",
            "some-uuid",
        ]);
        assert!(
            result.is_err(),
            "expected clap to reject --key with --credential-id"
        );
    }

    /// `peko model add --custom ...` parses the full custom-flag set.
    #[test]
    fn add_args_custom_flags_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "model",
            "add",
            "--custom",
            "--id",
            "my-llama",
            "--api-format",
            "openai_completions",
            "--base-url",
            "http://localhost:8080/v1",
            "--model",
            "llama-3.1-8b",
            "--context-window",
            "8192",
            "--max-output-tokens",
            "1024",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Model(ModelCommands::Add(args)) => {
                assert!(args.custom);
                assert_eq!(args.id.as_deref(), Some("my-llama"));
                assert_eq!(args.api_format.as_deref(), Some("openai_completions"));
                assert_eq!(args.base_url.as_deref(), Some("http://localhost:8080/v1"));
                assert_eq!(args.model.as_deref(), Some("llama-3.1-8b"));
                assert_eq!(args.context_window, Some(8192));
                assert_eq!(args.max_output_tokens, Some(1024));
            }
            _ => panic!("expected model add"),
        }
    }

    /// End-to-end: one command adds an entry and stores the key in the
    /// vault, wiring `credential_id` on the entry.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn one_shot_add_writes_catalog_and_vault() {
        use crate::common::vault::Vault;
        use peko_providers::catalog::ModelCatalog;
        use secrecy::{ExposeSecret, SecretString};

        let paths = fresh_paths();

        let args = AddArgs {
            template: Some("anthropic".into()),
            id: None,
            model: Some("claude-3-5-haiku-latest".into()),
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            key: Some("sk-ant-test-key".into()),
            credential_id: None,
            context_window: None,
            max_output_tokens: None,
        };
        add_cmd(args, &paths)
            .await
            .expect("one-shot add should succeed");

        // 1. Catalog entry exists with a wired credential_id.
        let cat = ModelCatalog::load_or_init(&paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .unwrap();
        let entry = cat
            .get("anthropic-claude-3-5-haiku-latest")
            .await
            .expect("entry should exist");
        assert!(entry.requires_key);
        let cid = entry
            .credential_id
            .clone()
            .expect("credential_id should be set");

        // 2. Key landed in the vault under the `llm` namespace.
        let passphrase = SecretString::new("test-model-cmd".to_string().into());
        let vault = Vault::load_with_passphrase(paths.resolver().vault(), &passphrase).unwrap();
        let stored = vault.get_credential(&cid).expect("credential should exist");
        assert_eq!(stored.namespace, "llm");
        assert_eq!(stored.material.expose_secret(), "sk-ant-test-key");
    }

    /// Bare `peko model add` (no template, no custom) errors with a
    /// pointer at the right invocation.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn bare_add_errors_with_actionable_hint() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: None,
            id: None,
            model: None,
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            key: None,
            credential_id: None,
            context_window: None,
            max_output_tokens: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--template") && msg.contains("--key"),
            "expected pointer at the scriptable flags, got: {msg}"
        );
    }

    /// `--key` against a key-less model (e.g. ollama) errors so the
    /// user doesn't silently drop a key they're trying to set.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn key_flag_rejects_keyless_model() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: Some("ollama".into()),
            id: None,
            model: Some("llama3.1".into()),
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            key: Some("ignored".into()),
            credential_id: None,
            context_window: None,
            max_output_tokens: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        assert!(
            err.to_string().contains("does not require a key"),
            "expected key-less rejection, got: {err}"
        );
    }

    /// `--credential-id` referencing a missing vault credential errors.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn credential_id_must_exist_in_vault() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: Some("anthropic".into()),
            id: None,
            model: Some("claude-3-5-haiku-latest".into()),
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            key: None,
            credential_id: Some("no-such-credential".into()),
            context_window: None,
            max_output_tokens: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        assert!(
            err.to_string().contains("credential not found"),
            "expected missing-credential rejection, got: {err}"
        );
    }
}
