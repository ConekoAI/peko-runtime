//! Per-channel configuration.
//!
//! PR-2 introduces a small TOML config file at `<channel_dir>/config.toml`
//! that mirrors the multi-model-subagents (PR #346) parent-driven model
//! choice + cost ceiling. The shape is intentionally minimal — three
//! fields, all optional — and the file is only read by the responder
//! (commit 2b). The rest of `peko-channel` ignores it.
//!
//! ## File location
//!
//! ```text
//! <runtime_dir>/channels/<chan_id>/config.toml
//! ```
//!
//! PR-5b ships `<channel_dir>/{meta.json, members.json, events.jsonl,
//! cursors.json}`. This adds `config.toml` as a fourth sibling. The
//!
//! `config.toml` file is only read by future per-channel dispatch
//! policies (e.g. PR-3's pin/config-set flow) — the rest of
//! `peko-channel` ignores it.
//!
//! ## Atomic-write convention
//!
//! Same `tmp + fsync + rename` pattern as `cursors.json` and `meta.json` —
//! channels do not invent a new convention.
//!
//! ## Defaults
//!
//! PR-2 starts every channel with empty `model_list`, `cost_ceiling_usd:
//! None`, `default_subagent_type: None`. The responder falls through to
//! the principal's defaults (F40 cost ceiling + the principal's
//! configured model) when channel config is empty.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::port::{ChannelError, Result};

// ---------------------------------------------------------------------------
// File shape
// ---------------------------------------------------------------------------

/// On-disk config for a single channel. Stored at `<chan_dir>/config.toml`.
///
/// All fields are optional with sensible defaults — a channel with no
/// `config.toml` (or a partial one) gets `Self::default()`. This lets
/// callers add fields in future without breaking older config files.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigOnDisk {
    /// Subagent model ids to round-robin through when this channel
    /// triggers a `consider_response` dispatch. Empty means "use the
    /// principal's configured model".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_list: Vec<String>,

    /// Per-spawn USD ceiling. When set, overrides the principal's
    /// `QuotaConfig::cost_per_call_max` for this channel's dispatches.
    /// When `None`, the responder falls through to the principal's
    /// ceiling (F40).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_ceiling_usd: Option<f64>,

    /// Default subagent type for this channel's dispatches. `None` lets
    /// the principal's default apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_subagent_type: Option<String>,
}

impl ConfigOnDisk {
    /// Standard location of the config file inside a channel directory.
    pub fn path_in(channel_dir: &Path) -> PathBuf {
        channel_dir.join("config.toml")
    }

    /// Persist config to `<channel_dir>/config.toml` atomically (tmp +
    /// fsync + rename). Mirrors `ChannelCursors::save` and `MetaJson::save`.
    pub async fn save(&self, channel_dir: &Path) -> Result<()> {
        let path = Self::path_in(channel_dir);
        fs::create_dir_all(channel_dir).await?;

        let pid = std::process::id();
        let tmp = channel_dir.join(format!(".config.toml.{pid}.tmp"));

        let bytes = toml::to_string_pretty(self).map_err(|e| {
            ChannelError::Adapter(format!("serialize ConfigOnDisk: {e}"))
        })?;
        {
            let mut f = fs::File::create(&tmp).await.map_err(|e| {
                ChannelError::Adapter(format!("create {}: {e}", tmp.display()))
            })?;
            f.write_all(bytes.as_bytes()).await.map_err(|e| {
                ChannelError::Adapter(format!("write {}: {e}", tmp.display()))
            })?;
            f.sync_all().await.map_err(|e| {
                ChannelError::Adapter(format!("fsync {}: {e}", tmp.display()))
            })?;
        }
        fs::rename(&tmp, &path).await.map_err(|e| {
            ChannelError::Adapter(format!(
                "rename {} -> {}: {e}",
                tmp.display(),
                path.display()
            ))
        })?;
        Ok(())
    }

    /// Load config from `<channel_dir>/config.toml`. Returns
    /// `ConfigOnDisk::default()` if the file doesn't exist (fresh
    /// channel, never configured). Returns `ChannelError::Adapter` on a
    /// malformed file.
    pub async fn load(channel_dir: &Path) -> Result<Self> {
        let path = Self::path_in(channel_dir);
        match fs::read_to_string(&path).await {
            Ok(text) => toml::from_str(&text).map_err(|e| {
                ChannelError::Adapter(format!("decode {}: {e}", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ChannelError::Adapter(format!(
                "read {}: {e}",
                path.display()
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config_has_empty_model_list_and_no_ceiling() {
        let c = ConfigOnDisk::default();
        assert!(c.model_list.is_empty(), "default model_list must be empty");
        assert!(
            c.cost_ceiling_usd.is_none(),
            "default cost_ceiling_usd must be None"
        );
        assert!(
            c.default_subagent_type.is_none(),
            "default default_subagent_type must be None"
        );
    }

    #[tokio::test]
    async fn round_trip_via_disk() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_cfg");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let c = ConfigOnDisk {
            model_list: vec!["claude-sonnet-4.6".into(), "claude-haiku-4.5".into()],
            cost_ceiling_usd: Some(0.25),
            default_subagent_type: Some("writer".into()),
        };
        c.save(&channel_dir).await.unwrap();

        let loaded = ConfigOnDisk::load(&channel_dir).await.unwrap();
        assert_eq!(loaded, c);
    }

    #[tokio::test]
    async fn partial_toml_decodes_with_defaults_for_missing_fields() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_partial");
        std::fs::create_dir_all(&channel_dir).unwrap();

        // Hand-craft a partial TOML — only `model_list` present. The
        // other two fields must fall through to defaults.
        let partial = "model_list = [\"claude-opus-4.8\"]\n";
        std::fs::write(ConfigOnDisk::path_in(&channel_dir), partial).unwrap();

        let loaded = ConfigOnDisk::load(&channel_dir).await.unwrap();
        assert_eq!(loaded.model_list, vec!["claude-opus-4.8".to_string()]);
        assert!(loaded.cost_ceiling_usd.is_none());
        assert!(loaded.default_subagent_type.is_none());
    }

    #[tokio::test]
    async fn load_missing_file_returns_default() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("nope");
        std::fs::create_dir_all(&channel_dir).unwrap();
        let c = ConfigOnDisk::load(&channel_dir).await.unwrap();
        assert_eq!(c, ConfigOnDisk::default());
    }

    #[tokio::test]
    async fn save_atomic_overwrites_existing() {
        let dir = TempDir::new().unwrap();
        let channel_dir = dir.path().join("chan_overwrite");
        std::fs::create_dir_all(&channel_dir).unwrap();

        let v1 = ConfigOnDisk {
            model_list: vec!["a".into()],
            ..Default::default()
        };
        v1.save(&channel_dir).await.unwrap();

        let v2 = ConfigOnDisk {
            model_list: vec!["b".into(), "c".into()],
            cost_ceiling_usd: Some(0.5),
            ..Default::default()
        };
        v2.save(&channel_dir).await.unwrap();

        let loaded = ConfigOnDisk::load(&channel_dir).await.unwrap();
        assert_eq!(loaded, v2);

        // And no tmp files left behind.
        let leftover: Vec<_> = std::fs::read_dir(&channel_dir)
            .unwrap()
            .filter_map(std::io::Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".config.toml.")
            })
            .collect();
        assert!(leftover.is_empty(), "found stale tmp files: {leftover:?}");
    }
}