//! Provider management commands.
//!
//! These commands operate on the runtime-owned provider catalog
//! (`~/.peko/providers.toml`). They replace the previous design where
//! providers were declared as static code and provider selection was a
//! per-agent field.
//!
//! Every flow is fully non-interactive: agents and humans alike drive
//! it from a shell. A typical first-time setup is one command:
//!
//! ```text
//! peko provider add --template anthropic \
//!                   --key "$ANTHROPIC_API_KEY" \
//!                   --default \
//!                   --model claude-sonnet-4-5
//! ```
//!
//! The same effect is also achievable as three separate calls (useful
//! when the API key lives in a secret manager and is fetched at deploy
//! time):
//!
//! ```text
//! peko provider add --template anthropic
//! peko credential set anthropic
//! peko provider set-default anthropic --model claude-sonnet-4-5
//! ```
//!
//! Custom (non-template) providers are supported too:
//!
//! ```text
//! peko provider add --custom --name my-llama \
//!                   --api-format openai_completions \
//!                   --base-url http://localhost:8080/v1 \
//!                   --model llama-3.1-8b
//! ```

use crate::commands::GlobalPaths;
use crate::providers::catalog::{ApiFormat, ModelInfo, ProviderCatalog, ProviderCatalogEntry};
use crate::providers::templates;
use anyhow::{Context, Result};

/// Provider commands
#[derive(clap::Subcommand)]
pub enum ProviderCommands {
    /// List all providers in the runtime catalog.
    List {
        /// Show detailed information including base URL, models, and
        /// whether a key is stored.
        #[arg(long)]
        detailed: bool,
    },
    /// List the built-in preset templates available with `provider add`.
    Templates,
    /// Add a provider to the catalog. Either `--template` or
    /// `--custom` plus the relevant flags must be supplied.
    Add(AddArgs),
    /// Remove a provider from the catalog (does not delete its key).
    Remove {
        /// Provider id to remove.
        id: String,
    },
    /// Set the runtime default provider (and optionally model).
    SetDefault {
        /// Provider id to use as the default.
        provider: String,
        /// Optional model id; defaults to the provider's
        /// `default_model_id` if omitted.
        #[arg(long)]
        model: Option<String>,
    },
    /// Show the current runtime default provider + model.
    GetDefault,
}

/// Arguments for `peko provider add`.
#[derive(clap::Args)]
pub struct AddArgs {
    /// Seed from a built-in preset template (e.g. `anthropic`,
    /// `openai`, `ollama`). Mutually exclusive with `--custom`.
    #[arg(long, conflicts_with = "custom")]
    template: Option<String>,
    /// Provider id to use in the catalog. If omitted with
    /// `--template`, the template id is used.
    #[arg(long)]
    name: Option<String>,
    /// Override the display name (otherwise the template's display
    /// name is used, or the id for `--custom`).
    #[arg(long)]
    display_name: Option<String>,
    /// Add a fully custom provider (OpenAI-compatible or
    /// Anthropic-compatible endpoint).
    #[arg(long, conflicts_with = "template")]
    custom: bool,
    /// API format for a custom provider.
    /// One of `openai_completions`, `anthropic_messages`.
    #[arg(long, requires = "custom")]
    api_format: Option<String>,
    /// Base URL for a custom provider.
    #[arg(long, requires = "custom")]
    base_url: Option<String>,
    /// Whether the custom provider requires an API key. Defaults to
    /// true.
    #[arg(long, requires = "custom")]
    requires_key: Option<bool>,
    /// Add a model id to a custom provider (can be repeated).
    #[arg(long, requires = "custom", value_name = "MODEL_ID")]
    model: Vec<String>,
    /// Store an API key for this provider in the vault immediately.
    /// Equivalent to running `peko credential set <id>` afterwards.
    /// Ignored when the entry does not require a key.
    #[arg(long, value_name = "KEY")]
    key: Option<String>,
    /// Set the newly-added provider as the runtime default after
    /// adding it. Equivalent to running
    /// `peko provider set-default <id>` afterwards.
    #[arg(long)]
    default: bool,
    /// Default model id to use when `--default` is set. Defaults to
    /// the provider's `default_model_id` (i.e. the template's
    /// curated choice) when omitted. Requires `--default`.
    #[arg(long, requires = "default", value_name = "MODEL_ID")]
    default_model: Option<String>,
}

/// Execute a provider subcommand.
pub async fn execute(cmd: ProviderCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        ProviderCommands::List { detailed } => list_cmd(paths, detailed).await,
        ProviderCommands::Templates => templates_cmd().await,
        ProviderCommands::Add(args) => add_cmd(args, paths).await,
        ProviderCommands::Remove { id } => remove_cmd(&id, paths).await,
        ProviderCommands::SetDefault { provider, model } => {
            set_default_cmd(&provider, model.as_deref(), paths).await
        }
        ProviderCommands::GetDefault => get_default_cmd(paths).await,
    }
}

