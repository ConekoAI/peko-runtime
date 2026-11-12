//! Peko lifecycle management commands
//!
//! A peko (ADR-059 user-facing term; internal type: `Principal`) is a
//! top-level AI actor that owns identity, memory, intent, governance,
//! and thin Markdown role files (ADR-064). The verbs here are
//! flattened to the top level (`peko create`, `peko list`, ...).

use std::io::IsTerminal;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::commands::GlobalPaths;
use peko_auth::ActorRef;
use peko_core::common::authority::TierPath;
use peko_core::common::paths::PathResolver;
use peko_core::ipc::{DaemonClient, ResponsePacket};
use peko_core::principal::config::PrincipalConfig;
use peko_core::principal::{
    factory::{DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory},
    PrincipalManager,
};

/// Subcommands for peko lifecycle management, flattened to the top
/// level (`peko create`, ...).
#[derive(Subcommand)]
pub enum PrincipalCommands {
    /// Create a new peko and wait until it is ready
    ///
    /// Makes the peko's folder, writes its definition (from `--seed`, or
    /// defaults), starts the daemon if needed, and waits while the peko
    /// sets itself up on its first turn. Pass `--detach` to return right
    /// away (for scripts).
    ///
    /// To change a peko later, edit its `peko.toml` and `roles/root.md`;
    /// edits apply on its next turn.
    Create {
        /// Peko name
        name: String,

        /// Configured model id to pin this peko to (see
        /// `peko model list`). Optional only when the `--seed`
        /// carries `preferred_model_id`; exactly one of the
        /// two must provide it. There is no runtime default model, so
        /// an unpinned peko fails every send.
        #[arg(long, value_name = "MODEL_ID")]
        model: Option<String>,

        /// Path to a seed: a `peko.toml` with the peko's definition —
        /// `[identity]`, `[intent]`, `preferred_model_id`,
        /// `[[permissions]]`, `exposure`, `quota`, and an optional
        /// inline `persona` (Markdown added to `kb/CONVENTIONS.md`, the
        /// house rules every role follows). Fields you
        /// leave out take defaults. The new peko always gets its own
        /// identity; `id`, `did`, and `boot_state` in the seed are
        /// ignored. TOML note: top-level keys (`preferred_model_id`,
        /// `persona`) must come BEFORE the first `[table]` header.
        /// A seed may list `setup` steps under `[intent]` (skills, MCP
        /// servers, command-line tools to get ready). The new peko
        /// carries them out on its first turn, running commands on this
        /// computer as you, so they are shown and you are asked first;
        /// `--yes` accepts them.
        #[arg(short = 's', long = "seed", value_name = "SEED_TOML")]
        seed: Option<String>,

        /// Force a destructive re-create. Without `--force`,
        /// `peko create <existing>` refuses with a clear
        /// error — protects identity, roles, memory, and session
        /// history from a one-keystroke wipe.
        /// With `--force`, the existing peko is removed first —
        /// its workspace, roles/, memory/, and session history are
        /// all wiped before the new peko is written. There is
        /// no undo. Combine with `--yes` to skip the confirmation
        /// prompt in non-interactive shells.
        #[arg(long)]
        force: bool,

        /// Answer yes to what `create` would ask: the destructive
        /// `--force` confirmation, and accepting a seed's `setup`
        /// steps. For scripts and CI.
        #[arg(long)]
        yes: bool,

        /// Don't wait for the peko's first turn. It still runs, as soon
        /// as the daemon gets to it.
        #[arg(long)]
        detach: bool,

        /// Stop waiting for the peko's first turn after this many
        /// seconds. Without it, `create` waits until the turn is done
        /// and shows what the peko is doing; Ctrl-C stops the waiting,
        /// not the peko. The peko is created either way, and its first
        /// turn goes on in the daemon.
        #[arg(long, value_name = "SECS")]
        wait_timeout: Option<u64>,
    },

    /// Put a peko online on PekoHub (private by default)
    Register {
        name: String,
        #[arg(long)]
        public: bool,
    },
    /// Take a peko off PekoHub; it stays on this computer
    Unregister { name: String },
    /// List pekos
    List,

    /// Show a peko's configuration and roles
    Show {
        /// Peko name
        name: String,
    },

    /// Remove a peko and all its data
    Remove {
        /// Peko name
        name: String,

        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
    },

