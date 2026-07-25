//! Extension discovery paths and types
//!
//! Defines standard discovery locations and the [`DiscoveredExtension`] type.

use std::path::PathBuf;

/// Discovered extension before loading
#[derive(Debug, Clone)]
pub struct DiscoveredExtension {
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub extension_type: String,
}

/// Standard extension discovery paths
pub mod discovery_paths {
    use std::path::PathBuf;

    #[must_use]
    pub fn user_config() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("peko/extensions"))
    }

    #[must_use]
    pub fn user_data() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("peko/extensions"))
    }

    #[must_use]
    pub fn project_local() -> PathBuf {
        PathBuf::from(".peko/extensions")
    }

    #[must_use]
    pub fn system_wide() -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            Some(PathBuf::from("/usr/share/peko/extensions"))
        }
        #[cfg(target_os = "macos")]
        {
            Some(PathBuf::from(
                "/Library/Application Support/peko/extensions",
            ))
        }
        #[cfg(target_os = "windows")]
        {
            Some(PathBuf::from("C:\\ProgramData\\peko\\extensions"))
        }
    }

    #[must_use]
    pub fn all() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Some(p) = user_config() {
            paths.push(p);
        }
        if let Some(p) = user_data() {
            paths.push(p);
        }
        paths.push(project_local());
        if let Some(p) = system_wide() {
            paths.push(p);
        }

        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_paths() {
        let paths = discovery_paths::all();
        assert!(!paths.is_empty());
        assert!(paths.contains(&PathBuf::from(".peko/extensions")));
    }
}
