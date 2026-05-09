//! Extension packaging integration tests
//!
//! End-to-end: install → export → install from `.ext`

use pekobot::extension::manager::packaging::{ExtensionPackager, ExtensionUnpackager};
use pekobot::extension::manager::ExtensionManager;
use pekobot::extension::types::ExtensionId;
use std::path::PathBuf;
use tempfile::TempDir;

fn create_test_extension(temp: &TempDir, id: &str) -> PathBuf {
    let ext_dir = temp.path().join(id);
    std::fs::create_dir_all(&ext_dir).unwrap();
    // For skill extensions, manifest.yaml is optional; SKILL.md with frontmatter is the primary manifest.
    // We still create manifest.yaml for completeness but the skill adapter uses SKILL.md.
    std::fs::write(
        ext_dir.join("manifest.yaml"),
        format!(
            "id: {id}\nname: Docker Skill\nextension_type: skill\nversion: 1.0.0\ndescription: Manage Docker containers\n"
        ),
    )
    .unwrap();
    std::fs::write(
        ext_dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: Manage Docker containers\n---\n\n# Docker Skill\n\nSome skill content.\n"),
    )
    .unwrap();
    // Add a subdirectory with extra files
    let sub_dir = ext_dir.join("templates");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("default.md"), "# Template\n").unwrap();
    ext_dir
}

fn create_manager_with_adapters() -> ExtensionManager {
    use pekobot::extensions::skill::SkillAdapter;

    let mut manager = ExtensionManager::new();
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager
}

#[tokio::test]
async fn test_extension_export_creates_valid_ext_package() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let mut manager = create_manager_with_adapters();
    manager.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    let result =
        ExtensionPackager::export(&manager, &ExtensionId::new("docker-skill"), &output_path);

    assert!(result.is_ok(), "Export failed: {:?}", result.err());
    assert!(output_path.exists(), "Output file should exist");

    // Verify it's a valid gzip file by checking magic bytes
    let header = std::fs::read(&output_path).unwrap();
    assert_eq!(&header[..2], &[0x1f, 0x8b], "Should be a gzip file");
}

#[tokio::test]
async fn test_extension_export_manifest_contents() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let mut manager = create_manager_with_adapters();
    manager.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&manager, &ExtensionId::new("docker-skill"), &output_path).unwrap();

    // Inspect the package
    let manifest = ExtensionUnpackager::inspect(&output_path).unwrap();
    assert_eq!(manifest.extension.id, "docker-skill");
    assert_eq!(manifest.extension.name, "docker-skill");
    assert_eq!(manifest.extension.extension_type, "skill");
    assert_eq!(manifest.extension.version, "1.0.0");
    assert_eq!(manifest.packaging.compression, "gzip");
    assert_eq!(manifest.packaging.archive_format, "tar");
    assert!(!manifest.packaging.checksums.is_empty());
    assert!(manifest
        .packaging
        .files
        .contains(&"extension/manifest.yaml".to_string()));
    assert!(manifest
        .packaging
        .files
        .contains(&"extension/SKILL.md".to_string()));
    assert!(manifest
        .packaging
        .files
        .contains(&"extension/templates/default.md".to_string()));
}

#[tokio::test]
async fn test_extension_install_from_ext_roundtrip() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let mut manager = create_manager_with_adapters();
    manager.install(&ext_dir).await.unwrap();

    // Export
    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&manager, &ExtensionId::new("docker-skill"), &output_path).unwrap();

    // Install to new location
    let install_dir = temp.path().join("installed");
    let installed_path = ExtensionUnpackager::install(&output_path, &install_dir).unwrap();

    // Verify installed files
    assert!(installed_path.exists());
    assert!(installed_path.join("manifest.yaml").exists());
    assert!(installed_path.join("SKILL.md").exists());
    assert!(installed_path.join("templates/default.md").exists());

    // Verify content matches original
    let original_manifest = std::fs::read_to_string(ext_dir.join("manifest.yaml")).unwrap();
    let installed_manifest = std::fs::read_to_string(installed_path.join("manifest.yaml")).unwrap();
    assert_eq!(original_manifest, installed_manifest);

    let original_skill = std::fs::read_to_string(ext_dir.join("SKILL.md")).unwrap();
    let installed_skill = std::fs::read_to_string(installed_path.join("SKILL.md")).unwrap();
    assert_eq!(original_skill, installed_skill);
}

#[tokio::test]
async fn test_extension_export_fails_for_missing_extension() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let mut manager = create_manager_with_adapters();
    manager.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("missing.ext");
    let result =
        ExtensionPackager::export(&manager, &ExtensionId::new("nonexistent"), &output_path);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "Expected 'not found' error, got: {err}"
    );
}

#[tokio::test]
async fn test_extension_install_checksum_mismatch_fails() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let mut manager = create_manager_with_adapters();
    manager.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&manager, &ExtensionId::new("docker-skill"), &output_path).unwrap();

    // Tamper with the file: overwrite with a package that has wrong checksum
    {
        let tar_gz = std::fs::File::create(&output_path).unwrap();
        let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_path("manifest.toml").unwrap();
        let bad_manifest = r#"
[format]
version = "1.0"
pekobot_version = "0.1.0"

[extension]
id = "docker-skill"
name = "Docker Skill"
extension_type = "skill"
version = "1.0.0"
description = "Manage Docker containers"

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

        let mut header = tar::Header::new_gnu();
        header.set_path("extension/manifest.yaml").unwrap();
        let content = b"tampered";
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, content.as_slice()).unwrap();

        tar.into_inner().unwrap();
    }

    let install_dir = temp.path().join("installed");
    let result = ExtensionUnpackager::install(&output_path, &install_dir);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Checksum mismatch"),
        "Expected checksum mismatch, got: {err}"
    );
}
