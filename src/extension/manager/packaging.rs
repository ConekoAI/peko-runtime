//! Extension packaging for creating portable `.ext` packages
//!
//! Exports installed extensions to `.ext` files (gzip-compressed tar archives)
//! that can be shared and installed on other Pekobot instances.

use crate::extension::manager::ExtensionManager;
use crate::extension::types::ExtensionId;
use anyhow::Context;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Extension package manifest metadata
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtensionPackageManifest {
    /// Package format information
    pub format: PackageFormat,
    /// Extension information
    pub extension: ExtensionInfo,
    /// Packaging metadata (checksums, compression)
    pub packaging: ExtensionPackagingMetadata,
    /// Bundled dependencies (ADR-036)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DepInfo>,
}

/// Package format version info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackageFormat {
    /// Format version
    pub version: String,
    /// Peko version that created this package
    pub peko_version: String,
}

/// Extension info within the package manifest
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtensionInfo {
    /// Extension ID
    pub id: String,
    /// Extension name
    pub name: String,
    /// Extension type
    pub extension_type: String,
    /// Extension version
    pub version: String,
    /// Description
    pub description: String,
}

/// Bundled dependency info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DepInfo {
    /// Dependency extension ID
    pub id: String,
    /// Dependency name
    pub name: String,
    /// Dependency version
    pub version: String,
}

/// Packaging metadata with checksums
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExtensionPackagingMetadata {
    /// List of files in the package (relative paths)
    pub files: Vec<String>,
    /// Checksums for each file (path -> "sha256:...")
    pub checksums: HashMap<String, String>,
    /// Compression format
    pub compression: String,
    /// Archive format
    pub archive_format: String,
}

impl ExtensionPackageManifest {
    /// Serialize to TOML
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Parse from TOML
    pub fn from_toml(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }
}

/// Extension packager for creating `.ext` packages
pub struct ExtensionPackager;

