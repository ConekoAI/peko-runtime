//! End-to-end user-journey scenario D3 (Phase D slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage — flow 5 from the Phase D plan: agent round-trip from
//! author through pekohub to collaborator, including the
//! auto-extension-pulled-on-agent-pull contract. Two `PekoCli`
//! instances (one per user) drive the runtime side; the PekoHub
//! backend in `pekohub/backend/tests/fixtures/server.ts` drives
//! the registry side.
//!
//! | Rust test                                       | Flow step                                                   |
//! |-------------------------------------------------|-------------------------------------------------------------|
//! | `agent_push_with_no_extensions_round_trip`      | Flow 5a: agent with no exts — push, pull, agent list        |
//! | `agent_pull_auto_pulls_declared_extension`      | Flow 5b: collab pull auto-pulls declared ext                 |
//! | `agent_pull_already_present_ext_no_repull`      | Flow 5c: collab already has the ext — no re-pull             |
//! | `agent_pull_failed_ext_does_not_block_pull`     | Flow 5d: collab can't pull a declared ext — agent still lands |
//!
//! ## Scope
//!
//! **Mock-LLM tier.** PekoHub is the long-lived fixture container
//! (or local node+tsx process) — `make docker-up` brings it up. The
//! pekohub DB is reset at the top of every test with `reset_pekohub`
//! so two-`PekoCli` flows don't collide on the `users_external_id_key`
//! / `users_namespace_key` unique constraints.
//!
//! All tests early-return if `PEKOHUB_URL` is unset (or
//! `MOCK_LLM_URL` is unset, since each test also needs the daemon
//! to drive `peko agent export` and `peko agent list`), so a bare
//! `cargo test` still passes.
//!
//! ## Why this is mock-LLM tier
//!
//! Same reasoning as D1/D2: what we are testing is orchestration
//! plumbing — the runtime successfully authenticates against
//! pekohub, the OCI agent push completes, the OCI agent pull
//! delivers a `.agent` blob, and the collab's `peko agent pull`
//! auto-pulls declared extensions. The agent list assertions prove
//! the agent is registered post-pull; the chat is a smoke test
//! covered more thoroughly by D1's `agent_chats_locally_with_keyword`.
//!
//! ## The structural facts this file relies on
//!
//! 1. **`peko agent export` is daemon-driven** — see
//!    [`src/commands/agent/handlers.rs:232`](../../src/commands/agent/handlers.rs#L232)
//!    (`DaemonClient::connect()`). So the author daemon must be
//!    running before any `peko agent export` call.
//! 2. **`peko agent push --file <file> <ref>` is the agent-side push
//!    path** used in this test. The CLI reads the `.agent` file,
//!    unpacks its layers into the local `AgentRegistry`, and pushes
//!    the manifest. `--file` is preferred over a local tag because
//!    it lets the test pin the input exactly.
//! 3. **`peko agent pull <ref>` (no `--output`) auto-imports the
//!    agent locally** and (since flow 5) calls
//!    `ensure_extensions_for_agent` to pull any declared
//!    `ExtensionRef` from the agent's manifest. Failures are
//!    captured in `extensions.failed` but do not block the import
//!    (per the contract comment at
//!    [`src/commands/agent/handlers.rs:1020-1027`](../../src/commands/agent/handlers.rs#L1020-L1027)).
//! 4. **The `extensions` field in `AgentManifest` is the source of
//!    truth for the auto-pull contract** — see the
//!    `ExtensionRef` type at
//!    [`src/portable/types.rs:104-109`](../../src/portable/types.rs#L104-L109).
//!    The runtime populates it during `peko agent export` by
//!    resolving each entry of `agent.config.extensions.enabled`
//!    to its installed extension's `manifest.source` (the
//!    registry ref that was used to pull it). The
//!    `agent → registry → agent` round-trip preserves the
//!    `extensions` list via the `dev.pekohub.extensions`
//!    OCI annotation (see
//!    [`src/registry/manifest.rs:290-394`](../../src/registry/manifest.rs#L290-L394)
//!    and [`src/commands/agent/handlers.rs:1087-1133`](../../src/commands/agent/handlers.rs#L1087-L1133)).
//! 5. **`peko ext pull` is the only path that sets
//!    `manifest.source`** (see
//!    [`src/commands/ext.rs:1179`](../../src/commands/ext.rs#L1179))
//!    — a locally-installed extension has `source = None` and is
//!    silently skipped from the agent export's extension_refs list.
//!    So the author must round-trip the ext through the registry
//!    (`peko ext push` → `peko ext pull`) before exporting the
//!    agent, otherwise the agent's manifest carries no extension
//!    refs.
//! 6. **`peko agent pull --json` output schema** is
//!    `{success, registry_ref, name, config_path, extensions:
//!    {pulled, already_present, failed}, manifest: {...}}` per
//!    [`src/commands/agent/handlers.rs:1029-1047`](../../src/commands/agent/handlers.rs#L1029-L1047).
//!    The output is mixed with `tracing::warn!` lines from the
//!    auto-ext-pull path (e.g. when an ext pull 404s), so the
//!    helper `extract_json` slices from the first `{` to the
//!    matching `}` before parsing.
//! 7. **`peko agent push --file <file> --json` output schema**
//!    is `{success, local_tag, registry_ref, manifest: {...}}` per
//!    [`src/commands/agent/handlers.rs:689-703`](../../src/commands/agent/handlers.rs#L689-L703).

