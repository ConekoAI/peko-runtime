//! The Hub account logged into this runtime's terminal (ADR-077).
//!
//! Independent of tunnel ownership: logging in or out never changes which
//! account registered the runtime. Local trust is owner trust (ADR-057),
//! so this account is also an owner-equivalent actor (ADR-078).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The authenticated human using the local terminal, independent of tunnel ownership.
/// Only public account metadata lives here; the API credential remains in the vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubLogin {
    pub user_id: String,
    pub hub_url: String,
}
impl HubLogin {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("runtime").join("hub-login.json")
    }
    pub fn load(config_dir: &Path) -> Result<Option<Self>> {
        let path = Self::path(config_dir);
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let login: Self = serde_json::from_slice(&data)?;
        uuid::Uuid::parse_str(&login.user_id).context("Hub login requires an account UUID")?;
        Ok(Some(login))
    }
    pub fn save(&self, config_dir: &Path) -> Result<()> {
        uuid::Uuid::parse_str(&self.user_id).context("Hub login requires an account UUID")?;
        let path = Self::path(config_dir);
        std::fs::create_dir_all(path.parent().expect("login parent"))?;
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, serde_json::to_vec(self)?)?;
        std::fs::rename(temp, path)?;
        Ok(())
    }
    pub fn logout(config_dir: &Path) -> Result<()> {
        match std::fs::remove_file(Self::path(config_dir)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl HubLogin {
    /// The logged-in account as an actor, if any.
    #[must_use]
    pub fn current_subject(config_dir: &Path) -> Option<peko_auth::Subject> {
        Self::load(config_dir)
            .ok()
            .flatten()
            .map(|login| peko_auth::Subject::User(format!("hub:{}", login.user_id)))
    }
}
