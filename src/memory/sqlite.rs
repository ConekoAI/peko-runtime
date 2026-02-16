//! SQLite memory backend

use tracing::info;

/// SQLite memory store
pub struct SqliteMemory {
    path: String,
}

impl SqliteMemory {
    pub fn new(path: &str) -> Self {
        info!("Initializing SQLite memory at: {}", path);
        Self {
            path: path.to_string(),
        }
    }

    pub fn initialize(&self) -> anyhow::Result<()> {
        // TODO: Create tables
        Ok(())
    }
}
