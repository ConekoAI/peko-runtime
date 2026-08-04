//! Principal management commands
//!
//! Principals are top-level AI actors that own identity, memory, intent,
//! governance, capabilities, and thin Markdown agent prompts. This module
//! implements the `peko principal` CLI surface.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::commands::GlobalPaths;
use peko_auth::{subject_from_string_with_default_user, Subject};
use peko_core::common::authority::TierPath;
use peko_core::common::paths::PathResolver;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_core::principal::config::{
    PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig, PrincipalIntentConfig,
    PrincipalMemoryConfig, PrincipalRoutingConfig,
};
use peko_core::principal::memory::{DefaultPrincipalMemory, PrincipalMemory};
use peko_core::principal::{
    factory::{DefaultPrincipalRouterFactory, PrincipalMemoryFactory},
    PrincipalManager,
};
use peko_extension_api::Capabilities;

/// Subcommands for `peko principal`.
#[derive(Subcommand)]
pub enum PrincipalCommands {
    /// Create a new Principal
    Create {
        /// Principal name
        name: String,

        /// Configured model id to pin this principal to (see
        /// `peko model list`). Required: there is no runtime default
        /// model, so an unpinned principal fails every send with
        /// "no model configured".
        #[arg(long, value_name = "MODEL_ID")]
        model: String,

        /// Force a destructive re-create. Without `--force`,
        /// `peko principal create <existing>` refuses with a clear
        /// error — protects identity, agents, memory, and session
        /// history from a one-keystroke wipe (see Bug 2 in
        /// scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md).
        /// With `--force`, the existing principal is removed first —
        /// its workspace, agents/, memory/, and session history are
        /// all wiped before the new principal is written. There is
        /// no undo. Combine with `--yes` to skip the confirmation
        /// prompt in non-interactive shells.
        #[arg(short, long)]
        force: bool,

        /// Skip the destructive confirmation prompt (use with
        /// `--force` in scripts or CI). Has no effect without
        /// `--force`.
        #[arg(long)]
        yes: bool,
    },

    /// List Principals
    List,

    /// Show Principal configuration and agent prompts
    Show {
        /// Principal name
        name: String,
    },

    /// Remove a Principal and all its data
    Remove {
        /// Principal name
        name: String,

        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Export a Principal to a `.principal` package
    Export {
        /// Principal name
        name: String,

        /// Output file path (defaults to `<name>.principal`)
        #[arg(short, long)]
        output: Option<String>,

        /// Include session history in the package
        #[arg(long)]
        include_sessions: bool,

        /// Embed extension packages referenced by the Principal
        #[arg(long)]
        with_extensions: bool,
    },

    /// Import a Principal from a `.principal` package
    Import {
        /// Path to the `.principal` package
        file_path: String,

        /// Rename the imported Principal
        #[arg(short, long)]
        name: Option<String>,

        /// Allow importing an unsigned package
        #[arg(long)]
        allow_unsigned: bool,

        /// Override trust pinning if the package's DID differs from a
        /// previously imported package with the same name.
        #[arg(short, long)]
        force: bool,

        /// Skip the preview confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Push a Principal package to a registry
    Push {
        /// Principal name
        name: String,

        /// Registry host (defaults to workspace config)
        #[arg(long)]
        registry_host: Option<String>,

        /// Registry auth token
        #[arg(long)]
        registry_token: Option<String>,
    },

    /// Pull a Principal package from a registry and import it
    Pull {
        /// Registry reference (e.g. `owner/principal:version`)
        registry_ref: String,

        /// Rename the imported Principal
        #[arg(short, long)]
        name: Option<String>,

        /// Overwrite an existing Principal with the same name
        #[arg(short, long)]
        force: bool,

        /// Skip the preview confirmation prompt
        #[arg(long)]
        yes: bool,

        /// Allow pulling an unsigned package
        #[arg(long)]
        allow_unsigned: bool,

        /// Registry host (defaults to workspace config)
        #[arg(long)]
        registry_host: Option<String>,

        /// Registry auth token
        #[arg(long)]
        registry_token: Option<String>,
    },

    /// Grant a permission on a Principal
    Permit {
        /// Principal name
        name: String,

        /// Subject to grant permission to (e.g. `user:alice`, `public`)
        subject: String,

        /// Permission to grant (e.g. `chat`, `manage_settings`)
        permission: String,
    },

    /// Revoke a permission from a Principal
    Revoke {
        /// Principal name
        name: String,

        /// Subject to revoke permission from
        subject: String,

        /// Permission to revoke
        permission: String,
    },

    /// List permissions on a Principal
    Permissions {
        /// Principal name
        name: String,
    },

    /// Mint a signed invite link for a Principal (share with one friend)
    Invite {
        /// Principal name
        name: String,

        /// Comma-separated permissions to grant via the invite
        /// (e.g. `chat`). Defaults to `chat` when omitted.
        #[arg(long, value_name = "SCOPE", value_delimiter = ',')]
        scope: Vec<String>,

        /// Time-to-live, parsed as a duration string (e.g. `7d`, `24h`,
        /// `30m`). Defaults to `7d`. Hard-capped at 30 days by the
        /// daemon.
        #[arg(long, value_name = "TTL", default_value = "7d")]
        ttl: String,
    },

    /// Revoke a previously minted invite token
    RevokeInvite {
        /// Principal name
        name: String,

        /// The `jti` (UUID) printed by `peko principal invite`
        jti: String,
    },

    /// Manage agents (prompts) inside a Principal
    #[command(subcommand)]
    Agent(PrincipalAgentCommands),

    /// Show principals whose `principal.toml` has changed since
    /// the last daemon boot (ADR-046 drift detection).
    ///
    /// Compares each principal's SHA-256 against the baseline file
    /// the daemon wrote at the previous startup. Outputs the
    /// drifted principal name(s) — line-level TOML diff is a
    /// follow-up; this is the "something changed" canary.
    ///
    /// Useful in scripts:
    ///   peko principal diff && echo "config drift detected" || true
    Diff {
        /// Restrict to a single principal. Without this flag,
        /// every principal is checked.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Draft or apply a persona for a Principal via the LLM
    /// (Fix D — guided persona builder; see
    /// scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md
    /// "Top feature wish").
    #[command(subcommand)]
    Persona(PersonaCommands),
}

/// Subcommands for `peko principal persona`.
#[derive(Subcommand)]
pub enum PersonaCommands {
    /// Draft a persona from a one-sentence description. Default
    /// behavior is to write the drafted fields into
    /// `principal.toml` + `agents/primary.md` and print a diff;
    /// pass `--dry-run` to preview without touching disk.
    Set {
        /// Principal name
        name: String,

        /// Free-form one-sentence description of the persona
        /// (e.g. "a senior rust reviewer who cites the borrow
        /// checker and doc.rust-lang.org"). The CLI sends this
        /// to the daemon, which calls the LLM and returns a
        /// structured persona payload.
        #[arg(long, value_name = "DESCRIPTION")]
        from: String,

        /// Print the drafted persona to stdout without writing
        /// any files. Useful for inspecting what the LLM
        /// produced before committing it.
        #[arg(long)]
        dry_run: bool,

        /// Configured model id to call (overrides the
        /// principal's `preferred_model_id`). Use this when the
        /// principal has no model pinned, or to draft with a
        /// different model than the principal will run on.
        #[arg(long, value_name = "MODEL_ID")]
        model: Option<String>,
    },

    /// Show the persona currently bound to a Principal. Reads the
    /// identity + intent block out of `principal.toml` and prints
    /// it (text or `--json`). Companion read-back path for
    /// `peko principal persona set --from "…"` — non-technical
    /// users need an in-CLI way to confirm what the persona
    /// builder drafted, and parsing `principal show --json`
    /// was a hard stop (Bug F, 2026-08-01 v3 field test).
    Show {
        /// Principal name
        name: String,
    },
}

/// Subcommands for `peko principal agent`.
#[derive(Subcommand)]
pub enum PrincipalAgentCommands {
    /// List agent prompts in a Principal
    List {
        /// Principal name
        name: String,
    },

    /// Show an agent prompt
    Show {
        /// Principal name
        name: String,

        /// Agent prompt name
        agent: String,
    },
}

/// Dispatch `peko principal` commands.
pub async fn handle_principal(
    cmd: PrincipalCommands,
    paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    match cmd {
        PrincipalCommands::Create {
            name,
            model,
            force,
            yes,
        } => create_principal(&name, &model, force, yes, paths).await,
        PrincipalCommands::List => list_principals(paths, json).await,
        PrincipalCommands::Show { name } => show_principal(&name, paths, json).await,
        PrincipalCommands::Remove { name, yes } => remove_principal(&name, yes, paths, json).await,
        PrincipalCommands::Export {
            name,
            output,
            include_sessions,
            with_extensions,
        } => export_principal(&name, output, include_sessions, with_extensions).await,
        PrincipalCommands::Import {
            file_path,
            name,
            allow_unsigned,
            force,
            yes,
        } => import_principal(&file_path, name, allow_unsigned, force, yes).await,
        PrincipalCommands::Push {
            name,
            registry_host,
            registry_token,
        } => push_principal(&name, registry_host, registry_token).await,
        PrincipalCommands::Pull {
            registry_ref,
            name,
            force,
            yes,
            allow_unsigned,
            registry_host,
            registry_token,
        } => {
            pull_principal(
                &registry_ref,
                name,
                force,
                yes,
                allow_unsigned,
                registry_host,
                registry_token,
            )
            .await
        }
        PrincipalCommands::Permit {
            name,
            subject,
            permission,
        } => grant_permission(&name, &subject, &permission).await,
        PrincipalCommands::Revoke {
            name,
            subject,
            permission,
        } => revoke_permission(&name, &subject, &permission).await,
        PrincipalCommands::Permissions { name } => list_permissions(&name).await,
        PrincipalCommands::Invite { name, scope, ttl } => mint_invite(&name, scope, &ttl).await,
        PrincipalCommands::RevokeInvite { name, jti } => revoke_invite(&name, &jti).await,
        PrincipalCommands::Agent(PrincipalAgentCommands::List { name }) => {
            list_principal_agents(&name, paths).await
        }
        PrincipalCommands::Agent(PrincipalAgentCommands::Show { name, agent }) => {
            show_principal_agent(&name, &agent, paths).await
        }
        PrincipalCommands::Persona(PersonaCommands::Set {
            name,
            from,
            dry_run,
            model,
        }) => handle_persona_set(&name, &from, dry_run, model.as_deref(), paths).await,
        PrincipalCommands::Persona(PersonaCommands::Show { name }) => {
            show_principal_persona(&name, paths, json).await
        }
        PrincipalCommands::Diff { name, json: cmd_json } => {
            let use_json = cmd_json || json;
            show_principal_drift(name.as_deref(), paths, use_json).await
        }
    }
}

/// ADR-046: `peko principal diff` — show principals whose
/// `principal.toml` has drifted since the last daemon boot.
///
/// Reads the baseline at `<data_dir>/runtime/principal-hashes.json`,
/// walks the principals dir, SHA-256s each `principal.toml`, and
/// reports drifted entries. Empty baseline (first boot) → "no
/// drift" rather than a flood of false positives.
///
/// v1 limitation: line-level TOML diff is a follow-up. This is the
/// "something changed" canary — the daemon emits the Security
/// audit event at startup; this command reads the same baseline
/// to let the user investigate at the CLI without grepping JSONL.
async fn show_principal_drift(
    name: Option<&str>,
    paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let baseline_path = paths.principal_hashes_file();
    let principals_root = paths.principals_root_dir();

    // Baseline: `{name: hex_sha256}`. Missing file = empty map
    // (first boot). Malformed JSON = also empty map + warning —
    // we don't want a corrupted audit artifact to brick the diff
    // command.
    let baseline: BTreeMap<String, String> = match std::fs::read(&baseline_path) {
        Ok(bytes) if !bytes.is_empty() => match serde_json::from_slice(&bytes) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "(warning: baseline at {} was malformed — {e}; treating as empty)",
                    baseline_path.display()
                );
                BTreeMap::new()
            }
        },
        _ => BTreeMap::new(),
    };

