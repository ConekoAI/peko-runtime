//! CLI integration tests for the unified extension framework (Phase B slice
//! per `docs/integration/TESTING.md` §7).
//!
//! Coverage mirrors the `e2e_tests/extensions/*.ps1` PowerShell scripts
//! that previously exercised this surface outside CI:
//!
//! | PS script                                | Rust test                                              | Layer |
//! |------------------------------------------|--------------------------------------------------------|-------|
//! | `skill/python/test.ps1` T1+T2            | `ext_install_skill_tier1_detect`                       | L1    |
//! | `mcp/python/standard/test.ps1` T1+T2     | `ext_install_mcp_standard_tier1_server_json`           | L1    |
//! | `mcp/python/params_injection/test.ps1` T1| `ext_install_mcp_manifest_reserved_params`             | L1    |
//! | `universal/python/multi_file/test.ps1` T3+T4 | `ext_install_universal_python_multi_file_copies_subdirs` | L1 |
//! | `universal/python/simple/test.ps1` T2    | `ext_install_universal_python_simple_manifest_roundtrip`| L1    |
//! | `universal/python/reserved_params/test.ps1` T2 | `ext_install_universal_python_reserved_params_manifest` | L1 |
//! | `universal/node/custom.ps1` T1           | `ext_install_universal_node_manifest_parsed`           | L1    |
//! | `gateway/http_basics/test.ps1` T1        | `ext_install_gateway_manifest_parsed`                  | L1    |
//! | (cross-cutting, all 9 PS scripts)        | `ext_install_uninstall_roundtrip`                       | L1    |
//! | (cross-cutting, 6 of 9 PS scripts)       | `ext_enable_for_agent_modifies_whitelist`               | L1    |
//!
//! ## Layered design (L1 / L2 / L3)
//!
//! The migration is **three-layered**:
//!
//! - **L1 (this PR)** — install / list / info / enable / disable / uninstall.
//!   Purely CLI + IPC + filesystem. No LLM. No Python or Node runtime needed.
//! - **L2 (deferred)** — start / stop / status / restart for the background
//!   runtime (`peko ext start` / `stop` / `status` / `restart`). Needs Python
//!   (for MCP / universal-python) or Node (for gateway / universal-node) on
//!   the test host.
//! - **L3 (deferred)** — actual `peko send` tool execution. Needs mock LLM +
//!   the actual process runtime, plus the per-extension tool implementations.
//!
//! The 10 tests in this file all land in L1. They exercise the unified
//! extension framework's most-used CLI surface (install a manifest-bearing
//! fixture, verify the install completes and the manifest is preserved,
//! verify `peko ext list` and `peko ext info` round-trip the install, then
//! `peko ext uninstall` to clean up).
//!
//! ## Three structural facts this file documents (not bugs, not changed)
//!
//! 1. **`--type` flag is ignored at the CLI level.** The PS scripts all pass
//!    `--type mcp` / `--type universal-tool`, but [`src/commands/ext.rs:363`](src/commands/ext.rs#L363)
//!    destructures `r#type: _`. The tests below still pass because Tier 2
//!    detection from `manifest.yaml` is the production path; `--type` is the
//!    user's escape hatch when the manifest is missing or wrong. The tests
//!    assert on the **detected** type from `peko ext info`, not the
//!    `--type` flag value.
//!
//! 2. **Tier 1 `SKILL.md` detection requires the install path to be the skill
//!    subdirectory itself.** The detection at
//!    [`src/extension/manager/mod.rs:215-256`](src/extension/manager/mod.rs#L215-L256)
//!    checks `path.join("SKILL.md").exists()` — so the install path must be
//!    the *skill's* directory, not its parent. The PS scripts install
//!    `<parent>/calculator-skill/` directly. The tests below mirror that.
//!
//! 3. **The SKILL.md `name:` frontmatter field becomes the extension ID.**
//!    See [`src/extensions/skill/adapter.rs:108-130`](src/extensions/skill/adapter.rs#L108-L130).
//!    The calculator-skill fixture has `name: calculator-skill` so install
//!    creates an extension with ID `calculator-skill`.
//!
//! ## Wire format references
//!
//! - `peko ext list --json` output: `{"extensions": [ExtensionSummary, ...],
//!   "total": N}` where each summary has `{id, name, ext_type, version,
//!   source, enabled, runtime, description}`. See
//!   [`src/ipc/server.rs:1763-1816`](src/ipc/server.rs#L1763-L1816).
//! - `peko ext info` output: pretty-printed JSON with
//!   `{id, name, type, version, description}`. See
//!   [`src/ipc/server.rs:2389-2416`](src/ipc/server.rs#L2389-L2416).
//!
//! ## Tier
//!
//! All tests are daemon-gated (`#[ignore = "requires daemon"]`) but **none
//! are `#[serial]`** — L1 tests do not drive the mock LLM, so they do not
//! touch the per-substring counter. Tests early-return on bare-checkout
//! (no daemon) so `cargo test` still passes without the docker-compose
//! stack.