    /// Save a peko to a `.peko` file, to back it up or move it
    ///
    /// The file holds everything: identity, definition, memory,
    /// conversations, schedules, plans, and workspace tooling.
    Export {
        /// Peko name
        name: String,

        /// Output file path (defaults to `<name>.peko`)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Export a peko's seed: its definition without its identity,
    /// owner, permissions, or memory
    ///
    /// Share it on PekoHub, or start a new peko from it with
    /// `peko create <name> -s <file>`. The new peko is its own peko,
    /// not a copy.
    Seed {
        /// Peko name
        name: String,

        /// Write the seed here (prints it when omitted)
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Import a peko from a `.peko` package
    Import {
        /// Path to the `.peko` package
        file_path: String,

        /// Rename the imported peko
        #[arg(short, long)]
        name: Option<String>,

        /// Overwrite an existing local peko
        #[arg(short, long)]
        force: bool,

        /// Import as a new peko with its own keys and IDs. Use it to
        /// import a copy of a peko that already lives on this computer
        /// (with `--name` when the name is taken).
        #[arg(long)]
        rotate_keys: bool,

        /// Deprecated: imports display an inventory and proceed directly
        #[arg(long, hide = true)]
        yes: bool,
    },

    /// Grant a permission on a peko
    Permit {
        /// Peko name
        name: String,

        /// Who gets it: an address such as `user:hub:<account>` or
        /// `peko:local:<id>`, or `public` for everyone
        subject: String,

        /// Permission to grant (e.g. `chat`, `manage_settings`).
        /// `trusted` lets the peko use all its tools, including the
        /// shell, when it works for this person.
        permission: String,
    },

    /// Transfer a peko to a new owner (only its current owner can)
    Transfer {
        /// Peko name
        name: String,

        /// The new owner: `user:local:local` (this runtime's local user) or
        /// `user:hub:<account>`
        owner: String,
    },

    /// List the approval requests waiting for you, a peko's owner
    ///
    /// When a peko works for someone other than you and wants to do
    /// something sensitive that its POLICY.md doesn't settle, it asks you
    /// first. Decide with `peko approve` / `peko deny`.
    Approvals {
        /// Peko name
        name: String,
    },

    /// Approve a waiting request: the call runs for whoever asked
    Approve {
        /// Peko name
        name: String,
        /// Request id (`apr_…`, from `peko approvals`)
        id: String,
        /// Also allow this requester such calls from now on (adds a rule to
        /// the peko's POLICY.md)
        #[arg(long)]
        always: bool,
    },

    /// Deny a waiting request
    Deny {
        /// Peko name
        name: String,
        /// Request id (`apr_…`, from `peko approvals`)
        id: String,
    },

    /// Revoke a permission from a peko
    Revoke {
        /// Peko name
        name: String,

        /// Whose permission to revoke: an address such as
        /// `user:hub:<account>` or `peko:local:<id>`, or `public`
        subject: String,

        /// Permission to revoke
        permission: String,
    },

    /// List permissions on a peko
    Permissions {
        /// Peko name
        name: String,
    },

    /// Mint a signed invite link for a peko (share with one friend)
    Invite {
        /// Peko name
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
        /// Peko name
        name: String,

        /// The `jti` (UUID) printed by `peko invite`
        jti: String,
    },

    /// Show pekos whose `peko.toml` has changed since the daemon
    /// last started
    ///
    /// Compares each file with the copy the daemon fingerprinted when
    /// it started, and prints the names of pekos that changed.
    ///
    /// Useful in scripts:
    ///   peko diff && echo "config drift detected" || true
    Diff {
        /// Restrict to a single peko. Without this flag,
        /// every peko is checked.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Dispatch the peko lifecycle commands (the flattened top-level
/// verbs: `peko create`, `peko list`, ...).
async fn register_principal(name: &str, visibility: &str, _paths: &GlobalPaths) -> Result<()> {
    let client = DaemonClient::connect().await?;
    match client.principal_register(name, visibility).await? {
        ResponsePacket::Error { message, .. } => anyhow::bail!(message),
        response => {
            println!("{}", serde_json::to_string_pretty(&response)?);
            Ok(())
        }
    }
}

pub async fn handle_principal(
    cmd: PrincipalCommands,
    paths: &GlobalPaths,
    json: bool,
) -> Result<()> {
    match cmd {
        PrincipalCommands::Create {
            name,
            model,
            seed,
            force,
            yes,
            detach,
            wait_timeout,
        } => {
            create_principal(
                &name,
                model.as_deref(),
                seed.as_deref(),
                force,
                yes,
                detach,
                wait_timeout,
                paths,
            )
            .await
        }
        PrincipalCommands::Register { name, public } => {
            register_principal(&name, if public { "public" } else { "private" }, paths).await
        }
        PrincipalCommands::Unregister { name } => {
            register_principal(&name, "local_only", paths).await
        }
        PrincipalCommands::List => list_principals(paths, json).await,
        PrincipalCommands::Show { name } => show_principal(&name, paths, json).await,
        PrincipalCommands::Remove { name, yes } => remove_principal(&name, yes, paths, json).await,
        PrincipalCommands::Export { name, output } => export_principal(&name, output).await,
        PrincipalCommands::Seed { name, output } => export_seed(&name, output, paths),
        PrincipalCommands::Import {
            file_path,
            name,
            force,
            rotate_keys,
            yes,
        } => import_principal(&file_path, name, force, rotate_keys, yes).await,
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
        PrincipalCommands::Transfer { name, owner } => transfer_principal(&name, &owner).await,
        PrincipalCommands::Approvals { name } => list_approvals(&name, json).await,
        PrincipalCommands::Approve { name, id, always } => {
            decide_approval(&name, &id, true, always, json).await
        }
        PrincipalCommands::Deny { name, id } => {
            decide_approval(&name, &id, false, false, json).await
        }
        PrincipalCommands::Permissions { name } => list_permissions(&name).await,
        PrincipalCommands::Invite { name, scope, ttl } => mint_invite(&name, scope, &ttl).await,
        PrincipalCommands::RevokeInvite { name, jti } => revoke_invite(&name, &jti).await,
        PrincipalCommands::Diff {
            name,
            json: cmd_json,
        } => {
            let use_json = cmd_json || json;
            show_principal_drift(name.as_deref(), paths, use_json).await
        }
    }
}

/// ADR-046: `peko diff` — show principals whose
/// `peko.toml` has drifted since the last daemon boot.
///
/// Reads the baseline at `<data_dir>/runtime/principal-hashes.json`,
/// walks the principals dir, SHA-256s each `peko.toml`, and
/// reports drifted entries. Empty baseline (first boot) → "no
/// drift" rather than a flood of false positives.
///
/// v1 limitation: line-level TOML diff is a follow-up. This is the
/// "something changed" canary — the daemon emits the Security
/// audit event at startup; this command reads the same baseline
/// to let the user investigate at the CLI without grepping JSONL.
async fn show_principal_drift(name: Option<&str>, paths: &GlobalPaths, json: bool) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    let baseline_path = paths.principal_hashes_file();
    let principals_root = paths.principals_root_dir();

    // Baseline: `{name: {principal: hex_sha256, hooks, mcp}}`, as the
    // daemon writes it (an older shape was `{name: hex_sha256}`; both
    // read). Missing file = empty map (first boot). Malformed JSON = also
    // empty map + warning — we don't want a corrupted audit artifact to
    // brick the diff command.
    let baseline: BTreeMap<String, String> = match std::fs::read(&baseline_path) {
        Ok(bytes) if !bytes.is_empty() => match serde_json::from_slice(&bytes) {
            Ok(b) => definition_hashes(b),
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

    // Walk `<root>/<name>/peko.toml`. Same depth bounds as
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
        let file_name = entry.file_name();
        if file_name != peko_core::common::paths::PEKO_CONFIG_FILE
            && file_name != peko_core::common::paths::LEGACY_PEKO_CONFIG_FILE
        {
            continue;
        }
        let Some(principal_name) = entry
            .path()
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        else {
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
            println!("✅ No drift baseline yet — run the daemon once to capture current hashes.");
        } else {
            println!("✅ No peko config drift detected.");
        }
        return Ok(());
    }

    println!("⚠️  {} drifted peko(s):", drifted.len());
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
    println!("\nRun `peko audit tail --since 1d` for the daemon's startup-time drift event(s).");

    Ok(())
}

/// The `peko.toml` hash per peko from the daemon's drift baseline: each
/// entry is `{"principal": "<sha256>", ...}`, or a bare hash in the older
/// shape. Entries of any other shape are skipped.
fn definition_hashes(
    baseline: std::collections::BTreeMap<String, serde_json::Value>,
) -> std::collections::BTreeMap<String, String> {
    baseline
        .into_iter()
        .filter_map(|(name, entry)| {
            let hash = match entry {
                serde_json::Value::String(hash) => hash,
                serde_json::Value::Object(mut fields) => match fields.remove("principal")? {
                    serde_json::Value::String(hash) => hash,
                    _ => return None,
                },
                _ => return None,
            };
            Some((name, hash))
        })
        .collect()
}

/// Whether the seed's setup steps are accepted (ADR-104). The new peko
/// carries them out on its first turn, running commands and downloading
/// code as this OS user, so they are always shown; `--yes` accepts them,
/// a terminal asks, and anything else refuses.
fn accept_setup(name: &str, steps: &[String], yes: bool, interactive: bool) -> Result<bool> {
    if steps.is_empty() {
        return Ok(true);
    }
    println!(
        "This seed asks '{name}' to set up {} thing(s) on its first turn. It will run \
         commands and download code on this computer, as you:\n{}",
        steps.len(),
        peko_core::principal::seed::setup_listing(steps)
    );
    if yes {
        return Ok(true);
    }
    if !interactive {
        anyhow::bail!(
            "the seed's setup steps were not accepted. Read them above, then pass --yes \
             to accept them, or remove `setup` from the seed's [intent]."
        );
    }
    confirm_prompt("Let it carry these out?")
}

/// What to print about setup once the first turn is done (ADR-104): the
/// peko's own report, or a line saying it wrote none.
fn setup_report(workspace: &std::path::Path, steps: &[String]) -> Option<String> {
    if steps.is_empty() {
        return None;
    }
    let path = workspace.join(peko_core::principal::seed::SETUP_REPORT_PATH);
    Some(match std::fs::read_to_string(&path) {
        Ok(report) if !report.trim().is_empty() => {
            format!("Setup report ({}):\n{}", path.display(), report.trim_end())
        }
        _ => format!(
            "The peko wrote no setup report at {}. Ask it which setup steps are ready.",
            path.display()
        ),
    })
}

/// `peko create` (ADR-054, ADR-105): decide what to create, have the
/// daemon create it, and (unless `--detach`) wait for its first turn.
///
/// The daemon is the only thing that creates a peko. This command reads
/// the seed, gets the setup steps accepted, picks the model, and asks.
#[allow(clippy::too_many_arguments)]
async fn create_principal(
    name: &str,
    model_id: Option<&str>,
    seed_path: Option<&str>,
    force: bool,
    yes: bool,
    detach: bool,
    wait_timeout: Option<u64>,
    paths: &GlobalPaths,
) -> Result<()> {
    let Some(plan) = plan_create(name, model_id, seed_path, force, yes, paths).await? else {
        println!("Create cancelled.");
        return Ok(());
    };
    ensure_daemon(paths).await?;
    let client = DaemonClient::connect()
        .await
        .context("the daemon is running but its IPC socket would not connect")?;

    if plan.replace {
        match client.principal_remove(name).await? {
            peko_core::ipc::ResponsePacket::Error { message, .. } => anyhow::bail!(
                "destructive re-create failed; the existing peko may be partially removed: \
                 {message}"
            ),
            _ => println!("Removed the existing peko '{name}'"),
        }
    }

    let response = client
        .request_response(peko_core::ipc::RequestPacket::PrincipalCreate {
            request_id: 0,
            name: name.to_string(),
            description: None,
            model_id: plan.model_id.clone(),
            seed: plan.seed,
            accept_setup: plan.accept_setup,
        })
        .await?;
    match response {
        peko_core::ipc::ResponsePacket::PrincipalCreated { principal, .. } => println!(
            "Created peko '{name}' at {} (model: {})",
            principal.workspace_path, plan.model_id
        ),
        peko_core::ipc::ResponsePacket::Error { message, .. } => {
            anyhow::bail!("could not create peko '{name}': {message}")
        }
        other => anyhow::bail!("unexpected daemon response to create: {other:?}"),
    }
    wait_for_first_turn(name, paths, detach, wait_timeout).await
}

/// What `peko create` asks the daemon for.
#[derive(Debug)]
struct CreatePlan {
    model_id: String,
    /// The seed's text, as the daemon parses it.
    seed: Option<String>,
    /// The seed lists setup steps and they were accepted (ADR-104 D3).
    accept_setup: bool,
    /// `--force`: an existing peko of this name is removed first.
    replace: bool,
}

/// Everything `create` decides before the daemon is involved: the
/// overwrite guard, the seed and the acceptance of its setup steps, and
/// the model. Writes nothing. `None` means the person said no at a
/// prompt.
async fn plan_create(
    name: &str,
    model_id: Option<&str>,
    seed_path: Option<&str>,
    force: bool,
    yes: bool,
    paths: &GlobalPaths,
) -> Result<Option<CreatePlan>> {
    use peko_providers::catalog::ModelCatalog;

    // The seed is read and its setup steps accepted before anything is
    // written or wiped (ADR-104).
    let seed_text = seed_path
        .map(|path| std::fs::read_to_string(path).with_context(|| format!("read seed {path}")))
        .transpose()?;
    let seed = match (&seed_text, seed_path) {
        (Some(text), Some(path)) => Some(
            peko_core::principal::seed::parse_seed(text).with_context(|| format!("seed {path}"))?,
        ),
        _ => None,
    };
    let mut accept = false;
    if let Some(seed) = &seed {
        if !accept_setup(name, seed.setup(), yes, std::io::stdin().is_terminal())? {
            return Ok(None);
        }
        accept = !seed.setup().is_empty();
    }

    // Refuse to silently overwrite an existing peko (Bug 2 in
    // scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md).
    // `--force` is the explicit destructive override: the existing peko
    // is removed, with its folder, roles, memory and conversations,
    // before the new one is created. There is no undo.
    let shared_layout = paths.resolver().principal_layout(name).shared;
    let exists = shared_layout.config_file.exists();
    if exists && !force {
        anyhow::bail!(
            "peko '{name}' already exists at {}.\n\
             Refusing to overwrite an existing peko.\n\
             To replace it, pass --force (destructive; see --help) or remove it first:\n  \
             peko remove {name}\n  \
             peko create {name} --model <MODEL_ID>",
            shared_layout.root.display()
        );
    }
    if exists && !yes && std::io::stdin().is_terminal() {
        let prompt = format!(
            "DESTRUCTIVE: re-create peko '{name}'? This wipes its folder, \
             roles/, memory, and conversations. There is no undo. Proceed?"
        );
        if !confirm_prompt(&prompt)? {
            return Ok(None);
        }
    }

    let seed_model = seed
        .as_ref()
        .and_then(|seed| seed.config.preferred_model_id.clone());
    // Enforce the model pin at creation: there is no runtime default
    // model, so an unpinned principal would fail every send with
    // "no model configured". `--model` overrides the seed value; with
    // neither, the only enabled model in the catalog is used (the
    // first-run case). Validate against the catalog now so a typo'd id
    // fails here instead of at first use.
    let cat_path = paths.config_dir.join(ModelCatalog::FILENAME);
    let catalog = ModelCatalog::load_or_init(&cat_path).await?;
    let model_id = match model_id.map(str::to_string).or_else(|| seed_model.clone()) {
        Some(m) => m,
        None => {
            let enabled: Vec<String> = catalog
                .list_enabled()
                .await
                .into_iter()
                .map(|m| m.id)
                .collect();
            match enabled.as_slice() {
                [only] => {
                    println!("Using model '{only}', the only one set up.");
                    only.clone()
                }
                [] => anyhow::bail!(
                    "no model is set up yet. Add one with: peko model add --api-format <fmt> \
                     --base-url <url> --key \"$YOUR_KEY\""
                ),
                many => anyhow::bail!(
                    "pick the model it uses with --model <MODEL_ID>: {}",
                    many.join(", ")
                ),
            }
        }
    };
    if catalog.get_enabled(&model_id).await.is_none() {
        anyhow::bail!(
            "model '{model_id}' is not in the catalog (or is disabled).\n\
             Add one with: peko model add --api-format <fmt> --base-url <url> --key \"$YOUR_KEY\"\n\
             Or list configured models: peko model list"
        );
    }

    Ok(Some(CreatePlan {
        model_id,
        seed: seed_text,
        accept_setup: accept,
        replace: exists,
    }))
}

/// Make sure a daemon is running: it is what creates the peko and runs
/// its first turn.
async fn ensure_daemon(paths: &GlobalPaths) -> Result<()> {
    let service = peko_core::common::services::DaemonProcessService::new(paths.resolver().clone());
    if service.is_daemon_running().await.unwrap_or(false) {
        return Ok(());
    }
    println!("Starting the daemon…");
    // Same defaults as `peko daemon start` (interval 5s, 50 reconnect
    // attempts — see the Start command).
    service
        .spawn_daemon_with(5, 50, false)
        .await
        .context("failed to spawn the daemon sidecar")?;
    // The IPC socket binds only after the boot scan.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let ready = match DaemonClient::connect().await {
            Ok(client) => client.ping().await.is_ok(),
            Err(_) => false,
        };
        if ready {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("daemon did not become ready within 30s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Unless `detach`, block until the peko's first turn is over, showing
/// what it does.
async fn wait_for_first_turn(
    name: &str,
    paths: &GlobalPaths,
    detach: bool,
    wait_timeout: Option<u64>,
) -> Result<()> {
    if detach {
        println!(
            "Peko '{name}' is created and will set itself up shortly; \
             watch it with `peko audit ledger {name} --follow`"
        );
        return Ok(());
    }

    // ── P2 wait: block until the genesis turn is done ───────────────
    println!(
        "Waiting for '{name}' to set itself up (its first turn). Ctrl-C stops the \
         waiting, not the peko; to stop the peko: peko stop {name} --main"
    );
    let ledger_dir = paths.principal_layout(name).local.ledger_dir;
    let mut shown = FirstTurnProgress::default();
    let schedule_path = paths.resolver().cron_schedule(name);
    let config_path = paths.resolver().principal_layout(name).shared.config_file;
    let deadline =
        wait_timeout.map(|secs| std::time::Instant::now() + std::time::Duration::from_secs(secs));
    let setup = loop {
        for line in shown.new_steps(&ledger_dir) {
            println!("  · {line}");
        }
        let scheduler = peko_cron::CronScheduler::new(&schedule_path)
            .with_context(|| format!("open cron schedule for '{name}'"))?;
        if genesis_completed(&scheduler)? {
            // History is finalized before the daemon stamps completion.
            // Wait for both persisted writes, not this CLI's stale cache.
            let config: PrincipalConfig = toml::from_str(&std::fs::read_to_string(&config_path)?)?;
            if config.boot_state() == peko_core::principal::config::BootState::Organized {
                break config.intent.setup;
            }
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() > deadline) {
            anyhow::bail!(
                "'{name}' did not finish setting up within {}s. The peko is created \
                 and its first turn goes on in the daemon. Watch it: \
                 peko audit ledger {name} --follow. Stop it: peko stop {name} --main",
                wait_timeout.unwrap_or_default()
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    };
    let workspace = paths.resolver().principal_layout(name).shared.root;
    if let Some(report) = setup_report(&workspace, &setup) {
        println!("{report}");
    }
    println!(
        "🎉 Peko '{name}' is ready. \
         Talk to it: peko send {name} \"…\" — read along: peko log {name}"
    );

    Ok(())
}

/// What `create` has already printed of the peko's first turn: one line
/// per tool call, read from its tool ledger (ADR-079).
#[derive(Default)]
struct FirstTurnProgress {
    seen: std::collections::HashSet<String>,
}

impl FirstTurnProgress {
    /// Lines for the calls started since the last look. A ledger that
    /// does not exist yet, or cannot be read, shows nothing.
    fn new_steps(&mut self, ledger_dir: &std::path::Path) -> Vec<String> {
        peko_core::principal::setup_status::steps(ledger_dir)
            .into_iter()
            .filter(|step| self.seen.insert(step.call_id.clone()))
            .map(|step| step.line)
            .collect()
    }
}

/// One-shot jobs are reaped even on failure. Only a finalized run is
/// evidence of initialization; a missing job without history is pending.
fn genesis_completed(scheduler: &peko_cron::CronScheduler) -> Result<bool> {
    let runs = scheduler.get_run_history(peko_core::principal::genesis::GENESIS_JOB_ID, 1)?;
    let Some(run) = runs.first().filter(|run| run.finished_at.is_some()) else {
        return Ok(false);
    };
    // Done, or stopped by its owner (ADR-104 D5): either way it is over.
    if peko_core::principal::genesis::genesis_is_over(&run.status) {
        if run.status == peko_cron::STOPPED_STATUS {
            println!("The first turn was stopped before it finished.");
        }
        return Ok(true);
    }
    anyhow::bail!(
        "the peko's first turn failed ({}): {}. Check the daemon log; restart the daemon \
         to try again",
        run.status,
        run.error
            .as_deref()
            .or(run.output.as_deref())
            .unwrap_or("no detail recorded")
    )
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
            if path.is_dir() && peko_core::common::paths::peko_config_path(&path).exists() {
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
        // list is `[]`, not "No pekos found."
        println!("{}", render_list_principals_json(&names)?);
        return Ok(());
    }

    if names.is_empty() {
        println!("No pekos found.");
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
    // ADR-081 §3: the actor registry is the only source of a Hub ID.
    let hub_id = {
        use peko_channel::ChannelPort;
        peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: paths.resolver().runtime_dir(),
            shared_dir: None,
        })
        .actor_info(&ActorRef::from(&principal.id))
        .await
        .hub_id
    };

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

    // Build a catalog summary for the show output. Phase 5
    // (ADR-047 §5.3) adds the catalog block to the `peko show`
    // payload. The catalog is the per-message view of what the
    // principal has installed — built-in tools + agents + workspace
    // entries. Empty workspaces render as an empty `catalog` object so
    // downstream tooling doesn't have to special-case missing.
    let catalog = peko_core::principal::catalog::PrincipalCatalog::build(
        &principal.workspace_path,
        &principal.agent_prompts,
    );

    if json {
        // Structured view — same shape as `peko log --json` for
        // downstream tooling. `path` is the absolute workspace path;
        // `agents` is an array of {name, description, prompt_path};
        // `persona` is the identity + intent block (Bug D, 2026-08-01
        // v2) so non-tech users can confirm the persona fields without
        // parsing peko.toml by hand.
        //
        // `catalog` (ADR-047 §5.3) is the per-principal view of
        // installed tooling. `entries[]` is grouped by `kind` so a
        // caller can pivot by category (built-in, agent, workspace
        // tool / hook / skill / mcp).
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
        struct CatalogEntryView<'a> {
            id: &'a str,
            kind: &'a str,
            enabled: bool,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CatalogView<'a> {
            entries: Vec<CatalogEntryView<'a>>,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ShowView<'a> {
            local_id: &'a str,
            hub_id: Option<&'a str>,
            registration: &'a str,
            name: &'a str,
            display_name: &'a str,
            did: Option<&'a str>,
            workspace: &'a std::path::Path,
            preferred_model_id: Option<&'a str>,
            agents: Vec<AgentView<'a>>,
            persona: PersonaView<'a>,
            catalog: CatalogView<'a>,
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
        let catalog_view = CatalogView {
            entries: catalog
                .entries()
                .iter()
                .map(|e| CatalogEntryView {
                    id: e.id.as_str(),
                    kind: e.kind.as_str(),
                    enabled: e.enabled,
                })
                .collect(),
        };
        let view = ShowView {
            local_id: &principal.id.0,
            hub_id: hub_id.as_deref(),
            registration: if hub_id.is_none() {
                "local_only"
            } else {
                config.exposure.as_str()
            },
            name: &config.name,
            display_name: &display_name,
            did: did.as_ref().map(|d| d.0.as_str()),
            workspace: &principal.workspace_path,
            preferred_model_id: preferred_model_id.as_deref(),
            agents,
            persona,
            catalog: catalog_view,
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }

    let did_str = did.map(|d| d.0).unwrap_or_else(|| "(none)".to_string());

    println!("Peko: {}", display_name);
    let config = principal.config.read().await;
    println!("  Local ID: peko:local:{}", principal.id.0);
    if let Some(hub) = &hub_id {
        println!("  Hub ID: peko:hub:{hub}");
    }
    println!(
        "  Registration: {}",
        if hub_id.is_none() {
            "local_only"
        } else {
            config.exposure.as_str()
        }
    );
    drop(config);
    println!("  Signing DID: {}", did_str);
    println!("  Workspace: {}", principal.workspace_path.display());

    // Show where the root agent will route its LLM calls. Surface the
    // pinned configured model so users can confirm their override took
    // effect without trawling config files.
    let model_line = match preferred_model_id {
        Some(id) => format!("{id} (pinned to this peko)"),
        None => "(none — run `peko model add` and pin a model)".to_string(),
    };
    println!("  Model: {model_line}");

    println!("  Roles:");
    for (agent_name, prompt) in &principal.agent_prompts {
        let desc = prompt
            .frontmatter
            .description
            .as_deref()
            .unwrap_or("(no description)");
        println!("    - {} ({}): {desc}", agent_name, prompt.path.display());
    }

    // Catalog summary (ADR-047 §5.3). Group by kind so the operator
    // can see "5 built-ins, 3 agents, 1 tool, 2 hooks, 0 skills, 1
    // MCP" at a glance.
    let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> =
        std::collections::BTreeMap::new();
    for entry in catalog.entries() {
        let slot = by_kind
            .entry(entry.kind.as_str())
            .or_insert((0usize, 0usize));
        slot.0 += 1;
        if entry.enabled {
            slot.1 += 1;
        }
    }
    if by_kind.is_empty() {
        println!("  Catalog: (empty)");
    } else {
        println!("  Catalog:");
        for (kind, (total, enabled)) in &by_kind {
            // Role templates are catalogued under the internal kind
            // `agent`; users know them as roles (ADR-064).
            let kind = if *kind == "agent" { "role" } else { kind };
            println!("    - {kind}: {enabled}/{total} enabled");
        }
    }
    Ok(())
}

async fn remove_principal(name: &str, yes: bool, paths: &GlobalPaths, json: bool) -> Result<()> {
    let manager = build_manager(paths);
    // Load first so the principal is registered in the in-memory manager,
    // and so a missing principal fails with a clear error before prompting.
    let _principal = load_principal(name, &manager, paths).await?;

    if !yes && !confirm_prompt(&format!("Remove peko '{name}' and all its data?"))? {
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
        println!("Removed peko '{name}'");
    }
    Ok(())
}

/// Resolve a path the user typed against the CLI's working directory.
/// The daemon runs in its own directory, so a relative path sent as-is
/// would land wherever the daemon happened to be started.
fn absolute_from_cwd(path: &str) -> Result<String> {
    let path = std::path::Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("cannot read the current directory")?
            .join(path)
    };
    Ok(absolute.display().to_string())
}

async fn export_principal(name: &str, output: Option<String>) -> Result<()> {
    let output = absolute_from_cwd(&output.unwrap_or_else(|| format!("{name}.peko")))?;
    let client = DaemonClient::connect().await?;
    let response = client.principal_export(name, Some(output)).await?;

    match response {
        ResponsePacket::PrincipalExported {
            name, output_path, ..
        } => {
            println!("Exported peko '{name}' to {output_path}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to export peko: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

/// `peko seed` — reads the peko's `peko.toml` directly; the daemon
/// need not be running.
fn export_seed(name: &str, output: Option<String>, paths: &GlobalPaths) -> Result<()> {
    let config_path =
        peko_core::common::paths::peko_config_path(&paths.principals_root_dir().join(name));
    let config = std::fs::read_to_string(&config_path).with_context(|| {
        format!(
            "no peko named '{name}' (looked for {})",
            config_path.display()
        )
    })?;
    // The persona a seed gave this peko lives in its conventions; the
    // exported seed carries it again.
    let conventions = config_path
        .parent()
        .map(|root| root.join("kb").join("CONVENTIONS.md"))
        .and_then(|path| std::fs::read_to_string(path).ok());
    let seed = peko_core::principal::seed::seed_from_workspace(&config, conventions.as_deref())?;
    match output {
        Some(path) => {
            let path = absolute_from_cwd(&path)?;
            std::fs::write(&path, &seed).with_context(|| format!("write {path}"))?;
            println!("Wrote the seed for '{name}' to {path}");
            println!("  Publish it on PekoHub: https://pekohub.ai/seeds/new");
            println!("  Or grow a peko from it: peko create <name> -s {path}");
        }
        None => print!("{seed}"),
    }
    Ok(())
}

async fn import_principal(
    file_path: &str,
    name: Option<String>,
    force: bool,
    rotate_keys: bool,
    _yes: bool,
) -> Result<()> {
    let file_path = absolute_from_cwd(file_path)?;
    let file_path = file_path.as_str();
    let client = DaemonClient::connect().await?;

    // Always preview first. The preview is read-only and surfaces bundled
    // role prompts, executable manifests, skills, and validation issues.
    let preview = client
        .principal_import_preview(file_path, name.clone(), force, rotate_keys)
        .await?;

    let preview = decode_preview_response(preview, "import")?;

    render_import_preview(&preview, rotate_keys);

    if !preview.validation_errors.is_empty() {
        anyhow::bail!(
            "Package has validation errors. Fix the package or re-export from the source runtime."
        );
    }

    let response = client
        .principal_import(
            file_path,
            name,
            force,
            Some(preview.manifest_checksum.clone()),
            rotate_keys,
        )
        .await?;

    match response {
        ResponsePacket::PrincipalImported {
            name, config_path, ..
        } => {
            println!("Imported peko '{name}' at {config_path}");
            if rotate_keys {
                println!("  New actor with new keys; `peko show {name}` lists its ID and DID.");
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to import peko: {message}");
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
    inventory: peko_core::registry::packaging::ExecutableInventory,
    manifest_checksum: String,
    validation_errors: Vec<String>,
    validation_warnings: Vec<String>,
}

/// Decode the local snapshot preview.
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
            inventory,
            manifest_checksum,
            validation_errors,
            validation_warnings,
            ..
        } => Ok(PrincipalImportPreview {
            name,
            version,
            did,
            description,
            agents,
            inventory,
            manifest_checksum,
            validation_errors,
            validation_warnings,
        }),
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("Failed to preview peko {operation}: {message}");
        }
        other => {
            anyhow::bail!("Unexpected response from daemon: {other:?}");
        }
    }
}

fn render_import_preview(preview: &PrincipalImportPreview, rotate_keys: bool) {
    println!("Peko import preview:");
    println!("  Name:        {}", preview.name);
    println!("  Peko version: {}", preview.version);
    if rotate_keys {
        println!(
            "  DID:         new (--rotate-keys; package has {})",
            preview.did
        );
    } else {
        println!("  DID:         {}", preview.did);
    }
    if let Some(desc) = &preview.description {
        println!("  Description: {desc}");
    }
    println!("{}", preview.inventory.render());
    if preview.agents.is_empty() {
        println!("  Roles:       (none)");
    } else {
        println!("  Roles:");
        for agent in &preview.agents {
            println!("    - {agent}");
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

fn parse_permission(value: &str) -> Result<peko_auth::Permission> {
    match value.to_lowercase().as_str() {
        "chat" => Ok(peko_auth::Permission::Chat),
        "view_settings" | "view-settings" | "viewsettings" => {
            Ok(peko_auth::Permission::ViewSettings)
        }
        "manage_settings" | "manage-settings" | "managesettings" => {
            Ok(peko_auth::Permission::ManageSettings)
        }
        "trusted" => Ok(peko_auth::Permission::Trusted),
        other => anyhow::bail!("Unknown permission: {other}"),
    }
}

/// An explicit actor reference (ADR-073/081): `kind:local:<id>`,
/// `kind:hub:<id>`, or `public`. Bare names are refused — they never
/// matched an actor.
fn parse_actor_ref(text: &str) -> Result<ActorRef> {
    if text == "public" {
        return Ok(ActorRef::Public);
    }
    ActorRef::parse_scoped(text).map_err(|e| anyhow::anyhow!("{e}, or public"))
}

async fn transfer_principal(name: &str, owner: &str) -> Result<()> {
    let owner = parse_actor_ref(owner)?;
    let client = DaemonClient::connect().await?;
    match client.principal_transfer(name, owner).await? {
        ResponsePacket::PrincipalOwnerTransferred { name, owner, .. } => {
            println!("Transferred '{name}' to {}", owner.canonical_address());
            Ok(())
        }
        ResponsePacket::PrincipalTransferOffered { message, .. } => {
            println!("{message}");
            Ok(())
        }
        ResponsePacket::Error { message, .. } => anyhow::bail!("Transfer failed: {message}"),
        other => anyhow::bail!("Unexpected response from daemon: {other:?}"),
    }
}

/// `peko approvals <peko>`: the requests waiting for the owner.
async fn list_approvals(name: &str, json: bool) -> Result<()> {
    let client = DaemonClient::connect().await?;
    match client.approval_list(name).await? {
        ResponsePacket::ApprovalList { approvals, .. } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&approvals)?);
            } else if approvals.is_empty() {
                println!("No approval requests are waiting for '{name}'.");
            } else {
                for a in &approvals {
                    if a.kind.as_deref() == Some("owner_task") {
                        // ADR-100: a task handed to the owner, approved once.
                        println!(
                            "{}  you asked {name} for this {}; approved, it runs as you, \
                             outside that conversation, with only this brief (approve once, \
                             never --always):",
                            a.id, a.reason
                        );
                        for line in a.summary.lines() {
                            println!("    | {line}");
                        }
                        println!("    expires {}", a.expires_at);
                        continue;
                    }
                    let who = a.actor_name.as_deref().map_or_else(
                        || super::shown_address(&a.actor),
                        |n| format!("{n} ({})", super::shown_address(&a.actor)),
                    );
                    println!("{}  {who} [{}] wants {}", a.id, a.trust, a.summary);
                    println!("    reviewer: {}", a.reason);
                    println!("    expires {}", a.expires_at);
                }
                println!();
                // ADR-100: a task run as you is approved once, never always.
                let always = if approvals
                    .iter()
                    .any(|a| a.kind.as_deref() != Some("owner_task"))
                {
                    " [--always]"
                } else {
                    ""
                };
                println!("Decide with: peko approve {name} <id>{always}  |  peko deny {name} <id>");
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response from daemon: {other:?}"),
    }
}

/// `peko approve` / `peko deny`: the owner's decision on one request.
async fn decide_approval(
    name: &str,
    id: &str,
    approve: bool,
    always: bool,
    json: bool,
) -> Result<()> {
    let client = DaemonClient::connect().await?;
    match client.approval_decide(name, id, approve, always).await? {
        ResponsePacket::ApprovalDecided {
            id,
            approved,
            message,
            ..
        } => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "id": id, "approved": approved, "message": message })
                );
            } else {
                println!("{message}");
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("Unexpected response from daemon: {other:?}"),
    }
}

async fn grant_permission(name: &str, subject_str: &str, permission_str: &str) -> Result<()> {
    let subject = parse_actor_ref(subject_str)?;
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
            println!(
                "Granted {:?} on '{}' to {}",
                permission,
                name,
                super::shown_address(&subject.to_string())
            );
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
    let subject = parse_actor_ref(subject_str)?;
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
            println!(
                "Revoked {:?} on '{}' from {}",
                permission,
                name,
                super::shown_address(&subject.to_string())
            );
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
                println!("No permissions granted on peko '{name}'.");
                return Ok(());
            }
            println!("Permissions on peko '{name}':");
            for grant in permissions {
                println!(
                    "  {:?} for {} (granted by {} at {})",
                    grant.permission,
                    super::shown_address(&grant.subject.to_string()),
                    super::shown_address(&grant.granted_by.to_string()),
                    grant.granted_at
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
/// seconds. Used by `peko invite --ttl <value>`. Bare
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
    // bare `peko invite alice` still produces a usable link.
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
            println!("Minted invite for peko '{name}'");
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
            println!("Revoked invite {jti} on peko '{name}'.");
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
                .ok_or_else(|| anyhow::anyhow!("peko '{name}' not found"))?,
        )
        .map_err(|e| anyhow::anyhow!("authority error: {e}"))?
        .into_path_buf();
    if !config_path.exists() {
        anyhow::bail!("peko '{name}' not found");
    }

    manager
        .load(&config_path)
        .await
        .context("failed to load peko")
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
        resolver,
        Arc::new(DefaultPrincipalMemoryFactory),
        Arc::new(DefaultPrincipalRouterFactory),
        peko_core::async_exec::executor::standalone_inbox_registry(),
    )
}

/// JSON envelope for `peko list --json`. Empty list is `[]`
/// (never "No pekos found."), matching `log --json` / `show --json`.
fn render_list_principals_json(names: &[String]) -> serde_json::Result<String> {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Item<'a> {
        name: &'a str,
    }
    let items: Vec<Item> = names.iter().map(|n| Item { name: n }).collect();
    serde_json::to_string_pretty(&items)
}

/// JSON envelope for `peko remove <name> --json`. The
/// `removed` boolean lets scripts distinguish success from a future
/// cancellation-with-nonzero-exit without parsing prose.
fn render_remove_principal_json(name: &str) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&serde_json::json!({
        "removed": true,
        "name": name,
    }))
}

// Memory placement is the default: `PrincipalManager` passes the
// principal's Local tier root (`{data_dir}/principals/{name}/local/`) to
// the factory, so `DefaultPrincipalMemoryFactory` is used directly.

#[cfg(test)]
mod tests {
    #[test]
    fn diff_reads_the_daemons_baseline_entries() {
        let baseline: std::collections::BTreeMap<String, serde_json::Value> = serde_json::from_str(
            r#"{"ada": {"principal": "aa", "hooks": {}, "mcp": {}}, "bo": "bb", "cy": 3}"#,
        )
        .unwrap();
        let hashes = super::definition_hashes(baseline);
        assert_eq!(hashes.get("ada").map(String::as_str), Some("aa"));
        assert_eq!(hashes.get("bo").map(String::as_str), Some("bb"));
        assert!(!hashes.contains_key("cy"));
    }

    use super::*;
    use crate::commands::{from_cli, Cli, Commands};
    use clap::Parser;

    /// Export and import paths reach the daemon absolute, so a relative
    /// path means the user's directory, not the daemon's.
    #[test]
    fn typed_paths_resolve_against_the_cli_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            absolute_from_cwd("my-peko.peko").unwrap(),
            cwd.join("my-peko.peko").display().to_string()
        );
        assert_eq!(
            absolute_from_cwd("backups/x.peko").unwrap(),
            cwd.join("backups/x.peko").display().to_string()
        );
        // On Windows `/tmp/x.peko` has no drive, so it isn't absolute.
        let absolute = if cfg!(windows) {
            r"C:\tmp\x.peko"
        } else {
            "/tmp/x.peko"
        };
        assert_eq!(absolute_from_cwd(absolute).unwrap(), absolute);
    }
    use peko_auth::Permission;

    #[test]
    fn genesis_wait_requires_finalized_success_after_oneshot_reaping() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = peko_cron::CronScheduler::new(temp.path().join("cron.json")).unwrap();
        let job = peko_core::principal::genesis::genesis_job(
            &peko_core::principal::provision::default_config(
                "seedling",
                None,
                ActorRef::User("local".into()),
                "m".into(),
            ),
            chrono::Utc::now(),
        )
        .unwrap();
        scheduler.add_job(&job).unwrap();
        scheduler.delete_job(&job.id).unwrap();
        assert!(
            !genesis_completed(&scheduler).unwrap(),
            "absence proves nothing"
        );

        for (index, status) in ["failed", "error", "cancelled", "success"]
            .iter()
            .enumerate()
        {
            let run_id = format!("genesis-run-{index}");
            scheduler
                .record_run(&peko_cron::CronRun {
                    id: run_id.clone(),
                    job_id: job.id.clone(),
                    started_at: chrono::Utc::now() + chrono::Duration::seconds(index as i64),
                    finished_at: None,
                    status: "running".into(),
                    output: None,
                    error: None,
                })
                .unwrap();
            assert!(
                !genesis_completed(&scheduler).unwrap(),
                "running is pending"
            );
            scheduler
                .finalize_run(&run_id, status, None, Some("provider unavailable".into()))
                .unwrap();
            if *status == "success" {
                assert!(genesis_completed(&scheduler).unwrap());
            } else {
                let error = genesis_completed(&scheduler).unwrap_err().to_string();
                assert!(error.contains(status), "{error}");
                assert!(error.contains("provider unavailable"), "{error}");
            }
        }
    }

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
        assert_eq!(parse_permission("TRUSTED").unwrap(), Permission::Trusted);
        // ADR-081: permissions nothing checked are retired.
        assert!(parse_permission("expose").is_err());
    }

    #[test]
    fn parse_permission_rejects_unknown() {
        assert!(parse_permission("fly").is_err());
    }

    #[test]
    fn principal_permit_parses_positional_args() {
        let cli = Cli::try_parse_from(["peko", "permit", "myprincipal", "user:alice", "chat"])
            .expect("should parse permit");

        match cli.command {
            Commands::Peko(PrincipalCommands::Permit {
                name,
                subject,
                permission,
            }) => {
                assert_eq!(name, "myprincipal");
                assert_eq!(subject, "user:alice");
                assert_eq!(permission, "chat");
            }
            _other => panic!("expected flattened top-level permit command"),
        }
    }

    #[test]
    fn retired_registry_commands_and_signature_flag_are_rejected() {
        use clap::CommandFactory;
        let command = Cli::command();
        for name in ["push", "pull", "search", "registry"] {
            assert!(
                command.find_subcommand(name).is_none(),
                "{name} must be retired"
            );
        }
        assert!(
            Cli::try_parse_from(["peko", "import", "snapshot.peko", "--allow-unsigned"]).is_err()
        );
    }

    #[test]
    fn principal_import_parses_yes_flag() {
        let cli = Cli::try_parse_from([
            "peko",
            "import",
            "/tmp/pkg.peko",
            "--name",
            "renamed",
            "--force",
            "--yes",
        ])
        .expect("should parse import with --yes");

        match cli.command {
            Commands::Peko(PrincipalCommands::Import {
                file_path,
                name,
                force,
                yes,
                ..
            }) => {
                assert_eq!(file_path, "/tmp/pkg.peko");
                assert_eq!(name, Some("renamed".to_string()));
                assert!(force);
                assert!(yes);
            }
            _other => panic!("expected flattened top-level import command"),
        }
    }

    #[test]
    fn principal_import_without_yes_defaults() {
        let cli = Cli::try_parse_from(["peko", "import", "/tmp/pkg.peko"])
            .expect("should parse import without --yes");

        match cli.command {
            Commands::Peko(PrincipalCommands::Import {
                yes, rotate_keys, ..
            }) => {
                assert!(!yes);
                assert!(!rotate_keys);
            }
            _other => panic!("expected flattened top-level import command"),
        }
    }

    #[test]
    fn principal_import_parses_rotate_keys() {
        let cli = Cli::try_parse_from([
            "peko",
            "import",
            "/tmp/pkg.peko",
            "--rotate-keys",
            "--name",
            "copy",
        ])
        .expect("should parse import with --rotate-keys");

        match cli.command {
            Commands::Peko(PrincipalCommands::Import {
                name, rotate_keys, ..
            }) => {
                assert!(rotate_keys);
                assert_eq!(name.as_deref(), Some("copy"));
            }
            _other => panic!("expected flattened top-level import command"),
        }
    }

    #[test]
    fn principal_remove_parses() {
        let cli = Cli::try_parse_from(["peko", "remove", "myprincipal", "--yes"])
            .expect("should parse remove with --yes");

        match cli.command {
            Commands::Peko(PrincipalCommands::Remove { name, yes }) => {
                assert_eq!(name, "myprincipal");
                assert!(yes);
            }
            _other => panic!("expected flattened top-level remove command"),
        }
    }

    /// ADR-059: the lifecycle verbs are flattened to the top level.
    /// `peko create <name>` parses into `PrincipalCommands::Create`.
    #[test]
    fn top_level_create_parses_adr059() {
        let cli = Cli::try_parse_from([
            "peko", "create", "my-peko", "--model", "kimi-k2", "--detach",
        ])
        .expect("should parse top-level create (ADR-059)");

        match cli.command {
            Commands::Peko(PrincipalCommands::Create {
                name,
                model,
                detach,
                ..
            }) => {
                assert_eq!(name, "my-peko");
                assert_eq!(model.as_deref(), Some("kimi-k2"));
                assert!(detach);
            }
            _other => panic!("expected flattened top-level create command"),
        }
    }

    /// ADR-059: `peko list` reaches the same handler as the retired
    /// nested `peko principal list` form.
    #[test]
    fn top_level_list_parses_adr059() {
        let cli =
            Cli::try_parse_from(["peko", "list"]).expect("should parse top-level list (ADR-059)");
        match cli.command {
            Commands::Peko(PrincipalCommands::List) => {}
            _other => panic!("expected flattened top-level list command"),
        }
    }

    /// ADR-059 cleanup: the legacy `peko principal <sub>` namespace is
    /// retired — clap rejects it as an unknown subcommand.
    #[test]
    fn principal_namespace_alias_no_longer_parses() {
        let result = Cli::try_parse_from(["peko", "principal", "list"]);
        assert!(
            result.is_err(),
            "the retired `peko principal` namespace must not parse"
        );
    }

    /// Bug D (2026-08-01 v2): `principal show --json` didn't expose the
    /// persona fields (`[identity.description]`, `[intent.goals]`,
    /// `[intent.values]`), so a non-tech user had no in-CLI way to
    /// confirm what got written. The fix adds a `persona:
    /// {description, goals, values}` block to the `ShowView` envelope.
    /// Pin the field set + camelCase rename so scripts that pipe
    /// `jq '.persona.goals'` keep working.
    ///
    /// ADR-047 §5.3: the envelope also carries a `catalog` block so
    /// downstream tooling can read the per-principal tooling snapshot
    /// without parsing `principal show` text output.
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
        struct CatalogEntryView<'a> {
            id: &'a str,
            kind: &'a str,
            enabled: bool,
        }
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct CatalogView<'a> {
            entries: Vec<CatalogEntryView<'a>>,
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
            catalog: CatalogView<'a>,
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
            preferred_model_id: Some("example-model"),
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
            catalog: CatalogView {
                entries: vec![CatalogEntryView {
                    id: "builtin:tool:Read",
                    kind: "builtin",
                    enabled: true,
                }],
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

        // ADR-047 §5.3: catalog block carries entries with the right
        // shape. Without this, downstream `jq '.catalog.entries[]'`
        // consumers would silently get `null`.
        for field in ["catalog", "entries", "kind", "enabled"] {
            assert!(
                pretty.contains(&format!("\"{field}\"")),
                "catalog block missing `{field}`; got:\n{pretty}"
            );
        }
        assert!(
            pretty.contains("\"builtin:tool:Read\""),
            "catalog.entries[].id did not round-trip; got:\n{pretty}"
        );
    }

