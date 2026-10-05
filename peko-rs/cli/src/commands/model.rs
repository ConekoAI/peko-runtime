//! Model management commands.
//!
//! These commands operate on the runtime-owned model catalog
//! (`~/.peko/models.toml`). The catalog is model-first: each entry
//! bundles endpoint info (base URL, API format), the wire model id,
//! context-window metadata, and an optional `credential_id` pointing
//! into the vault. There is no separate provider layer.
//!
//! Models use explicit endpoint settings. API keys are stored in the vault.
//! ```text
//! peko model add --id my-model --api-format anthropic_messages \
//!   --base-url https://api.anthropic.com --model <wire-id> --key "$API_KEY"
//! ```

use crate::commands::GlobalPaths;
use anyhow::{Context, Result};
use peko_core::common::vault::{Credential, CredentialKind, Vault};
use peko_providers::catalog::{ApiFormat, ModelCatalog, ModelConfig};
use peko_providers::spec::{PricingHint, ThinkingMode, ToolSupport};
use serde::Serialize;

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
        /// Emit machine-readable JSON instead of the default
        /// human-readable summary. Mirrors `peko model show --json`
        /// and `peko model compare --json` so callers can pipe
        /// any of the read-only commands into jq / scripts.
        #[arg(long)]
        json: bool,
    },
    /// Retired preset discovery; prints migration guidance.
    #[command(hide = true)]
    Templates,
    /// Show one configured model in detail.
    Show {
        /// Configured model id to show.
        id: String,
        /// Emit machine-readable JSON. The same shape is used by
        /// `peko model list --json` so a caller can pipe either
        /// into jq.
        #[arg(long)]
        json: bool,
        /// Print the `peko model add` command that would recreate
        /// this entry, instead of the human-readable detail view.
        /// Useful for sharing a configuration across machines or
        /// checking what's actually persisted on disk.
        #[arg(long, conflicts_with = "json")]
        copy_as_cli: bool,
    },
    /// Compare 2+ configured models side by side as a capability
    /// matrix (vision, tools, thinking, json_mode, pricing,
    /// context window). The first column is the field name; each
    /// remaining column is one requested model.
    Compare {
        /// Configured model ids to compare (at least two).
        #[arg(required = true, value_name = "MODEL_ID")]
        ids: Vec<String>,
        /// Emit the matrix as JSON. Schema is
        /// `{ fields: [...], rows: [[name, v1, v2, ...]] }`.
        #[arg(long)]
        json: bool,
    },
    /// Filter configured models by capability predicate. At least
    /// one of `--vision` / `--tools` / `--thinking` /
    /// `--json-mode` / `--no-key` / `--enabled` / `--disabled` is
    /// required; combining predicates ANDs them.
    Search(SearchArgs),
    /// Add a model using its base URL, API format, and wire model id.
    Add(AddArgs),
    /// Edit an existing catalog entry. Only the supplied flags
    /// (`--note`) are touched; everything else is preserved.
    Edit(EditArgs),
    /// Remove a model from the catalog (does not delete its credential).
    Remove {
        /// Configured model id to remove.
        id: String,
        /// Print what would be removed without touching the
        /// catalog. Useful for double-checking before destructive
        /// operations on a shared machine.
        #[arg(long)]
        dry_run: bool,
    },
    /// Live-test a configured model: ping its endpoint with the stored
    /// credential and report the outcome.
    Test {
        /// Configured model id to test.
        id: String,
    },
}

/// Arguments for `peko model search`.
///
/// Every flag is a positive capability predicate. `--no-vision` /
/// `--no-tools` are the inverse predicates; combining `--vision`
/// with `--no-vision` is rejected so the caller can't construct an
/// unsatisfiable query.
#[derive(clap::Args)]
pub struct SearchArgs {
    /// Match entries with `spec.image_input == true`.
    #[arg(long, conflicts_with = "no_vision")]
    vision: bool,
    /// Match entries with `spec.image_input == false` (text-only).
    #[arg(long)]
    no_vision: bool,
    /// Match entries whose `spec.tool_support` is at least
    /// `FunctionCalling`.
    #[arg(long, conflicts_with = "no_tools")]
    tools: bool,
    /// Match entries whose `spec.tool_support == None`
    /// (no tool support).
    #[arg(long)]
    no_tools: bool,
    /// Match entries whose `spec.thinking` is not `Disabled`.
    #[arg(long)]
    thinking: bool,
    /// Match entries whose `spec.json_mode == true`.
    #[arg(long)]
    json_mode: bool,
    /// Match entries whose `spec.pricing` is populated.
    #[arg(long)]
    priced: bool,
    /// Match entries whose `requires_key == false` (local / keyless
    /// endpoints).
    #[arg(long)]
    no_key: bool,
    /// Match only enabled entries.
    #[arg(long, conflicts_with = "disabled")]
    enabled: bool,
    /// Match only disabled entries.
    #[arg(long)]
    disabled: bool,
    /// Free-text needle against id / display_name / model_id
    /// (case-insensitive substring).
    #[arg(long, value_name = "NEEDLE")]
    contains: Option<String>,
    /// Emit machine-readable JSON instead of the human-readable
    /// list. The shape matches `peko model list --json` so callers
    /// can pipe either.
    #[arg(long)]
    json: bool,
}

/// Arguments for `peko model add`.
#[derive(clap::Args, Default)]
pub struct AddArgs {
    /// Retired: supply --api-format and --base-url explicitly.
    #[arg(long, hide = true, conflicts_with = "custom")]
    template: Option<String>,
    /// Configured model id. Defaults to the wire model id.
    #[arg(long)]
    id: Option<String>,
    /// Wire model id (the id the API expects on the wire, e.g.
    /// `gpt-4o`, `claude-sonnet-4-5`). Required.
    #[arg(long, value_name = "WIRE_MODEL_ID")]
    model: Option<String>,
    /// Display name. Defaults to the configured model id.
    #[arg(long)]
    display_name: Option<String>,
    /// Compatibility flag; all models now use explicit endpoint settings.
    #[arg(long, hide = true, conflicts_with = "template")]
    custom: bool,
    /// API format: openai_completions, openai_responses, or anthropic_messages.
    #[arg(long)]
    api_format: Option<String>,
    /// API base URL, including any required path prefix.
    #[arg(long)]
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
    /// Context window in tokens. Unknown when omitted.
    #[arg(long, value_name = "TOKENS")]
    context_window: Option<u32>,
    /// Maximum output tokens. Unknown when omitted.
    #[arg(long, value_name = "TOKENS")]
    max_output_tokens: Option<u32>,
    /// Endpoint does not require an API key.
    #[arg(long)]
    no_key: bool,
    /// Extra HTTP header; repeat for multiple headers.
    #[arg(long = "header", value_name = "NAME=VALUE")]
    headers: Vec<String>,
    /// Optional capability and pricing descriptor as JSON.
    #[arg(long, value_name = "JSON")]
    spec: Option<String>,
    /// Optional adapter compatibility hints as JSON.
    #[arg(long, value_name = "JSON")]
    compat: Option<String>,
    /// Print what would be added without touching the catalog or
    /// vault. Useful for double-checking `--key "$ENV_VAR"`
    /// expansions and `--credential-id` references before they
    /// land on disk.
    #[arg(long)]
    dry_run: bool,
    /// Phase 2 of `feature/multi-model-subagents`: attach a
    /// free-text note to the entry. Parent agents read this via
    /// the `model_list` tool before choosing which model to
    /// spawn with — standardized `ModelSpec` flags cannot
    /// capture subjective annotations like "use it for cron".
    /// Empty string is rejected; pass no flag to leave the note
    /// unchanged on `edit`. Capped at 500 chars (enforced by
    /// the catalog).
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
}