impl ExtensionPackager {
    /// Export an installed extension to a `.ext` package.
    ///
    /// # Arguments
    /// * `manager` - The extension manager containing the installed extension
    /// * `id` - The extension ID to export
    /// * `output_path` - Where to write the `.ext` file
    ///
    /// # Returns
    /// The path to the created `.ext` package
    pub fn export(
        manager: &ExtensionManager,
        id: &ExtensionId,
        output_path: impl AsRef<Path>,
    ) -> anyhow::Result<PathBuf> {
        let output_path = output_path.as_ref();

        // Look up the extension
        let ext = manager
            .get_extension(id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

        let source_path = &ext.path;
        if !source_path.exists() {
            anyhow::bail!("Extension path does not exist: {}", source_path.display());
        }

        // Ensure parent directory exists
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create output directory: {}", parent.display())
                })?;
            }
        }

        // Collect all files from the extension directory
        let mut all_files: HashMap<String, Vec<u8>> = HashMap::new();
        Self::collect_files_recursive(source_path, "extension", &mut all_files)?;

        // Build packaging metadata with checksums
        let mut packaging = ExtensionPackagingMetadata {
            files: Vec::new(),
            checksums: HashMap::new(),
            compression: "gzip".to_string(),
            archive_format: "tar".to_string(),
        };

        for (path, content) in &all_files {
            packaging.files.push(path.clone());
            let checksum = Self::compute_checksum(content);
            packaging.checksums.insert(path.clone(), checksum);
        }

        // Create package manifest
        let manifest = ExtensionPackageManifest {
            format: PackageFormat {
                version: "1.0".to_string(),
                peko_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            extension: ExtensionInfo {
                id: ext.manifest.id.to_string(),
                name: ext.manifest.name.clone(),
                extension_type: ext.extension_type.clone(),
                version: ext.manifest.version.clone(),
                description: ext.manifest.description.clone(),
            },
            packaging,
            dependencies: Vec::new(),
        };

        // Create tar.gz archive
        let tar_gz = std::fs::File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add manifest
        let manifest_toml = manifest
            .to_toml()
            .context("Failed to serialize extension package manifest")?;
        let mut header = tar::Header::new_gnu();
        header.set_path("manifest.toml")?;
        header.set_size(manifest_toml.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest_toml.as_bytes())?;

        // Add all extension files
        for (path, content) in &all_files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();

            tar.append(&header, content.as_slice())
                .with_context(|| format!("Failed to add file: {path}"))?;
        }

        tar.finish()
            .context("Failed to finalize extension archive")?;

        Ok(output_path.to_path_buf())
    }

    /// Export an installed extension with its dependencies to a `.ext` package.
    ///
    /// # Arguments
    /// * `manager` - The extension manager containing the installed extension
    /// * `id` - The extension ID to export
    /// * `dep_ids` - IDs of dependency extensions to bundle
    /// * `output_path` - Where to write the `.ext` file
    ///
    /// # Returns
    /// The path to the created `.ext` package
    pub fn export_with_deps(
        manager: &ExtensionManager,
        id: &ExtensionId,
        dep_ids: &[ExtensionId],
        output_path: impl AsRef<Path>,
    ) -> anyhow::Result<PathBuf> {
        let output_path = output_path.as_ref();

        // Look up the primary extension
        let ext = manager
            .get_extension(id)
            .ok_or_else(|| anyhow::anyhow!("Extension '{id}' not found"))?;

        let source_path = &ext.path;
        if !source_path.exists() {
            anyhow::bail!("Extension path does not exist: {}", source_path.display());
        }

        // Ensure parent directory exists
        if let Some(parent) = output_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("Failed to create output directory: {}", parent.display())
                })?;
            }
        }

        // Collect all files from the extension directory
        let mut all_files: HashMap<String, Vec<u8>> = HashMap::new();
        Self::collect_files_recursive(source_path, "extension", &mut all_files)?;

        // Collect dependency files
        let mut dep_manifests = Vec::new();
        for dep_id in dep_ids {
            if let Some(dep_ext) = manager.get_extension(dep_id) {
                let dep_path = &dep_ext.path;
                if dep_path.exists() {
                    Self::collect_files_recursive(dep_path, &format!("deps/{}", dep_id), &mut all_files)?;
                    dep_manifests.push(DepInfo {
                        id: dep_id.to_string(),
                        name: dep_ext.manifest.name.clone(),
                        version: dep_ext.manifest.version.clone(),
                    });
                }
            }
        }

        // Build packaging metadata with checksums
        let mut packaging = ExtensionPackagingMetadata {
            files: Vec::new(),
            checksums: HashMap::new(),
            compression: "gzip".to_string(),
            archive_format: "tar".to_string(),
        };

        for (path, content) in &all_files {
            packaging.files.push(path.clone());
            let checksum = Self::compute_checksum(content);
            packaging.checksums.insert(path.clone(), checksum);
        }

        // Create package manifest
        let manifest = ExtensionPackageManifest {
            format: PackageFormat {
                version: "1.0".to_string(),
                peko_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            extension: ExtensionInfo {
                id: ext.manifest.id.to_string(),
                name: ext.manifest.name.clone(),
                extension_type: ext.extension_type.clone(),
                version: ext.manifest.version.clone(),
                description: ext.manifest.description.clone(),
            },
            packaging,
            dependencies: dep_manifests,
        };

        // Create tar.gz archive
        let tar_gz = std::fs::File::create(output_path)
            .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        // Add manifest
        let manifest_toml = manifest
            .to_toml()
            .context("Failed to serialize extension package manifest")?;
        let mut header = tar::Header::new_gnu();
        header.set_path("manifest.toml")?;
        header.set_size(manifest_toml.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, manifest_toml.as_bytes())?;

        // Add all extension files
        for (path, content) in &all_files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path)?;
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();

            tar.append(&header, content.as_slice())
                .with_context(|| format!("Failed to add file: {path}"))?;
        }

        tar.finish()
            .context("Failed to finalize extension archive")?;

        Ok(output_path.to_path_buf())
    }

    /// Collect all files recursively from a source directory into a flat map
    fn collect_files_recursive(
        source: &Path,
        prefix: &str,
        files: &mut HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            let path = entry.path();
            let file_name = entry.file_name();
            let rel_path = format!("{}/{}", prefix, file_name.to_string_lossy());

            if path.is_dir() {
                Self::collect_files_recursive(&path, &rel_path, files)?;
            } else {
                let content = std::fs::read(&path)
                    .with_context(|| format!("Failed to read file: {}", path.display()))?;
                files.insert(rel_path, content);
            }
        }
        Ok(())
    }

    /// Compute SHA-256 checksum for data
    fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }
}

