//! Credential management commands.
//!
//! These commands manage runtime secrets stored in the encrypted vault at
//! `{config_dir}/vault.enc` (see `crate::common::vault`). Provider API keys
//! are the most common use case, but the vault also holds registry tokens,
//! identity private keys, and tunnel private keys.
//!
//! Typical flows:
//!
//! ```text
//! # Set an API key (hidden prompt)
//! peko credential set openai
//!
//! # List providers that have a key stored
//! peko credential list
//!
//! # Format-only validation
//! peko credential test openai
//!
//! # Remove a key
//! peko credential delete openai
//!
//! # Migrate legacy keychain entries into the vault
//! peko credential migrate
//! ```

use crate::commands::GlobalPaths;
use crate::common::vault::Vault;
use crate::providers::catalog::ProviderCatalog;
use anyhow::{Context, Result};

/// Credential commands
#[derive(clap::Subcommand)]
pub enum CredentialCommands {
    /// Store an API key for a provider in the vault.
    ///
    /// The key is read from the terminal with hidden echo (or from
    /// `--key` for scripting). It is encrypted at rest inside the vault.
    ///
    /// By default, `provider` must match an id present in
    /// `providers.toml`. The check prevents a typo (e.g. `miniax`
    /// instead of `minimax`) from silently creating an orphan vault
    /// entry that no provider can read. Pass `--allow-unknown` to
    /// bypass the check for backup-restore workflows where keys
    /// arrive before their provider is added.
    Set {
        /// Provider id (e.g. `openai`, `anthropic`, `groq`).
        provider: String,
        /// Read the key from this argument instead of prompting.
        /// Intended for non-interactive use; prefer the prompt in
        /// shell history-sensitive environments.
        #[arg(long)]
        key: Option<String>,
        /// Allow storing a key for a provider id that isn't in the
        /// catalog. See the subcommand doc for the use case.
        #[arg(long)]
        allow_unknown: bool,
    },
    /// Remove a stored key.
    Delete {
        /// Provider id.
        provider: String,
    },
    /// List provider ids that currently have a stored key.
    List,
    /// Live validation: ping the provider's real API with the stored
    /// key and report whether the request succeeded (HTTP 2xx) or
    /// what went wrong (401 / 403 / 404 / 429 / 5xx / connection
    /// refused / DNS / timeout). The old shape-only check couldn't
    /// tell `sk-opena-12345` from a real key — this does. For
    /// local / keyless providers (e.g. Ollama) the ping goes out
    /// without an Authorization header.
    Test {
        /// Provider id.
        provider: String,
    },
    /// Migrate legacy provider keys from the OS keychain into the vault.
    ///
    /// This reads any keys previously stored by `peko credential set` and
    /// writes them into the encrypted vault. The original keychain entries
    /// are left in place as a safety measure; delete them manually once
    /// you have verified the migration.
    Migrate,
}

/// Execute a credential subcommand.
pub async fn execute(cmd: CredentialCommands, paths: &GlobalPaths) -> Result<()> {
    let vault =
        Vault::load(paths.resolver().vault()).with_context(|| "failed to load credential vault")?;
    // Snapshot the catalog's id list once. `set` validates against it;
    // `test`/`delete` consult it for nearest-neighbor suggestions when
    // the requested id has no stored key. A fresh run with no
    // providers.toml (empty catalog) returns an empty vec.
    let known_provider_ids = load_known_provider_ids(paths).await;

    match cmd {
        CredentialCommands::Set {
            provider,
            key,
            allow_unknown,
        } => set_cmd(&vault, &provider, key, allow_unknown, &known_provider_ids).await,
        CredentialCommands::Delete { provider } => {
            delete_cmd(&vault, &provider, &known_provider_ids).await
        }
        CredentialCommands::List => list_cmd(&vault).await,
        CredentialCommands::Test { provider } => {
            test_cmd(&vault, &provider, &known_provider_ids).await
        }
        CredentialCommands::Migrate => migrate_cmd(&vault).await,
    }
}