#[path = "../common/mod.rs"]
mod common;
use common::{reset_pekohub, PekoCli, PekohubBackend};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers (shared with s2 — duplicated here to keep s3 self-contained
// per the per-scenario isolation pattern established by D1/D2)
// ---------------------------------------------------------------------------

/// Read `PEKOHUB_URL` and `MOCK_LLM_URL` env. Returns Some(urls) only
/// when both are set and non-empty. Tests early-return on None so a
/// bare `cargo test` on a checkout without the docker-compose stack
/// still passes.
fn hub_and_llm_urls() -> Option<(String, String)> {
    let hub = std::env::var("PEKOHUB_URL").ok()?;
    if hub.is_empty() {
        return None;
    }
    let llm = std::env::var("MOCK_LLM_URL").ok()?;
    if llm.is_empty() {
        return None;
    }
    Some((hub, llm))
}

/// Run a `peko …` command and return (stdout, stderr, status).
fn run(
    cli: &PekoCli,
    args: &[&str],
    timeout: Duration,
) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = common::run_with_timeout(
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

/// Mint a PekoHub API key for the user identified by `jwt`.
async fn mint_api_key(
    client: &reqwest::Client,
    backend_url: &str,
    jwt: &str,
    name: &str,
) -> String {
    let resp = client
        .post(format!("{backend_url}/v1/auth/api-keys"))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("api-key POST transport failed");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "api-key mint failed: status={status}, body={body}"
    );
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("api-key response not JSON: {e}; body={body}"));
    v["key"]
        .as_str()
        .unwrap_or_else(|| panic!("api-key response missing `key`: {body}"))
        .to_string()
}

/// Create a test user AND return its database-assigned numeric id.
/// Needed because the JWT `sub` claim is the user_id (per the
/// pekohub auth plugin at
/// [pekohub/backend/src/plugins/auth.ts:122](../../pekohub/backend/src/plugins/auth.ts#L122)
/// which resolves `decoded.sub` to a `users.id` lookup).
async fn create_test_user_with_id(
    client: &reqwest::Client,
    base_url: &str,
    namespace: &str,
) -> (i64, String) {
    common::create_test_user(client, base_url, namespace).await
}