/// Tell the running daemon to re-read `providers.toml` from disk so
/// the in-flight root agent sees the mutation just persisted by the
/// caller. Silent on connection failure — the daemon may not be
/// running (cold start, dev workflow), in which case the next
/// `peko daemon start` will pick up the new state from disk anyway.
async fn notify_daemon_reload() {
    let Ok(client) = crate::ipc::DaemonClient::connect().await else {
        return;
    };
    match client.reload_providers().await {
        Ok(crate::ipc::ResponsePacket::ProviderReloaded {
            providers_count,
            keys_count,
            ..
        }) => {
            if providers_count > 0 || keys_count > 0 {
                println!("Daemon reloaded: {providers_count} provider(s), {keys_count} key(s).");
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
    paths.config_dir.join(ProviderCatalog::FILENAME)
}

async fn open_catalog(paths: &GlobalPaths) -> Result<std::sync::Arc<ProviderCatalog>> {
    let path = catalog_path(paths);
    ProviderCatalog::load_or_init(&path).await
}

async fn list_cmd(paths: &GlobalPaths, detailed: bool) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entries = cat.list_all().await;

    if entries.is_empty() {
        println!("No providers in the catalog.");
        println!("Add one with: peko provider add --template <anthropic|openai|ollama|...>");
        println!("Or:           peko provider add --custom --name <id> --api-format <fmt> --base-url <url>");
        return Ok(());
    }

    println!("Provider catalog ({} entries):\n", entries.len());
    let (default_pid, default_model_id) = cat.get_default().await;

    for e in &entries {
        let status = if e.enabled { "✓" } else { "✗" };
        let marker = if default_pid.as_deref() == Some(e.id.as_str()) {
            " (default)"
        } else {
            ""
        };
        let from_tmpl = e
            .template_id
            .as_deref()
            .map(|t| format!(" [from {t}]"))
            .unwrap_or_default();

        println!(
            "  [{status}] {} - {}{marker}{from_tmpl}",
            e.id, e.display_name
        );

        if detailed {
            println!("      format:        {}", e.api_format);
            println!("      base_url:      {}", e.base_url);
            println!("      default_model: {}", e.default_model_id);
            println!(
                "      requires_key:  {}{}",
                e.requires_key,
                if e.requires_key {
                    " (use `peko credential set <id>`)"
                } else {
                    ""
                }
            );
            println!(
                "      models ({}){}:",
                e.models.len(),
                if Some(&e.default_model_id) == default_model_id.as_ref() {
                    " — * = runtime default"
                } else {
                    ""
                }
            );
            for m in &e.models {
                let star = if Some(&m.id) == default_model_id.as_ref() {
                    " *"
                } else {
                    ""
                };
                let ctx = m
                    .context_length
                    .map(|c| format!(" ({c} ctx)"))
                    .unwrap_or_default();
                let dn = m
                    .display_name
                    .as_deref()
                    .map(|n| format!(" — {n}"))
                    .unwrap_or_default();
                println!("        - {}{star}{dn}{ctx}", m.id);
            }
            if !e.headers.is_empty() {
                println!("      headers:       {} item(s)", e.headers.len());
            }
            println!();
        }
    }

    if detailed && default_pid.is_none() {
        println!("\nNo runtime default set. Use `peko provider set-default <id>` to choose one.");
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
    }
    println!("\nUse: peko provider add --template <id>");
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
               peko provider add --template anthropic --key \"$ANTHROPIC_API_KEY\" --default\n\
             \n\
             List templates:\n\
               peko provider templates"
        );
    }

    let cat = open_catalog(paths).await?;
    let entry = if let Some(template_id) = args.template.as_deref() {
        let tmpl = templates::find_template(template_id).with_context(|| {
            format!(
                "unknown template '{template_id}'. Run `peko provider templates` to list available ones."
            )
        })?;
        let id = args.name.unwrap_or_else(|| tmpl.id.to_string());
        ProviderCatalogEntry::from_template(tmpl, id, args.display_name)
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
            .name
            .clone()
            .with_context(|| "--name is required with --custom")?;
        if id.is_empty() {
            anyhow::bail!("--name must not be empty");
        }
        if args.model.is_empty() {
            anyhow::bail!(
                "--custom providers must declare at least one --model <model-id> (use the id the API expects on the wire)"
            );
        }
        let default_model_id = args.model[0].clone();
        let models: Vec<ModelInfo> = args
            .model
            .iter()
            .map(|mid| ModelInfo::new(mid.clone()))
            .collect();
        ProviderCatalogEntry {
            id: id.clone(),
            display_name: args.display_name.clone().unwrap_or_else(|| id.clone()),
            template_id: None,
            api_format,
            base_url,
            models,
            default_model_id,
            headers: Default::default(),
            requires_key: args.requires_key.unwrap_or(true),
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    } else {
        unreachable!("guarded by the bare-invocation check above");
    };

    cat.upsert(entry.clone()).await?;
    println!("Added provider '{}' ({}).", entry.id, entry.display_name);

    // Fold in `--key`: store the API key in the vault immediately if the
    // entry requires one. Skipped silently for key-less providers
    // (e.g. Ollama) so the same command works for both.
    if let Some(key) = args.key.as_deref() {
        if key.is_empty() {
            anyhow::bail!("--key must not be empty");
        }
        if entry.requires_key {
            let vault = crate::common::vault::Vault::load(paths.resolver().vault())
                .context("failed to load credential vault")?;
            let secret = secrecy::SecretString::from(key.to_string());
            vault
                .set_provider_key(&entry.id, &secret)
                .with_context(|| format!("failed to store key for '{}' in vault", entry.id))?;
            println!("Stored API key for '{}' in the vault.", entry.id);
        } else {
            anyhow::bail!(
                "--key supplied but provider '{}' does not require a key (set --requires-key true to override)",
                entry.id
            );
        }
    } else if entry.requires_key {
        // No key supplied, key required — tell the user exactly what to
        // run, no prompt.
        println!(
            "Next: store its API key with: peko credential set {} --key \"$YOUR_KEY\"",
            entry.id
        );
    }

    // Fold in `--default`: promote the new entry to the runtime
    // default. `--default-model` overrides the provider's curated
    // default if the user wants something different.
    if args.default {
        let model = args
            .default_model
            .clone()
            .unwrap_or_else(|| entry.default_model_id.clone());
        cat.set_default(Some(entry.id.clone()), Some(model.clone()))
            .await?;
        println!("Default: {} / {}", entry.id, model);
    }

    notify_daemon_reload().await;
    Ok(())
}

async fn remove_cmd(id: &str, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    if cat.remove(id).await? {
        println!("Removed provider '{id}'.");
        notify_daemon_reload().await;
    } else {
        println!("No provider '{id}' in the catalog.");
    }
    Ok(())
}

async fn set_default_cmd(provider: &str, model: Option<&str>, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    cat.set_default(Some(provider.to_string()), model.map(str::to_string))
        .await?;
    let model_part = model
        .map(|m| format!(" (model {m})"))
        .unwrap_or_else(|| " (provider's default model)".to_string());
    println!("Runtime default set to {provider}{model_part}.");
    notify_daemon_reload().await;
    Ok(())
}

async fn get_default_cmd(paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let (pid, mid) = cat.get_default().await;
    match pid {
        Some(p) => {
            let fallback = cat.get(&p).await.map(|e| e.default_model_id);
            let model = mid.or(fallback).unwrap_or_default();
            println!("Default: {p} / {model}");
        }
        None => {
            println!("No runtime default set. Use `peko provider set-default <id>`.");
        }
    }
    Ok(())
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
            "PEKO_provider_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        std::env::set_var("PEKO_MASTER_PASSPHRASE", "test-provider-cmd");
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            temp.join("config").to_str().unwrap(),
            "--data-dir",
            temp.join("data").to_str().unwrap(),
            "--cache-dir",
            temp.join("cache").to_str().unwrap(),
            "provider",
            "list",
        ]);
        GlobalPaths::from_cli(&cli)
    }

    /// `peko provider add` with no flags must NOT launch an interactive
    /// flow — agents have to be able to detect "no input" and recover.
    /// The AddArgs structure is intentionally flag-free in this shape
    /// so the runtime can error deterministically.
    #[test]
    fn add_args_bare_invocation_has_no_template_or_custom() {
        let cli = Cli::try_parse_from(["peko", "provider", "add"]).unwrap();
        match cli.command {
            crate::commands::Commands::Provider(ProviderCommands::Add(args)) => {
                assert!(args.template.is_none());
                assert!(!args.custom);
                assert!(args.key.is_none());
                assert!(!args.default);
                assert!(args.default_model.is_none());
            }
            _ => panic!("expected provider add"),
        }
    }

    /// `--key` and `--default` and `--default-model` flags parse.
    #[test]
    fn add_args_one_shot_flags_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "provider",
            "add",
            "--template",
            "anthropic",
            "--key",
            "sk-test",
            "--default",
            "--default-model",
            "claude-3-5-haiku-latest",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Provider(ProviderCommands::Add(args)) => {
                assert_eq!(args.template.as_deref(), Some("anthropic"));
                assert_eq!(args.key.as_deref(), Some("sk-test"));
                assert!(args.default);
                assert_eq!(
                    args.default_model.as_deref(),
                    Some("claude-3-5-haiku-latest")
                );
            }
            _ => panic!("expected provider add"),
        }
    }

    /// `--default-model` requires `--default` (clap guard).
    #[test]
    fn default_model_requires_default_flag() {
        let result = Cli::try_parse_from([
            "peko",
            "provider",
            "add",
            "--template",
            "anthropic",
            "--default-model",
            "claude-3-5-haiku-latest",
        ]);
        assert!(
            result.is_err(),
            "expected clap to reject --default-model without --default"
        );
    }

    /// `peko provider add --custom ...` parses the full custom-flag set.
    #[test]
    fn add_args_custom_flags_parse() {
        let cli = Cli::try_parse_from([
            "peko",
            "provider",
            "add",
            "--custom",
            "--name",
            "my-llama",
            "--api-format",
            "openai_completions",
            "--base-url",
            "http://localhost:8080/v1",
            "--model",
            "llama-3.1-8b",
        ])
        .unwrap();
        match cli.command {
            crate::commands::Commands::Provider(ProviderCommands::Add(args)) => {
                assert!(args.custom);
                assert_eq!(args.name.as_deref(), Some("my-llama"));
                assert_eq!(args.api_format.as_deref(), Some("openai_completions"));
                assert_eq!(args.base_url.as_deref(), Some("http://localhost:8080/v1"));
                assert_eq!(args.model, vec!["llama-3.1-8b".to_string()]);
            }
            _ => panic!("expected provider add"),
        }
    }

    /// End-to-end: one command adds an entry, stores the key in the
    /// vault, and promotes the entry to default. Mirrors the
    /// "first-time setup" example in the module doc-comment.
    #[tokio::test]
    #[serial_test::serial]
    async fn one_shot_add_writes_catalog_vault_and_default() {
        use crate::common::vault::Vault;
        use crate::providers::catalog::ProviderCatalog;
        use secrecy::{ExposeSecret, SecretString};

        let paths = fresh_paths();

        let args = AddArgs {
            template: Some("anthropic".into()),
            name: None,
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            requires_key: None,
            model: Vec::new(),
            key: Some("sk-ant-test-key".into()),
            default: true,
            default_model: Some("claude-3-5-haiku-latest".into()),
        };
        add_cmd(args, &paths)
            .await
            .expect("one-shot add should succeed");

        // 1. Catalog entry exists.
        let cat = ProviderCatalog::load_or_init(&paths.config_dir.join(ProviderCatalog::FILENAME))
            .await
            .unwrap();
        let entry = cat.get("anthropic").await.expect("entry should exist");
        assert_eq!(entry.id, "anthropic");
        assert!(entry.requires_key);

        // 2. Key landed in the vault. Load explicitly with the
        //    passphrase we set in `fresh_paths` to bypass the env-var
        //    race that hits when other parallel tests mutate
        //    `PEKO_MASTER_PASSPHRASE` between our write and read.
        let passphrase = SecretString::new("test-provider-cmd".to_string().into());
        let vault = Vault::load_with_passphrase(paths.resolver().vault(), &passphrase).unwrap();
        let stored = vault
            .get_provider_key("anthropic")
            .expect("key should be stored");
        assert_eq!(stored.expose_secret(), "sk-ant-test-key");

        // 3. Default is set with the requested model override.
        let (pid, mid) = cat.get_default().await;
        assert_eq!(pid.as_deref(), Some("anthropic"));
        assert_eq!(mid.as_deref(), Some("claude-3-5-haiku-latest"));
    }

    /// Bare `peko provider add` (no template, no custom) errors with a
    /// pointer at the right invocation.
    #[tokio::test]
    #[serial_test::serial]
    async fn bare_add_errors_with_actionable_hint() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: None,
            name: None,
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            requires_key: None,
            model: Vec::new(),
            key: None,
            default: false,
            default_model: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--template") && msg.contains("--key") && msg.contains("--default"),
            "expected pointer at the scriptable flags, got: {msg}"
        );
    }

    /// `--key` against a key-less provider (e.g. ollama) errors so the
    /// user doesn't silently drop a key they're trying to set.
    #[tokio::test]
    #[serial_test::serial]
    async fn key_flag_rejects_keyless_provider() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: Some("ollama".into()),
            name: None,
            display_name: None,
            custom: false,
            api_format: None,
            base_url: None,
            requires_key: None,
            model: Vec::new(),
            key: Some("ignored".into()),
            default: false,
            default_model: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        assert!(
            err.to_string().contains("does not require a key"),
            "expected key-less rejection, got: {err}"
        );
    }
}