/// Read the provider id list from `providers.toml` once at command
/// dispatch time. A corrupt or missing file is treated as "no known
/// ids" — validation can still surface a "no catalog yet" hint but
/// won't gate the command.
async fn load_known_provider_ids(paths: &GlobalPaths) -> Vec<String> {
    let catalog_path = paths.config_dir.join(ProviderCatalog::FILENAME);
    let Ok(catalog) = ProviderCatalog::load_or_init(&catalog_path).await else {
        return Vec::new();
    };
    catalog
        .list_all()
        .await
        .into_iter()
        .map(|e| e.id)
        .collect()
}

async fn set_cmd(
    vault: &Vault,
    provider: &str,
    key: Option<String>,
    allow_unknown: bool,
    known_provider_ids: &[String],
) -> Result<()> {
    // Validate before the secret prompt so we don't ask the user to
    // type a key they're about to throw away. `--allow-unknown`
    // bypasses for backup-restore workflows.
    if !allow_unknown {
        validate_known_provider(provider, known_provider_ids)?;
    }
    let secret_value = match key {
        Some(k) if !k.is_empty() => k,
        _ => prompt_hidden("API key: ")?,
    };
    let secret = secrecy::SecretString::from(secret_value);
    vault
        .set_provider_key(provider, &secret)
        .with_context(|| format!("failed to store key for '{provider}' in vault"))?;
    println!("Stored key for '{provider}' in the vault.");
    notify_daemon_reload().await;
    Ok(())
}

async fn delete_cmd(
    vault: &Vault,
    provider: &str,
    known_provider_ids: &[String],
) -> Result<()> {
    if vault.delete_provider_key(provider)? {
        println!("Removed key for '{provider}'.");
        notify_daemon_reload().await;
    } else {
        suggest_for_missing(provider, &vault.list_providers(), known_provider_ids);
        println!("No key stored for '{provider}'.");
    }
    Ok(())
}

/// Tell the running daemon to re-read the vault so the in-flight
/// root agent sees the key just stored/deleted. Silent on connection
/// failure (daemon may not be running; the next `peko daemon start`
/// will pick up the new state from disk).
async fn notify_daemon_reload() {
    let Ok(client) = crate::ipc::DaemonClient::connect().await else {
        return;
    };
    if let Err(e) = client.reload_providers().await {
        eprintln!("Daemon reload failed: {e}");
    }
}

async fn list_cmd(vault: &Vault) -> Result<()> {
    let accounts = vault.list_providers();
    if accounts.is_empty() {
        println!("No provider API keys stored in the vault.");
        return Ok(());
    }
    println!("Providers with stored keys ({}):", accounts.len());
    for a in accounts {
        println!("  - {a}");
    }
    Ok(())
}