    #[test]
    fn principal_create_model_pin_precedence() {
        // `--model` is optional only when the `--seed` pins
        // `preferred_model_id`; the flag overrides the seed value.
        // Parse-level: no model and no seed is fine at parse time —
        // the requirement is enforced in `create_principal`.
        let cli = Cli::try_parse_from(["peko", "create", "alice", "--model", "anthropic-haiku"])
            .expect("should parse create with --model");
        match cli.command {
            Commands::Peko(PrincipalCommands::Create {
                name,
                model,
                force,
                yes,
                seed,
                detach,
                wait_timeout,
            }) => {
                assert_eq!(name, "alice");
                assert_eq!(model.as_deref(), Some("anthropic-haiku"));
                assert!(!force);
                assert!(!yes);
                assert!(seed.is_none());
                assert!(!detach);
                assert_eq!(wait_timeout, None, "no limit unless asked");
            }
            _other => panic!("expected flattened top-level create command"),
        }
    }

    #[test]
    fn principal_create_parses_seed() {
        let cli = Cli::try_parse_from([
            "peko",
            "create",
            "scout",
            "-s",
            "/tmp/scout.peko.toml",
            "--detach",
        ])
        .expect("should parse create with -s seed");

        match cli.command {
            Commands::Peko(PrincipalCommands::Create { name, seed, .. }) => {
                assert_eq!(name, "scout");
                assert_eq!(seed.as_deref(), Some("/tmp/scout.peko.toml"));
            }
            _other => panic!("expected flattened top-level create command"),
        }
    }