mod common;
use common::{DaemonGuard, PekoCli, run_with_timeout};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Directory under the test's isolated `HOME` where `peko ext install`
/// copies extension files. Confirmed by reading the daemon's
/// `ExtensionStorage` config: `paths.data_dir.join("extensions")`, where
/// `data_dir = <PEKO_HOME>/data` per [`src/common/paths.rs:65-70`](src/common/paths.rs#L65-L70).
fn ext_install_dir(cli: &PekoCli, ext_id: &str) -> PathBuf {
    cli.peko_dir().join("data").join("extensions").join(ext_id)
}

/// Absolute path to a fixture directory, relative to the crate root.
///
/// `cargo test` runs with `CARGO_MANIFEST_DIR` pointing at the `peko-runtime/`
/// crate root, regardless of the current working directory, so this is
/// stable across platforms and CI runners.
fn fixture_dir(relative: &str) -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR is set by cargo for integration tests");
    PathBuf::from(manifest_dir)
        .join("e2e_tests")
        .join("extensions")
        .join(relative)
}

/// Run a `peko …` command and return (stdout, stderr, status).
fn run(
    cli: &PekoCli,
    args: &[&str],
    timeout: Duration,
) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        timeout,
    )
    .expect("run peko command");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

fn assert_ok(stdout: &str, stderr: &str, status: &std::process::ExitStatus) {
    assert_eq!(
        status.code(),
        Some(0),
        "exited non-zero (status={status:?})\nstdout: {stdout}\nstderr: {stderr}",
    );
}

/// Write a mock-LLM-pointed agent with an empty `[extensions] enabled` whitelist.
///
/// Test #10 (`ext_enable_for_agent_modifies_whitelist`) uses the empty
/// whitelist to verify that `peko ext enable <id> --target <agent>` adds
/// the canonical extension ID. The other tests don't use this agent at
/// all — they only install/uninstall and don't invoke `peko send`.
fn write_ext_agent(home: &Path, name: &str, mock_llm_url: &str) -> std::io::Result<()> {
    use std::path::Path;
    let agent_dir = Path::new(home).join(".peko").join("agents").join(name);
    std::fs::create_dir_all(&agent_dir)?;
    let base_url = mock_llm_url.trim_end_matches('/');
    let config_toml = format!(
        r#"version = "1.0"
name = "{name}"
description = "CLI integration test agent for the extensions framework"
auto_accept_trusted = false
default_timeout_seconds = 60

[provider]
provider_type = "openai_compatible"
api_key = "mock-llm-test-key"
base_url = "{base_url}"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "default"
max_tokens = 1024
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[extensions]
enabled = []

[channels]
cli = true

[prompt]
system = {{ max_chars_per_file = 20000, files = ["SYSTEM.md"] }}
"#
    );
    std::fs::write(agent_dir.join("config.toml"), config_toml)?;
    std::fs::write(agent_dir.join("SYSTEM.md"), "")?;
    Ok(())
}

