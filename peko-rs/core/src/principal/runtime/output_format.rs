//! Output format preference used by slash commands and other structured
//! daemon responses.

use serde::{Deserialize, Serialize};

/// Preferred output format for a command response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Human-readable plain text.
    #[default]
    Human,
    /// Machine-readable JSON.
    Json,
}

impl OutputFormat {
    /// True if this is the JSON format.
    #[must_use]
    pub fn is_json(self) -> bool {
        matches!(self, OutputFormat::Json)
    }
}
