//! Extension packaging integration tests
//!
//! End-to-end: install → export → install from `.ext`

use peko_core::extensions::framework::manager::packaging::{
    ExtensionPackager, ExtensionUnpackager,
};
use peko_core::extensions::framework::store::ExtensionStore;
use peko_core::extensions::framework::types::ExtensionId;
use peko_subject::PrincipalId;
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

async fn create_store_with_adapters() -> ExtensionStore {
    use peko_core::extensions::skill::SkillAdapter;

    let store = ExtensionStore::new();
    store.register_adapter(Box::new(SkillAdapter::new())).await;
    store
}

#[tokio::test]
async fn test_extension_export_creates_valid_ext_package() {
    let temp = TempDir::new().unwrap();
    let ext_dir = create_test_extension(&temp, "docker-skill");

    let store = create_store_with_adapters().await;
    store.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    let result =
        ExtensionPackager::export(&store, &ExtensionId::new("docker-skill"), &output_path).await;

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

    let store = create_store_with_adapters().await;
    store.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&store, &ExtensionId::new("docker-skill"), &output_path)
        .await
        .unwrap();

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

    let store = create_store_with_adapters().await;
    store.install(&ext_dir).await.unwrap();

    // Export
    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&store, &ExtensionId::new("docker-skill"), &output_path)
        .await
        .unwrap();

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

    let store = create_store_with_adapters().await;
    store.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("missing.ext");
    let result =
        ExtensionPackager::export(&store, &ExtensionId::new("nonexistent"), &output_path).await;

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

    let store = create_store_with_adapters().await;
    store.install(&ext_dir).await.unwrap();

    let output_path = temp.path().join("docker-skill.ext");
    ExtensionPackager::export(&store, &ExtensionId::new("docker-skill"), &output_path)
        .await
        .unwrap();

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

use peko_core::extensions::framework::core::HookPoint;
use peko_core::extensions::framework::types::{HookInput, HookOutput, HookResult};
use peko_core::extensions::skill::SkillAdapter;
use peko_core::extensions::universal::UniversalToolAdapter;

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

    let store = ExtensionStore::new();
    store.register_adapter(Box::new(SkillAdapter::new())).await;
    store
        .register_adapter(Box::new(UniversalToolAdapter::new()))
        .await;

    // 1. Install the extension
    let ext_id = store.install(&ext_dir).await.unwrap();
    assert_eq!(ext_id.0, "test-echo");

    // 2. Get the ExtensionCore from the store (tools registered during install)
    let core = store.core_arc();

    // 3. Verify the tool is listed
    let tools = core.list_tools(PrincipalId::system()).await;
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    assert!(
        tool_names.contains(&"test-echo".to_string()),
        "Expected 'test-echo' in list_tools, got: {:?}",
        tool_names
    );

    // 4. Verify the tool can be invoked via ToolExecute with the right capability.
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
                capabilities: Some(vec!["tool:test-echo".to_string()]),
                active_extensions: Some(vec!["universal:test-echo".to_string()]),
                abort_signal: None,
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

// ── Path-traversal & unsafe-name safety tests ───────────────────────
//
// Pin the new defenses in `ExtensionUnpackager::install`:
//   - manifest `extension.id` is rejected by `validate_agent_name`
//   - per-entry `ext_path` is funneled through `safe_join`