    /// ADR-060 cleanup: the retired `-f` / `--file` compat aliases for
    /// `--seed` no longer parse; only `-s` / `--seed` remain.
    #[test]
    fn principal_create_seed_aliases_removed() {
        for flag in ["-s", "--seed"] {
            let cli =
                Cli::try_parse_from(["peko", "create", "scout", flag, "/tmp/scout.peko.toml"])
                    .unwrap_or_else(|e| panic!("`{flag}` should parse as the seed flag: {e}"));

            match cli.command {
                Commands::Peko(PrincipalCommands::Create { seed, .. }) => {
                    assert_eq!(
                        seed.as_deref(),
                        Some("/tmp/scout.peko.toml"),
                        "`{flag}` must bind the seed field"
                    );
                }
                _other => panic!("expected flattened top-level create command"),
            }
        }

        for flag in ["-f", "--file"] {
            assert!(
                Cli::try_parse_from(["peko", "create", "scout", flag, "/tmp/scout.peko.toml",])
                    .is_err(),
                "retired alias `{flag}` must no longer parse"
            );
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
            "list",
        ]);
        let paths = from_cli(&cli);

        // Empty catalog → any model id is rejected at creation, before
        // the workspace is written. `detach` keeps the test off the
        // daemon-spawn path (the model check fails first anyway).
        let result = plan_create("alice", Some("no-such-model"), None, false, false, &paths).await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("unknown model must fail creation"),
        };
        assert!(
            format!("{err:#}").contains("no-such-model"),
            "error should name the offending model id: {err:#}"
        );
    }

    #[tokio::test]
    async fn create_without_a_model_says_how_to_add_or_pick_one() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "list",
        ]);
        let paths = from_cli(&cli);
        let err = match plan_create("alice", None, None, false, false, &paths).await {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("no model must fail creation"),
        };
        assert!(err.contains("peko model add --api-format"), "{err}");

        use peko_providers::catalog::ModelCatalog;
        let cat = ModelCatalog::load_or_init(&paths.config_dir.join(ModelCatalog::FILENAME))
            .await
            .unwrap();
        for id in ["a", "b"] {
            let mut m = peko_providers::ModelConfig::new(
                id,
                peko_providers::ApiFormat::OpenaiCompletions,
                "http://localhost:1/v1",
                id,
            );
            m.requires_key = false;
            cat.upsert(m).await.unwrap();
        }
        let err = match plan_create("alice", None, None, false, false, &paths).await {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("two models must ask which"),
        };
        assert!(err.contains("--model") && err.contains("a, b"), "{err}");
    }

    #[test]
    fn principal_create_parses_force_flag() {
        // `--force` is the explicit override for the overwrite guard —
        // see Bug 2 in scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
        // (Long-form only: `-s` already selects the seed, so no short
        // flag is free for `--force`.)
        let cli = Cli::try_parse_from([
            "peko",
            "create",
            "scout",
            "--model",
            "anthropic-haiku",
            "--force",
        ])
        .expect("should parse create with --force");

        match cli.command {
            Commands::Peko(PrincipalCommands::Create {
                name,
                model,
                force,
                yes,
                ..
            }) => {
                assert_eq!(name, "scout");
                assert_eq!(model.as_deref(), Some("anthropic-haiku"));
                assert!(force);
                assert!(!yes);
            }
            _other => panic!("expected flattened top-level create command"),
        }
    }

    /// ADR-081 D4: a seed carries no authority — its `[owner]` and grants
    /// are dropped, and the creating local user owns the result.
    #[tokio::test]
    async fn create_from_seed_drops_owner_and_grants() {
        use peko_providers::catalog::ModelCatalog;
        crate::test_support::init_credentials();

        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from([
            "peko",
            "--config-dir",
            dir.path().join("config").to_str().unwrap(),
            "--data-dir",
            dir.path().join("data").to_str().unwrap(),
            "--cache-dir",
            dir.path().join("cache").to_str().unwrap(),
            "list",
        ]);
        let paths = from_cli(&cli);
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(
            paths.config_dir.join(ModelCatalog::FILENAME),
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
        let seed = dir.path().join("seed.toml");
        std::fs::write(
            &seed,
            r#"
name = "ignored"
owner = { kind = "user", id = "hub:seed-author" }

[intent]
setup = ["Install ripgrep"]

[[permissions]]
subject = { kind = "user", id = "hub:friend" }
permission = "trusted"
granted_at = ""
granted_by = { kind = "user", id = "hub:seed-author" }
"#,
        )
        .unwrap();

        // `--yes` accepts the seed's setup steps (ADR-104). The plan
        // carries the seed as written; the daemon grows the peko from it
        // (`principal::provision` drops the seed's owner and grants).
        let plan = plan_create(
            "sprout",
            Some("demo-model"),
            Some(seed.to_str().unwrap()),
            false,
            true,
            &paths,
        )
        .await
        .expect("a seeded create is planned")
        .expect("nothing was declined");
        assert_eq!(plan.model_id, "demo-model");
        assert!(plan.accept_setup, "the steps were accepted");
        assert!(!plan.replace);
        assert_eq!(
            plan.seed.as_deref(),
            Some(std::fs::read_to_string(&seed).unwrap().as_str())
        );
        // Planning writes nothing.
        assert!(!paths
            .resolver()
            .principal_layout("sprout")
            .shared
            .root
            .exists());
    }

    /// ADR-104: setup steps are accepted by `--yes` or at a terminal;
    /// a script that did not pass `--yes` is refused.
    #[test]
    fn setup_steps_must_be_accepted_before_create() {
        let steps = vec!["Install ripgrep".to_string()];
        assert!(accept_setup("scout", &[], false, false).unwrap());
        assert!(accept_setup("scout", &steps, true, false).unwrap());
        let refused = accept_setup("scout", &steps, false, false).unwrap_err();
        assert!(refused.to_string().contains("--yes"), "{refused}");
    }

    /// ADR-104: `create` ends by showing the peko's own setup report.
    #[test]
    fn create_shows_the_setup_report() {
        let workspace = tempfile::tempdir().unwrap();
        let steps = vec!["Install ripgrep".to_string()];
        assert!(setup_report(workspace.path(), &[]).is_none());
        let missing = setup_report(workspace.path(), &steps).unwrap();
        assert!(missing.contains("wrote no setup report"), "{missing}");

        std::fs::create_dir_all(workspace.path().join("kb")).unwrap();
        std::fs::write(
            workspace.path().join("kb/setup.md"),
            "- ripgrep: ready (`rg --version`)\n",
        )
        .unwrap();
        let report = setup_report(workspace.path(), &steps).unwrap();
        assert!(report.starts_with("Setup report ("), "{report}");
        assert!(
            report.ends_with("- ripgrep: ready (`rg --version`)"),
            "{report}"
        );
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

        // A peko of this name exists, with a sentinel file the
        // overwrite (if it happened) would destroy.
        let shared = paths.resolver().principal_layout("scout").shared;
        std::fs::create_dir_all(&shared.roles_dir).unwrap();
        std::fs::write(&shared.config_file, "name = \"scout\"\n").unwrap();
        std::fs::write(shared.roles_dir.join("sentinel.md"), "do-not-delete")
            .expect("write sentinel");

        // Without --force: refused.
        let err = match plan_create("scout", Some("demo-model"), None, false, false, &paths).await {
            Err(e) => e,
            Ok(_) => panic!("a create without --force must fail"),
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already exists") && msg.contains("remove"),
            "error must name the guard + the recovery path: {msg}"
        );

        // With --force and --yes the plan says to replace it. Planning
        // itself removes nothing: the daemon does, when asked.
        let plan = plan_create("scout", Some("demo-model"), None, true, true, &paths)
            .await
            .expect("create with --force --yes is planned")
            .expect("nothing was declined");
        assert!(plan.replace, "--force replaces the existing peko");
        assert!(
            shared.roles_dir.join("sentinel.md").exists(),
            "planning must not destroy workspace data"
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
}