/// Drive `peko login --api-key <key> --registry <url>` on the given
/// PekoCli instance.
fn peko_login(cli: &PekoCli, api_key: &str, registry_url: &str) {
    let (out, err, status) = run(
        cli,
        &["login", "--api-key", api_key, "--registry", registry_url],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Compute the registry reference the author will push to.
fn registry_ref(backend_url: &str, author_ns: &str, agent_name: &str, tag: &str) -> String {
    format!("{backend_url}/{author_ns}/{agent_name}:{tag}")
}

/// Compute the same registry reference with the URL scheme stripped —
/// needed because the runtime's `RegistryRef::full_ref()` strips the
/// scheme (see `src/registry/client.rs:107-114`), and the JSON output
/// emits the stripped form.
fn host_only(backend_url: &str) -> &str {
    backend_url
        .strip_prefix("http://")
        .or_else(|| backend_url.strip_prefix("https://"))
        .unwrap_or(backend_url)
}

/// Write a minimal Tier 1 skill extension to a scratch dir.
fn write_calculator_skill(scratch: &Path) -> std::io::Result<PathBuf> {
    let skill_dir = scratch.join("calculator-skill");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"---
name: calculator-skill
description: Sum two numbers (test fixture)
---
# Sum
Compute a + b.
"#,
    )?;
    Ok(skill_dir)
}

/// Local install of the skill fixture. `peko ext install` IPCs to the
/// daemon, so the daemon must already be running when this is called.
fn install_local_skill(cli: &PekoCli) {
    let skill_dir =
        write_calculator_skill(&cli.home().join("scratch")).expect("write skill fixture");
    let (out, err, status) = run(
        cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
}

/// Path to the on-disk extensions dir for the given CLI. With
/// `PEKO_HOME=<tempdir>/.peko`, the data dir is `<tempdir>/.peko/data`
/// and the extensions are at `<tempdir>/.peko/data/extensions/<id>/`.
fn ext_source_path(cli: &PekoCli, ext_id: &str) -> PathBuf {
    cli.peko_dir()
        .join("data")
        .join("extensions")
        .join(ext_id)
        .join(".source")
}

/// Write a fake `.source` file in the extension's storage dir. This
/// is the runtime's contract for "where did this ext come from" — see
/// `ExtensionStorage::read_source` at
/// [`src/extension/manager/storage.rs:196-200`](../../src/extension/manager/storage.rs#L196-L200)
/// and how `ExtensionManager::load_all` reads it at
/// [`src/extension/manager/mod.rs:429-430`](../../src/extension/manager/mod.rs#L429-L430).
/// Used in flow 5d to fabricate a "bad" ext ref for the
/// `agent_pull_failed_ext_does_not_block_pull` test.
fn write_ext_source(cli: &PekoCli, ext_id: &str, registry_ref: &str) {
    let p = ext_source_path(cli, ext_id);
    std::fs::create_dir_all(p.parent().unwrap()).expect("create ext storage dir");
    std::fs::write(&p, registry_ref).expect("write .source file");
}

/// Push-then-pull an extension: the author installs the skill,
/// pushes it to the registry, then pulls it back so the local
/// `manifest.source` is populated (required for
/// `peko agent export` to include the ext in
/// `extensions.enabled`'s resolved `ExtensionRef` list).
///
/// This is a single daemon-scope: install + push happen with the
/// daemon running; the subsequent pull must also run with the
/// daemon up (it IPC-installs via
/// `DaemonClient::connect()` at
/// [`src/commands/ext.rs:594`](../../src/commands/ext.rs#L594)).
fn round_trip_extension_through_registry(
    cli: &PekoCli,
    ext_id: &str,
    pushed_ref: &str,
) {
    // Install (daemon-driven).
    install_local_skill(cli);
    // Push (no daemon needed; OCI client).
    let (out, err, status) = run(
        cli,
        &["--json", "ext", "push", ext_id, pushed_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    // Pull (daemon-driven; populates manifest.source).
    let (out, err, status) = run(
        cli,
        &["--json", "ext", "pull", pushed_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
}

/// Export an agent to a `.agent` file via the daemon-driven
/// `peko agent export`. Returns the path of the exported file.
///
/// `peko agent export` takes the agent name via `--name` (not
/// positionally) per the `AgentCommands::Export` def at
/// [`src/commands/agent.rs:87-104`](../../src/commands/agent.rs#L87-L104).
fn export_agent_to_file(cli: &PekoCli, agent_name: &str) -> PathBuf {
    let out_path = cli.home().join(format!("{agent_name}.agent"));
    let (out, err, status) = run(
        cli,
        &[
            "agent",
            "export",
            "--name",
            agent_name,
            "--output",
            &out_path.to_string_lossy(),
        ],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out_path.exists(),
        "exported .agent file does not exist at {}; stdout: {out}\nstderr: {err}",
        out_path.display(),
    );
    out_path
}

/// Extract the trailing JSON object that has a top-level `success`
/// field from a `peko` command's stdout.
///
/// The `peko agent pull --json` output is preceded by the
/// auto-ext-pull's progress messages and may have `tracing::warn!`
/// log lines interleaved (the test harness's tracing subscriber
/// writes WARN to STDOUT on this platform). Those WARN lines can
/// contain pekohub's error JSON like `{"errors":[...]}`, which
/// would otherwise be picked up by a naive "first `{`" parser.
/// We walk all top-level JSON objects and return the LAST one
/// that has a top-level `"success"` field — that is, by
/// construction, the `peko agent pull` JSON (the pekohub error
/// JSON has no `success` field).
fn extract_json(stdout: &str) -> &str {
    // Iterate over all top-level `{...}` objects in the stream,
    // tracking each one's byte range, then return the last one
    // whose body has `"success"` as a top-level key.
    let bytes = stdout.as_bytes();
    let mut i = 0;
    let mut last_success: Option<(usize, usize)> = None;
    while i < bytes.len() {
        if bytes[i] != b'{' {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth: i32 = 0;
        let mut in_string = false;
        let mut escape = false;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if escape {
                escape = false;
                j += 1;
                continue;
            }
            match b {
                b'\\' if in_string => escape = true,
                b'"' => in_string = !in_string,
                b'{' if !in_string => depth += 1,
                b'}' if !in_string => {
                    depth -= 1;
                    if depth == 0 {
                        // Found the end of this top-level object.
                        let obj = &stdout[start..=j];
                        // Cheap top-level-shape check: the
                        // object must have `"success"` as a
                        // top-level key. The `peko agent pull`
                        // JSON is pretty-printed with `"success"`
                        // deep in the body (it's the last key in
                        // the output schema), so we search the
                        // whole object — not just the head. The
                        // pekohub error JSON has no `"success"`
                        // key at all, so this filter is sound.
                        if obj.contains("\"success\"") {
                            last_success = Some((start, j));
                        }
                        j += 1;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        i = j.max(i + 1);
    }
    match last_success {
        Some((s, e)) => &stdout[s..=e],
        None => panic!("no JSON object with top-level `success` in stdout: {stdout}"),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Flow 5a (positive): author pushes an agent with no declared
/// extensions; collaborator pulls it. Asserts the pull JSON shows
/// `extensions.{pulled, already_present, failed}` all empty, and the
/// agent is registered post-pull (visible in `peko agent list`).
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn agent_push_with_no_extensions_round_trip() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s3_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s3-author-key").await;
    let (collab_id, _collab_ns) =
        create_test_user_with_id(&client, &backend.url, "s3_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &_collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s3-collab-key").await;

    // ── Author side: write agent, export, push ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);
    let author_agent = "s3_author_agent";
    common::write_v3_mock_agent(author.home(), author_agent, &mock_url)
        .expect("write author mock agent");
    let author_daemon = common::DaemonGuard::spawn(&author);
    let author_file = export_agent_to_file(&author, author_agent);
    let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
    let (out, err, status) = run(
        &author,
        &[
            "--json",
            "agent",
            "push",
            "<file>",
            &pushed_ref,
            "--file",
            &author_file.to_string_lossy(),
        ],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("push --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(v["success"], serde_json::json!(true), "push JSON: {v}");
    let expected_ref = format!("{}/{}/{author_agent}:v1.0", host_only(&backend.url), author_ns);
    assert_eq!(v["registry_ref"], expected_ref, "push JSON: {v}");
    // Manifest layers: 2 (config + identity) for the
    // `write_v3_mock_agent` shape — the config layer is `agent.toml`,
    // the identity layer is the DID doc.
    assert_eq!(
        v["manifest"]["layers"],
        serde_json::json!(2),
        "push JSON: {v}"
    );
    // local_tag is "<file>" for --file pushes
    // (see `src/commands/agent/handlers.rs:680-684`).
    assert_eq!(v["local_tag"], "<file>", "push JSON: {v}");
    drop(author_daemon);

    // ── Collaborator side: pull, agent list ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);

    // `peko agent pull --json` (without --output) auto-imports the
    // agent locally and prints the post-import JSON with the
    // extension-pull results.
    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "pull", &pushed_ref],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    let pull: serde_json::Value = serde_json::from_str(extract_json(&out))
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(pull["success"], serde_json::json!(true), "pull JSON: {pull}");
    assert_eq!(
        pull["name"],
        serde_json::json!(author_agent),
        "pull JSON: {pull}"
    );
    // No extensions were declared by the author — all three lists are empty.
    assert_eq!(
        pull["extensions"]["pulled"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["already_present"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["failed"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );

    // `peko agent list` is IPC-driven (see
    // `src/commands/agent/handlers.rs:24-25`), so the collab
    // daemon must be up.
    let _collab_daemon = common::DaemonGuard::spawn(&collab);
    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "list"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(author_agent),
        "agent list should include the pulled agent: stdout={out} stderr={err}",
    );
}

/// Flow 5b (positive): author's agent declares
/// `extensions = [calculator-skill]`. The ext is round-tripped
/// through the registry first so the author's local
/// `manifest.source` is set (otherwise the export silently skips
/// the ext — see helpers docstring). After the author pushes the
/// agent, the collaborator's `peko agent pull` auto-pulls the
/// declared ext and installs it; the collab can then list it.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn agent_pull_auto_pulls_declared_extension() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s3_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s3-author-key").await;
    let (collab_id, _collab_ns) =
        create_test_user_with_id(&client, &backend.url, "s3_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &_collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s3-collab-key").await;

    // ── Author side: round-trip the ext through the registry, then
    //    enable on the agent, export + push the agent. ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);

    let ext_id = "calculator-skill";
    let ext_ref = registry_ref(&backend.url, &author_ns, ext_id, "v1.0");
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        round_trip_extension_through_registry(&author, ext_id, &ext_ref);
    }

    let author_agent = "s3_author_agent";
    common::write_v3_mock_agent(author.home(), author_agent, &mock_url)
        .expect("write author mock agent");
    // Enable the ext on the agent (daemon-driven).
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        let (out, err, status) = run(
            &author,
            &["ext", "enable", ext_id, "--target", author_agent],
            Duration::from_secs(10),
        );
        assert_ok(&out, &err, &status);
        // Export (daemon-driven). The export reads the enabled
        // list and resolves each entry to an ExtensionRef using
        // the local `manifest.source` (set by the pull above).
        let author_file = export_agent_to_file(&author, author_agent);
        // Push the .agent file to the registry.
        let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
        let (out, err, status) = run(
            &author,
            &[
                "--json",
                "agent",
                "push",
                "<file>",
                &pushed_ref,
                "--file",
                &author_file.to_string_lossy(),
            ],
            Duration::from_secs(20),
        );
        assert_ok(&out, &err, &status);
    }

    // ── Collaborator side: pull the agent (no ext pre-installed)
    //    → auto-pulls calculator-skill → list shows the ext. ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);
    let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");

    // `peko agent pull` is NOT daemon-driven (it does the OCI
    // fetch + import in-process at
    // `src/commands/agent/handlers.rs:846+`). But the auto-ext-pull
    // IPC-installs each `ExtensionRef` via the daemon, so we still
    // need the collab daemon running.
    let _collab_daemon = common::DaemonGuard::spawn(&collab);

    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "pull", &pushed_ref],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    let pull: serde_json::Value = serde_json::from_str(extract_json(&out))
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(pull["success"], serde_json::json!(true), "pull JSON: {pull}");
    assert_eq!(
        pull["extensions"]["pulled"],
        serde_json::json!([ext_id]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["already_present"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["failed"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );

    // `peko ext list` is IPC-driven. Should now show calculator-skill.
    let (out, err, status) = run(&collab, &["--json", "ext", "list"], Duration::from_secs(10));
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(ext_id),
        "ext list should include the auto-pulled extension: stdout={out} stderr={err}",
    );
}

/// Flow 5c (positive): collaborator pre-installs the ext (via
/// `peko ext pull`), THEN pulls the agent. The auto-ext-pull
/// should see the ext is already present and report it under
/// `already_present` rather than re-pulling.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn agent_pull_already_present_ext_no_repull() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s3_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s3-author-key").await;
    let (collab_id, _collab_ns) =
        create_test_user_with_id(&client, &backend.url, "s3_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &_collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s3-collab-key").await;

    // ── Author side: same as test 2 ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);

    let ext_id = "calculator-skill";
    let ext_ref = registry_ref(&backend.url, &author_ns, ext_id, "v1.0");
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        round_trip_extension_through_registry(&author, ext_id, &ext_ref);
    }
    let author_agent = "s3_author_agent";
    common::write_v3_mock_agent(author.home(), author_agent, &mock_url)
        .expect("write author mock agent");
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        let (out, err, status) = run(
            &author,
            &["ext", "enable", ext_id, "--target", author_agent],
            Duration::from_secs(10),
        );
        assert_ok(&out, &err, &status);
        let author_file = export_agent_to_file(&author, author_agent);
        let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
        let (out, err, status) = run(
            &author,
            &[
                "--json",
                "agent",
                "push",
                "<file>",
                &pushed_ref,
                "--file",
                &author_file.to_string_lossy(),
            ],
            Duration::from_secs(20),
        );
        assert_ok(&out, &err, &status);
    }

    // ── Collaborator side: pre-install the ext via pull, THEN
    //    pull the agent. The auto-pull should see the ext is
    //    present. ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);
    let _collab_daemon = common::DaemonGuard::spawn(&collab);

    // Pre-install: pull the ext before the agent.
    let (out, err, status) = run(
        &collab,
        &["--json", "ext", "pull", &ext_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);

    // Now pull the agent.
    let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "pull", &pushed_ref],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    let pull: serde_json::Value = serde_json::from_str(extract_json(&out))
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(pull["success"], serde_json::json!(true), "pull JSON: {pull}");
    assert_eq!(
        pull["extensions"]["pulled"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["already_present"],
        serde_json::json!([ext_id]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["failed"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
}

/// Flow 5d (negative-ish): author's agent declares an extension
/// whose `manifest.source` is fabricated to point at a ref the
/// collab cannot reach (the registry returns 404 / the OCI fetch
/// fails). The auto-ext-pull captures this in
/// `extensions.failed`, but the agent import still succeeds (per
/// the contract at
/// [`src/commands/agent/handlers.rs:1020-1027`](../../src/commands/agent/handlers.rs#L1020-L1027):
/// "Failures are logged but do not break the pull — the user can
/// install missing extensions manually afterwards.").
///
/// We avoid the runtime's normal "skip the ext if no source" path
/// by writing a `.source` file directly (the runtime's
/// `ExtensionStorage::read_source` reads it and
/// `ExtensionManager::load_all` writes it onto the loaded
/// manifest).
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn agent_pull_failed_ext_does_not_block_pull() {
    let Some((_hub_url, mock_url)) = hub_and_llm_urls() else {
        eprintln!("PEKOHUB_URL or MOCK_LLM_URL not set; skipping");
        return;
    };

    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s3_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s3-author-key").await;
    let (collab_id, _collab_ns) =
        create_test_user_with_id(&client, &backend.url, "s3_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &_collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s3-collab-key").await;

    // ── Author side: install the ext locally, fabricate a bad
    //    `.source` for it (so the export will include it as an
    //    `ExtensionRef`), enable on the agent, export + push. ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);
    let ext_id = "calculator-skill";
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        install_local_skill(&author);
        // Fabricate a bad ref. The collab will try to pull this
        // exact ref and the pekohub backend will 404 it (no such
        // bundle under author_ns). Use the author_ns to make the
        // URL well-formed — it still won't exist.
        let bad_ref = registry_ref(&backend.url, &author_ns, "missing-skill", "v9.9");
        write_ext_source(&author, ext_id, &bad_ref);
    }
    let author_agent = "s3_author_agent";
    common::write_v3_mock_agent(author.home(), author_agent, &mock_url)
        .expect("write author mock agent");
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        let (out, err, status) = run(
            &author,
            &["ext", "enable", ext_id, "--target", author_agent],
            Duration::from_secs(10),
        );
        assert_ok(&out, &err, &status);
        let author_file = export_agent_to_file(&author, author_agent);
        let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
        let (out, err, status) = run(
            &author,
            &[
                "--json",
                "agent",
                "push",
                "<file>",
                &pushed_ref,
                "--file",
                &author_file.to_string_lossy(),
            ],
            Duration::from_secs(20),
        );
        assert_ok(&out, &err, &status);
    }

    // ── Collaborator side: pull the agent. The declared ext
    //    cannot be pulled (404 from pekohub), but the agent must
    //    still import. ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);
    let _collab_daemon = common::DaemonGuard::spawn(&collab);
    let pushed_ref = registry_ref(&backend.url, &author_ns, author_agent, "v1.0");
    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "pull", &pushed_ref],
        Duration::from_secs(30),
    );
    assert_ok(&out, &err, &status);
    let pull: serde_json::Value = serde_json::from_str(extract_json(&out))
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(pull["success"], serde_json::json!(true), "pull JSON: {pull}");
    // Failed list contains the ext id (not the registry_ref) — see
    // the `failed` field mapping at
    // `src/commands/agent/handlers.rs:1038`.
    assert_eq!(
        pull["extensions"]["failed"],
        serde_json::json!([ext_id]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["pulled"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );
    assert_eq!(
        pull["extensions"]["already_present"],
        serde_json::json!([]),
        "pull JSON: {pull}"
    );

    // Agent list should still show the agent.
    let (out, err, status) = run(
        &collab,
        &["--json", "agent", "list"],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains(author_agent),
        "agent list should include the pulled agent (despite ext failure): stdout={out} stderr={err}",
    );
}
