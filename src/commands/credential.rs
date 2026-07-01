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
use anyhow::{Context, Result};

/// Credential commands
#[derive(clap::Subcommand)]
pub enum CredentialCommands {
    /// Store an API key for a provider in the vault.
    ///
    /// The key is read from the terminal with hidden echo (or from
    /// `--key` for scripting). It is encrypted at rest inside the vault.
    Set {
        /// Provider id (e.g. `openai`, `anthropic`, `groq`).
        provider: String,
        /// Read the key from this argument instead of prompting.
        /// Intended for non-interactive use; prefer the prompt in
        /// shell history-sensitive environments.
        #[arg(long)]
        key: Option<String>,
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

    match cmd {
        CredentialCommands::Set { provider, key } => set_cmd(&vault, &provider, key).await,
        CredentialCommands::Delete { provider } => delete_cmd(&vault, &provider).await,
        CredentialCommands::List => list_cmd(&vault).await,
        CredentialCommands::Test { provider } => test_cmd(&vault, &provider).await,
        CredentialCommands::Migrate => migrate_cmd(&vault).await,
    }
}

async fn set_cmd(vault: &Vault, provider: &str, key: Option<String>) -> Result<()> {
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

async fn delete_cmd(vault: &Vault, provider: &str) -> Result<()> {
    if vault.delete_provider_key(provider)? {
        println!("Removed key for '{provider}'.");
        notify_daemon_reload().await;
    } else {
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

async fn test_cmd(vault: &Vault, provider: &str) -> Result<()> {
    match vault.test_provider_key(provider) {
        Some(true) => println!("Stored key for '{provider}' looks well-formed."),
        Some(false) => println!("Stored key for '{provider}' has an unexpected shape."),
        None => println!("No key stored for '{provider}'."),
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