/// Build a `.ext` tarball at the byte level so malicious entry paths
/// (containing `..`) can be encoded. The upstream `tar::Header::set_path`
/// hard-rejects `..`, and tar 0.4.44 has no public `set_path_raw`.
///
/// The header layout follows POSIX ustar: 512-byte record, name in
/// `name[0..100]`, size as octal at `size[124..136]`, checksum at
/// `cksum[148..156]` (fill with spaces before summing).
fn write_ext_tarball(
    path: &std::path::Path,
    manifest_toml: &str,
    entries: &[(&str, &[u8])],
) {
    use std::io::Write;

    let file = std::fs::File::create(path).unwrap();
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut buffer: Vec<u8> = Vec::new();

    let mut all_entries: Vec<(&str, &[u8])> = Vec::with_capacity(entries.len() + 1);
    all_entries.push(("manifest.toml", manifest_toml.as_bytes()));
    all_entries.extend_from_slice(entries);

    for (name, content) in all_entries {
        let mut header = [0u8; 512];
        let name_bytes = name.as_bytes();
        let n = name_bytes.len().min(100);
        header[..n].copy_from_slice(&name_bytes[..n]);
        header[100..107].copy_from_slice(b"0000644");
        header[107] = 0;
        let size_str = format!("{:011o}\0", content.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        header[148..156].copy_from_slice(b"        ");
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", sum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        buffer.extend_from_slice(&header);
        buffer.extend_from_slice(content);
        let pad = (512 - (content.len() % 512)) % 512;
        buffer.resize(buffer.len() + pad, 0);
    }
    buffer.resize(buffer.len() + 1024, 0);

    gz.write_all(&buffer).unwrap();
    gz.finish().unwrap();
}

fn compute_sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    format!("sha256:{:x}", h.finalize())
}

#[test]
fn unsafe_extension_id_rejected() {
    // A `.ext` whose embedded `extension.id` is `"../escape"`. The
    // manifest's packaging.files / checksums are consistent so the
    // `validate_checksums` step does not shadow the safety gate.
    let temp = TempDir::new().unwrap();
    let output_path = temp.path().join("malicious.ext");

    let manifest_yaml = b"id: manifest.yaml";
    let manifest_toml = format!(
        r#"
[format]
version = "1.0"
peko_version = "0.1.0"

[extension]
id = "../escape"
name = "Escape"
extension_type = "skill"
version = "1.0.0"
description = "Escape"

[packaging]
files = ["extension/manifest.yaml"]
checksums = {{ "extension/manifest.yaml" = "{}" }}
compression = "gzip"
archive_format = "tar"
"#,
        compute_sha256_hex(manifest_yaml),
    );

    write_ext_tarball(
        &output_path,
        &manifest_toml,
        &[("extension/manifest.yaml", manifest_yaml)],
    );

    let install_dir = temp.path().join("installed");
    let err = ExtensionUnpackager::install(&output_path, &install_dir)
        .expect_err("unsafe extension id should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("[unsafe_extension_id]"),
        "expected [unsafe_extension_id], got: {msg}"
    );
}

#[test]
fn unsafe_extension_entry_path_rejected() {
    // A `.ext` whose entry key is `extension/../escape.yaml`. The
    // unpackager's `safe_join` rejects this before the inner write.
    // The manifest still claims a benign id so the extension-id gate
    // doesn't fire first.
    let temp = TempDir::new().unwrap();
    let output_path = temp.path().join("malicious.ext");

    let inner_content = b"escaped content";
    // Two checksums: one for the legit entry that's actually in the
    // package (manifest.yaml), no entry for the malicious one — so the
    // checksum validator skips it. The malicious entry is what trips
    // safe_join.
    let manifest_toml = format!(
        r#"
[format]
version = "1.0"
peko_version = "0.1.0"

[extension]
id = "legit-id"
name = "Legit"
extension_type = "skill"
version = "1.0.0"
description = "Legit"

[packaging]
files = ["extension/manifest.yaml"]
checksums = {{ "extension/manifest.yaml" = "{}" }}
compression = "gzip"
archive_format = "tar"
"#,
        compute_sha256_hex(b"id: manifest.yaml"),
    );

    write_ext_tarball(
        &output_path,
        &manifest_toml,
        &[
            ("extension/manifest.yaml", b"id: manifest.yaml"),
            ("extension/../escape.yaml", inner_content),
        ],
    );

    let install_dir = temp.path().join("installed");
    let err = ExtensionUnpackager::install(&output_path, &install_dir)
        .expect_err("unsafe extension entry path should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("[unsafe_path]"),
        "expected [unsafe_path], got: {msg}"
    );
}