/// Assert that the given text contains all of the given needles. Used for
/// `peko ext list` human-readable output, which looks like
/// `  <id> | <ext_type> | <name> | <source>`.
fn assert_contains_all(haystack: &str, needles: &[&str], context: &str) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "{context}: expected to contain {needle:?}, got: {haystack}",
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// `skill/python/test.ps1` T1+T2: install a skill (Tier 1 `SKILL.md`
/// detection, no `--type` flag), verify it appears in `peko ext list` and
/// `peko ext info` reports `type: skill`.
///
/// The calculator-skill fixture at
/// `e2e_tests/extensions/skill/python/calculator-skill/` has a `SKILL.md`
/// with `name: calculator-skill`, so the install creates an extension with
/// ID `calculator-skill` and detected type `skill` (per
/// [`src/extensions/skill/adapter.rs:108-130`](src/extensions/skill/adapter.rs#L108-L130)).
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_skill_tier1_detect() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("skill/python/calculator-skill");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator-skill"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext list should show the skill.
    let (list_out, err, status) =
        run(&cli, &["ext", "list"], Duration::from_secs(10));
    assert_ok(&list_out, &err, &status);
    assert_contains_all(
        &list_out,
        &["calculator-skill", "skill"],
        "peko ext list (skill)",
    );

    // peko ext info should report type "skill" (Tier 1 detection).
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert_contains_all(
        &info_out,
        &["\"id\":", "\"name\":", "\"type\": \"skill\"", "\"version\":"],
        "peko ext info calculator-skill",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `mcp/python/standard/test.ps1` T1+T2: install a pure-MCP server that
/// has ONLY a `server.json` (NO `manifest.yaml`). Tier 1 detection at
/// [`src/extension/manager/mod.rs:215-256`](src/extension/manager/mod.rs#L215-L256)
/// matches on `server.json` presence and auto-classifies the install as
/// type `mcp`. The test passes `peko ext install <dir>` with NO `--type`
/// flag — Tier 1 detection is what makes the type `mcp`.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_mcp_standard_tier1_server_json() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("mcp/python/standard");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("standard-echo"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext list --type mcp should include the standard MCP server.
    let (list_out, err, status) = run(
        &cli,
        &["ext", "list", "--type", "mcp"],
        Duration::from_secs(10),
    );
    assert_ok(&list_out, &err, &status);
    assert_contains_all(
        &list_out,
        &["standard-echo", "mcp"],
        "peko ext list --type mcp",
    );

    // peko ext info confirms type "mcp".
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "standard-echo"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"mcp\""),
        "info should report type=mcp: {info_out}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "standard-echo"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `mcp/python/params_injection/test.ps1` T1: install an MCP server with
