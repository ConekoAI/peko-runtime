//! Slash command dispatch for `peko send`.
//!
//! Intercepts `/`-prefixed user input before it reaches the daemon and
//! routes to built-in slash commands. Future slash commands from
//! extensions will be dispatched here as well.

pub mod help;

use crate::commands::GlobalPaths;
use anyhow::Result;

/// If the message is a slash command, handle it and return `Some(())`.
/// If it is not a slash command, return `None` so normal `peko send`
/// processing continues.
///
/// Callers should strip any escape prefix (e.g. `\/`) before calling
/// this function; this module only handles genuine `/`-prefixed slash
/// commands.
pub async fn maybe_handle_slash(
    principal_name: &str,
    message: &str,
    paths: &GlobalPaths,
    json: bool,
) -> Result<Option<()>> {
    let trimmed = message.trim_start();
    if !trimmed.starts_with('/') {
        return Ok(None);
    }

    // Drop the leading '/' and split the first token from any arguments.
    let after_slash = &trimmed[1..];
    let (name, _rest) = if let Some((n, r)) = after_slash.split_once(char::is_whitespace) {
        (n, Some(r.trim_start()))
    } else {
        (after_slash, None)
    };

    match name {
        "" => anyhow::bail!("Empty slash command. Type /help for available commands."),
        "help" => {
            help::handle_help(principal_name, paths, json).await?;
            Ok(Some(()))
        }
        other => anyhow::bail!(
            "Unknown slash command '/{other}'. Only /help is available in v0."
        ),
    }
}