async fn test_cmd(
    vault: &Vault,
    provider: &str,
    known_provider_ids: &[String],
) -> Result<()> {
    // Resolve the provider id to the default credential id for that
    // provider namespace. RP4 will widen the CLI to accept an explicit
    // credential id; during RP3A `peko credential test <provider>` is
    // preserved as a provider-keyed shortcut.
    let credential_id = match find_default_credential_id(vault, provider) {
        Some(id) => id,
        None => {
            anyhow::bail!("no key stored for '{provider}'");
        }
    };

    let client = match crate::ipc::DaemonClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            anyhow::bail!(
                "cannot reach the daemon (is `peko daemon start` running?): {e}"
            );
        }
    };
    let resp = client.credential_test(&credential_id).await?;
    let (lines, exit_code) = render_credential_tested(provider, &resp);
    for line in &lines {
        println!("{line}");
    }
    if message_invites_suggestion(provider, &resp) {
        suggest_for_missing(provider, &vault.list_providers(), known_provider_ids);
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

/// Find the id of the default credential at `provider:{provider}`.
fn find_default_credential_id(vault: &Vault, provider: &str) -> Option<String> {
    let namespace = format!("provider:{provider}");
    let summaries = vault.list_credentials(&crate::common::vault::CredentialFilter {
        namespace: Some(namespace),
        kind: None,
    });
    summaries.into_iter().find(|s| s.name == "default").map(|s| s.id)
}

/// Pure formatter for `CredentialTested` so the IPC plumbing is
/// tested by `cargo build` and the human-readable output is
/// covered by unit tests without spawning a daemon. Returns
/// `(lines, exit_code)` — exit code is 0 for success, 2 for a
/// validator failure, distinct from `1` (clap usage error) so
/// scripts can branch on outcome cleanly.
fn render_credential_tested(
    provider: &str,
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
        let mut lines = vec![format!("✓ {message} ({latency_ms}ms)")];
        if let Some(model) = model_used {
            lines.push(format!("  via {model} (~1 token billed)"));
        }
        (lines, 0)
    } else {
        let mut lines = vec![format!("✗ {message}")];
        if let Some(code) = http_status {
            lines.push(format!("  HTTP {code} after {latency_ms}ms"));
        } else if message.contains("unknown provider") || message.contains("no key stored") {
            // Config-level errors — the validator never dialed, so
            // there's no "connection" to fail. A short tail line
            // keeps the format consistent (always 2 lines for !ok).
            lines.push(format!("  ({latency_ms}ms — request was not sent)"));
        } else {
            lines.push(format!("  connection failed after {latency_ms}ms"));
        }
        let _ = provider; // provider name surfaces via the daemon's `message`
        (lines, 2)
    }
}

/// True iff the validator's failure message names a configuration
/// gap the user can fix from this command — a missing key
/// (`no key stored for X`) or a typo (`unknown provider: X`). Both
/// benefit from the nearest-neighbor hint. Real failures (401,
/// 429, connection refused) already carry enough context in the
/// HTTP / connection line — adding a typo suggestion there would
/// be noise.
fn message_invites_suggestion(
    _provider: &str,
    resp: &crate::ipc::packet::ResponsePacket,
) -> bool {
    if let crate::ipc::packet::ResponsePacket::CredentialTested { message, .. } = resp {
        message.contains("no key stored") || message.contains("unknown provider")
    } else {
        false
    }
}

async fn migrate_cmd(_vault: &Vault) -> Result<()> {
    // RP3B: legacy per-provider OS keychain entries are no longer a
    // supported secret source. The unified vault (`crate::common::vault`)
    // is the single source of truth for provider API keys.
    println!("No legacy keychain entries to migrate.");
    Ok(())
}

/// Prompt the user on stdin with hidden echo.
///
/// Uses `rpassword` (already a dependency for `portable/crypto.rs`).
/// Falls back to plain stdin on stdin-not-a-tty, with a clear note in
/// the logs that the input may be visible in the shell history.
fn prompt_hidden(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let value = rpassword::prompt_password(prompt)
            .map_err(|e| anyhow::anyhow!("failed to read hidden prompt: {e}"))?;
        Ok(value)
    } else {
        eprintln!(
            "(warning: stdin is not a TTY; reading key visibly. \
             Pipe via `peko credential set <id> --key <KEY>` for non-interactive use.)"
        );
        let mut s = String::new();
        stdin
            .read_line(&mut s)
            .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;
        Ok(s.trim().to_string())
    }
}