/// Arguments for `peko model edit`. Today only `--note` is
/// editable; future per-field edits (display_name, headers,
/// credential_id) can extend the struct without breaking the
/// CLI shape.
#[derive(clap::Args)]
pub struct EditArgs {
    /// Configured model id to edit.
    id: String,
    /// Replace the user note attached to this entry. Pass an
    /// empty string in `--note ""` to clear an existing note.
    /// Omit the flag to leave the note unchanged. Capped at
    /// 500 chars.
    #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
    note: Option<String>,
    /// Print what would change without touching the catalog.
    #[arg(long)]
    dry_run: bool,
}

/// Execute a model subcommand.
pub async fn execute(cmd: ModelCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        ModelCommands::List { detailed, json } => list_cmd(paths, detailed, json).await,
        ModelCommands::Templates => templates_cmd().await,
        ModelCommands::Show {
            id,
            json,
            copy_as_cli,
        } => show_cmd(&id, paths, json, copy_as_cli).await,
        ModelCommands::Compare { ids, json } => compare_cmd(&ids, paths, json).await,
        ModelCommands::Search(args) => search_cmd(args, paths).await,
        ModelCommands::Add(args) => add_cmd(args, paths).await,
        ModelCommands::Edit(args) => edit_cmd(args, paths).await,
        ModelCommands::Remove { id, dry_run } => remove_cmd(&id, paths, dry_run).await,
        ModelCommands::Test { id } => test_cmd(&id, paths).await,
    }
}

/// Tell the running daemon to re-read `models.toml` from disk so the
/// in-flight root agent sees the mutation just persisted by the caller.
/// Silent on connection failure — the daemon may not be running (cold
/// start, dev workflow), in which case the next `peko daemon start`
/// will pick up the new state from disk anyway.
async fn notify_daemon_reload() {
    let Ok(client) = peko_core::ipc::DaemonClient::connect().await else {
        return;
    };
    match client.reload_providers().await {
        Ok(peko_core::ipc::ResponsePacket::ModelReloaded {
            models_count,
            keys_count,
            ..
        }) => {
            if models_count > 0 || keys_count > 0 {
                println!("Daemon reloaded: {models_count} model(s), {keys_count} key(s).");
            }
        }
        Ok(peko_core::ipc::ResponsePacket::Error { message, .. }) => {
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

async fn list_cmd(paths: &GlobalPaths, detailed: bool, json: bool) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entries = cat.list_all().await;

    if json {
        let summaries: Vec<ModelSummaryWire> =
            entries.iter().map(ModelSummaryWire::from_config).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "count": summaries.len(),
                "entries": summaries,
            }))?
        );
        return Ok(());
    }

    if entries.is_empty() {
        println!("No models in the catalog.");
        println!("Add one with: peko model add --api-format <fmt> --base-url <url> --model <wire-id> [--id <name>] [--key <key>]");
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
            if let Some(ref note) = e.note {
                println!("      note:          {}", truncate_note(note, 80));
            }
            println!();
        }
    }

    Ok(())
}

