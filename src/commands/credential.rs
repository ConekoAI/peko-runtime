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
use crate::common::secret_store::{OsKeychainSecretStore, SecretStore};
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
    /// Cheap format-only check on a stored key.
    ///
    /// For real round-trip tests, use `peko provider fetch-models`.
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
    match vault.test_provider_key(provider) {
        Some(true) => println!("Stored key for '{provider}' looks well-formed."),
        Some(false) => println!("Stored key for '{provider}' has an unexpected shape."),
        None => {
            suggest_for_missing(provider, &vault.list_providers(), known_provider_ids);
            println!("No key stored for '{provider}'.");
        }
    }
    Ok(())
}

async fn migrate_cmd(vault: &Vault) -> Result<()> {
    let legacy = OsKeychainSecretStore::new();
    let accounts = legacy
        .list_accounts()
        .with_context(|| "failed to list legacy keychain entries")?;

    if accounts.is_empty() {
        println!("No legacy keychain entries found to migrate.");
        return Ok(());
    }

    let mut migrated = 0;
    for account in accounts {
        match legacy.get(&account) {
            Ok(Some(secret)) => {
                if vault.get_provider_key(&account).is_some() {
                    println!("  - '{account}' already exists in vault, skipping");
                    continue;
                }
                vault.set_provider_key(&account, &secret)?;
                println!("  + Migrated '{account}'");
                migrated += 1;
            }
            Ok(None) => {
                println!("  - '{account}' not found in keychain, skipping");
            }
            Err(e) => {
                println!("  ! Failed to read '{account}': {e}");
            }
        }
    }

    println!("Migrated {migrated} provider key(s) into the vault.");
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
}

