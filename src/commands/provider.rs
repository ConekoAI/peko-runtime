//! Provider management commands.
//!
//! These commands operate on the runtime-owned provider catalog
//! (`~/.peko/providers.toml`). They replace the previous design where
//! providers were declared as static code and provider selection was a
//! per-agent field.
//!
//! Typical flows:
//!
//! ```text
//! # 1. Add a provider from a preset template
//! peko provider add --template anthropic
//!
//! # 2. Store its API key in the OS keychain
//! peko credential set anthropic
//!
//! # 3. Make it the runtime default
//! peko provider set-default anthropic --model claude-sonnet-4-5
//!
//! # 4. Inspect the catalog
//! peko provider list --detailed
//! ```
//!
//! Adding a custom (non-template) provider is also supported:
//!
//! ```text
//! peko provider add --custom --name my-llama \
//!                   --api-format openai_completions \
//!                   --base-url http://localhost:8080/v1
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
        anyhow::bail!("either --template <id> or --custom (with --api-format, --base-url, --model) is required");
    };

    cat.upsert(entry.clone()).await?;
    println!("Added provider '{}' ({}).", entry.id, entry.display_name);
    if entry.requires_key {
        println!(
            "Next: store its API key with: peko credential set {}",
            entry.id
        );
    }
    Ok(())
}

async fn remove_cmd(id: &str, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    if cat.remove(id).await? {
        println!("Removed provider '{id}'.");
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