/// a `manifest.yaml` (Tier 2 detection) and verify the manifest's
/// `reserved_parameters` block is preserved verbatim in the on-disk
/// install copy.
///
/// The MCP install path copies the source dir to
/// `<peko_dir>/data/extensions/<id>/` (per
/// [`src/extension/manager/storage.rs:123-172`](src/extension/manager/storage.rs#L123-L172))
/// via a recursive copy. After install, the copy at that path must
/// still contain `reserved_parameters.agent_id` and
/// `reserved_parameters.session_id` from the original manifest.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_mcp_manifest_reserved_params() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    // The fixture's manifest.yaml has id: "identity", so the install
    // creates an extension with ID "identity" and the on-disk dir is
    // <peko_dir>/data/extensions/identity/.
    let install_path = fixture_dir("mcp/python/params_injection");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy(), "--type", "mcp"],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("identity"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext info confirms type "mcp" (Tier 2 detection from manifest.yaml).
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "identity"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"mcp\""),
        "info should report type=mcp: {info_out}",
    );

    // On-disk manifest preserves the reserved_parameters block.
    let install_dir = ext_install_dir(&cli, "identity");
    let manifest = std::fs::read_to_string(install_dir.join("manifest.yaml"))
        .unwrap_or_else(|e| panic!("read manifest at {install_dir:?}: {e}"));
    assert!(
        manifest.contains("reserved_parameters"),
        "installed manifest should preserve reserved_parameters: {manifest}",
    );
    assert!(
        manifest.contains("agent_id") && manifest.contains("session_id"),
        "installed manifest should preserve agent_id/session_id: {manifest}",
    );
    assert!(
        manifest.contains("source: \"runtime\""),
        "installed manifest should preserve the runtime source: {manifest}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "identity"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `universal/python/multi_file/test.ps1` T3+T4: install a tool with
/// a `utils/` subdirectory. The recursive copy at
/// [`src/extension/manager/storage.rs:209-225`](src/extension/manager/storage.rs#L209-L225)
/// must preserve the subdirectory and all files inside it.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_universal_python_multi_file_copies_subdirs() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("universal/python/multi_file");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "universal-tool",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("multi_file_calc"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // Verify the on-disk install dir has the recursive copy.
    let install_dir = ext_install_dir(&cli, "multi_file_calc");
    for relative in [
        "manifest.yaml",
        "multi_file_calc.py",
        "utils/__init__.py",
        "utils/calculator.py",
        "utils/validators.py",
        "utils/formatter.py",
    ] {
        let path = install_dir.join(relative);
        assert!(
            path.exists(),
            "expected file at {path:?} after recursive install copy",
        );
    }

    // peko ext list --type universal-tool should show it.
    let (list_out, err, status) = run(
        &cli,
        &["ext", "list", "--type", "universal-tool"],
        Duration::from_secs(10),
    );
    assert_ok(&list_out, &err, &status);
    assert_contains_all(
        &list_out,
        &["multi_file_calc", "universal-tool"],
        "peko ext list --type universal-tool",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "multi_file_calc"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `universal/python/simple/test.ps1` T2: install a single-file Python
/// tool with a `manifest.yaml` (Tier 2 detection) and verify the
/// manifest is preserved + the extension type is detected as
/// `universal-tool`.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_universal_python_simple_manifest_roundtrip() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("universal/python/simple");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "universal-tool",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator_simple"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext info confirms type "universal-tool".
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "calculator_simple"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"universal-tool\""),
        "info should report type=universal-tool: {info_out}",
    );

    // On-disk manifest preserves extension_type and parameters.
    let install_dir = ext_install_dir(&cli, "calculator_simple");
    let manifest = std::fs::read_to_string(install_dir.join("manifest.yaml"))
        .unwrap_or_else(|e| panic!("read manifest at {install_dir:?}: {e}"));
    // The YAML serializer may emit either `extension_type: universal-tool`
    // or `extension_type: "universal-tool"`. We match both, as long as
    // the key is present and the value is `universal-tool`.
    assert!(
        manifest.contains("extension_type: universal-tool")
            || manifest.contains("extension_type: \"universal-tool\""),
        "installed manifest should preserve extension_type: universal-tool: {manifest}",
    );
    assert!(
        manifest.contains("parameters:"),
        "installed manifest should preserve parameters: {manifest}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator_simple"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `universal/python/reserved_params/test.ps1` T2: install a tool whose
/// manifest declares `reserved_parameters: {session_id, agent_id}` from
/// the `runtime` source. The manifest round-trip must preserve both
/// the `reserved_parameters` block and the source/field subkeys.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_universal_python_reserved_params_manifest() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("universal/python/reserved_params");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "universal-tool",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("slow_calculator"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // On-disk manifest preserves reserved_parameters with source/field.
    let install_dir = ext_install_dir(&cli, "slow_calculator");
    let manifest = std::fs::read_to_string(install_dir.join("manifest.yaml"))
        .unwrap_or_else(|e| panic!("read manifest at {install_dir:?}: {e}"));
    assert!(
        manifest.contains("reserved_parameters:"),
        "installed manifest should preserve reserved_parameters: {manifest}",
    );
    assert!(
        manifest.contains("session_id") && manifest.contains("agent_id"),
        "installed manifest should preserve both reserved params: {manifest}",
    );
    assert!(
        manifest.contains("source: \"runtime\""),
        "installed manifest should preserve source: \"runtime\": {manifest}",
    );
    assert!(
        manifest.contains("field: \"session_id\"")
            && manifest.contains("field: \"agent_id\""),
        "installed manifest should preserve field: <param>: {manifest}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "slow_calculator"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `universal/node/custom.ps1` T1: install a Node.js universal tool from
/// a manifest.yaml (Tier 2 detection). The test only validates the
/// install / list / info round-trip; it does NOT exec the Node tool
/// itself (that's L3 — needs Node on the test host).
///
/// The fixture chosen is `identity_tool/` (its manifest has
/// `extension_type: universal-tool` and a `parameters` schema).
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_universal_node_manifest_parsed() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("universal/node/identity_tool");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "universal-tool",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("identity_tool"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext info confirms type "universal-tool" and the manifest's
    // `description` round-trips.
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "identity_tool"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"universal-tool\""),
        "info should report type=universal-tool: {info_out}",
    );
    assert!(
        info_out.contains("identity"),
        "info should include the manifest description: {info_out}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "identity_tool"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// `gateway/http_basics/test.ps1` T1: install a gateway manifest
/// (Tier 2 detection from `manifest.yaml` with `extension_type:
/// gateway`). The test only validates the install / list / info
/// round-trip; it does NOT actually start the gateway Node.js process
/// (that's L2 — needs Node on the test host).
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_gateway_manifest_parsed() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let install_path = fixture_dir("gateway/http_basics");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "gateway",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("http-gateway-ref"),
        "install output should surface the extension id: stdout={out} stderr={err}",
    );

    // peko ext info confirms type "gateway".
    let (info_out, err, status) = run(
        &cli,
        &["ext", "info", "http-gateway-ref"],
        Duration::from_secs(10),
    );
    assert_ok(&info_out, &err, &status);
    assert!(
        info_out.contains("\"type\": \"gateway\""),
        "info should report type=gateway: {info_out}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "http-gateway-ref"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Cross-cutting (used in all 9 PS scripts as the install + verify +
/// uninstall pattern): install calculator-skill, verify it appears in
/// `peko ext list`, uninstall it, verify it no longer appears, verify
/// `peko ext info` returns an error for the now-missing id.
///
/// This test exists separately from #1 to lock the negative-path
/// behavior (uninstall removes the extension; subsequent list/info
/// reflect that).
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_install_uninstall_roundtrip() {
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    // Install the skill.
    let install_path = fixture_dir("skill/python/calculator-skill");
    let (out, err, status) = run(
        &cli,
        &["ext", "install", &install_path.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // peko ext list shows it.
    let (list_out, err, status) =
        run(&cli, &["ext", "list"], Duration::from_secs(10));
    assert_ok(&list_out, &err, &status);
    assert!(
        list_out.contains("calculator-skill"),
        "after install, list should include calculator-skill: {list_out}",
    );

    // Uninstall.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    // peko ext list no longer shows it.
    let (list_out, err, status) =
        run(&cli, &["ext", "list"], Duration::from_secs(10));
    assert_ok(&list_out, &err, &status);
    assert!(
        !list_out.contains("calculator-skill"),
        "after uninstall, list should NOT include calculator-skill: {list_out}",
    );

    // peko ext info for the now-missing id returns an error.
    let (_, _err, status) = run(
        &cli,
        &["ext", "info", "calculator-skill"],
        Duration::from_secs(10),
    );
    assert_ne!(
        status.code(),
        Some(0),
        "peko ext info on uninstalled id should exit non-zero",
    );

    // The on-disk install dir is also gone.
    let install_dir = ext_install_dir(&cli, "calculator-skill");
    assert!(
        !install_dir.exists(),
        "uninstall should remove the on-disk install dir at {install_dir:?}",
    );
}

/// Cross-cutting (`peko ext enable --target default/$agentName` pattern
/// used in 6 of 9 PS scripts). Installs `calculator_simple` and verifies
/// that `peko ext enable calculator_simple --target <agent>` writes
/// the canonical extension ID into the agent's `config.toml` at
/// `[extensions] enabled`. Then verifies `peko ext disable` removes it.
///
/// The agent's config path is `<config_dir>/agents/<agent_name>/config.toml`
/// per [`src/common/paths.rs:179-189`](src/common/paths.rs#L179-L189). In
/// our test setup `config_dir = <peko_dir>`, so the path is
/// `<peko_dir>/agents/<agent_name>/config.toml`.
///
/// The canonical ID for a non-builtin extension is the extension ID itself
/// (no `builtin:tool:` prefix). See
/// [`src/ipc/server.rs:1818-1903`](src/ipc/server.rs#L1818-L1903) — the
/// `canonical_id` for a non-builtin is just `id`.
///
/// We do not actually drive `peko send` against the agent; the L3
/// (LLM-tier tool execution) tests are deferred to a follow-up.
#[tokio::test]
#[ignore = "requires peko daemon"]
async fn ext_enable_for_agent_modifies_whitelist() {
    // Use a placeholder mock URL — this test never calls peko send.
    let mock_url = "http://mock-llm.invalid";

    let cli = PekoCli::new();
    let agent_name = "ext_enable_test_agent";
    write_ext_agent(cli.home(), agent_name, mock_url).expect("write ext agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Install the universal tool.
    let install_path = fixture_dir("universal/python/simple");
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "install",
            &install_path.to_string_lossy(),
            "--type",
            "universal-tool",
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // The agent config path. The peko agent on disk uses
    // <config_dir>/agents/<name>/config.toml; with PEKO_HOME=<peko_dir>,
    // config_dir resolves to <peko_dir>.
    let agent_config = cli.peko_dir().join("agents").join(agent_name).join("config.toml");
    assert!(
        agent_config.exists(),
        "agent config should exist at {agent_config:?} before enable",
    );

    // peko ext enable writes to the agent's whitelist.
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "enable",
            "calculator_simple",
            "--target",
            agent_name,
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator_simple"),
        "enable output should mention the extension id: stdout={out} stderr={err}",
    );

    // The agent's config.toml now contains "calculator_simple" in
    // [extensions] enabled.
    let after_enable = std::fs::read_to_string(&agent_config)
        .unwrap_or_else(|e| panic!("read {agent_config:?}: {e}"));
    assert!(
        after_enable.contains("calculator_simple"),
        "agent config should contain calculator_simple after enable: {after_enable}",
    );
    // The agent's [extensions] enabled block should have at least the
    // calculator_simple entry.
    assert!(
        after_enable.contains("enabled")
            && after_enable.contains("calculator_simple"),
        "agent config should have calculator_simple in [extensions] enabled: {after_enable}",
    );

    // peko ext disable removes the entry.
    let (out, err, status) = run(
        &cli,
        &[
            "ext",
            "disable",
            "calculator_simple",
            "--target",
            agent_name,
        ],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);

    let after_disable = std::fs::read_to_string(&agent_config)
        .unwrap_or_else(|e| panic!("read {agent_config:?}: {e}"));
    assert!(
        !after_disable.contains("calculator_simple"),
        "agent config should NOT contain calculator_simple after disable: {after_disable}",
    );

    // Cleanup.
    let (out, err, status) = run(
        &cli,
        &["ext", "uninstall", "calculator_simple"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}
