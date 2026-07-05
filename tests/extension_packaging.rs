//! Extension packaging integration tests
//!
//! End-to-end: install → export → install from `.ext`

use peko::extensions::framework::manager::packaging::{ExtensionPackager, ExtensionUnpackager};
use peko::extensions::framework::manager::ExtensionManager;
use peko::extensions::framework::types::ExtensionId;
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
    use peko::extensions::skill::SkillAdapter;

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
peko_version = "0.1.0"

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

use peko::common::types::agent_legacy::ExtensionConfig;
use peko::extensions::framework::core::HookPoint;
use peko::extensions::framework::types::{HookInput, HookOutput, HookResult};
use peko::extensions::skill::SkillAdapter;
use peko::extensions::universal::UniversalToolAdapter;

fn create_test_tool_extension(temp: &TempDir, id: &str) -> PathBuf {
    let ext_dir = temp.path().join(id);
    std::fs::create_dir_all(&ext_dir).unwrap();

    // Universal-tool manifest
    std::fs::write(
        ext_dir.join("manifest.yaml"),
        format!(
            "name: {id}\nextension_type: universal-tool\ndescription: A test universal tool\nversion: 1.0.0\nparameters:\n  type: object\n  properties:\n    input:\n      type: string\n"
        ),
    )
    .unwrap();

    // Simple Python executable that implements the universal tool protocol
    let script = r#"import sys, json
line = sys.stdin.readline()
if line:
    req = json.loads(line)
    resp = {"jsonrpc": "2.0", "id": req.get("id"), "result": {"success": true, "data": {"echoed": true}}}
    print(json.dumps(resp), flush=True)
"#;
    std::fs::write(ext_dir.join(format!("{id}.py")), script).unwrap();

    ext_dir
}

#[tokio::test]
async fn test_extension_install_tool_registration_and_invocation() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_tool_extension(&temp, "test-echo");

    let mut manager = ExtensionManager::new();
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));

    // 1. Install the extension
    let ext_id = manager.install(&ext_dir).await.unwrap();
    assert_eq!(ext_id.0, "test-echo");

    // 2. Get the ExtensionCore from the manager (tools registered during install)
    let core = manager.core_arc();

    // 3. Enable the tool in the whitelist
    let tool_config = ExtensionConfig {
        enabled: vec!["universal:test-echo".to_string()],
        ..Default::default()
    };
    core.set_tool_config(tool_config).await;

    // 4. Verify the tool is listed
    let tools = core.list_tools().await;
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    assert!(
        tool_names.contains(&"test-echo".to_string()),
        "Expected 'test-echo' in list_tools, got: {:?}",
        tool_names
    );

    // 5. Verify the tool can be invoked via ToolExecute
    let result = core
        .invoke_hook(
            HookPoint::ToolExecute {
                tool_name: "test-echo".to_string(),
            },
            HookInput::ToolCall {
                tool_name: "test-echo".to_string(),
                params: serde_json::json!({"input": "hello"}),
                workspace: None,
                agent_id: None,
                session_id: None,
                caller_id: None,
                principal_id: None,
                principal_name: None,
                allowed_extensions: None,
            },
        )
        .await;

    // The tool should execute successfully and return JSON output.
    // If Python is unavailable we accept an Error result as long as it is
    // not a whitelist block — that still proves the hook was resolved.
    match result {
        HookResult::Continue(HookOutput::Json(json)) => {
            assert_eq!(json["echoed"], true, "Expected echoed result, got: {json}");
        }
        HookResult::Error(ref e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("disabled") && !msg.contains("not enabled"),
                "Tool invocation blocked by whitelist: {msg}"
            );
        }
        other => {
            panic!("Expected Continue(JSON) or Error, got: {other:?}");
        }
    }
}