/// Reject a provider id that isn't in the catalog; on rejection, list
/// the known ids and offer nearest-neighbor suggestions so the user
/// can spot a typo without running `peko provider list` first.
///
/// Synchronous — `set_cmd` already has the catalog snapshot in hand
/// and we want a hard fail *before* the secret prompt. Returns
/// `anyhow::Error` whose Display chain is exactly what the user sees.
fn validate_known_provider(provider: &str, known_provider_ids: &[String]) -> Result<()> {
    if known_provider_ids.iter().any(|id| id == provider) {
        return Ok(());
    }

    let mut msg = format!("unknown provider id '{provider}'");

    if known_provider_ids.is_empty() {
        msg.push_str(
            "\nThe provider catalog is empty. Add the provider first with \
             `peko provider add --template <name>` (use --allow-unknown here \
             only if you're pre-loading keys before the provider is added).",
        );
    } else {
        let quoted: Vec<String> = known_provider_ids
            .iter()
            .map(|s| format!("'{s}'"))
            .collect();
        msg.push_str(&format!(
            "\nKnown provider ids: {}",
            quoted.join(", ")
        ));
        let suggestions = nearest_neighbors(provider, known_provider_ids, 3);
        if !suggestions.is_empty() {
            let q: Vec<String> = suggestions
                .iter()
                .map(|s| format!("'{s}'"))
                .collect();
            msg.push_str(&format!("\nDid you mean: {}", q.join(", ")));
        }
    }

    msg.push_str(
        "\nUse --allow-unknown to store a key for this id anyway (e.g. for \
         backup-restore before the provider is added).",
    );

    anyhow::bail!("{msg}")
}

/// Emit a "did you mean …?" line on stdout when the requested id
/// has no stored key — the user almost certainly typoed. Looks at
/// both the currently-stored keys and the catalog so a fresh
/// typo (never stored) still gets a suggestion toward a known
/// provider id.
///
/// Silently does nothing if there's nothing close enough (>= 4 edits).
fn suggest_for_missing(
    target: &str,
    stored_keys: &[String],
    known_provider_ids: &[String],
) {
    // Merge stored + known into a single candidate set so suggestions
    // cover both "you typoed an existing stored id" and "you typoed a
    // known provider". Dedup is cheap at this scale.
    let mut candidates: Vec<String> = Vec::with_capacity(stored_keys.len() + known_provider_ids.len());
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

/// Return up to `limit` candidate strings within edit distance 3 of
/// `target`, ordered by ascending distance then by candidate string
/// for determinism. Ties broken alphabetically.
///
/// `candidates` is typically the union of stored-key ids and known-
/// catalog ids. Empty input is a no-op.
///
/// The 3-edit cap filters out noise from obviously-different ids (e.g.
/// "openai" vs "anthropic" is 7 edits) so suggestions stay relevant.
fn nearest_neighbors(target: &str, candidates: &[String], limit: usize) -> Vec<String> {
    if limit == 0 || candidates.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, &String)> = candidates
        .iter()
        .map(|c| (levenshtein(target, c), c))
        .filter(|(d, _)| *d <= 3)
        .collect();

    // Sort by (distance, candidate-string) for deterministic output —
    // same input → same suggestion list across runs.
    scored.sort_by(|(da, sa), (db, sb)| da.cmp(db).then_with(|| sa.cmp(sb)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, c)| c.clone()).collect()
}