/// Truncate a user note to `max_chars` runes with a trailing `…`
/// when shortened. Used by `peko model list --detailed` so the
/// table stays compact; `peko model show` prints the full text.
fn truncate_note(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

async fn templates_cmd() -> Result<()> {
    anyhow::bail!("model templates have been retired; use peko model add --api-format <fmt> --base-url <url> --model <wire-id>")
}

async fn add_cmd(args: AddArgs, paths: &GlobalPaths) -> Result<()> {
    if args.template.is_some() {
        anyhow::bail!("model templates have been retired; supply --api-format, --base-url, and --model explicitly");
    }
    let model_id = args.model.clone().context("--model <wire-id> is required; use peko model add --api-format <fmt> --base-url <url> --model <wire-id> [--key <key>]")?;
    let format = args.api_format.as_deref().context(
        "--api-format is required (openai_completions | openai_responses | anthropic_messages)",
    )?;
    let api_format =
        ApiFormat::from_wire(format).with_context(|| format!("unknown --api-format '{format}'"))?;
    let base_url = args.base_url.clone().context("--base-url is required")?;
    let id = args.id.clone().unwrap_or_else(|| model_id.clone());
    let mut entry = ModelConfig::new(id, api_format, base_url, model_id);
    if let Some(name) = &args.display_name {
        entry.display_name = name.clone();
    }
    entry.requires_key = !args.no_key;
    entry.context_window = args.context_window;
    entry.max_output_tokens = args.max_output_tokens;
    entry.note = args.note.clone();
    entry.spec = args
        .spec
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid --spec JSON")?;
    entry.compat = args
        .compat
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("invalid --compat JSON")?;
    for header in &args.headers {
        let (name, value) = header
            .split_once('=')
            .context("--header must have NAME=VALUE form")?;
        entry.headers.insert(name.to_string(), value.to_string());
    }
    entry.validate()?;
    if args.key.is_some() && args.credential_id.is_some() {
        anyhow::bail!("--key and --credential-id are mutually exclusive");
    }
    if args.key.as_deref().is_some_and(|key| key.trim().is_empty()) {
        anyhow::bail!("--key must not be empty");
    }
    if args
        .credential_id
        .as_deref()
        .is_some_and(|id| id.trim().is_empty())
    {
        anyhow::bail!("--credential-id must not be empty");
    }
    if !entry.requires_key && (args.key.is_some() || args.credential_id.is_some()) {
        anyhow::bail!("model '{}' does not require a key", entry.id);
    }
    let cat = open_catalog(paths).await?;
    if cat.get(&entry.id).await.is_some() {
        anyhow::bail!(
            "model id '{}' already exists; edit it or remove it before re-adding",
            entry.id
        );
    }
    if args.dry_run {
        println!("[dry-run] Would add model '{}'.", entry.id);
        println!("           model_id:     {}", entry.model_id);
        println!("           api_format:   {}", entry.api_format);
        println!("           base_url:     {}", entry.base_url);
        println!("           requires_key: {}", entry.requires_key);
        return Ok(());
    }

    // Wire the credential: either reference an existing vault credential
    // by id, or store a new API key in the vault under `llm` and point
    // the entry at it.
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

    cat.upsert(entry).await?;
    println!("Added model '{entry_id}' ({entry_display}).");

    if requires_key && !has_credential {
        println!(
            "Next: store its API key with: peko credential set llm {entry_id} --kind api_key --material \"$YOUR_KEY\"\n\
             Then set this entry's credential_id in models.toml to the returned vault id.\n\
             Supply --key when initially adding a model to store and wire it in one step."
        );
    }

    notify_daemon_reload().await;
    Ok(())
}

/// Edit an existing catalog entry. Only the supplied flags are
/// touched; everything else is preserved. Phase 2 ships only
/// `--note` (and the dry-run escape hatch).
async fn edit_cmd(args: EditArgs, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let id = &args.id;
    let mut entry = cat
        .get(id)
        .await
        .with_context(|| format!("model not found in catalog: {id}"))?;

    let before_note = entry.note.clone();

    if let Some(note) = args.note.clone() {
        if note.is_empty() {
            // Empty string in `--note ""` clears the note.
            entry.note = None;
        } else {
            entry.note = Some(note);
        }
    }

    // Validate the resulting entry via the catalog's normal
    // `upsert` path so the 500-char cap (and any future caps)
    // apply uniformly across add / edit / IPC.
    let new_note = entry.note.clone();
    if args.dry_run {
        println!("[dry-run] Would edit model '{id}'.");
        if before_note != new_note {
            println!("  note: {:?} -> {:?}", before_note, new_note);
        } else {
            println!("  (no changes)");
        }
        return Ok(());
    }

    cat.upsert(entry)
        .await
        .with_context(|| format!("failed to update model '{id}' — note must be ≤500 chars"))?;

    if before_note != new_note {
        println!("Updated note on '{id}'.");
    } else {
        println!("No changes to '{id}'.");
    }

    notify_daemon_reload().await;
    Ok(())
}

async fn remove_cmd(id: &str, paths: &GlobalPaths, dry_run: bool) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entry = cat.get(id).await;
    match (&entry, dry_run) {
        (Some(e), true) => {
            println!(
                "[dry-run] Would remove model '{id}' ({} — wire: {}, fmt: {}).",
                e.display_name, e.model_id, e.api_format
            );
            // NOTE: --dry-run does NOT call into the credential
            // vault. A future PR can extend `--dry-run` to also
            // surface orphan-credential info once the catalog ↔
            // vault reference metadata is plumbed through.
            Ok(())
        }
        (Some(_), false) => {
            if cat.remove(id).await? {
                println!("Removed model '{id}'.");
                notify_daemon_reload().await;
            } else {
                // Race: another writer beat us to the catalog.
                // Not an error — the desired end state is reached.
                println!("No model '{id}' in the catalog.");
            }
            Ok(())
        }
        (None, _) => {
            println!("No model '{id}' in the catalog.");
            Ok(())
        }
    }
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

// ============================================================================
// PR 5 — `peko model show | compare | search`, `--json`, `--dry-run`,
//        `--copy-as-cli`
// ============================================================================

/// Machine-readable wire shape for one configured model entry.
///
/// `serde` renames the snake_case fields to camelCase so the JSON
/// shape matches the rest of the IPC envelope (which is camelCase
/// for cross-crate boundaries). The same struct drives
/// `list --json`, `show --json`, `compare --json`, and `search
/// --json` so callers can pipe any of them through `jq` without
/// remapping the keys.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelSummaryWire {
    id: String,
    display_name: String,
    template_id: Option<String>,
    model_id: String,
    api_format: String,
    base_url: String,
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    requires_key: bool,
    enabled: bool,
    credential_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    headers: Vec<(String, String)>,
    /// Optional user-supplied capabilities. None means unknown.
    spec: Option<ModelSpecWire>,
    /// Phase 2 of `feature/multi-model-subagents`: free-text
    /// user note attached to this entry. Surfaced to the parent
    /// agent via the `model_list` tool and to operators via
    /// `peko model show`. Skipped from JSON when absent so
    /// pre-Phase-2 entries deserialize cleanly into the new
    /// field set.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

impl ModelSummaryWire {
    fn from_config(cfg: &ModelConfig) -> Self {
        Self {
            id: cfg.id.clone(),
            display_name: cfg.display_name.clone(),
            template_id: cfg.template_id.clone(),
            model_id: cfg.model_id.clone(),
            api_format: cfg.api_format.as_str().to_string(),
            base_url: cfg.base_url.clone(),
            context_window: cfg.context_window,
            max_output_tokens: cfg.max_output_tokens,
            requires_key: cfg.requires_key,
            enabled: cfg.enabled,
            credential_id: cfg.credential_id.clone(),
            headers: cfg
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            spec: cfg.spec.map(ModelSpecWire::from_spec),
            note: cfg.note.clone(),
        }
    }
}

/// Machine-readable wire shape for `ModelSpec` (PR 1 descriptor).
///
/// Mirrors the on-disk / IPC shape — snake_case enum variants,
/// nested `pricing` object, `Option` skips — so JSON emitted here
/// matches what `peko-core`'s IPC handler emits for the same
/// entry. The CLI is the canonical "see what is persisted" view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelSpecWire {
    image_input: bool,
    audio_input: bool,
    tool_support: String,
    streaming: bool,
    thinking: String,
    json_mode: bool,
    /// ADR-061 (D4): judgment-class model flag. Not settable via CLI
    /// flags (spec fields are hand-edit-only in `models.toml`); shown
    /// here so `peko model show --json` reflects the persisted entry.
    decisions: bool,
    pricing: Option<PricingHintWire>,
}

impl ModelSpecWire {
    fn from_spec(s: peko_providers::spec::ModelSpec) -> Self {
        Self {
            image_input: s.image_input,
            audio_input: s.audio_input,
            tool_support: tool_support_wire(s.tool_support).to_string(),
            streaming: s.streaming,
            thinking: thinking_wire(s.thinking).to_string(),
            json_mode: s.json_mode,
            decisions: s.decisions,
            pricing: s.pricing.map(PricingHintWire::from_hint),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct PricingHintWire {
    input_per_million: Option<f64>,
    output_per_million: Option<f64>,
}

impl PricingHintWire {
    fn from_hint(p: PricingHint) -> Self {
        Self {
            input_per_million: p.input_per_million,
            output_per_million: p.output_per_million,
        }
    }
}

fn tool_support_wire(t: ToolSupport) -> &'static str {
    match t {
        ToolSupport::None => "none",
        ToolSupport::FunctionCalling => "function_calling",
        ToolSupport::Full => "full",
    }
}

fn thinking_wire(t: ThinkingMode) -> &'static str {
    match t {
        ThinkingMode::Disabled => "disabled",
        ThinkingMode::Optional => "optional",
        ThinkingMode::Required => "required",
        ThinkingMode::CustomBudget => "custom_budget",
    }
}

/// Emit one configured model either as human-readable detail or as
/// JSON, or render the `peko model add` command that would
/// recreate it.
async fn show_cmd(id: &str, paths: &GlobalPaths, json: bool, copy_as_cli: bool) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entry = cat
        .get(id)
        .await
        .with_context(|| format!("model not found in catalog: {id}"))?;

    if json {
        let wire = ModelSummaryWire::from_config(&entry);
        println!("{}", serde_json::to_string_pretty(&wire)?);
        return Ok(());
    }

    if copy_as_cli {
        let cmd = render_add_command(&entry);
        println!("{cmd}");
        return Ok(());
    }

    print_detail(&entry);
    Ok(())
}

