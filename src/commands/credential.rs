//! Credential management commands.
//!
//! These commands manage provider API keys, which are stored in the
//! OS keychain via `crate::common::secret_store`. They replace the
//! earlier `peko auth set` flow that wrote plaintext JSON.
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
//! ```

use crate::commands::GlobalPaths;
use crate::common::secret_store::{OsKeychainSecretStore, SecretStore};
use anyhow::{Context, Result};
use std::sync::Arc;

/// Credential commands
#[derive(clap::Subcommand)]
pub enum CredentialCommands {
    /// Store an API key for a provider in the OS keychain.
    ///
    /// The key is read from the terminal with hidden echo (or from
    /// `--key` for scripting). It is never written to disk in
    /// plaintext.
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
}

/// Execute a credential subcommand.
pub async fn execute(cmd: CredentialCommands, _paths: &GlobalPaths) -> Result<()> {
    let store: Arc<dyn SecretStore> = Arc::new(OsKeychainSecretStore::new());
    match cmd {
        CredentialCommands::Set { provider, key } => set_cmd(store.as_ref(), &provider, key).await,
        CredentialCommands::Delete { provider } => delete_cmd(store.as_ref(), &provider).await,
        CredentialCommands::List => list_cmd(store.as_ref()).await,
        CredentialCommands::Test { provider } => test_cmd(store.as_ref(), &provider).await,
    }
}

async fn set_cmd(
    store: &dyn SecretStore,
    provider: &str,
    key: Option<String>,
) -> Result<()> {
    let secret_value = match key {
        Some(k) if !k.is_empty() => k,
        _ => prompt_hidden("API key: ")?,
    };
    let secret = secrecy::SecretString::from(secret_value);
    store.set(provider, &secret).with_context(|| {
        format!(
            "failed to store key for '{provider}' in the OS keychain (is a secret service running?)"
        )
    })?;
    println!("Stored key for '{provider}' in the OS keychain.");
    Ok(())
}

async fn delete_cmd(store: &dyn SecretStore, provider: &str) -> Result<()> {
    if store.delete(provider)? {
        println!("Removed key for '{provider}'.");
    } else {
        println!("No key stored for '{provider}'.");
    }
    Ok(())
}

async fn list_cmd(store: &dyn SecretStore) -> Result<()> {
    let accounts = store.list_accounts()?;
    if accounts.is_empty() {
        println!(
            "No keys stored in the OS keychain (or the platform keychain \
             does not support enumeration)."
        );
        return Ok(());
    }
    println!("Providers with stored keys ({}):", accounts.len());
    for a in accounts {
        println!("  - {a}");
    }
    Ok(())
}

async fn test_cmd(store: &dyn SecretStore, provider: &str) -> Result<()> {
    match store.test_format(provider)? {
        Some(true) => println!("Stored key for '{provider}' looks well-formed."),
        Some(false) => println!("Stored key for '{provider}' has an unexpected shape."),
        None => println!("No key stored for '{provider}'."),
    }
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