/// Extension unpackager for installing `.ext` packages
pub struct ExtensionUnpackager;

impl ExtensionUnpackager {
    /// Install a `.ext` package into a target directory.
    ///
    /// Returns the path to the extracted extension directory.
    pub fn install(
        package_path: impl AsRef<Path>,
        target_dir: impl AsRef<Path>,
    ) -> anyhow::Result<PathBuf> {
        let package_path = package_path.as_ref();
        let target_dir = target_dir.as_ref();

        // Extract package
        let files = Self::extract_package(package_path.as_ref())?;

        // Parse and validate manifest
        let manifest = Self::parse_manifest(&files)?;
        Self::validate_checksums(&manifest, &files)?;

        // Determine target extension directory
        let ext_dir = target_dir.join(&manifest.extension.id);

        // Remove existing if present
        if ext_dir.exists() {
            // On Windows a previous process may hold a lock; retry briefly.
            let mut last_err = None;
            for attempt in 0..10 {
                match std::fs::remove_dir_all(&ext_dir) {
                    Ok(()) => {
                        last_err = None;
                        break;
                    }
                    Err(e) => {
                        last_err = Some(e);
                        if attempt + 1 < 10 {
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            }
            if let Some(e) = last_err {
                return Err(anyhow::anyhow!(
                    "Failed to remove existing extension: {}: {e}",
                    ext_dir.display()
                ));
            }
        }

        // Extract extension files
        for (path, content) in &files {
            if let Some(ext_path) = path.strip_prefix("extension/") {
                let target_path = ext_dir.join(ext_path);
                if let Some(parent) = target_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target_path, content)
                    .with_context(|| format!("Failed to write file: {}", target_path.display()))?;
            }
        }

        Ok(ext_dir)
    }

    /// Inspect a `.ext` package without installing
    pub fn inspect(package_path: impl AsRef<Path>) -> anyhow::Result<ExtensionPackageManifest> {
        let files = Self::extract_package(package_path.as_ref())?;
        Self::parse_manifest(&files)
    }

    /// Extract package files from tar.gz
    fn extract_package(package_path: &Path) -> anyhow::Result<HashMap<String, Vec<u8>>> {
        let tar_gz = std::fs::File::open(package_path)
            .with_context(|| format!("Failed to open package: {}", package_path.display()))?;
        let tar = flate2::read::GzDecoder::new(tar_gz);
        let mut archive = tar::Archive::new(tar);

        let mut files = HashMap::new();

        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_string_lossy().to_string();

            let mut content = Vec::new();
            entry.read_to_end(&mut content)?;

            files.insert(path, content);
        }

        Ok(files)
    }

    /// Parse manifest from extracted files
    fn parse_manifest(
        files: &HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<ExtensionPackageManifest> {
        let manifest_bytes = files
            .get("manifest.toml")
            .ok_or_else(|| anyhow::anyhow!("Missing manifest.toml in extension package"))?;
        let manifest_str = std::str::from_utf8(manifest_bytes)?;
        let manifest = ExtensionPackageManifest::from_toml(manifest_str)?;
        Ok(manifest)
    }

    /// Validate checksums for all files
    fn validate_checksums(
        manifest: &ExtensionPackageManifest,
        files: &HashMap<String, Vec<u8>>,
    ) -> anyhow::Result<()> {
        for (path, expected_checksum) in &manifest.packaging.checksums {
            // Skip the manifest itself
            if path == "manifest.toml" {
                continue;
            }

            let content = files.get(path).ok_or_else(|| {
                anyhow::anyhow!("Package is missing file listed in packaging metadata: {path}")
            })?;

            let computed = Self::compute_checksum(content);
            if computed != *expected_checksum {
                anyhow::bail!(
                    "Checksum mismatch for '{path}': expected {expected_checksum}, got {computed}"
                );
            }
        }

        Ok(())
    }

    /// Compute SHA-256 checksum
    fn compute_checksum(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("sha256:{:x}", hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::manager::ExtensionManager;
    use crate::extension::types::{ExtensionId, ExtensionManifest};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_extension(temp: &TempDir, id: &str) -> PathBuf {
        let ext_dir = temp.path().join(id);
        std::fs::create_dir_all(&ext_dir).unwrap();
        std::fs::write(
            ext_dir.join("manifest.yaml"),
            format!(
                "id: {id}\nname: Test Extension\nextension_type: skill\nversion: 1.0.0\ndescription: A test extension\n"
            ),
        )
        .unwrap();
        std::fs::write(ext_dir.join("SKILL.md"), "# Test Skill\n").unwrap();
        ext_dir
    }

    fn create_manager_with_extension(temp: &TempDir, id: &str) -> (ExtensionManager, PathBuf) {
        let ext_dir = create_test_extension(temp, id);
        let storage_dir = temp.path().join("storage");
        std::fs::create_dir_all(&storage_dir).unwrap();

        let mut manager = ExtensionManager::new().with_storage_dir(storage_dir);
        // Manually insert the loaded extension since we don't have adapters in unit tests
        let manifest = ExtensionManifest::new(
            id,
            "skill",
            "Test Extension",
            "A test extension",
            "1.0.0",
            ext_dir.clone(),
        );
        let loaded = crate::extension::manager::LoadedExtension {
            manifest,
            extension_type: "skill".to_string(),
            hook_ids: Vec::new(),
            path: ext_dir.clone(),
        };
        manager.extensions.insert(ExtensionId::new(id), loaded);

        (manager, ext_dir)
    }

    #[test]
    fn test_extension_packager_export_creates_ext_file() {
        let temp = TempDir::new().unwrap();
        let (manager, _ext_dir) = create_manager_with_extension(&temp, "test-skill");

        let output_path = temp.path().join("test-skill.ext");
        let result =
            ExtensionPackager::export(&manager, &ExtensionId::new("test-skill"), &output_path);
        assert!(result.is_ok(), "Export failed: {:?}", result.err());
        assert!(output_path.exists());
    }

    #[test]
    fn test_extension_packager_export_fails_for_missing_extension() {
        let temp = TempDir::new().unwrap();
        let (manager, _ext_dir) = create_manager_with_extension(&temp, "test-skill");

        let output_path = temp.path().join("missing.ext");
        let result =
            ExtensionPackager::export(&manager, &ExtensionId::new("nonexistent"), &output_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_extension_unpackager_install_roundtrip() {
        let temp = TempDir::new().unwrap();
        let (manager, _ext_dir) = create_manager_with_extension(&temp, "test-skill");

        // Export
        let output_path = temp.path().join("test-skill.ext");
        ExtensionPackager::export(&manager, &ExtensionId::new("test-skill"), &output_path).unwrap();

        // Install to new location
        let install_dir = temp.path().join("installed");
        let result = ExtensionUnpackager::install(&output_path, &install_dir);
        assert!(result.is_ok(), "Install failed: {:?}", result.err());

        let installed_path = result.unwrap();
        assert!(installed_path.exists());
        assert!(installed_path.join("manifest.yaml").exists());
        assert!(installed_path.join("SKILL.md").exists());
    }

    #[test]
    fn test_extension_unpackager_inspect() {
        let temp = TempDir::new().unwrap();
        let (manager, _ext_dir) = create_manager_with_extension(&temp, "docker-skill");

        let output_path = temp.path().join("docker-skill.ext");
        ExtensionPackager::export(&manager, &ExtensionId::new("docker-skill"), &output_path)
            .unwrap();

        let manifest = ExtensionUnpackager::inspect(&output_path).unwrap();
        assert_eq!(manifest.extension.id, "docker-skill");
        assert_eq!(manifest.extension.name, "Test Extension");
        assert_eq!(manifest.extension.extension_type, "skill");
        assert_eq!(manifest.packaging.compression, "gzip");
        assert!(!manifest.packaging.checksums.is_empty());
    }

    #[test]
    fn test_extension_unpackager_checksum_validation() {
        let temp = TempDir::new().unwrap();
        let (manager, _ext_dir) = create_manager_with_extension(&temp, "test-skill");

        let output_path = temp.path().join("test-skill.ext");
        ExtensionPackager::export(&manager, &ExtensionId::new("test-skill"), &output_path).unwrap();

        // Tamper with the file by recreating it with bad content
        {
            let tar_gz = std::fs::File::create(&output_path).unwrap();
            let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);

            // Add a manifest with correct structure but wrong checksums won't match
            let mut header = tar::Header::new_gnu();
            header.set_path("manifest.toml").unwrap();
            let bad_manifest = r#"
[format]
version = "1.0"
peko_version = "0.1.0"

[extension]
id = "test-skill"
name = "Test"
extension_type = "skill"
version = "1.0.0"
description = "Test"

[packaging]
files = ["extension/manifest.yaml"]
checksums = { "extension/manifest.yaml" = "sha256:0000000000000000000000000000000000000000000000000000000000000000" }
compression = "gzip"
archive_format = "tar"
"#;
            header.set_size(bad_manifest.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, bad_manifest.as_bytes()).unwrap();

            // Add the actual file with different content
            let mut header = tar::Header::new_gnu();
            header.set_path("extension/manifest.yaml").unwrap();
            let content = b"tampered";
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, content.as_slice()).unwrap();

            // into_inner() finishes the tar and returns the GzEncoder, which is then dropped and finished
            tar.into_inner().unwrap();
        }

        // Install should fail due to checksum mismatch
        let install_dir = temp.path().join("installed");
        let result = ExtensionUnpackager::install(&output_path, &install_dir);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Checksum mismatch"),
            "Expected checksum mismatch error, got: {err}"
        );
    }

    #[test]
    fn test_extension_manifest_serde() {
        let manifest = ExtensionPackageManifest {
            format: PackageFormat {
                version: "1.0".to_string(),
                peko_version: "0.1.0".to_string(),
            },
            extension: ExtensionInfo {
                id: "test".to_string(),
                name: "Test".to_string(),
                extension_type: "skill".to_string(),
                version: "1.0.0".to_string(),
                description: "A test".to_string(),
            },
            packaging: ExtensionPackagingMetadata {
                files: vec!["extension/manifest.yaml".to_string()],
                checksums: {
                    let mut m = HashMap::new();
                    m.insert(
                        "extension/manifest.yaml".to_string(),
                        "sha256:abc123".to_string(),
                    );
                    m
                },
                compression: "gzip".to_string(),
                archive_format: "tar".to_string(),
            },
            dependencies: Vec::new(),
        };

        let toml = manifest.to_toml().unwrap();
        let parsed = ExtensionPackageManifest::from_toml(&toml).unwrap();
        assert_eq!(parsed.extension.id, "test");
        assert_eq!(parsed.packaging.files.len(), 1);
    }
}