fn print_detail(e: &ModelConfig) {
    let status = if e.enabled { "✓" } else { "✗" };
    let from_tmpl = e
        .template_id
        .as_deref()
        .map(|t| format!(" [from {t}]"))
        .unwrap_or_default();
    println!("[{status}] {} - {}{from_tmpl}", e.id, e.display_name);
    println!("    model_id:        {}", e.model_id);
    println!("    api_format:      {}", e.api_format);
    println!("    base_url:        {}", e.base_url);
    if let Some(ctx) = e.context_window {
        println!("    context_window:  {ctx}");
    }
    if let Some(mot) = e.max_output_tokens {
        println!("    max_output_tokens:{mot}");
    }
    println!("    requires_key:    {}", e.requires_key);
    match &e.credential_id {
        Some(cid) => println!("    credential_id:   {cid}"),
        None if e.requires_key => println!(
            "    credential_id:   (none — store one with `peko credential set llm {} --kind api_key`)",
            e.id
        ),
        None => {}
    }
    if !e.headers.is_empty() {
        println!("    headers:         {} item(s)", e.headers.len());
        for (k, v) in &e.headers {
            println!("      {k}: {v}");
        }
    }
    match e.spec {
        Some(s) => {
            println!("    spec:");
            println!("      image_input:    {}", s.image_input);
            println!("      audio_input:    {}", s.audio_input);
            println!(
                "      tool_support:   {}",
                tool_support_wire(s.tool_support)
            );
            println!("      streaming:      {}", s.streaming);
            println!("      thinking:       {}", thinking_wire(s.thinking));
            println!("      json_mode:      {}", s.json_mode);
            println!("      decisions:      {}", s.decisions);
            if let Some(p) = s.pricing {
                println!(
                    "      pricing:        in ${}/Mtok, out ${}/Mtok",
                    p.input_per_million
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| "—".to_string()),
                    p.output_per_million
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| "—".to_string()),
                );
            }
        }
        None => {
            println!("    spec:            (unknown — no capability metadata supplied)");
        }
    }
    if let Some(note) = &e.note {
        println!("    note:            {note}");
    }
    println!("    created_at:      {}", e.created_at.to_rfc3339());
    println!("    updated_at:      {}", e.updated_at.to_rfc3339());
}

/// Render the `peko model add` invocation that would recreate this
/// entry verbatim (minus the credential material — we never echo
/// the API key, only the `credential_id` reference).
///
/// Used by `--copy-as-cli` to share explicit settings with shell-safe arguments.
fn render_add_command(e: &ModelConfig) -> String {
    let mut parts: Vec<String> = vec!["peko".to_string(), "model".to_string(), "add".to_string()];
    parts.push("--api-format".to_string());
    parts.push(e.api_format.as_str().to_string());
    parts.push("--base-url".to_string());
    parts.push(e.base_url.clone());
    if !e.requires_key {
        parts.push("--no-key".to_string());
    }
    if let Some(ctx) = e.context_window {
        parts.push("--context-window".to_string());
        parts.push(ctx.to_string());
    }
    if let Some(mot) = e.max_output_tokens {
        parts.push("--max-output-tokens".to_string());
        parts.push(mot.to_string());
    }
    for (name, value) in &e.headers {
        parts.push("--header".to_string());
        parts.push(format!("{name}={value}"));
    }
    if let Some(spec) = &e.spec {
        parts.push("--spec".to_string());
        parts.push(serde_json::to_string(spec).expect("serializable spec"));
    }
    if let Some(compat) = &e.compat {
        parts.push("--compat".to_string());
        parts.push(serde_json::to_string(compat).expect("serializable compat"));
    }
    parts.push("--id".to_string());
    parts.push(e.id.clone());
    if e.display_name != e.id {
        parts.push("--display-name".to_string());
        parts.push(e.display_name.clone());
    }
    parts.push("--model".to_string());
    parts.push(e.model_id.clone());
    if let Some(cid) = &e.credential_id {
        parts.push("--credential-id".to_string());
        parts.push(cid.clone());
    }
    if let Some(note) = &e.note {
        parts.push("--note".to_string());
        parts.push(note.clone());
    }
    parts
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_/.:=".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\"'\"'"))
    }
}

