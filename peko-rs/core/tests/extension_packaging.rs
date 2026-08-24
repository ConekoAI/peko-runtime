//! Universal-tool extension packaging integration test.
//!
//! Phase 2 PR 1 (ADR-047 §2.4) deletes the `SkillAdapter`. The
//! `ExtensionPackager` / `ExtensionUnpackager` skill-based tests
//! (install → export → install-from-`.ext`) used SKILL.md fixtures
//! loaded through the framework, which now has no skill adapter.
//! Phase 7 deletes the entire `.ext` packaging format in favor of a
//! literal tar of the principal workspace, so those tests are not
//! restored.

use peko_core::extensions::framework::manager::packaging::ExtensionUnpackager;
use peko_core::extensions::framework::store::ExtensionStore;
use peko_subject::PrincipalId;
use std::path::PathBuf;
use tempfile::TempDir;

use peko_core::extensions::framework::core::HookPoint;
use peko_core::extensions::framework::types::{HookInput, HookOutput, HookResult};
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