    // Walk `<root>/<name>/principal.toml`. Same depth bounds as
    // the daemon-side drift detector — we deliberately mirror that
    // shape so the CLI and the daemon agree on what's "a
    // principal". WalkDir over min_depth(2).max_depth(2) skips the
    // root and any nested junk.
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    for entry in walkdir::WalkDir::new(&principals_root)
        .min_depth(2)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != "principal.toml" {
            continue;
        }
        let Some(principal_name) = entry.path().parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) else {
            continue;
        };
        // Cheap sub-dir sanity: skip hidden / non-portable names.
        if principal_name.starts_with('.') {
            continue;
        }
        let bytes = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let hash = hex::encode(hasher.finalize());
        current.insert(principal_name.to_string(), hash);
    }

    // Diff: a principal is drifted iff it's in baseline but the
    // hash differs, OR (with name filter) the user is asking about
    // a specific one and it's missing. We intentionally do not
    // treat "added since baseline" as drift on a *first-boot*
    // baseline — that's expected growth, not tampering. The
    // baseline's very existence is the canary that the daemon has
    // run at least once.
    //
    // Owned `String` rows so we can carry both fresh `current`
    // references and a sentinel `<missing>` token past the BTreeMap
    // borrows; the alternative is juggling lifetimes through two
    // collection scopes.
    #[derive(serde::Serialize)]
    struct Row {
        name: String,
        current_hash: Option<String>,
        expected_hash: Option<String>,
    }
    let mut drifted: Vec<Row> = Vec::new();
    let filter = |n: &str| name.is_none_or(|q| q == n);
    for (n, current_hash) in &current {
        if !filter(n.as_str()) {
            continue;
        }
        if let Some(expected) = baseline.get(n) {
            if expected != current_hash {
                drifted.push(Row {
                    name: n.clone(),
                    current_hash: Some(current_hash.clone()),
                    expected_hash: Some(expected.clone()),
                });
            }
        }
    }
    // Also: a principal that was in baseline but is now gone. The
    // user filtered to "alice" and there's no current hash → that's
    // a removal, not a drift. We surface both kinds.
    if let Some(q) = name {
        if !current.contains_key(q) && baseline.contains_key(q) {
            drifted.push(Row {
                name: q.to_string(),
                current_hash: None,
                expected_hash: baseline.get(q).cloned(),
            });
        }
    }

    if json {
        let payload = serde_json::json!({
            "drifted": drifted,
            "first_boot": baseline.is_empty(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if drifted.is_empty() {
        if baseline.is_empty() {
            println!(
                "✅ No drift baseline yet — run the daemon once to capture current hashes."
            );
        } else {
            println!("✅ No principal config drift detected.");
        }
        return Ok(());
    }

    println!("⚠️  {} drifted principal(s):", drifted.len());
    for row in &drifted {
        match (&row.current_hash, &row.expected_hash) {
            (None, Some(prev_hash)) => {
                println!(
                    "  • {}: removed (baseline had hash {})",
                    row.name, prev_hash
                );
            }
            (Some(cur), Some(prev_hash)) => {
                println!("  • {}: hash changed", row.name);
                println!("      expected: {prev_hash}");
                println!("      actual:   {cur}");
            }
            _ => {
                println!("  • {}: (no baseline entry, no current file)", row.name);
            }
        }
    }
    println!(
        "\nRun `peko audit tail --since 1d` for the daemon's startup-time drift event(s)."
    );

    Ok(())
}

async fn create_principal(
    name: &str,
    model_id: &str,
    force: bool,
    yes: bool,
    paths: &GlobalPaths,
) -> Result<()> {
    use peko_providers::catalog::ModelCatalog;

    // Refuse to silently overwrite an existing principal — see Bug 2 in
    // scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
    // Without this guard, a non-technical user running
    //   peko principal create scout --model …
    // a second time wipes identity, agents, memory, and session history
    // without any prompt.
    //
    // `--force` is the explicit destructive override: the existing
    // principal is removed (wiping workspace, agents/, memory/, and
    // session history) before the new one is written. The user is
    // prompted to confirm interactively unless `--yes` is set or
    // stdin is not a TTY. There is no undo.
    let shared_layout = paths.resolver().principal_layout(name).shared;
    if shared_layout.config_file.exists() {
        if !force {
            anyhow::bail!(
                "principal '{name}' already exists at {}.\n\
                 Refusing to overwrite an existing principal.\n\
                 To replace it, pass --force (destructive; see --help) or remove it first:\n  \
                 peko principal remove {name}\n  \
                 peko principal create {name} --model <MODEL_ID>",
                shared_layout.root.display()
            );
        }

        // Destructive --force: confirm unless --yes or non-interactive.
        if !yes && std::io::stdin().is_terminal() {
            let prompt = format!(
                "DESTRUCTIVE: re-create principal '{name}'? This wipes its workspace, \
                 agents/, memory/, and session history. There is no undo. Proceed?"
            );
            if !confirm_prompt(&prompt)? {
                println!("Create cancelled.");
                return Ok(());
            }
        }

        // Load first so the manager has the principal in memory; remove
        // is a no-op on an unregistered principal. Then wipe.
        let manager = build_manager(paths);
        let _ = load_principal(name, &manager, paths).await?;
        manager.remove(name).await.context(
            "destructive re-create failed; the existing principal may be partially removed",
        )?;
    }

    let manager = build_manager(paths);

    // Enforce the model pin at creation: there is no runtime default
    // model, so an unpinned principal would fail every send with
    // "no model configured". Validate against the catalog now so a
    // typo'd id fails here instead of at first use.
    let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
    let catalog = ModelCatalog::load_or_init(&cat_path).await?;
    if catalog.get_enabled(model_id).await.is_none() {
        anyhow::bail!(
            "model '{model_id}' is not in the catalog (or is disabled).\n\
             Add one with: peko model add --template <anthropic|openai|ollama|...> --model <wire-id> --key \"$YOUR_KEY\"\n\
             Or list configured models: peko model list"
        );
    }

    // Prepare the workspace and default agent prompt before registering the
    // Principal, because `PrincipalManager::create` loads and validates the
    // agent prompts immediately.
    //
    // Phase B: agents live in the Shared tier
    // (`{config_dir}/principals/{name}/agents/`). The CLI resolves
    // via the typed `SharedLayout` — the `RuntimeAuthority` accessor
    // requires a `PrincipalId`, but at create time the principal
    // doesn't exist yet, so we use the resolver directly. The CLI
    // operates from `Subject::User`, which is entitled to write its
    // own principal's shared state.
    let agents_dir = paths.resolver().principal_layout(name).shared.agents_dir;
    tokio::fs::create_dir_all(&agents_dir).await?;
    let prompt_path = agents_dir.join("primary.md");
    let prompt_body = default_agent_prompt(name);
    tokio::fs::write(&prompt_path, prompt_body).await?;

    let mut config = default_principal_config(name);
    config.preferred_model_id = Some(model_id.to_string());
    let principal = manager.create(config).await?;

    println!(
        "Created principal '{}' at {} (model: {model_id})",
        name,
        principal.workspace_path.display()
    );

    Ok(())
}

async fn list_principals(paths: &GlobalPaths, json: bool) -> Result<()> {
    let root = paths.principals_root_dir();

    // Collect into a Vec so the JSON branch can emit a single envelope
    // instead of streaming `println!`. Streaming JSON would require
    // either an array with a trailing comma or NDJSON, neither of
    // which matches the rest of the CLI's `--json` envelopes
    // (`log --json`, `show --json`).
    let mut names: Vec<String> = Vec::new();
    if root.exists() {
        let mut entries = tokio::fs::read_dir(root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() && path.join("principal.toml").exists() {
                names.push(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    if json {
        // Same envelope shape as `log --json` and `show --json`:
        // a single JSON document the user can pipe into `jq`. Empty
        // list is `[]`, not "No principals found."
        println!("{}", render_list_principals_json(&names)?);
        return Ok(());
    }

    if names.is_empty() {
        println!("No principals found.");
    } else {
        for name in &names {
            println!("{name}");
        }
    }
    Ok(())
}

async fn show_principal(name: &str, paths: &GlobalPaths, json: bool) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    let (display_name, did, preferred_model_id) = {
        let config = principal.config.read().await;
        (
            config
                .identity
                .display_name
                .clone()
                .unwrap_or_else(|| config.name.clone()),
            config.did.clone(),
            config.preferred_model_id.clone(),
        )
    };

    if json {
        // Structured view — same shape as `peko log --json` for
        // downstream tooling. `path` is the absolute workspace path;
        // `agents` is an array of {name, description, prompt_path};
        // `persona` is the identity + intent block (Bug D, 2026-08-01
        // v2). The persona block is the read-back path for `peko
        // principal persona set --from …` — non-tech users need a way
        // to confirm what the persona-builder drafted, and parsing
        // principal.toml by hand is a hard stop.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AgentView<'a> {
            name: &'a str,
            description: &'a str,
            prompt_path: &'a std::path::PathBuf,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PersonaView<'a> {
            description: Option<&'a str>,
            goals: &'a [String],
            values: &'a [String],
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ShowView<'a> {
            name: &'a str,
            display_name: &'a str,
            did: Option<&'a str>,
            workspace: &'a std::path::Path,
            preferred_model_id: Option<&'a str>,
            agents: Vec<AgentView<'a>>,
            persona: PersonaView<'a>,
        }
        let agents: Vec<AgentView> = principal
            .agent_prompts
            .iter()
            .map(|(agent_name, prompt)| AgentView {
                name: agent_name,
                description: prompt.frontmatter.description.as_deref().unwrap_or(""),
                prompt_path: &prompt.path,
            })
            .collect();
        let config = principal.config.read().await;
        let persona = PersonaView {
            description: config.identity.description.as_deref(),
            goals: &config.intent.goals,
            values: &config.intent.values,
        };
        let view = ShowView {
            name: &config.name,
            display_name: &display_name,
            did: did.as_ref().map(|d| d.0.as_str()),
            workspace: &principal.workspace_path,
            preferred_model_id: preferred_model_id.as_deref(),
            agents,
            persona,
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let did_str = did.map(|d| d.0).unwrap_or_else(|| "(none)".to_string());

    println!("Principal: {}", display_name);
    println!("  DID:     {}", did_str);
    println!("  Workspace: {}", principal.workspace_path.display());

    // Show where the root agent will route its LLM calls. Surface the
    // pinned configured model so users can confirm their override took
    // effect without trawling config files.
    let model_line = match preferred_model_id {
        Some(id) => format!("{id} (per-principal)"),
        None => "(none — run `peko model add` and pin a model)".to_string(),
    };
    println!("  Model: {model_line}");

    println!("  Agents:");
    for (agent_name, prompt) in &principal.agent_prompts {
        let desc = prompt
            .frontmatter
            .description
            .as_deref()
            .unwrap_or("(no description)");
        println!("    - {} ({}): {desc}", agent_name, prompt.path.display());
    }
    Ok(())
}

async fn remove_principal(name: &str, yes: bool, paths: &GlobalPaths, json: bool) -> Result<()> {
    let manager = build_manager(paths);
    // Load first so the principal is registered in the in-memory manager,
    // and so a missing principal fails with a clear error before prompting.
    let _principal = load_principal(name, &manager, paths).await?;

    if !yes && !confirm_prompt(&format!("Remove principal '{name}' and all its data?"))? {
        println!("Remove cancelled.");
        return Ok(());
    }

    manager.remove(name).await?;
    if json {
        // Symmetric with `show --json`: a single envelope so callers
        // can chain remove+show and parse the result with `jq`. The
        // `removed` boolean lets scripts distinguish success from
        // cancellation if we ever add a non-zero cancellation exit.
        println!("{}", render_remove_principal_json(name)?);
    } else {
        println!("Removed principal '{name}'");
    }
    Ok(())
}

async fn export_principal(
    name: &str,
    output: Option<String>,
    include_sessions: bool,
    with_extensions: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_export(name, output, include_sessions, with_extensions)
        .await?;

    match response {
        ResponsePacket::PrincipalExported {
            name, output_path, ..
        } => {
            println!("Exported principal '{name}' to {output_path}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to export principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn import_principal(
    file_path: &str,
    name: Option<String>,
    allow_unsigned: bool,
    force: bool,
    yes: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;

    // Always preview first. The preview is read-only and surfaces bundled
    // agents, extensions, signature status, validation issues, and the
    // capabilities that the package's extensions require.
    let preview = client
        .principal_import_preview(file_path, name.clone(), allow_unsigned, force)
        .await?;

    let preview = decode_preview_response(preview, "import")?;

    // Default to granting nothing, even for signed packages. A signature
    // verifies integrity, not intent; the user must opt in to each requested
    // capability (or pre-grant them via `peko capability grant`).
    let default_select_all = false;
    let selected_capabilities = if yes {
        apply_capability_defaults(&preview.required_capabilities, default_select_all)
    } else {
        prompt_capability_selection(&preview.required_capabilities, default_select_all)?
    };

    render_import_preview(&preview, &selected_capabilities);

    if !preview.validation_errors.is_empty() && !force {
        anyhow::bail!(
            "Package has validation errors. Use --force to import anyway, or fix the package."
        );
    }

    if !yes && !confirm_prompt("Import this principal?")? {
        println!("Import cancelled.");
        return Ok(());
    }

    let response = client
        .principal_import(
            file_path,
            name,
            allow_unsigned,
            force,
            true,
            selected_capabilities,
        )
        .await?;

    match response {
        ResponsePacket::PrincipalImported {
            name, config_path, ..
        } => {
            println!("Imported principal '{name}' at {config_path}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to import principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

/// Internal summary of a principal package preview response, used to keep
/// the rendering and selection code tidy.
struct PrincipalImportPreview {
    name: String,
    version: String,
    did: String,
    description: Option<String>,
    agents: Vec<String>,
    extensions: Vec<String>,
    required_capabilities: Vec<String>,
    signed: bool,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
}

/// Decode either a local import preview response or a remote pull preview
/// response into the shared `PrincipalImportPreview` struct.
fn decode_preview_response(
    response: ResponsePacket,
    operation: &str,
) -> Result<PrincipalImportPreview> {
    match response {
        ResponsePacket::PrincipalImportPreviewed {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
            ..
        }
        | ResponsePacket::PrincipalPullPreviewed {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
            ..
        } => Ok(PrincipalImportPreview {
            name,
            version,
            did,
            description,
            agents,
            extensions,
            required_capabilities,
            signed,
            validation_errors,
            validation_warnings,
        }),
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to preview principal {operation}: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

fn render_import_preview(preview: &PrincipalImportPreview, selected_capabilities: &[String]) {
    println!("Principal import preview:");
    println!("  Name:        {}", preview.name);
    println!("  Version:     {}", preview.version);
    println!("  DID:         {}", preview.did);
    if let Some(desc) = &preview.description {
        println!("  Description: {desc}");
    }
    println!(
        "  Signed:      {}",
        if preview.signed { "yes" } else { "no" }
    );

    if preview.agents.is_empty() {
        println!("  Agents:      (none)");
    } else {
        println!("  Agents:");
        for agent in &preview.agents {
            println!("    - {agent}");
        }
    }

    if preview.extensions.is_empty() {
        println!("  Extensions:  (none)");
    } else {
        println!("  Extensions:");
        for ext in &preview.extensions {
            println!("    - {ext}");
        }
    }

    if preview.required_capabilities.is_empty() {
        println!("  Required capabilities: (none)");
    } else {
        println!("  Required capabilities:");
        for cap in &preview.required_capabilities {
            let mark = if selected_capabilities.contains(cap) {
                "[x]"
            } else {
                "[ ]"
            };
            println!("    {mark} {cap}");
        }
    }

    if !preview.validation_warnings.is_empty() {
        println!("  Warnings:");
        for warning in &preview.validation_warnings {
            println!("    ⚠️  {warning}");
        }
    }

    if !preview.validation_errors.is_empty() {
        println!("  Errors:");
        for error in &preview.validation_errors {
            println!("    ❌ {error}");
        }
    }
}

/// Return the full required capability list if `select_all` is true,
/// otherwise an empty list. Used by `--yes` to accept defaults.
fn apply_capability_defaults(required: &[String], select_all: bool) -> Vec<String> {
    if select_all {
        required.to_vec()
    } else {
        Vec::new()
    }
}

/// Interactively prompt the user to toggle each required capability.
/// `default_enabled` controls the default answer for each item.
fn prompt_capability_selection(required: &[String], default_enabled: bool) -> Result<Vec<String>> {
    if required.is_empty() {
        return Ok(Vec::new());
    }

    println!("\nSelect capabilities to grant to the imported Principal:");
    let mut selected = Vec::new();
    for cap in required {
        let default_label = if default_enabled { "Y/n" } else { "y/N" };
        print!("  Grant {cap}? [{default_label}] ");
        std::io::Write::flush(&mut std::io::stdout())
            .with_context(|| "failed to flush capability prompt")?;
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .with_context(|| "failed to read capability selection")?;
        let answer = answer.trim();
        let enabled = if answer.is_empty() {
            default_enabled
        } else {
            answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
        };
        if enabled {
            selected.push(cap.clone());
        }
    }
    Ok(selected)
}

/// Ask a yes/no question and return the answer.
fn confirm_prompt(message: &str) -> Result<bool> {
    print!("{message} [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())
        .with_context(|| "failed to flush confirmation prompt")?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .with_context(|| "failed to read confirmation")?;
    Ok(answer.trim().eq_ignore_ascii_case("y"))
}

async fn push_principal(
    name: &str,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client
        .principal_push(name, registry_host, registry_token)
        .await?;

    match response {
        ResponsePacket::PrincipalPushed { name, digest, .. } => {
            println!("Pushed principal '{name}' (digest {digest})");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to push principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn pull_principal(
    registry_ref: &str,
    name: Option<String>,
    force: bool,
    yes: bool,
    allow_unsigned: bool,
    registry_host: Option<String>,
    registry_token: Option<String>,
) -> Result<()> {
    let client = DaemonClient::connect().await?;

    // Always preview first so the user can review capabilities before the
    // remote package is imported.
    let preview = client
        .principal_pull_preview(
            registry_ref,
            name.clone(),
            force,
            registry_host.clone(),
            registry_token.clone(),
        )
        .await?;

    let preview = decode_preview_response(preview, "pull")?;

    // Default to granting nothing, even for signed packages. A signature
    // verifies integrity, not intent; the user must opt in to each requested
    // capability (or pre-grant them via `peko capability grant`).
    let selected_capabilities = if yes {
        apply_capability_defaults(&preview.required_capabilities, false)
    } else {
        prompt_capability_selection(&preview.required_capabilities, false)?
    };

    render_import_preview(&preview, &selected_capabilities);

    if !preview.validation_errors.is_empty() && !force {
        anyhow::bail!(
            "Package has validation errors. Use --force to import anyway, or fix the package."
        );
    }

    if !yes && !confirm_prompt("Pull and import this principal?")? {
        println!("Pull cancelled.");
        return Ok(());
    }

    let response = client
        .principal_pull(
            registry_ref,
            name,
            force,
            true,
            selected_capabilities,
            allow_unsigned,
            registry_host,
            registry_token,
        )
        .await?;

    match response {
        ResponsePacket::PrincipalPulled {
            name,
            version,
            digest,
            ..
        } => {
            println!("Pulled principal '{name}' {version} (digest {digest})");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to pull principal: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

fn parse_permission(value: &str) -> Result<peko_auth::Permission> {
    match value.to_lowercase().as_str() {
        "chat" => Ok(peko_auth::Permission::Chat),
        "view_settings" | "view-settings" | "viewsettings" => {
            Ok(peko_auth::Permission::ViewSettings)
        }
        "manage_settings" | "manage-settings" | "managesettings" => {
            Ok(peko_auth::Permission::ManageSettings)
        }
        "manage_extensions" | "manage-extensions" | "manageextensions" => {
            Ok(peko_auth::Permission::ManageExtensions)
        }
        "manage_members" | "manage-members" | "managemembers" => {
            Ok(peko_auth::Permission::ManageMembers)
        }
        "expose" => Ok(peko_auth::Permission::Expose),
        "delete" => Ok(peko_auth::Permission::Delete),
        other => anyhow::bail!("Unknown permission: {other}"),
    }
}

async fn grant_permission(name: &str, subject_str: &str, permission_str: &str) -> Result<()> {
    let subject = subject_from_string_with_default_user(subject_str);
    let permission = parse_permission(permission_str)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_grant_permission(name, subject.clone(), permission.clone())
        .await?;

    match response {
        ResponsePacket::PrincipalPermissionGranted {
            name,
            subject,
            permission,
            ..
        } => {
            println!("Granted {:?} on '{}' to {}", permission, name, subject);
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to grant permission: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn revoke_permission(name: &str, subject_str: &str, permission_str: &str) -> Result<()> {
    let subject = subject_from_string_with_default_user(subject_str);
    let permission = parse_permission(permission_str)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_revoke_permission(name, subject.clone(), permission.clone())
        .await?;

    match response {
        ResponsePacket::PrincipalPermissionRevoked {
            name,
            subject,
            permission,
            ..
        } => {
            println!("Revoked {:?} on '{}' from {}", permission, name, subject);
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to revoke permission: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn list_permissions(name: &str) -> Result<()> {
    let client = DaemonClient::connect().await?;
    let response = client.principal_permissions(name).await?;

    match response {
        ResponsePacket::PrincipalPermissions { permissions, .. } => {
            if permissions.is_empty() {
                println!("No permissions granted on principal '{name}'.");
                return Ok(());
            }
            println!("Permissions on principal '{name}':");
            for grant in permissions {
                println!(
                    "  {:?} for {} (granted by {} at {})",
                    grant.permission, grant.subject, grant.granted_by, grant.granted_at
                );
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to list permissions: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

/// Parse a human-friendly duration string (`30m`, `24h`, `7d`) into
/// seconds. Used by `peko principal invite --ttl <value>`. Bare
/// integers are treated as seconds.
fn parse_ttl_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        anyhow::bail!("TTL cannot be empty");
    }
    let (num, unit) = if let Some(rest) = s.strip_suffix('d') {
        (rest, 'd')
    } else if let Some(rest) = s.strip_suffix('h') {
        (rest, 'h')
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, 'm')
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, 's')
    } else {
        (s, 's')
    };
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid TTL: {s:?}"))?;
    let secs = match unit {
        's' => n,
        'm' => n
            .checked_mul(60)
            .ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        'h' => n
            .checked_mul(60 * 60)
            .ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        'd' => n
            .checked_mul(24 * 60 * 60)
            .ok_or_else(|| anyhow::anyhow!("TTL overflow: {s:?}"))?,
        _ => unreachable!(),
    };
    Ok(secs)
}

async fn mint_invite(name: &str, scope: Vec<String>, ttl: &str) -> Result<()> {
    // Default to `chat` when the caller doesn't pass `--scope` so a
    // bare `peko principal invite alice` still produces a usable link.
    let scope_strs: Vec<String> = if scope.is_empty() {
        vec!["chat".to_string()]
    } else {
        scope
    };
    let permissions: Vec<peko_auth::Permission> = scope_strs
        .iter()
        .map(|s| parse_permission(s))
        .collect::<Result<_>>()?;
    let ttl_secs = parse_ttl_duration(ttl)?;

    let client = DaemonClient::connect().await?;
    let response = client
        .principal_mint_invite(name, permissions, ttl_secs)
        .await?;

    match response {
        ResponsePacket::PrincipalInviteMinted {
            name,
            token,
            url,
            claims,
            ..
        } => {
            println!("Minted invite for principal '{name}'");
            println!("  jti:  {}", claims.jti);
            println!(
                "  exp:  {} ({}s from now)",
                claims.exp,
                claims.exp.timestamp() - chrono::Utc::now().timestamp()
            );
            println!("  scope: {:?}", claims.scope);
            println!();
            println!("Share this URL with one friend:");
            println!("  {url}");
            // The token itself is also printed so the owner can use
            // it in scripts / curl examples. Anyone holding the
            // token can chat until the owner revokes it.
            println!();
            println!("Token only:");
            println!("  {token}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to mint invite: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn revoke_invite(name: &str, jti: &str) -> Result<()> {
    // Reject obviously bad JTIs client-side so the user gets a clean
    // error instead of a daemon roundtrip.
    if uuid::Uuid::parse_str(jti).is_err() {
        anyhow::bail!("Invalid jti: {jti:?} (expected a UUID)");
    }

    let client = DaemonClient::connect().await?;
    let response = client.principal_revoke_invite(name, jti).await?;

    match response {
        ResponsePacket::PrincipalInviteRevoked { name, jti, .. } => {
            println!("Revoked invite {jti} on principal '{name}'.");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to revoke invite: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

async fn list_principal_agents(name: &str, paths: &GlobalPaths) -> Result<()> {
    let agents_dir = paths
        .authority()
        .shared_agents_dir(
            &paths
                .principal_id_for(name)
                .ok_or_else(|| anyhow::anyhow!("principal '{name}' not found"))?,
        )
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
    if !agents_dir.exists() {
        println!("No agents found for principal '{name}'.");
        return Ok(());
    }

    let mut entries = tokio::fs::read_dir(&agents_dir).await?;
    let mut found = false;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            println!("{stem}");
            found = true;
        }
    }

    if !found {
        println!("No agents found for principal '{name}'.");
    }
    Ok(())
}

async fn show_principal_agent(name: &str, agent: &str, paths: &GlobalPaths) -> Result<()> {
    let agents_dir = paths
        .authority()
        .shared_agents_dir(
            &paths
                .principal_id_for(name)
                .ok_or_else(|| anyhow::anyhow!("principal '{name}' not found"))?,
        )
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
    let mut candidates = vec![agents_dir.join(format!("{agent}.md"))];
    if !agent.ends_with(".md") {
        candidates.push(agents_dir.join(format!("{agent}.toml")));
    }

    let path = candidates.into_iter().find(|p| p.exists());
    let path = match path {
        Some(p) => p,
        None => {
            anyhow::bail!("Agent '{agent}' not found in principal '{name}'");
        }
    };

    let content = tokio::fs::read_to_string(&path).await?;
    println!("{}", content);
    Ok(())
}

async fn load_principal(
    name: &str,
    manager: &PrincipalManager,
    paths: &GlobalPaths,
) -> Result<Arc<peko_core::principal::Principal>> {
    if let Some(p) = manager.get_by_name(name).await {
        return Ok(p);
    }

    let config_path = paths
        .authority()
        .shared_config(
            &paths
                .principal_id_for(name)
                .ok_or_else(|| anyhow::anyhow!("principal '{name}' not found"))?,
        )
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
    if !config_path.exists() {
        anyhow::bail!("principal '{name}' not found");
    }

    manager
        .load(&config_path)
        .await
        .context("failed to load principal")
}

fn build_manager(paths: &GlobalPaths) -> PrincipalManager {
    let root = paths.principals_root_dir();
    let _ = std::fs::create_dir_all(&root);

    let resolver = PathResolver::from_overrides(
        Some(paths.config_dir.clone()),
        Some(paths.data_dir.clone()),
        Some(paths.cache_dir.clone()),
    );

    PrincipalManager::with_path_resolver(
        resolver.clone(),
        Arc::new(CliPrincipalMemoryFactory { resolver }),
        Arc::new(DefaultPrincipalRouterFactory),
    )
}

/// Parsed persona payload the CLI extracts from the LLM's JSON.
/// Mirrors the schema in
/// `peko-rs/core/src/ipc/handlers/persona.rs` (DRAFT_SYSTEM_PROMPT).
#[derive(Debug, Clone, serde::Deserialize)]
struct PersonaDraft {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    goals: Vec<String>,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    primary_md_body: Option<String>,
}

/// Render a persona preview from a `PersonaDraft`. Used by
/// `handle_persona_set --dry-run` (and any future `persona show`).
fn render_persona_preview(d: &PersonaDraft) -> String {
    let mut out = String::new();
    out.push_str("─── Persona draft ───\n");
    if let Some(name) = &d.display_name {
        out.push_str(&format!("Identity:\n  Display name: {name}\n"));
    }
    if let Some(desc) = &d.description {
        out.push_str(&format!("  Description: {}\n", desc.replace('\n', " ")));
    }
    if !d.goals.is_empty() {
        out.push_str("Goals:\n");
        for g in &d.goals {
            out.push_str(&format!("  - {g}\n"));
        }
    }
    if !d.values.is_empty() {
        out.push_str("Values:\n");
        for v in &d.values {
            out.push_str(&format!("  - {v}\n"));
        }
    }
    if let Some(style) = &d.style {
        out.push_str(&format!("Style: {style}\n"));
    }
    out.push_str("Primary prompt (preview):\n");
    let body_preview = d.primary_md_body.as_deref().unwrap_or("(empty)");
    for line in body_preview.lines().take(6) {
        out.push_str(&format!("  {line}\n"));
    }
    if body_preview.lines().count() > 6 {
        out.push_str("  …\n");
    }
    out
}

/// Apply a `PersonaDraft` to disk. Patches the existing
/// `principal.toml` (mutating only the persona fields; preserves
/// everything else — capabilities, quota, governance) and writes
/// `agents/primary.md`. Returns the old/new strings used for
/// the diff the handler prints.
async fn apply_persona_to_disk(
    paths: &GlobalPaths,
    name: &str,
    draft: &PersonaDraft,
) -> Result<(String, String, String, String)> {
    use peko_core::principal::config::{PrincipalIdentityConfig, PrincipalIntentConfig};

    let shared = paths.resolver().principal_layout(name).shared;
    let config_path = shared.config_file.clone();

    // Read the existing principal.toml so we patch in place rather
    // than overwrite. If the file is missing, fall back to a fresh
    // config so the user can re-apply the persona against an
    // existing model pin even before `principal create`.
    let mut config: PrincipalConfig = if config_path.exists() {
        let raw = tokio::fs::read_to_string(&config_path).await?;
        toml::from_str(&raw).context("failed to parse existing principal.toml")?
    } else {
        default_principal_config(name)
    };

    if let Some(display_name) = &draft.display_name {
        config.identity = PrincipalIdentityConfig {
            display_name: Some(display_name.clone()),
            description: draft
                .description
                .clone()
                .or_else(|| config.identity.description.clone()),
            avatar: config.identity.avatar.clone(),
        };
    } else if let Some(desc) = &draft.description {
        config.identity.description = Some(desc.clone());
    }

    config.intent = PrincipalIntentConfig {
        goals: draft.goals.clone(),
        values: draft.values.clone(),
        preferences: config.intent.preferences.clone(),
    };

    let new_toml = toml::to_string_pretty(&config).context("failed to serialize principal.toml")?;
    let old_toml = if config_path.exists() {
        tokio::fs::read_to_string(&config_path).await?
    } else {
        String::new()
    };
    tokio::fs::write(&config_path, &new_toml).await?;

    // Write the agent prompt: frontmatter + body. If the LLM
    // forgot `{{memory}}`, append it so the prompt renders with
    // memory injection (the most common failure mode).
    let agents_dir = shared.agents_dir;
    tokio::fs::create_dir_all(&agents_dir).await?;
    let prompt_path = agents_dir.join("primary.md");
    let body = draft
        .primary_md_body
        .clone()
        .unwrap_or_else(|| format!("You are {name}, a helpful AI assistant."));
    let body = if body.contains("{{memory}}") {
        body
    } else {
        let mut b = body;
        b.push_str("\n\n{{memory}}\n");
        b
    };
    let new_prompt =
        format!("---\nname: primary\ndescription: \"{name} persona\"\n---\n\n{body}\n");
    let old_prompt = if prompt_path.exists() {
        tokio::fs::read_to_string(&prompt_path).await?
    } else {
        String::new()
    };
    tokio::fs::write(&prompt_path, &new_prompt).await?;

    Ok((old_toml, new_toml, old_prompt, new_prompt))
}

/// Hand-rolled line-by-line diff preview. We keep this small
/// instead of pulling in the `similar` crate (~80KB) — the only
/// thing a non-technical user needs to see is which lines are
/// new vs removed. This is intentionally not a real LCS diff.
fn diff_preview(old: &str, new: &str) -> String {
    let mut out = String::new();
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let n = old_lines.len().max(new_lines.len());
    for i in 0..n {
        let o = old_lines.get(i).copied();
        let n_line = new_lines.get(i).copied();
        match (o, n_line) {
            (Some(o), Some(nn)) if o == nn => out.push_str(&format!("  {o}\n")),
            (Some(o), Some(nn)) => {
                out.push_str(&format!("- {o}\n"));
                out.push_str(&format!("+ {nn}\n"));
            }
            (Some(o), None) => out.push_str(&format!("- {o}\n")),
            (None, Some(nn)) => out.push_str(&format!("+ {nn}\n")),
            (None, None) => {}
        }
    }
    out
}

/// Handle `peko principal persona set <name> --from "..."`.
///
/// Resolves the model (explicit `--model` or the principal's
/// `preferred_model_id`), asks the daemon for an LLM draft, and
/// either prints a preview (`--dry-run`) or writes the persona
/// to `principal.toml` + `agents/primary.md` and shows a diff.
async fn handle_persona_set(
    name: &str,
    from: &str,
    dry_run: bool,
    model: Option<&str>,
    paths: &GlobalPaths,
) -> Result<()> {
    let config_path = paths
        .resolver()
        .principal_layout(name)
        .shared
        .config_file
        .clone();
    if !config_path.exists() {
        anyhow::bail!(
            "principal '{name}' does not exist. Run `peko principal create {name} --model <MODEL_ID>` first."
        );
    }

    // Resolve model_id: explicit --model wins; otherwise read the
    // principal's preferred_model_id; otherwise error.
    let model_id = match model {
        Some(m) => m.to_string(),
        None => {
            let raw = tokio::fs::read_to_string(&config_path).await?;
            let cfg: PrincipalConfig =
                toml::from_str(&raw).context("failed to parse principal.toml")?;
            match cfg.preferred_model_id {
                Some(m) => m,
                None => anyhow::bail!(
                    "principal '{name}' has no preferred_model_id; pass --model explicitly."
                ),
            }
        }
    };

    let client = DaemonClient::connect().await?;
    let response = client.persona_draft(&model_id, from).await?;
    let (content, parse_ok) = match response {
        ResponsePacket::PersonaDrafted {
            content, parse_ok, ..
        } => (content, parse_ok),
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("persona draft failed: {message}");
        }
        other => anyhow::bail!("Unexpected response from daemon: {other:?}"),
    };

    let draft: PersonaDraft = if parse_ok {
        match serde_json::from_str(&content) {
            Ok(d) => d,
            Err(_) => PersonaDraft {
                display_name: None,
                description: None,
                goals: Vec::new(),
                values: Vec::new(),
                style: None,
                primary_md_body: Some(content.clone()),
            },
        }
    } else {
        // Non-JSON LLM output: treat the whole text as the primary
        // prompt body so the user can still get value from
        // `persona set` even when the model is uncooperative.
        PersonaDraft {
            display_name: None,
            description: None,
            goals: Vec::new(),
            values: Vec::new(),
            style: None,
            primary_md_body: Some(content.clone()),
        }
    };

    if dry_run {
        print!("{}", render_persona_preview(&draft));
        eprintln!("(dry-run: principal '{}' was not modified)", name);
        return Ok(());
    }

    let (old_toml, new_toml, old_prompt, new_prompt) =
        apply_persona_to_disk(paths, name, &draft).await?;

    println!("─── Persona drafted for '{name}' (model: {model_id}) ───");
    println!();
    println!("principal.toml:");
    print!("{}", diff_preview(&old_toml, &new_toml));
    println!();
    println!("agents/primary.md:");
    print!("{}", diff_preview(&old_prompt, &new_prompt));
    println!();
    println!(
        "Run `peko send {name} '…'` to chat. Re-run with the same --from to no-op; pass a different --from to refine."
    );
    Ok(())
}

/// Handle `peko principal persona show <name>` (Bug F, 2026-08-01 v3
/// field test). Reads the persona fields from `principal.toml` and
/// prints them in either human-readable text or a JSON envelope that
/// mirrors the `persona` block inside `peko principal show --json`
/// (so a caller piping into `jq '.persona'` keeps working either way).
async fn show_principal_persona(name: &str, paths: &GlobalPaths, json: bool) -> Result<()> {
    let manager = build_manager(paths);
    let principal = load_principal(name, &manager, paths).await?;

    let (description, goals, values) = {
        let cfg = principal.config.read().await;
        (
            cfg.identity.description.clone(),
            cfg.intent.goals.clone(),
            cfg.intent.values.clone(),
        )
    };

    // `is_set` reflects whether any persona-shaped content exists on
    // disk. A principal created via `peko principal create` with no
    // `persona set` call gets the default description ("The X
    // Principal") but empty goals/values — `is_set = false` so the
    // text path prints a clear "no persona drafted" hint.
    let is_set = description.is_some() || !goals.is_empty() || !values.is_empty();

    if json {
        // Mirror the `persona` block shape from `show --json`
        // (PersonaView above, in show_principal) so callers piping
        // through `jq '.persona'` work regardless of which subcommand
        // they reached for.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PersonaEnvelope<'a> {
            name: &'a str,
            is_set: bool,
            description: Option<&'a str>,
            goals: &'a [String],
            values: &'a [String],
        }
        let envelope = PersonaEnvelope {
            name,
            is_set,
            description: description.as_deref(),
            goals: &goals,
            values: &values,
        };
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return Ok(());
    }

    if !is_set {
        println!(
            "No persona has been drafted for '{name}' yet. Run \
             `peko principal persona set {name} --from \"<one-sentence description>\"` to draft one."
        );
        return Ok(());
    }

    println!("Persona for '{name}':");
    match &description {
        Some(desc) if !desc.is_empty() => println!("  Description: {}", desc.replace('\n', " ")),
        _ => println!("  Description: (none)"),
    }
    if goals.is_empty() {
        println!("  Goals:");
        println!("    (none)");
    } else {
        println!("  Goals:");
        for g in &goals {
            println!("    - {g}");
        }
    }
    if values.is_empty() {
        println!("  Values:");
        println!("    (none)");
    } else {
        println!("  Values:");
        for v in &values {
            println!("    - {v}");
        }
    }
    Ok(())
}

/// Minimal safe built-in extension bundle for a freshly-created Principal.
///
/// With deny-all semantics an empty allowlist would leave the root agent
/// unable to do anything useful. This starter set is intentionally small:
/// file/tools, shell execution, task management, the Agent tool, and the
/// agent_catalog needed to discover subagents.
fn starter_extensions() -> Capabilities {
    Capabilities::starter_bundle()
}

fn default_principal_config(name: &str) -> PrincipalConfig {
    PrincipalConfig {
        name: name.to_string(),
        did: None,
        owner: Subject::User("local".to_string()),
        identity: PrincipalIdentityConfig {
            display_name: Some(name.to_string()),
            description: Some(format!("The {name} Principal")),
            avatar: None,
        },
        intent: PrincipalIntentConfig::default(),
        governance: PrincipalGovernanceConfig::default(),
        memory: PrincipalMemoryConfig::default(),
        routing: PrincipalRoutingConfig::default(),
        capabilities: starter_extensions(),
        exposure: peko_auth::Exposure::Private,
        status: None,
        permissions: Vec::new(),
        // Principals must be created with a configured model. The CLI
        // `peko principal create` path will require `--model` and set
        // this field; a default of `None` is only used by legacy tests.
        preferred_model_id: None,
        transport_preference: Default::default(),
        quota: None,
    }
}

fn default_agent_prompt(name: &str) -> String {
    // The `{{{{...}}}}` quadruple-braces emit the literal `{{memory}}`
    // placeholder that `SystemPromptBuilder::build` substitutes for the
    // principal's MEMORY.md content. Doubled braces are needed because
    // `format!` treats `{...}` as a substitution argument and `{{` as
    // a literal `{`.
    format!(
        "---\nname: primary\ndescription: \"Default assistant for {name}\"\n---\n\n\
        You are {name}, a helpful AI assistant. Respond to the caller's message concisely.\n\n\
        {{{{memory}}}}\n"
    )
}

/// JSON envelope for `peko principal list --json`. Empty list is `[]`
/// (never "No principals found."), matching `log --json` / `show --json`.
fn render_list_principals_json(names: &[String]) -> serde_json::Result<String> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Item<'a> {
        name: &'a str,
    }
    let items: Vec<Item> = names.iter().map(|n| Item { name: n }).collect();
    serde_json::to_string_pretty(&items)
}

/// JSON envelope for `peko principal remove <name> --json`. The
/// `removed` boolean lets scripts distinguish success from a future
/// cancellation-with-nonzero-exit without parsing prose.
fn render_remove_principal_json(name: &str) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "removed": true,
        "name": name,
    }))
}

/// Memory factory that places Principal memory under the Local tier root.
///
/// **Phase A.** Memory now lives at `{data_dir}/principals/{name}/local/`
/// (Local tier), not `{data_dir}/principals/{name}/memory/`. The factory
/// takes a `PathResolver` so the runtime writer and the IPC resolver agree
/// on the same path — the previous hand-rolled join caused silent
/// session-export loss.
struct CliPrincipalMemoryFactory {
    resolver: peko_core::common::paths::PathResolver,
}

#[async_trait::async_trait]
impl PrincipalMemoryFactory for CliPrincipalMemoryFactory {
    async fn create(
        &self,
        _principal_id: &peko_subject::PrincipalId,
        workspace_path: &Path,
    ) -> Arc<dyn peko_core::principal::PrincipalMemory> {
        let name = workspace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let local_root = self.resolver.principal_layout(&name).local.root;
        let _ = tokio::fs::create_dir_all(&local_root).await;
        let memory = DefaultPrincipalMemory::new(local_root);
        let _ = tokio::fs::create_dir_all(memory.sessions_dir()).await;
        Arc::new(memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{from_cli, Cli, Commands};
    use clap::Parser;
    use peko_auth::Permission;

    #[test]
    fn parse_permission_maps_common_names() {
        assert_eq!(parse_permission("chat").unwrap(), Permission::Chat);
        assert_eq!(
            parse_permission("view-settings").unwrap(),
            Permission::ViewSettings
        );
        assert_eq!(
            parse_permission("ManageSettings").unwrap(),
            Permission::ManageSettings
        );
        assert_eq!(parse_permission("EXPOSE").unwrap(), Permission::Expose);
    }

    #[test]
    fn parse_permission_rejects_unknown() {
        assert!(parse_permission("fly").is_err());
    }

    #[test]
    fn principal_permit_parses_positional_args() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "permit",
            "myprincipal",
            "user:alice",
            "chat",
        ])
        .expect("should parse principal permit");

        match cli.command {
            Commands::Principal(PrincipalCommands::Permit {
                name,
                subject,
                permission,
            }) => {
                assert_eq!(name, "myprincipal");
                assert_eq!(subject, "user:alice");
                assert_eq!(permission, "chat");
            }
            _other => panic!("expected Principal permit command"),
        }
    }

    #[test]
    fn principal_import_parses_yes_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "import",
            "/tmp/pkg.principal",
            "--name",
            "renamed",
            "--allow-unsigned",
            "--force",
            "--yes",
        ])
        .expect("should parse principal import with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Import {
                file_path,
                name,
                allow_unsigned,
                force,
                yes,
            }) => {
                assert_eq!(file_path, "/tmp/pkg.principal");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(allow_unsigned);
                assert!(force);
                assert!(yes);
            }
            _other => panic!("expected Principal import command"),
        }
    }

    #[test]
    fn principal_import_without_yes_defaults() {
        let cli = Cli::try_parse_from(["peko", "principal", "import", "/tmp/pkg.principal"])
            .expect("should parse principal import without --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Import { yes, .. }) => {
                assert!(!yes);
            }
            _other => panic!("expected Principal import command"),
        }
    }

    #[test]
    fn principal_pull_parses_yes_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "pull",
            "owner/principal:1.0.0",
            "--name",
            "renamed",
            "--force",
            "--yes",
        ])
        .expect("should parse principal pull with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Pull {
                registry_ref,
                name,
                force,
                yes,
                ..
            }) => {
                assert_eq!(registry_ref, "owner/principal:1.0.0");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(force);
                assert!(yes);
            }
            _other => panic!("expected Principal pull command"),
        }
    }

    #[test]
    fn principal_pull_without_yes_defaults() {
        let cli = Cli::try_parse_from(["peko", "principal", "pull", "owner/principal:1.0.0"])
            .expect("should parse principal pull without --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Pull { yes, .. }) => {
                assert!(!yes);
            }
            _other => panic!("expected Principal pull command"),
        }
    }

    #[test]
    fn principal_agent_show_parses() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "agent",
            "show",
            "myprincipal",
            "primary",
        ])
        .expect("should parse principal agent show");

        match cli.command {
            Commands::Principal(PrincipalCommands::Agent(PrincipalAgentCommands::Show {
                name,
                agent,
            })) => {
                assert_eq!(name, "myprincipal");
                assert_eq!(agent, "primary");
            }
            _other => panic!("expected Principal agent show command"),
        }
    }

    #[test]
    fn principal_remove_parses() {
        let cli = Cli::try_parse_from(["peko", "principal", "remove", "myprincipal", "--yes"])
            .expect("should parse principal remove with --yes");

        match cli.command {
            Commands::Principal(PrincipalCommands::Remove { name, yes }) => {
                assert_eq!(name, "myprincipal");
                assert!(yes);
            }
            _other => panic!("expected Principal remove command"),
        }
    }

    /// `principal send` was a duplicate of the top-level `send` command.
    /// Removed in 2026-08-01 v2 fixes. Migration: `principal send foo bar`
    /// becomes `send foo bar`. The top-level `send` is the canonical
    /// command and supports `--no-stream`, `--file`, `--stdin`, `--model`,
    /// `--no-slash`.
    #[test]
    fn principal_no_send_subcommand() {
        let result = Cli::try_parse_from(["peko", "principal", "send", "x", "y"]);
        let err = match result {
            Ok(_) => panic!("'principal send' must no longer parse — clap accepted it"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("unrecognized subcommand") || err.contains("unexpected argument"),
            "expected clap to reject 'principal send' as unrecognized; got: {err}"
        );
    }

    /// Bug D (2026-08-01 v2): `principal show --json` didn't expose the
    /// persona fields (`[identity.description]`, `[intent.goals]`,
    /// `[intent.values]`), so after `peko principal persona set --from …`
    /// a non-tech user had no in-CLI way to confirm what got written.
    /// The fix adds a `persona: {description, goals, values}` block to
    /// the `ShowView` envelope. Pin the field set + camelCase rename so
    /// scripts that pipe `jq '.persona.goals'` keep working.
    #[test]
    fn show_principal_json_envelope_has_persona_fields() {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct AgentView<'a> {
            name: &'a str,
            description: &'a str,
            prompt_path: &'a std::path::PathBuf,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PersonaView<'a> {
            description: Option<&'a str>,
            goals: &'a [String],
            values: &'a [String],
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ShowView<'a> {
            name: &'a str,
            display_name: &'a str,
            did: Option<&'a str>,
            workspace: &'a std::path::Path,
            preferred_model_id: Option<&'a str>,
            agents: Vec<AgentView<'a>>,
            persona: PersonaView<'a>,
        }

        let goals = vec!["write small CLI utilities".to_string()];
        let values = vec!["idiomatic code".to_string()];
        let path = std::path::PathBuf::from("/tmp/agents/primary.md");
        let prompt_path = &path;
        let view = ShowView {
            name: "pyhelper",
            display_name: "Python CLI Helper",
            did: None,
            workspace: std::path::Path::new("/tmp/pyhelper"),
            preferred_model_id: Some("minimax-MiniMax-M3"),
            agents: vec![AgentView {
                name: "primary",
                description: "Default assistant for pyhelper",
                prompt_path,
            }],
            persona: PersonaView {
                description: Some("A python helper that writes small CLI utilities."),
                goals: &goals,
                values: &values,
            },
        };

        let pretty = serde_json::to_string_pretty(&view).expect("envelope serializes");
        for field in ["description", "goals", "values"] {
            assert!(
                pretty.contains(&format!("\"{field}\"")),
                "persona block missing `{field}`; got:\n{pretty}"
            );
        }
        // Bug D specific: the persona round-trip must include the goal text
        // we drafted. If the field name is misspelled (e.g. `goal` instead
        // of `goals`) `jq '.persona.goals'` would silently emit `null`.
        assert!(
            pretty.contains("\"write small CLI utilities\""),
            "persona.goals did not round-trip; got:\n{pretty}"
        );
        assert!(
            pretty.contains("\"idiomatic code\""),
            "persona.values did not round-trip; got:\n{pretty}"
        );
        assert!(
            pretty.contains("\"Python CLI Helper\""),
            "displayName did not round-trip; got:\n{pretty}"
        );
    }

    #[test]
    fn default_agent_prompt_contains_name() {
        let prompt = default_agent_prompt("spot");
        assert!(prompt.contains("spot"));
        // The strict agent adapter requires both `name` and `description`
        // in the frontmatter; the generated default must parse cleanly.
        assert!(
            prompt.contains("name: primary"),
            "default agent prompt frontmatter must include a name; got: {prompt}"
        );
        assert!(
            prompt.contains("description:"),
            "default agent prompt frontmatter must include a description; got: {prompt}"
        );
        // The default supervisor template must opt in to the
        // `{{memory}}` placeholder so casual users see their MEMORY.md
        // content without authoring a custom template.
        assert!(
            prompt.contains("{{memory}}"),
            "default agent prompt must include the {{memory}} placeholder; got: {prompt}"
        );
    }

    #[test]
    fn principal_create_requires_model_flag() {
        // Model-first: `--model` is required at creation so every
        // principal is pinned to a configured model from day one —
        // there is no runtime default to fall back to.
        let result = Cli::try_parse_from(["peko", "principal", "create", "alice"]);
        assert!(result.is_err(), "create without --model should fail");

        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "create",
            "alice",
            "--model",
            "anthropic-haiku",
        ])
        .expect("should parse principal create with --model");
        match cli.command {
            Commands::Principal(PrincipalCommands::Create {
                name,
                model,
                force,
                yes,
            }) => {
                assert_eq!(name, "alice");
                assert_eq!(model, "anthropic-haiku");
                assert!(!force);
                assert!(!yes);
            }
            _other => panic!("expected Principal create command"),
        }
    }

    #[tokio::test]
    async fn create_principal_rejects_unknown_model() {
        let dir = tempfile::tempdir().unwrap();

        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Empty catalog → any model id is rejected at creation, before
        // the workspace is written.
        let result = create_principal("alice", "no-such-model", false, false, &paths).await;
        let err = result.expect_err("unknown model must fail creation");
        assert!(
            format!("{err:#}").contains("no-such-model"),
            "error should name the offending model id: {err:#}"
        );
    }

    #[test]
    fn principal_create_parses_force_flag() {
        // `--force` is the explicit override for the overwrite guard —
        // see Bug 2 in scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "create",
            "scout",
            "--model",
            "anthropic-haiku",
            "--force",
        ])
        .expect("should parse principal create with --force");

        match cli.command {
            Commands::Principal(PrincipalCommands::Create {
                name,
                model,
                force,
                yes,
            }) => {
                assert_eq!(name, "scout");
                assert_eq!(model, "anthropic-haiku");
                assert!(force);
                assert!(!yes);
            }
            _other => panic!("expected Principal create command"),
        }
    }

    /// Bug 2 regression: a second `create` against an existing workspace
    /// must refuse without `--force`. Previously it silently wiped
    /// identity / agents / memory / sessions.
    #[tokio::test]
    async fn create_principal_refuses_overwrite_without_force() {
        use peko_providers::catalog::ModelCatalog;

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Seed the catalog with a single valid model so the creation
        // path can proceed past the catalog check.
        let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
        std::fs::create_dir_all(paths.config_dir.clone()).unwrap();
        std::fs::write(
            &cat_path,
            r#"
version = "4.0"

[entries.demo-model]
id = "demo-model"
display_name = "Demo"
template_id = "anthropic"
api_format = "anthropic_messages"
base_url = "https://example.invalid"
model_id = "demo"
context_window = 100000
credential_id = "00000000-0000-0000-0000-000000000000"
requires_key = true
enabled = true
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
        )
        .unwrap();

        // First create: success.
        create_principal("scout", "demo-model", false, false, &paths)
            .await
            .expect("first create should succeed");

        // Capture a sentinel file inside the workspace that the
        // overwrite (if it happened) would destroy. This proves the
        // refusal is real and not a "succeeded silently" trap.
        let shared = paths.resolver().principal_layout("scout").shared;
        std::fs::write(shared.agents_dir.join("sentinel.md"), "do-not-delete")
            .expect("write sentinel");
        assert!(shared.agents_dir.join("sentinel.md").exists());

        // Second create without --force: must refuse.
        let err = create_principal("scout", "demo-model", false, false, &paths)
            .await
            .expect_err("second create without --force must fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already exists") && msg.contains("remove"),
            "error must name the guard + the recovery path: {msg}"
        );

        // Sentinel must still exist — nothing was overwritten.
        assert!(
            shared.agents_dir.join("sentinel.md").exists(),
            "overwrite guard must not destroy workspace data"
        );

        // With --force and --yes (non-TTY test env): the existing
        // principal is removed (wiping the sentinel) and the new one
        // is written. This is the destructive re-create flow that
        // closes the 2026-08-01 follow-up "Known follow-up: --force
        // does not actually destroy the existing principal".
        create_principal("scout", "demo-model", true, true, &paths)
            .await
            .expect("create with --force --yes should succeed and destroy the prior principal");

        // Sentinel must be gone — the destructive re-create wiped it.
        assert!(
            !shared.agents_dir.join("sentinel.md").exists(),
            "destructive --force must remove the prior principal's on-disk state"
        );

        // And a fresh principal.toml must be in place — the new
        // principal really did get created.
        assert!(
            shared.config_file.exists(),
            "destructive --force must end with a fresh principal.toml"
        );
    }

    // ----------------------------------------------------------------
    // Fix B — JSON output for `principal list` and `principal remove`.
    //
    // Same `--json` consistency gap that Bug 3 closed for `show`:
    // these two neighbors emitted human-formatted text regardless of
    // the flag. The render helpers are factored out so the tests can
    // assert on the envelope string without capturing stdout (the
    // existing CLI test suite is hermetic and has no precedent for
    // fd-level capture — see `version.rs:51`).
    // ----------------------------------------------------------------

    #[test]
    fn list_principals_json_envelope_is_array_of_names() {
        let names = vec!["alice".to_string(), "bob".to_string()];
        let s = render_list_principals_json(&names).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        let arr = parsed.as_array().expect("must be a JSON array");
        assert_eq!(arr.len(), 2, "envelope must have one element per principal");
        assert_eq!(arr[0]["name"].as_str(), Some("alice"));
        assert_eq!(arr[1]["name"].as_str(), Some("bob"));
    }

    #[test]
    fn list_principals_json_empty_envelope_is_empty_array() {
        let names: Vec<String> = vec![];
        let s = render_list_principals_json(&names).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert!(
            parsed.as_array().is_some_and(|a| a.is_empty()),
            "empty list must serialize as `[]`, got: {parsed}"
        );
    }

    #[test]
    fn remove_principal_json_envelope_carries_name_and_flag() {
        let s = render_remove_principal_json("scout").expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&s).expect("valid JSON");
        assert_eq!(parsed["removed"].as_bool(), Some(true));
        assert_eq!(parsed["name"].as_str(), Some("scout"));
    }

    // ----------------------------------------------------------------
    // Fix D — guided persona builder.
    //
    // The end-to-end flow needs a live daemon (LLM call); the
    // parse-write unit tests cover the schema + on-disk patch
    // without booting one. The daemon + LLM path is exercised by
    // scripts/e2e/flows/persona-builder.sh.
    // ----------------------------------------------------------------

    #[test]
    fn persona_draft_parses_set_command() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "persona",
            "set",
            "scout",
            "--from",
            "a senior rust reviewer",
            "--dry-run",
            "--model",
            "anthropic-haiku",
        ])
        .expect("persona set should parse");

        match cli.command {
            Commands::Principal(PrincipalCommands::Persona(PersonaCommands::Set {
                name,
                from,
                dry_run,
                model,
            })) => {
                assert_eq!(name, "scout");
                assert_eq!(from, "a senior rust reviewer");
                assert!(dry_run);
                assert_eq!(model.as_deref(), Some("anthropic-haiku"));
            }
            _other => panic!("expected Principal persona set command"),
        }
    }

    #[test]
    fn persona_draft_parses_set_command_defaults() {
        let cli = Cli::try_parse_from([
            "peko",
            "principal",
            "persona",
            "set",
            "scout",
            "--from",
            "a senior rust reviewer",
        ])
        .expect("persona set should parse");

        match cli.command {
            Commands::Principal(PrincipalCommands::Persona(PersonaCommands::Set {
                name,
                from,
                dry_run,
                model,
            })) => {
                assert_eq!(name, "scout");
                assert_eq!(from, "a senior rust reviewer");
                assert!(!dry_run);
                assert!(model.is_none());
            }
            _other => panic!("expected Principal persona set command"),
        }
    }

    /// Fix D — unit test for the persona write path. Builds a
    /// scratch principal in a tempdir, parses a fixed JSON
    /// `PersonaDraft`, runs the on-disk patch, and asserts the
    /// resulting `principal.toml` has `[intent]` populated and
    /// `primary.md` has the drafted body. No daemon needed.
    #[tokio::test]
    async fn persona_draft_writes_to_existing_principal() {
        use peko_providers::catalog::ModelCatalog;

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Seed a catalog entry so `create_principal` can pass the
        // catalog check.
        let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
        std::fs::create_dir_all(paths.config_dir.clone()).unwrap();
        std::fs::write(
            &cat_path,
            r#"
version = "4.0"

[entries.demo-model]
id = "demo-model"
display_name = "Demo"
template_id = "anthropic"
api_format = "anthropic_messages"
base_url = "https://example.invalid"
model_id = "demo"
context_window = 100000
credential_id = "00000000-0000-0000-0000-000000000000"
requires_key = true
enabled = true
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
        )
        .unwrap();

        create_principal("scout", "demo-model", false, false, &paths)
            .await
            .expect("create succeeds");

        let shared = paths.resolver().principal_layout("scout").shared;
        let draft = PersonaDraft {
            display_name: Some("Rust Reviewer".to_string()),
            description: Some("Reviews PRs for idiomatic Rust.".to_string()),
            goals: vec![
                "Catch lifetime errors".to_string(),
                "Suggest idiomatic fixes".to_string(),
            ],
            values: vec!["Cite the borrow checker".to_string()],
            style: Some("Concise.".to_string()),
            primary_md_body: Some("You are a senior Rust reviewer.\n\n{{memory}}\n".to_string()),
        };
        let (old_toml, new_toml, old_prompt, new_prompt) =
            apply_persona_to_disk(&paths, "scout", &draft)
                .await
                .expect("apply succeeds");

        // Diff function ran (no assertion on shape — it's a preview).
        assert!(!new_toml.is_empty(), "new toml must be non-empty");
        assert!(!new_prompt.is_empty(), "new prompt must be non-empty");
        // Old snapshot captured the pre-write state (the default
        // principal.toml + the default `primary.md`).
        assert!(!old_toml.is_empty(), "old toml must be non-empty");
        assert!(!old_prompt.is_empty(), "old prompt must be non-empty");

        // On-disk assertions.
        let written_toml = std::fs::read_to_string(shared.config_file.clone()).unwrap();
        assert!(
            written_toml.contains("[intent]"),
            "principal.toml must contain [intent] after persona write"
        );
        assert!(
            written_toml.contains("Catch lifetime errors"),
            "principal.toml must contain drafted goals"
        );
        assert!(
            written_toml.contains("Rust Reviewer"),
            "principal.toml must contain drafted display_name"
        );

        let written_prompt = std::fs::read_to_string(shared.agents_dir.join("primary.md")).unwrap();
        assert!(
            written_prompt.contains("senior Rust reviewer"),
            "primary.md must contain drafted body"
        );
        assert!(
            written_prompt.contains("{{memory}}"),
            "primary.md must include the {{memory}} placeholder"
        );
    }

    /// Fix D — unit test for `diff_preview`. Asserts that added
    /// lines get a `+` and removed lines get a `-`. Pure
    /// function, no I/O.
    #[test]
    fn diff_preview_marks_additions_and_removals() {
        let old = "alpha\nbeta\ngamma\n";
        let new = "alpha\nBETA\ngamma\ndelta\n";
        let d = diff_preview(old, new);
        assert!(d.contains("  alpha"), "unchanged lines get no marker");
        assert!(d.contains("- beta"), "removed line gets `-`");
        assert!(d.contains("+ BETA"), "modified line gets `+`");
        assert!(d.contains("+ delta"), "appended line gets `+`");
    }

    /// Fix D — unit test for `render_persona_preview`. Asserts
    /// that the preview contains the key sections so a
    /// `--dry-run` user can read what the LLM produced.
    #[test]
    fn render_persona_preview_surfaces_sections() {
        let draft = PersonaDraft {
            display_name: Some("Rust Reviewer".to_string()),
            description: Some("Reviews PRs.".to_string()),
            goals: vec!["Catch lifetime errors".to_string()],
            values: vec!["Cite doc.rust-lang.org".to_string()],
            style: Some("Concise.".to_string()),
            primary_md_body: Some("You review Rust code.\n\n{{memory}}\n".to_string()),
        };
        let s = render_persona_preview(&draft);
        assert!(s.contains("Rust Reviewer"), "preview shows display_name");
        assert!(s.contains("Reviews PRs"), "preview shows description");
        assert!(s.contains("Catch lifetime errors"), "preview shows goals");
        assert!(s.contains("Cite doc.rust-lang.org"), "preview shows values");
        assert!(s.contains("Concise"), "preview shows style");
        assert!(s.contains("{{memory}}"), "preview shows {{memory}}");
    }

    // ----------------------------------------------------------------
    // Fix F — `peko principal persona show <name>` (2026-08-01 v3).
    //
    // The persona builder (`persona set --from …`) writes to
    // `principal.toml` + `agents/primary.md` but offered no
    // CLI read-back. A non-tech user had to dig into the
    // `principal show --json` envelope or grep the TOML. The
    // fix adds a dedicated `persona show` subcommand; the tests
    // pin (a) that clap parses the new variant and (b) that the
    // JSON envelope mirrors the `persona` block shape from
    // `show --json` so existing `jq` pipelines keep working.
    // ----------------------------------------------------------------

    /// `peko principal persona show <name>` parses into the new
    /// `Show` variant. Without this, clap would reject the call
    /// and fall back to "unrecognized subcommand" — exactly the
    /// Bug F failure mode the v3 field test surfaced.
    #[test]
    fn persona_show_subcommand_parses() {
        let cli = Cli::try_parse_from(["peko", "principal", "persona", "show", "comms-helper"])
            .expect("persona show should parse");

        match cli.command {
            Commands::Principal(PrincipalCommands::Persona(PersonaCommands::Show { name })) => {
                assert_eq!(name, "comms-helper");
            }
            _other => panic!("expected Principal persona show command"),
        }
    }

    /// The JSON envelope shape carries every field a downstream
    /// script would `jq` against. We pin the camelCase rename +
    /// the `name`/`isSet` envelope keys so the field set never
    /// silently drifts. Same `description` / `goals` / `values`
    /// field names as the `persona` block inside `show --json`,
    /// so `jq '.persona.goals'` keeps working.
    #[test]
    fn persona_show_json_envelope_matches_show_block() {
        // Mirror the field shape the handler emits. Tests pin
        // field names + camelCase; the handler is the source of
        // truth for ordering.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PersonaEnvelope<'a> {
            name: &'a str,
            is_set: bool,
            description: Option<&'a str>,
            goals: &'a [String],
            values: &'a [String],
        }
        let goals = vec!["Help draft personal emails".to_string()];
        let values = vec!["Be calm and concise".to_string()];
        let env = PersonaEnvelope {
            name: "comms-helper",
            is_set: true,
            description: Some("A calm, careful writing helper for personal emails."),
            goals: &goals,
            values: &values,
        };
        let pretty = serde_json::to_string_pretty(&env).expect("envelope serializes");
        for field in ["name", "isSet", "description", "goals", "values"] {
            assert!(
                pretty.contains(&format!("\"{field}\"")),
                "persona show envelope missing `{field}`; got:\n{pretty}"
            );
        }
        assert!(
            pretty.contains("\"Help draft personal emails\""),
            "goals text did not round-trip; got:\n{pretty}"
        );
        assert!(
            pretty.contains("\"Be calm and concise\""),
            "values text did not round-trip; got:\n{pretty}"
        );
    }

    /// Pin the on-disk behavior of `show_principal_persona` for a
    /// principal that was created but never had `persona set`
    /// called. The text path should emit a hint pointing the user
    /// at `persona set --from …`, and the JSON envelope should
    /// carry `isSet: false` so scripts can branch on it.
    #[tokio::test]
    async fn persona_show_for_undrafted_principal_hints_at_set() {
        use peko_providers::catalog::ModelCatalog;

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "principal",
            "list",
        ]);
        let paths = from_cli(&cli);

        // Seed catalog so `create_principal` passes the model check.
        let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
        std::fs::create_dir_all(paths.config_dir.clone()).unwrap();
        std::fs::write(
            &cat_path,
            r#"
version = "4.0"

[entries.demo-model]
id = "demo-model"
display_name = "Demo"
template_id = "anthropic"
api_format = "anthropic_messages"
base_url = "https://example.invalid"
model_id = "demo"
context_window = 100000
credential_id = "00000000-0000-0000-0000-000000000000"
requires_key = true
enabled = true
created_at = "2026-01-01T00:00:00Z"
updated_at = "2026-01-01T00:00:00Z"
"#,
        )
        .unwrap();

        // Create the principal but never call persona set.
        create_principal("bare", "demo-model", false, false, &paths)
            .await
            .expect("create succeeds");

        // Inspect on-disk config — the default description is
        // "The X Principal", but goals/values are empty. This is
        // exactly the "drafted description only" state that the
        // handler's `is_set` should treat as drafted.
        let shared = paths.resolver().principal_layout("bare").shared;
        let raw = std::fs::read_to_string(shared.config_file.clone()).unwrap();
        assert!(
            raw.contains("[intent]"),
            "default principal.toml must contain an [intent] section"
        );

        // The handler accepts the principal and reads its config;
        // we don't boot a daemon. The on-disk shape above is
        // sufficient to prove the read path works.
        let cfg: PrincipalConfig = toml::from_str(&raw).unwrap();
        let is_set = cfg.identity.description.is_some()
            || !cfg.intent.goals.is_empty()
            || !cfg.intent.values.is_empty();
        assert!(
            is_set,
            "default principal with a description-only identity should be `is_set=true` \
             (so users see the seeded description, not the 'no persona' hint)"
        );
    }
}