/// Side-by-side capability matrix for 2+ configured models.
///
/// Rows are capability fields (vision / audio / tools / thinking /
/// json_mode / streaming / pricing / context window / max output /
/// api_format / base_url). Columns are the requested model ids in
/// the order the user supplied them. The first column is the
/// field name; subsequent columns hold the value for each model.
async fn compare_cmd(ids: &[String], paths: &GlobalPaths, json: bool) -> Result<()> {
    if ids.len() < 2 {
        anyhow::bail!(
            "`peko model compare` needs at least 2 ids (got {})",
            ids.len()
        );
    }
    let cat = open_catalog(paths).await?;
    let mut entries = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = cat
            .get(id)
            .await
            .with_context(|| format!("model not found in catalog: {id}"))?;
        entries.push(entry);
    }

    // Field list is fixed so the matrix has predictable column
    // ordering regardless of which models are being compared.
    let fields: &[&str] = &[
        "id",
        "display_name",
        "model_id",
        "api_format",
        "base_url",
        "context_window",
        "max_output_tokens",
        "image_input",
        "audio_input",
        "tool_support",
        "streaming",
        "thinking",
        "json_mode",
        "pricing (input $/Mtok)",
        "pricing (output $/Mtok)",
        "credential_id",
        "enabled",
    ];

    let rows: Vec<Vec<String>> = fields
        .iter()
        .map(|f| {
            let mut row = vec![(*f).to_string()];
            for e in &entries {
                row.push(field_value(e, f));
            }
            row
        })
        .collect();

    if json {
        let payload = serde_json::json!({
            "fields": fields,
            "rows": rows,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    // Column widths: longest field-name column is the field names
    // themselves; data columns get the widest cell.
    let label_width = fields.iter().map(|f| f.len()).max().unwrap_or(0);
    let col_widths: Vec<usize> = (0..entries.len())
        .map(|i| {
            rows.iter()
                .map(|r| r.get(i + 1).map(String::len).unwrap_or(0))
                .max()
                .unwrap_or(0)
                .max(entries[i].id.len())
        })
        .collect();

    print!("{:<label_width$}  ", "field");
    for (i, e) in entries.iter().enumerate() {
        print!("{:<width$}  ", e.id, width = col_widths[i]);
    }
    println!();
    for row in &rows {
        print!("{:<label_width$}  ", row[0]);
        for (i, cell) in row.iter().enumerate().skip(1) {
            print!("{:<width$}  ", cell, width = col_widths[i - 1]);
        }
        println!();
    }
    Ok(())
}

fn field_value(e: &ModelConfig, field: &str) -> String {
    match field {
        "id" => e.id.clone(),
        "display_name" => e.display_name.clone(),
        "model_id" => e.model_id.clone(),
        "api_format" => e.api_format.to_string(),
        "base_url" => e.base_url.clone(),
        "context_window" => e
            .context_window
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "max_output_tokens" => e
            .max_output_tokens
            .map(|v| v.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "image_input" => e
            .spec
            .map(|s| s.image_input.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "audio_input" => e
            .spec
            .map(|s| s.audio_input.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "tool_support" => e
            .spec
            .map(|s| tool_support_wire(s.tool_support).to_string())
            .unwrap_or_else(|| "—".to_string()),
        "streaming" => e
            .spec
            .map(|s| s.streaming.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "thinking" => e
            .spec
            .map(|s| thinking_wire(s.thinking).to_string())
            .unwrap_or_else(|| "—".to_string()),
        "json_mode" => e
            .spec
            .map(|s| s.json_mode.to_string())
            .unwrap_or_else(|| "—".to_string()),
        "pricing (input $/Mtok)" => e
            .spec
            .and_then(|s| s.pricing)
            .and_then(|p| p.input_per_million)
            .map(|v| format!("${v}"))
            .unwrap_or_else(|| "—".to_string()),
        "pricing (output $/Mtok)" => e
            .spec
            .and_then(|s| s.pricing)
            .and_then(|p| p.output_per_million)
            .map(|v| format!("${v}"))
            .unwrap_or_else(|| "—".to_string()),
        "credential_id" => e.credential_id.clone().unwrap_or_else(|| "—".to_string()),
        "enabled" => e.enabled.to_string(),
        // Unreachable: `fields` is fixed above and we exhaustively
        // match. A typo here would surface as "—" rather than panic.
        _ => "—".to_string(),
    }
}

/// Filter the catalog by capability predicates (`--vision`,
/// `--tools`, `--thinking`, `--json-mode`, `--priced`, `--no-key`,
/// `--enabled` / `--disabled`) and an optional free-text needle.
///
/// Each predicate is optional; combining predicates ANDs them.
/// Refuses to run with no predicates and no `--contains` needle so
/// callers can't accidentally `peko model search` (no args) and
/// get the entire catalog.
async fn search_cmd(args: SearchArgs, paths: &GlobalPaths) -> Result<()> {
    let cat = open_catalog(paths).await?;
    let entries = cat.list_all().await;

    let needle = args.contains.as_ref().map(|n| n.to_lowercase());
    let any_predicate = args.vision
        || args.no_vision
        || args.tools
        || args.no_tools
        || args.thinking
        || args.json_mode
        || args.priced
        || args.no_key
        || args.enabled
        || args.disabled
        || needle.is_some();

    if !any_predicate {
        anyhow::bail!(
            "at least one predicate is required: --vision, --no-vision, --tools, --no-tools, \
             --thinking, --json-mode, --priced, --no-key, --enabled, --disabled, --contains <needle>"
        );
    }

    let mut matched: Vec<&ModelConfig> = Vec::new();
    for e in &entries {
        if !matches_predicate(e, &args, needle.as_deref()) {
            continue;
        }
        matched.push(e);
    }

    if args.json {
        let summaries: Vec<ModelSummaryWire> = matched
            .iter()
            .map(|e| ModelSummaryWire::from_config(e))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "count": summaries.len(),
                "entries": summaries,
            }))?
        );
        return Ok(());
    }

    if matched.is_empty() {
        println!("No models matched the given predicates.");
        return Ok(());
    }

    println!("Search matched {} model(s):\n", matched.len());
    for e in &matched {
        let status = if e.enabled { "✓" } else { "✗" };
        let spec_tag = e
            .spec
            .map(|s| {
                format!(
                    " [vision:{} tools:{} thinking:{}]",
                    s.image_input,
                    tool_support_wire(s.tool_support),
                    thinking_wire(s.thinking),
                )
            })
            .unwrap_or_else(|| " [no spec]".to_string());
        println!("  [{status}] {} - {}{spec_tag}", e.id, e.display_name);
    }
    Ok(())
}

fn matches_predicate(e: &ModelConfig, args: &SearchArgs, needle_lower: Option<&str>) -> bool {
    if args.vision && !e.spec.is_some_and(|s| s.image_input) {
        return false;
    }
    // Unknown metadata does not establish the absence of a capability.
    if args.no_vision && e.spec.is_none_or(|s| s.image_input) {
        return false;
    }
    if args.tools && !e.spec.is_some_and(|s| s.tool_support != ToolSupport::None) {
        return false;
    }
    if args.no_tools && !e.spec.is_some_and(|s| s.tool_support == ToolSupport::None) {
        return false;
    }
    if args.thinking && !e.spec.is_some_and(|s| s.thinking != ThinkingMode::Disabled) {
        return false;
    }
    if args.json_mode && !e.spec.is_some_and(|s| s.json_mode) {
        return false;
    }
    if args.priced && e.spec.is_none_or(|s| s.pricing.is_none()) {
        return false;
    }
    if args.no_key && e.requires_key {
        return false;
    }
    if args.enabled && !e.enabled {
        return false;
    }
    if args.disabled && e.enabled {
        return false;
    }
    if let Some(needle) = needle_lower {
        let haystack = format!("{} {} {}", e.id, e.display_name, e.model_id).to_lowercase();
        if !haystack.contains(needle) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::commands::{from_cli, Cli};
    use clap::Parser;

    /// Build a `GlobalPaths` rooted at a fresh tempdir, with a
    /// explicitly passphrase-only test vault (core is a normal dependency).
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

        crate::test_support::init_credentials();
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
        from_cli(&cli)
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
        use peko_core::common::vault::Vault;
        use peko_providers::catalog::ModelCatalog;
        use secrecy::{ExposeSecret, SecretString};

        let paths = fresh_paths();

        let args = AddArgs {
            no_key: false,
            headers: vec![],
            spec: None,
            compat: None,
            template: None,
            id: Some("anthropic-claude-3-5-haiku-latest".into()),
            model: Some("claude-3-5-haiku-latest".into()),
            display_name: None,
            custom: false,
            api_format: Some("anthropic_messages".into()),
            base_url: Some("https://api.anthropic.com".into()),
            key: Some("sk-ant-test-key".into()),
            credential_id: None,
            context_window: None,
            max_output_tokens: None,
            dry_run: false,
            // Phase 2 of `feature/multi-model-subagents` —
            // tests don't exercise the note flag, so the field
            // stays at its default.
            note: None,
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
        let passphrase = SecretString::new(crate::test_support::MASTER_PASSPHRASE.into());
        let vault = Vault::load_with_passphrase(paths.resolver().vault(), &passphrase).unwrap();
        assert_eq!(
            vault.unlock_method(),
            peko_core::common::vault::UnlockMethod::Passphrase
        );
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
            no_key: false,
            headers: vec![],
            spec: None,
            compat: None,
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
            dry_run: false,
            // Phase 2 of `feature/multi-model-subagents` —
            // tests don't exercise the note flag, so the field
            // stays at its default.
            note: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--api-format") && msg.contains("--key"),
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
            no_key: true,
            headers: vec![],
            spec: None,
            compat: None,
            template: None,
            id: Some("ollama-llama3.1".into()),
            model: Some("llama3.1".into()),
            display_name: None,
            custom: false,
            api_format: Some("openai_completions".into()),
            base_url: Some("http://localhost:11434/v1".into()),
            key: Some("ignored".into()),
            credential_id: None,
            context_window: None,
            max_output_tokens: None,
            dry_run: false,
            // Phase 2 of `feature/multi-model-subagents` —
            // tests don't exercise the note flag, so the field
            // stays at its default.
            note: None,
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
            no_key: false,
            headers: vec![],
            spec: None,
            compat: None,
            template: None,
            id: Some("anthropic-claude-3-5-haiku-latest".into()),
            model: Some("claude-3-5-haiku-latest".into()),
            display_name: None,
            custom: false,
            api_format: Some("anthropic_messages".into()),
            base_url: Some("https://api.anthropic.com".into()),
            key: None,
            credential_id: Some("no-such-credential".into()),
            context_window: None,
            max_output_tokens: None,
            dry_run: false,
            // Phase 2 of `feature/multi-model-subagents` —
            // tests don't exercise the note flag, so the field
            // stays at its default.
            note: None,
        };
        let err = add_cmd(args, &paths).await.unwrap_err();
        assert!(
            err.to_string().contains("credential not found"),
            "expected missing-credential rejection, got: {err}"
        );
    }

    // ----------------------------------------------------------------
    // PR 5 — `model show | compare | search`, `--json`, `--dry-run`,
    //        `--copy-as-cli`
    // ----------------------------------------------------------------

    /// Seed two models (one vision-capable via claude-sonnet-4-5,
    /// one text-only via ollama's llama3.1) so the test surface
    /// has both populated and absent `spec` shapes to filter
    /// against. Returns the `GlobalPaths` for downstream tests.
    async fn seed_two_models() -> GlobalPaths {
        let paths = fresh_paths();
        add_cmd(
            AddArgs {
                no_key: false, headers: vec![], spec: Some(r#"{"image_input":true,"tool_support":"function_calling","thinking":"optional"}"#.into()), compat: None,
                template: None,
                id: Some("anthropic-claude-sonnet-4-5".into()),
                model: Some("claude-sonnet-4-5".into()),
                display_name: None,
                custom: false,
                api_format: Some("anthropic_messages".into()),
                base_url: Some("https://api.anthropic.com".into()),
                key: Some("sk-ant-test-key".into()),
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: false,
                // Phase 2 — tests don't exercise the note flag,
                // so the field stays at its default.
                note: None,
            },
            &paths,
        )
        .await
        .expect("anthropic add should succeed");
        add_cmd(
            AddArgs {
                no_key: true,
                headers: vec![],
                spec: None,
                compat: None,
                template: None,
                id: Some("ollama-llama3.1".into()),
                model: Some("llama3.1".into()),
                display_name: None,
                custom: false,
                api_format: Some("openai_completions".into()),
                base_url: Some("http://localhost:11434/v1".into()),
                key: None,
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: false,
                // Phase 2 — tests don't exercise the note flag,
                // so the field stays at its default.
                note: None,
            },
            &paths,
        )
        .await
        .expect("ollama add should succeed");
        paths
    }

    #[tokio::test]
    async fn show_human_readable_includes_spec_section() {
        let paths = seed_two_models().await;
        show_cmd("anthropic-claude-sonnet-4-5", &paths, false, false)
            .await
            .expect("show should succeed");
    }

    #[tokio::test]
    async fn show_unknown_id_errors_with_actionable_message() {
        let paths = seed_two_models().await;
        let err = show_cmd("not-a-model", &paths, false, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("model not found"),
            "expected not-found rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn show_json_emits_model_summary_wire_shape() {
        let paths = seed_two_models().await;
        // We can't easily capture stdout from the CLI handlers,
        // but we can at least exercise the wire-shape builder
        // directly via the helper it wraps.
        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let entry = cat
            .get("anthropic-claude-sonnet-4-5")
            .await
            .expect("seeded");
        let wire = ModelSummaryWire::from_config(&entry);
        let s = serde_json::to_string(&wire).unwrap();
        assert!(s.contains("\"id\":\"anthropic-claude-sonnet-4-5\""));
        assert!(s.contains("\"apiFormat\":\"anthropic_messages\""));
        // spec is populated by PR 1 backfill — vision-capable
        assert!(s.contains("\"imageInput\":true"));
        assert!(s.contains("\"toolSupport\":\"function_calling\""));
    }

    // ─── Phase 2 of `feature/multi-model-subagents`: `--note` ──

    /// `--note` is threaded through `peko model add` and round-trips
    /// to disk + the wire projection.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn add_with_note_persists_and_round_trips() {
        let paths = fresh_paths();
        add_cmd(
            AddArgs {
                no_key: false,
                headers: vec![],
                spec: None,
                compat: None,
                template: None,
                id: Some("anthropic-claude-3-5-haiku-latest".into()),
                model: Some("claude-3-5-haiku-latest".into()),
                display_name: None,
                custom: false,
                api_format: Some("anthropic_messages".into()),
                base_url: Some("https://api.anthropic.com".into()),
                key: Some("sk-ant-test-key".into()),
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: false,
                note: Some("very cheap, use it for cron".into()),
            },
            &paths,
        )
        .await
        .expect("add should succeed");

        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let entry = cat
            .get("anthropic-claude-3-5-haiku-latest")
            .await
            .expect("entry exists");
        assert_eq!(
            entry.note.as_deref(),
            Some("very cheap, use it for cron"),
            "note must round-trip to disk"
        );

        // Wire projection also exposes the note.
        let wire = ModelSummaryWire::from_config(&entry);
        let s = serde_json::to_string(&wire).unwrap();
        assert!(s.contains("\"note\":\"very cheap, use it for cron\""));
    }

    /// `--note ""` clears the note via the edit path.
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn edit_with_empty_note_clears_existing_note() {
        let paths = fresh_paths();
        add_cmd(
            AddArgs {
                no_key: false,
                headers: vec![],
                spec: None,
                compat: None,
                template: None,
                id: Some("anthropic-claude-3-5-haiku-latest".into()),
                model: Some("claude-3-5-haiku-latest".into()),
                display_name: None,
                custom: false,
                api_format: Some("anthropic_messages".into()),
                base_url: Some("https://api.anthropic.com".into()),
                key: Some("sk-ant-test-key".into()),
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: false,
                note: Some("very cheap, use it for cron".into()),
            },
            &paths,
        )
        .await
        .expect("add succeeds");

        edit_cmd(
            EditArgs {
                id: "anthropic-claude-3-5-haiku-latest".into(),
                note: Some(String::new()),
                dry_run: false,
            },
            &paths,
        )
        .await
        .expect("edit succeeds");

        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let entry = cat
            .get("anthropic-claude-3-5-haiku-latest")
            .await
            .expect("entry exists");
        assert!(entry.note.is_none(), "empty string must clear the note");
    }

    /// `--note` longer than 500 chars is rejected by `edit_cmd` (the
    /// catalog validator enforces the cap).
    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn edit_with_overlong_note_errors() {
        let paths = fresh_paths();
        add_cmd(
            AddArgs {
                no_key: false,
                headers: vec![],
                spec: None,
                compat: None,
                template: None,
                id: Some("anthropic-claude-3-5-haiku-latest".into()),
                model: Some("claude-3-5-haiku-latest".into()),
                display_name: None,
                custom: false,
                api_format: Some("anthropic_messages".into()),
                base_url: Some("https://api.anthropic.com".into()),
                key: Some("sk-ant-test-key".into()),
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: false,
                note: None,
            },
            &paths,
        )
        .await
        .expect("add succeeds");

        let too_long: String =
            std::iter::repeat_n('x', peko_providers::catalog::NOTE_MAX_CHARS + 1).collect();
        let err = edit_cmd(
            EditArgs {
                id: "anthropic-claude-3-5-haiku-latest".into(),
                note: Some(too_long),
                dry_run: false,
            },
            &paths,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("500"),
            "expected 500-char message, got: {err}"
        );
    }

    /// `truncate_note` shortens long strings with `…`.
    #[test]
    fn truncate_note_shortens_with_ellipsis() {
        let s = "a".repeat(200);
        let t = truncate_note(&s, 10);
        assert!(t.ends_with('…'));
        // 9 chars + ellipsis char
        assert!(t.chars().count() <= 11);
    }

    #[tokio::test]
    async fn copy_as_cli_emits_recreatable_add_command() {
        // Construct a synthetic ModelConfig and verify the
        // rendered command round-trips into another add_cmd.
        let entry = peko_providers::catalog::ModelConfig::new(
            "anthropic-claude-sonnet-4-5",
            peko_providers::catalog::ApiFormat::AnthropicMessages,
            "https://api.anthropic.com",
            "claude-sonnet-4-5",
        );
        let cmd = render_add_command(&entry);
        assert!(cmd.starts_with("peko model add --api-format anthropic_messages "));
        assert!(cmd.contains("--id anthropic-claude-sonnet-4-5"));
        assert!(cmd.contains("--model claude-sonnet-4-5"));
        assert!(
            !cmd.contains("--key"),
            "credential material must never appear in --copy-as-cli output"
        );
    }

    #[tokio::test]
    async fn compare_requires_at_least_two_ids() {
        let paths = seed_two_models().await;
        let err = compare_cmd(&["anthropic-claude-sonnet-4-5".to_string()], &paths, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("at least 2"),
            "expected 2-id minimum rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn compare_renders_matrix_for_two_models() {
        let paths = seed_two_models().await;
        compare_cmd(
            &[
                "anthropic-claude-sonnet-4-5".to_string(),
                "ollama-llama3.1".to_string(),
            ],
            &paths,
            false,
        )
        .await
        .expect("compare should succeed");
    }

    #[tokio::test]
    async fn compare_json_emits_field_and_row_arrays() {
        let paths = seed_two_models().await;
        // Exercise `field_value` and the row builder directly.
        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let a = cat.get("anthropic-claude-sonnet-4-5").await.unwrap();
        let b = cat.get("ollama-llama3.1").await.unwrap();
        assert_eq!(field_value(&a, "api_format"), "anthropic_messages");
        assert_eq!(field_value(&b, "api_format"), "openai_completions");
        // Vision-capable sonnet (PR 1 backfilled spec) vs
        // ollama-llama3.1 (not in the PR 1 backfill set, so
        // spec is None).
        assert_eq!(field_value(&a, "image_input"), "true");
        assert_eq!(field_value(&b, "image_input"), "—");
        assert_eq!(field_value(&a, "tool_support"), "function_calling");
        assert_eq!(field_value(&b, "tool_support"), "—");
    }

    #[tokio::test]
    async fn search_refuses_without_predicates() {
        let paths = seed_two_models().await;
        let err = search_cmd(
            SearchArgs {
                vision: false,
                no_vision: false,
                tools: false,
                no_tools: false,
                thinking: false,
                json_mode: false,
                priced: false,
                no_key: false,
                enabled: false,
                disabled: false,
                contains: None,
                json: false,
            },
            &paths,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("at least one predicate"),
            "expected bare-invocation rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn search_vision_filters_to_spec_image_input() {
        let paths = seed_two_models().await;
        // We exercise the predicate helper directly because
        // search_cmd prints to stdout rather than returning data.
        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let sonnet = cat.get("anthropic-claude-sonnet-4-5").await.unwrap();
        let llama = cat.get("ollama-llama3.1").await.unwrap();
        let vision_args = SearchArgs {
            vision: true,
            no_vision: false,
            tools: false,
            no_tools: false,
            thinking: false,
            json_mode: false,
            priced: false,
            no_key: false,
            enabled: false,
            disabled: false,
            contains: None,
            json: false,
        };
        assert!(
            matches_predicate(&sonnet, &vision_args, None),
            "sonnet must match --vision"
        );
        assert!(
            !matches_predicate(&llama, &vision_args, None),
            "ollama-llama3.1 must NOT match --vision (no spec or text-only)"
        );
    }

    #[tokio::test]
    async fn search_no_key_matches_local_endpoints() {
        let paths = seed_two_models().await;
        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let sonnet = cat.get("anthropic-claude-sonnet-4-5").await.unwrap();
        let llama = cat.get("ollama-llama3.1").await.unwrap();
        let args = SearchArgs {
            vision: false,
            no_vision: false,
            tools: false,
            no_tools: false,
            thinking: false,
            json_mode: false,
            priced: false,
            no_key: true,
            enabled: false,
            disabled: false,
            contains: None,
            json: false,
        };
        assert!(!matches_predicate(&sonnet, &args, None));
        assert!(matches_predicate(&llama, &args, None));
    }

    #[tokio::test]
    async fn search_contains_is_case_insensitive_substring() {
        let paths = seed_two_models().await;
        let cat = peko_providers::catalog::ModelCatalog::load_or_init(
            &paths
                .config_dir
                .join(peko_providers::catalog::ModelCatalog::FILENAME),
        )
        .await
        .unwrap();
        let sonnet = cat.get("anthropic-claude-sonnet-4-5").await.unwrap();
        // mixed-case needle against lowercase haystack
        let args = SearchArgs {
            vision: false,
            no_vision: false,
            tools: false,
            no_tools: false,
            thinking: false,
            json_mode: false,
            priced: false,
            no_key: false,
            enabled: false,
            disabled: false,
            contains: Some("SONNET".into()),
            json: false,
        };
        assert!(
            matches_predicate(&sonnet, &args, Some("sonnet")),
            "--contains SONNET must match an entry whose id lowercases to sonnet"
        );
    }

    #[tokio::test]
    async fn dry_run_add_does_not_persist_to_catalog_or_vault() {
        let paths = fresh_paths();
        add_cmd(
            AddArgs {
                no_key: false, headers: vec![], spec: Some(r#"{"image_input":true,"tool_support":"function_calling","thinking":"optional"}"#.into()), compat: None,
                template: None,
                id: Some("anthropic-claude-sonnet-4-5".into()),
                model: Some("claude-sonnet-4-5".into()),
                display_name: None,
                custom: false,
                api_format: Some("anthropic_messages".into()),
                base_url: Some("https://api.anthropic.com".into()),
                // NB: this string is never persisted under
                // --dry-run. The vault write happens AFTER the
                // dry-run short-circuit returns Ok.
                key: Some("sk-DRY-RUN-NEVER-WRITE".into()),
                credential_id: None,
                context_window: None,
                max_output_tokens: None,
                dry_run: true,
                // Phase 2 — tests don't exercise the note flag,
                // so the field stays at its default.
                note: None,
            },
            &paths,
        )
        .await
        .expect("dry-run add should succeed without side effects");

        // Catalog must remain empty — dry-run must not write.
        let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
        let cat = ModelCatalog::load_or_init(&cat_path).await.unwrap();
        assert_eq!(
            cat.list_all().await.len(),
            0,
            "dry-run must not add to catalog"
        );

        // Vault must remain empty — the key above must not be
        // stored. We load the vault directly; no entry should
        // exist under namespace `llm`.
        let vault =
            Vault::load(paths.resolver().vault()).expect("vault load should succeed even empty");
        assert!(
            vault
                .list_credentials(&peko_core::common::vault::CredentialFilter::default())
                .is_empty(),
            "dry-run must not write the --key to the vault"
        );
    }

    #[tokio::test]
    async fn dry_run_remove_reports_without_persisting() {
        let paths = seed_two_models().await;
        remove_cmd("anthropic-claude-sonnet-4-5", &paths, true)
            .await
            .expect("dry-run remove should succeed");
        // Entry must still be present.
        let cat = ModelCatalog::load_or_init(&paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .unwrap();
        assert!(
            cat.get("anthropic-claude-sonnet-4-5").await.is_some(),
            "dry-run must not actually remove the entry"
        );
    }
    fn generic_args() -> AddArgs {
        AddArgs {
            api_format: Some("responses".into()),
            base_url: Some("https://example.com/v1".into()),
            model: Some("new-model".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn generic_add_defaults_id_and_preserves_optional_metadata() {
        let paths = fresh_paths();
        let mut args = generic_args();
        args.no_key = true;
        args.headers = vec!["X-Organization=example".into()];
        args.context_window = Some(123_456);
        args.max_output_tokens = Some(4096);
        args.spec = Some(
            r#"{"tool_support":"function_calling","pricing":{"input_per_million":0.5}}"#.into(),
        );
        add_cmd(args, &paths).await.unwrap();
        let cat = open_catalog(&paths).await.unwrap();
        let config = cat.get("new-model").await.unwrap();
        assert_eq!(config.api_format, ApiFormat::OpenAiResponses);
        assert_eq!(config.template_id, None);
        assert!(!config.requires_key);
        assert_eq!(config.headers.get("X-Organization").unwrap(), "example");
        assert_eq!(config.context_window, Some(123_456));
        assert_eq!(
            config.spec.unwrap().tool_support,
            ToolSupport::FunctionCalling
        );
        assert!(config.credential_id.is_none());
    }

    #[tokio::test]
    #[serial_test::serial(vault_passphrase)]
    async fn invalid_add_and_duplicate_id_do_not_write_credentials() {
        let paths = fresh_paths();
        let vault = Vault::load(paths.resolver().vault()).unwrap();
        let before = vault.list_credentials(&Default::default()).len();
        for invalid in ["bad-url", "long-note", "bad-header", "zero-limit"] {
            let mut args = generic_args();
            args.key = Some("sk-test-must-not-be-stored".into());
            match invalid {
                "bad-url" => args.base_url = Some("file:///tmp/model".into()),
                "long-note" => args.note = Some("x".repeat(501)),
                "bad-header" => args.headers = vec!["Bad Header=value".into()],
                _ => args.context_window = Some(0),
            }
            assert!(add_cmd(args, &paths).await.is_err());
        }
        let mut original = generic_args();
        original.no_key = true;
        add_cmd(original, &paths).await.unwrap();
        let mut duplicate = generic_args();
        duplicate.key = Some("sk-test-must-not-be-stored".into());
        assert!(add_cmd(duplicate, &paths)
            .await
            .unwrap_err()
            .to_string()
            .contains("already exists"));
        let vault = Vault::load(paths.resolver().vault()).unwrap();
        assert_eq!(vault.list_credentials(&Default::default()).len(), before);
        assert_eq!(
            open_catalog(&paths).await.unwrap().list_all().await.len(),
            1
        );
    }

    #[tokio::test]
    async fn dry_run_writes_neither_catalog_nor_vault() {
        let paths = fresh_paths();
        let mut args = generic_args();
        args.dry_run = true;
        args.key = Some("sk-dry-run".into());
        add_cmd(args, &paths).await.unwrap();
        assert!(!catalog_path(&paths).exists());
        assert!(!paths.resolver().vault().exists());
    }

    #[tokio::test]
    async fn retired_template_returns_migration_guidance() {
        let paths = fresh_paths();
        let args = AddArgs {
            template: Some("kimi".into()),
            ..Default::default()
        };
        let error = add_cmd(args, &paths).await.unwrap_err().to_string();
        assert!(error.contains("retired") && error.contains("--api-format"));
        assert!(!catalog_path(&paths).exists());
    }

    #[test]
    fn generic_flags_parse_without_custom_for_all_api_formats() {
        for format in [
            "openai_completions",
            "openai_responses",
            "anthropic_messages",
        ] {
            let cli = Cli::try_parse_from([
                "peko",
                "model",
                "add",
                "--api-format",
                format,
                "--base-url",
                "https://example.com/v1",
                "--model",
                "wire-model",
                "--no-key",
            ])
            .unwrap();
            let crate::commands::Commands::Model(ModelCommands::Add(args)) = cli.command else {
                panic!("model add")
            };
            assert!(!args.custom);
            assert!(args.no_key);
        }
    }

    #[test]
    fn copied_legacy_model_uses_explicit_settings_and_quotes_metadata() {
        let mut config = ModelConfig::new(
            "local",
            ApiFormat::OpenaiCompletions,
            "http://localhost:8080/v1",
            "new-model",
        );
        config.template_id = Some("retired-vendor".into());
        config.requires_key = false;
        config.note = Some("it's $(unsafe)".into());
        config.headers.insert("X-Test".into(), "two words".into());
        config.compat = Some(
            serde_json::from_str(r#"{"thinking_format":"DeepSeek","deferred_tools_mode":"Off"}"#)
                .unwrap(),
        );
        let command = render_add_command(&config);
        assert!(!command.contains("--template") && !command.contains("--custom"));
        assert!(command.contains("--no-key") && command.contains("--compat"));
        assert!(command.contains("--header 'X-Test=two words'"));
        assert_eq!(shell_quote("it's $(unsafe)"), "'it'\"'\"'s $(unsafe)'");
    }

    #[test]
    fn unknown_capabilities_do_not_match_negative_filters() {
        let config = ModelConfig::new(
            "unknown",
            ApiFormat::OpenaiCompletions,
            "https://example.com/v1",
            "model",
        );
        let cli = Cli::try_parse_from(["peko", "model", "search", "--no-vision"]).unwrap();
        let crate::commands::Commands::Model(ModelCommands::Search(args)) = cli.command else {
            panic!("model search")
        };
        assert!(!matches_predicate(&config, &args, None));
        let mut declared = config;
        declared.spec = Some(peko_providers::spec::ModelSpec::text_only());
        assert!(matches_predicate(&declared, &args, None));
    }
}
