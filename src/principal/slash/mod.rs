//! Daemon-side slash command dispatch.
//!
//! Slash commands are intercepted at the principal boundary so every
//! transport (CLI, IPC, GUI, tunnel/A2A) shares the same behavior.

pub mod help;

use crate::common::types::OutputFormat;
use crate::extensions::framework::store::ExtensionStore;
use crate::principal::Principal;
use peko_extension_host::services::Services as ExtensionServices;
use std::sync::Arc;

/// Result of handling a slash command.
#[derive(Debug, Clone)]
pub struct SlashResponse {
    /// Pre-rendered response text. This is either human-readable text or
    /// a JSON string, depending on the requested output format.
    pub content: String,
}

/// Error returned when slash dispatch fails.
#[derive(Debug, thiserror::Error)]
pub enum SlashError {
    #[error("unknown slash command '/{0}'. Only /help is available in v0.")]
    UnknownCommand(String),
    #[error("empty slash command. Type /help for available commands.")]
    EmptyCommand,
    #[error("failed to render /help: {0}")]
    HelpRender(#[from] anyhow::Error),
}

/// Dispatches slash commands for all callers that reach a Principal.
#[derive(Debug)]
pub struct SlashDispatcher {
    extension_store: Arc<ExtensionStore>,
    extension_services: Arc<ExtensionServices>,
}

impl SlashDispatcher {
    /// Create a new dispatcher bound to the daemon's extension state.
    #[must_use]
    pub fn new(
        extension_store: Arc<ExtensionStore>,
        extension_services: Arc<ExtensionServices>,
    ) -> Self {
        Self {
            extension_store,
            extension_services,
        }
    }

    /// If `message` is a slash command, handle it and return the rendered
    /// response. If it is not a slash command (or `no_slash` is true), return
    /// `None` so the caller can send the message to the root agent normally.
    ///
    /// A leading `\/"` is treated as an escape: the backslash is stripped
    /// and the remainder is sent to the root agent verbatim.
    pub async fn dispatch(
        &self,
        principal: &Principal,
        message: &str,
        no_slash: bool,
        format: OutputFormat,
    ) -> Result<Option<SlashResponse>, SlashError> {
        let trimmed = message.trim_start();

        // Escape hatch: `\/prefix` sends the literal `/prefix` text onward.
        if trimmed.strip_prefix("\\/").is_some() {
            return Ok(None);
        }

        if !trimmed.starts_with('/') || no_slash {
            return Ok(None);
        }

        // Drop the leading '/' and split the first token from any arguments.
        let after_slash = &trimmed[1..];
        let name = if let Some((n, _)) = after_slash.split_once(char::is_whitespace) {
            n
        } else {
            after_slash
        };

        match name {
            "" => Err(SlashError::EmptyCommand),
            "help" => {
                let content = help::handle_help(
                    principal,
                    &self.extension_store,
                    &self.extension_services,
                    format,
                )
                .await
                .map_err(SlashError::HelpRender)?;
                Ok(Some(SlashResponse { content }))
            }
            other => Err(SlashError::UnknownCommand(other.to_string())),
        }
    }
}