/// Iterative two-row Levenshtein distance. Operates on Unicode chars
/// not bytes (handles combining accents, emoji, CJK correctly).
///
/// O(len(a) * len(b)) time, O(min(len(a), len(b))) space. Provider
/// ids are short so the worst case (e.g. 32-char ids) is trivial.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();

    // Make `b` the shorter row to minimise memory.
    let (a, b) = if a.len() < b.len() { (b, a) } else { (a, b) };

    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::packet::ResponsePacket;

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

    /// Out-of-range distance (>= 4) drops the candidate entirely so
    /// unrelated ids don't appear in the suggestion list.
    #[test]
    fn nearest_neighbors_drops_distant_candidates() {
        let candidates = vec![
            "anthropic".to_string(), // 7 edits from "miniax"
            "openai".to_string(),    // 5 edits
        ];
        let got = nearest_neighbors("miniax", &candidates, 3);
        assert!(got.is_empty(), "got: {got:?}");
    }

    /// Empty / zero-limit input returns nothing rather than panicking.
    #[test]
    fn nearest_neighbors_handles_empty_inputs() {
        assert!(nearest_neighbors("minimax", &[], 3).is_empty());
        assert!(nearest_neighbors("minimax", &["minimax".to_string()], 0).is_empty());
    }

    /// Tie-breaker is alphabetical so two strings with the same
    /// distance always come out in the same order across runs.
    #[test]
    fn nearest_neighbors_breaks_ties_alphabetically() {
        // Both candidates are 1 edit from `foo`: insert one char
        // (`foox` = append 'x' at end, `xfoo` = prepend 'x'). Same
        // distance — alphabetical sort should pick `foox` first.
        let candidates = vec!["xfoo".to_string(), "foox".to_string()];
        let got = nearest_neighbors("foo", &candidates, 3);
        assert_eq!(got, vec!["foox".to_string(), "xfoo".to_string()],);
    }

    /// `validate_known_provider` should pass when the id is in the
    /// catalog and error with a "Did you mean" line for typos.
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
        assert!(
            msg.contains("--allow-unknown"),
            "error must mention the escape hatch, got: {msg}"
        );
    }

    #[test]
    fn validate_known_provider_handles_empty_catalog() {
        let err =
            validate_known_provider("minimax", &[]).expect_err("empty catalog must reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("The provider catalog is empty"), "got: {msg}");
    }

    /// Levenshtein correctness on a few hand-checked inputs so the
    /// helper isn't quietly broken (which would invalidate every
    /// assertion above that relies on edit-distance ordering).
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

    /// Successful OpenAI-compat ping: status line + latency, no
    /// `via <model>` line because `model_used` is `None`.
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
        assert!(lines[0].starts_with("✓ "), "got: {}", lines[0]);
        assert!(lines[0].contains("(187ms)"), "got: {}", lines[0]);
        assert!(lines[0].contains("124 models"), "got: {}", lines[0]);
    }

    /// Successful Anthropic-format ping: model_used drives the
    /// "via <model> (~1 token billed)" line.
    #[test]
    fn render_credential_tested_ok_anthropic_includes_via_model_line() {
        let resp = ResponsePacket::CredentialTested {
            request_id: 2,
            id: "id-anthropic".to_string(),
            ok: true,
            message: "Connection successful (1 token billed via claude-haiku-4-5)"
                .to_string(),
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
        assert!(lines[1].contains("~1 token billed"), "got: {}", lines[1]);
    }

    /// HTTP failure (401): exit 2, ✗ prefix, HTTP code on a second
    /// line — scripts branch on the exit code without scraping stdout.
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
        assert!(lines[0].starts_with("✗ "), "got: {}", lines[0]);
        assert!(lines[0].contains("invalid api key"), "got: {}", lines[0]);
        assert!(
            lines[1].contains("HTTP 401"),
            "expected HTTP status on second line, got: {:?}",
            lines
        );
    }

    /// Connection-level failure (no HTTP status): second line says
    /// "connection failed" so the user knows it's not a 4xx.
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
        assert!(lines[0].starts_with("✗ "));
        assert!(
            lines[1].contains("connection failed"),
            "expected connection-failed line, got: {:?}",
            lines
        );
    }

    /// "No key stored for X" or "unknown provider: X" — config
    /// gaps the user can fix from this command. Real failures
    /// (401, 429, connection refused) carry enough context in
    /// the HTTP / connection line — adding a typo suggestion there
    /// would be noise.
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
        assert!(
            message_invites_suggestion("miniax", &unknown),
            "typo on unknown provider must invite suggestion"
        );

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
        assert!(
            !message_invites_suggestion("openai", &auth),
            "401 doesn't invite a typo suggestion — the HTTP code line is enough"
        );
    }

    /// Config-level errors get a "request was not sent" tail line
    /// rather than the misleading "connection failed" — the
    /// validator never dialed.
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

    /// Unexpected variant (e.g. an old daemon returning
    /// `ProviderList` because `CredentialTested` isn't supported)
    /// must surface a clear "is the daemon up-to-date?" hint rather
    /// than panic.
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
        assert!(
            lines[0].contains("up-to-date"),
            "got: {}",
            lines[0]
        );
    }
}

