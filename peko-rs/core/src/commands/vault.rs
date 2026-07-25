//! Vault management commands.
//!
//! These commands manage the encrypted vault at
//! `{config_dir}/vault.enc` (see `crate::common::vault`) — in particular
//! the on-disk unlock mode (keychain-backed DEK vs. passphrase-derived
//! DEK). Provider API keys, registry tokens, identity private keys, and
//! tunnel private keys all live inside this vault; the migrate command
//! re-encrypts the entire vault under a new DEK when switching modes.
//!
//! Typical flows:
//!
//! ```text
//! # Default state on macOS: keychain mode, prompts for password every run.
//! peko credential list
//!
//! # Switch to passphrase mode. Prompts for the new passphrase and
//! # re-encrypts the vault. After this, set PEKO_MASTER_PASSPHRASE in
//! # your shell rc and no keychain prompt will ever appear again.
//! peko vault migrate --to passphrase
//!
//! # Switch back to keychain mode (e.g. on a CI box with no shell rc).
//! peko vault migrate --to keychain
//! ```

use crate::commands::GlobalPaths;
use crate::common::vault::{UnlockMethod, UnlockMethodOverride, Vault};
use anyhow::{Context, Result};
use clap::ValueEnum;
use secrecy::SecretString;

/// Vault management subcommands.
#[derive(clap::Subcommand)]
pub enum VaultCommands {
    /// Re-encrypt the vault under a different unlock mode.
    ///
    /// Re-encrypts the vault under a new DEK (passphrase-derived or a
    /// freshly generated key stored in the OS keychain) and updates the
    /// keychain entry as needed. Use this to switch between keychain
    /// mode (the macOS default) and passphrase mode.
    ///
    /// Refuses to run if a peko daemon is reachable over IPC, since the
    /// daemon holds a long-lived `Vault` whose `unlock_method` is not
    /// mutable via `reload()`. Stop the daemon with `peko daemon stop`
    /// first.
    Migrate {
        /// Target unlock mode.
        #[arg(long, value_enum)]
        to: UnlockMethodArg,

        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

/// Target unlock mode for the `migrate` subcommand.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum UnlockMethodArg {
    /// Re-encrypt the vault under a passphrase-derived DEK.
    ///
    /// After migration, `PEKO_MASTER_PASSPHRASE` must be set in the
    /// environment for `peko` to unlock the vault. The old keychain
    /// entry is deleted (best-effort).
    Passphrase,
    /// Re-encrypt the vault under a fresh DEK stored in the OS keychain.
    ///
    /// The DEK is generated locally and stored under service `peko`,
    /// account `vault-key` in the OS keychain. Subsequent `peko`
    /// invocations will prompt the keychain for access unless the
    /// binary has a stable code-signing identity.
    Keychain,
}

impl From<UnlockMethodArg> for UnlockMethod {
    fn from(arg: UnlockMethodArg) -> Self {
        match arg {
            UnlockMethodArg::Passphrase => UnlockMethod::Passphrase,
            UnlockMethodArg::Keychain => UnlockMethod::Keychain,
        }
    }
}

/// Execute a vault subcommand.
pub async fn execute(cmd: VaultCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        VaultCommands::Migrate { to, yes } => migrate_cmd(paths, to.into(), yes).await,
    }
}

async fn migrate_cmd(paths: &GlobalPaths, target: UnlockMethod, skip_confirm: bool) -> Result<()> {
    // Refuse to migrate while a daemon is running. The daemon holds a
    // long-lived `Arc<Vault>` whose `unlock_method` is set at
    // construction time and is not mutable via `reload()`; flipping
    // the on-disk mode out from under it leaves the daemon unable to
    // decrypt the new envelope.
    if let Ok(client) = crate::ipc::DaemonClient::connect().await {
        let _ = client;
        anyhow::bail!(
            "refusing to migrate while a peko daemon is running; \
             stop it with `peko daemon stop` and retry"
        );
    }

    // Load the vault without the env-var override interfering. The
    // migrate operation must observe the *current* on-disk state, not
    // what the user has asserted in `PEKO_UNLOCK_METHOD`.
    let mut vault = Vault::load_with_override(paths.resolver().vault(), UnlockMethodOverride::Auto)
        .with_context(|| "failed to load vault for migration")?;

    let current = vault.unlock_method();
    if current == target {
        println!("Vault is already in {target:?} mode; nothing to do.");
        return Ok(());
    }

    if !skip_confirm {
        eprintln!(
            "About to re-encrypt the vault at {}\n  from {current:?} mode to {target:?} mode.",
            vault.path().display()
        );
        if target == UnlockMethod::Passphrase {
            eprintln!(
                "You will be prompted for the new passphrase. Set \
                 PEKO_MASTER_PASSPHRASE in your shell rc to avoid keychain \
                 prompts on every `peko` invocation."
            );
        } else {
            eprintln!(
                "A new DEK will be generated and stored in the OS keychain. \
                 Subsequent `peko` invocations may prompt for the keychain \
                 password on unsigned binaries."
            );
        }
        eprint!("Continue? [y/N] ");
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .with_context(|| "failed to read confirmation")?;
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // For passphrase mode, read the passphrase (from env or hidden
    // prompt). For keychain mode, no extra input is needed.
    let passphrase = if target == UnlockMethod::Passphrase {
        Some(read_passphrase_for_migrate()?)
    } else {
        None
    };

    let new_mode = vault
        .migrate(target, passphrase.as_ref())
        .with_context(|| format!("failed to migrate vault to {target:?} mode"))?;

    println!(
        "Vault at {} is now in {new_mode:?} mode.",
        vault.path().display()
    );
    Ok(())
}

/// Read the passphrase to use for the migration. Prefers
/// `PEKO_MASTER_PASSPHRASE` from the env; falls back to a hidden prompt.
fn read_passphrase_for_migrate() -> Result<SecretString> {
    if let Ok(pw) = std::env::var(crate::common::vault::MASTER_PASSPHRASE_ENV) {
        if !pw.is_empty() {
            return Ok(SecretString::new(pw.into()));
        }
    }
    let value = prompt_hidden("New vault passphrase: ")?;
    if value.is_empty() {
        anyhow::bail!("refusing to migrate to an empty passphrase");
    }
    // Confirm the passphrase by reading it twice. `rpassword` doesn't
    // expose a "read twice" helper, so we just re-prompt and compare.
    let confirm = prompt_hidden("Confirm passphrase: ")?;
    if value != confirm {
        anyhow::bail!("passphrases do not match; migration aborted");
    }
    Ok(SecretString::new(value.into()))
}

/// Hidden-prompt helper. Mirrors the one in `commands/credential.rs`
/// rather than threading a dependency through, since vault and
/// credential are sibling subcommand modules.
fn prompt_hidden(prompt: &str) -> Result<String> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let value = rpassword::prompt_password(prompt)
            .map_err(|e| anyhow::anyhow!("failed to read hidden prompt: {e}"))?;
        Ok(value)
    } else {
        let mut s = String::new();
        stdin
            .read_line(&mut s)
            .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;
        Ok(s.trim().to_string())
    }
}
