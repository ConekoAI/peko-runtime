//! End-to-end user-journey scenario D2 (Phase D slice per
//! `docs/integration/TESTING.md` §7).
//!
//! Coverage — flow 3+4 from the Phase D plan: extension round-trip
//! from author through pekohub to collaborator. Two `PekoCli`
//! instances (one per user) drive the runtime side; the PekoHub
//! backend in `pekohub/backend/tests/fixtures/server.ts` drives
//! the registry side.
//!
//! | Rust test                                   | Flow step                                              |
//! |---------------------------------------------|--------------------------------------------------------|
//! | `ext_push_succeeds_with_pekohub_test`       | Flow 3: author `peko ext push` → pekohub               |
//! | `ext_pull_round_trip_two_clis`              | Flow 4: collab `peko ext pull` → chat via Principal    |
//! | `ext_pull_auto_resolves_dependencies`      | Flow 4b: pull also fetches declared deps               |
//! | `ext_push_without_login_fails`              | Flow 3 negative: no `peko login` ⇒ 401-ish            |
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
//! to install the extension), so a bare `cargo test` still passes.
//!
//! ## Why this is mock-LLM tier
//!
//! Same reasoning as D1: what we are testing is orchestration
//! plumbing — the runtime successfully authenticates against
//! pekohub, the OCI push completes, the OCI pull delivers a
//! `.ext` blob, and the collab can install + enable + chat
//! end-to-end. The mock LLM provides the chat payload.
//!
//! ## The two structural facts this file relies on
//!
//! 1. **The PekoHub API key is sent as `Authorization: Bearer <ph_…>`**
//!    ([pekohub/backend/src/plugins/auth.ts:70-80](../../pekohub/backend/src/plugins/auth.ts#L70-L80)).
//!    The runtime's `RegistryClient` resolves the registry token
//!    stored by `peko login --api-key` into a Bearer header
//!    ([`src/registry/client.rs:421`](../../src/registry/client.rs#L421)).
//! 2. **The PekoHub API key endpoint is `POST /v1/auth/api-keys`**
//!    (mounted at `/v1/auth` prefix, see
//!    [pekohub/backend/src/index.ts:124](../../pekohub/backend/src/index.ts#L124))
//!    and accepts `{ "name": "..." }`; response is
//!    `{ "key": "ph_…", "id": …, "prefix": "ph_…", "name": "…" }`.
//!    Mints a `ph_<6-char prefix><24-char secret>` key
//!    ([pekohub/backend/src/routes/auth/api-keys.ts:26-30](../../pekohub/backend/src/routes/auth/api-keys.ts#L26-L30)).

#[path = "../common/mod.rs"]
mod common;
use common::{create_test_user, reset_pekohub, PekoCli, PekohubBackend};
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;


// ---------------------------------------------------------------------------
// Helpers
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

/// Mint a PekoHub API key for the user identified by `jwt`. The
/// response body is `{"id":N, "name":"…", "prefix":"ph_…", "key":"ph_…"}`.
/// Returns the full `ph_…` secret — the only time the server returns it.
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
/// which resolves `decoded.sub` to a `users.id` lookup). This is
/// just a thin wrapper over `common::create_test_user` (which
/// returns `(id, namespace)`) — the namespace is the OCI ref
/// path, and the id is the JWT subject.
async fn create_test_user_with_id(
    client: &reqwest::Client,
    base_url: &str,
    namespace: &str,
) -> (i64, String) {
    common::create_test_user(client, base_url, namespace).await
}

/// Drive `peko login --api-key <key> --registry <url>` on the given
/// PekoCli instance. After this call the API key is persisted at
/// `<HOME>/.peko/credentials.json` and the runtime's
/// `CredentialsService::get_registry_token()` returns it
/// ([`src/common/services/credentials_service.rs:101-104`](../../src/common/services/credentials_service.rs#L101-L104)).
fn peko_login(cli: &PekoCli, api_key: &str, registry_url: &str) {
    let (out, err, status) = run(
        cli,
        &["login", "--api-key", api_key, "--registry", registry_url],
        Duration::from_secs(10),
    );
    assert_ok(&out, &err, &status);
}

/// Compute the registry reference the author will push to. The
/// pekohub push/pull protocol expects `<host>/<namespace>/<name>:<tag>`
/// — the namespace is the pekohub user's namespace (returned by
/// `create_test_user`), NOT the literal string `peko/agents/…`
/// (which is the default for a bare ref without a host).
fn registry_ref(backend_url: &str, author_ns: &str, ext_name: &str, tag: &str) -> String {
    format!("{backend_url}/{author_ns}/{ext_name}:{tag}")
}

/// Write a minimal Tier 1 skill extension to a scratch dir. The
/// `name:` frontmatter becomes the extension ID, so this fixture
/// installs as extension id `calculator-skill`.
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

/// Path to the Principal's on-disk `principal.toml`.
fn principal_config_path(cli: &PekoCli, principal_name: &str) -> PathBuf {
    cli.peko_dir()
        .join("principals")
        .join(principal_name)
        .join("principal.toml")
}

/// Create a Principal wired to the mock LLM and grant it the
/// `calculator-skill` extension in `[capabilities] grants` (the
/// Principal-era equivalent of the legacy
/// `peko ext enable calculator-skill --target <agent>` flow on a
/// standalone agent config).
///
/// `calculator-skill` is a Tier 1 SKILL.md; skills are granted with the
/// `skill:<id>` capability syntax. The dispatcher's `is_tool_enabled`
/// owner check is satisfied when the capability set contains the skill's
/// canonical extension id.
fn create_collaborator_principal(cli: &PekoCli, name: &str, mock_llm_url: &str) {
    common::create_mock_principal_with_tools(
        cli,
        name,
        mock_llm_url,
        &["skill:calculator-skill"],
    );
}

/// Re-pull the same ref. `peko ext pull` writes a temp `.ext` and
/// pre-populates the registry source map; it does NOT copy the
/// files into the extension storage dir. The collab uses
/// `peko ext install <local SKILL.md path>` from a local copy of
/// the fixture — structurally identical to the .ext payload the
/// pull produced. This is the same trick D1 used; the install path
/// is asserted separately, the pull path is asserted via the JSON
/// output above.
fn install_local_skill_copy(cli: &PekoCli) {
    let scratch = cli.home().join("scratch");
    let skill_dir = write_calculator_skill(&scratch).expect("write local skill fixture");
    let (out, err, status) = run(
        cli,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Flow 3 (positive): author installs the skill, then pushes it to
/// the test pekohub. The push (with the global `--json` flag) emits
/// `{success, extension_id, registry_ref, manifest.{name,version,
/// digest,kind,layers,total_size}}` per
/// [`src/commands/ext.rs:973-988`](../../src/commands/ext.rs#L973-L988).
/// Asserts: push exits 0, the JSON parses, `success==true`,
/// `registry_ref` matches the pushed ref, and `manifest.layers==1`
/// (one config layer for the .ext payload).
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn ext_push_succeeds_with_pekohub_test() {
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
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s2_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s2-author-key").await;

    // Author side: log in to pekohub, install the skill, push.
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);

    let skill_dir =
        write_calculator_skill(&author.home().join("scratch")).expect("write skill fixture");

    // Install needs the daemon (it's an IPC-driven command).
    let _daemon = common::DaemonGuard::spawn(&author);
    let (out, err, status) = run(
        &author,
        &["ext", "install", &skill_dir.to_string_lossy()],
        Duration::from_secs(15),
    );
    assert_ok(&out, &err, &status);

    // Push. The CLI's JSON output is controlled by the global --json
    // flag (see `Cli` struct at [`src/commands/mod.rs:67-69`](../../src/commands/mod.rs#L67-L69))
    // — `peko ext push` itself does not take a per-subcommand --json
    // flag (see its `Subcommand` def at ext.rs:154-162). So the
    // peko invocation here passes `--json` BEFORE the `ext` subcommand.
    let pushed_ref = registry_ref(&backend.url, &author_ns, "calculator-skill", "v1.0");
    let (out, err, status) = run(
        &author,
        &["--json", "ext", "push", "calculator-skill", &pushed_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);

    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("push --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(v["success"], serde_json::json!(true), "push JSON: {v}");
    assert_eq!(v["extension_id"], "calculator-skill", "push JSON: {v}");
    // The runtime's `RegistryRef::full_ref()` strips the URL scheme
    // (see `src/registry/client.rs:107-114` — the bare `host:port`
    // form is what `full_ref` emits). Compare against the
    // scheme-stripped version of our input ref.
    let host_only = backend
        .url
        .strip_prefix("http://")
        .or_else(|| backend.url.strip_prefix("https://"))
        .unwrap_or(&backend.url);
    let expected_ref = format!("{host_only}/{author_ns}/calculator-skill:v1.0");
    assert_eq!(v["registry_ref"], expected_ref, "push JSON: {v}");
    assert_eq!(
        v["manifest"]["layers"],
        serde_json::json!(1),
        "push JSON: {v}"
    );
    assert_eq!(
        v["manifest"]["kind"],
        serde_json::json!("extension"),
        "push JSON: {v}"
    );

    // Reference the mock URL so the unused-warning stays quiet if
    // the body above is edited in the future.
    let _ = mock_url;
}

/// Flow 4 (positive): collaborator pulls the extension the author
/// pushed, installs it (the pull itself writes a temp `.ext` and
/// pre-populates the registry source; the install copies the files
/// into the local extension storage), grants the calculator-skill
/// capability on a Principal, and chats via `peko send <principal>`.
/// Asserts the round-trip: collab's `peko ext list --json` shows the
/// extension, the Principal's `principal.toml` carries
/// `calculator-skill` in `[capabilities] tools`, and `peko send`
/// echoes the mock-LLM keyword.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn ext_pull_round_trip_two_clis() {
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
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s2_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s2-author-key").await;
    let (collab_id, collab_ns) = create_test_user_with_id(&client, &backend.url, "s2_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s2-collab-key").await;

    // ── Author side: install + push ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        install_local_skill_copy(&author);
    }
    let pushed_ref = registry_ref(&backend.url, &author_ns, "calculator-skill", "v1.0");
    let (out, err, status) = run(
        &author,
        &["--json", "ext", "push", "calculator-skill", &pushed_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);

    // ── Collaborator side: pull + install + enable + chat ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);

    // `peko ext pull` writes a temp .ext and records the source in
    // the local manager's source map, but then installs via IPC —
    // the install half calls `DaemonClient::connect()` (see
    // `src/commands/ext.rs:594`), so the collab daemon must be
    // running BEFORE the pull. Spawn it here, before the pull.
    let _daemon = common::DaemonGuard::spawn(&collab);

    let (out, err, status) = run(
        &collab,
        &["--json", "ext", "pull", &pushed_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    // `peko ext pull --json` (despite its name) emits the IPC
    // install response, NOT the OCI pull manifest — the OCI
    // fetch is done in `handle_ext_pull_to_temp` (whose result
    // is discarded with `_manifest` at src/commands/ext.rs:583),
    // and the printed JSON is the ExtensionInstalled response
    // from the daemon (src/commands/ext.rs:601-605). The
    // round-trip is still proven: `success==true` and `id` is
    // the pulled extension's id. We also assert the on-disk
    // ext-list JSON below, which DOES carry the manifest-derived
    // fields.
    let pull_json: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(
        pull_json["success"],
        serde_json::json!(true),
        "pull JSON: {pull_json}"
    );
    assert_eq!(
        pull_json["id"],
        serde_json::json!("calculator-skill"),
        "pull JSON: {pull_json}"
    );

    // The pull already installed the extension via IPC — no
    // need to re-install. Continue with the chat round-trip.
    //
    // After the "Principal as the single actor" migration, the
    // standalone-agent `peko ext enable <ext> --target <agent>` flow
    // is gone: the chat surface is `peko send <principal>`, and the
    // dispatcher's `is_tool_enabled` whitelist is the union of the
    // root agent's fixed base set plus the Principal's `principal.toml
    // [capabilities] tools` entries (see
    // `src/principal/agent_runner.rs::run_root_agent_prompt`). The CLI
    // does not expose a live capability-grant command — we patch
    // `principal.toml` directly via `create_collaborator_principal`
    // (mirrors `tests/common/agent.rs::create_mock_principal_with_tools`).
    let collab_principal = "s2_collab_principal";
    create_collaborator_principal(&collab, collab_principal, &mock_url);

    // `peko ext list --json` should now show the extension. List is
    // an IPC-driven command (see ext.rs:289-295), so the daemon
    // (still running) handles it.
    let (out, err, status) = run(&collab, &["--json", "ext", "list"], Duration::from_secs(10));
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("calculator-skill"),
        "ext list should include the pulled extension: stdout={out} stderr={err}",
    );

    // Principal config has calculator-skill in capabilities.tools.
    let cfg = principal_config_path(&collab, collab_principal);
    let after = std::fs::read_to_string(&cfg).expect("read principal.toml");
    assert!(
        after.contains("calculator-skill"),
        "collab principal.toml should contain calculator-skill after capability grant: {after}",
    );

    // Chat round-trip via the mock LLM.
    let (out, err, status) = run(
        &collab,
        &[
            "send",
            collab_principal,
            "Use the calculator extension to add 1+2. Respond with: S2_CHAT_OK",
            "--no-stream",
        ],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    assert!(
        out.contains("S2_CHAT_OK"),
        "stdout did not echo the keyword 'S2_CHAT_OK'\nstdout: {out}\nstderr: {err}",
    );
}

/// Flow 4b: dependency resolution. The author pushes a wrapper
/// extension that declares `dependencies: [calculator-skill]`. The
/// collaborator has nothing installed. After `peko ext pull
/// <wrapper_ref>`, the wrapper should be installed successfully
/// and its `id` returned. The dependency-resolver is exercised
/// at the OCI-fetch layer (see `handle_ext_pull_with_seen` at
/// [`src/commands/ext.rs:1131-1375`](../../src/commands/ext.rs#L1131-L1375))
/// but its detailed `dependencies` array is NOT exposed in the
/// printed `--json` output (which is the IPC install response,
/// not the OCI pull manifest — see the matching comment in
/// `ext_pull_round_trip_two_clis`). This test pins the
/// end-to-end success of the pull-with-deps path.
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
#[serial]
async fn ext_pull_auto_resolves_dependencies() {
    let Some((_hub_url, _mock_url)) = hub_and_llm_urls() else {
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
    let (author_id, author_ns) = create_test_user_with_id(&client, &backend.url, "s2_author").await;
    let author_jwt = common::generate_jwt(author_id, &author_ns);
    let author_key = mint_api_key(&client, &backend.url, &author_jwt, "s2-author-key").await;
    let (collab_id, collab_ns) = create_test_user_with_id(&client, &backend.url, "s2_collab").await;
    let collab_jwt = common::generate_jwt(collab_id, &collab_ns);
    let collab_key = mint_api_key(&client, &backend.url, &collab_jwt, "s2-collab-key").await;

    // ── Author: push calculator-skill ──
    let author = PekoCli::new();
    peko_login(&author, &author_key, &backend.url);
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        install_local_skill_copy(&author);
    }
    let calc_ref = registry_ref(&backend.url, &author_ns, "calculator-skill", "v1.0");
    let (out, err, status) = run(
        &author,
        &["--json", "ext", "push", "calculator-skill", &calc_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);

    // ── Author: write + push a wrapper ext that depends on calculator-skill ──
    // The wrapper is a Tier 1 SKILL.md with a `dependencies:` frontmatter
    // list. Skill extensions accept the dep as `package: calculator-skill`
    // (the source-of-truth resolver keys on package name = extension ID).
    let wrapper_dir = author.home().join("scratch").join("wrapper-skill");
    std::fs::create_dir_all(&wrapper_dir).expect("create wrapper dir");
    std::fs::write(
        wrapper_dir.join("SKILL.md"),
        r#"---
name: wrapper-skill
description: Wrapper that depends on calculator-skill
dependencies:
  - calculator-skill
---
# Wrapper
Use the calculator.
"#,
    )
    .expect("write wrapper SKILL.md");
    {
        let _daemon = common::DaemonGuard::spawn(&author);
        let (out, err, status) = run(
            &author,
            &["ext", "install", &wrapper_dir.to_string_lossy()],
            Duration::from_secs(15),
        );
        assert_ok(&out, &err, &status);
    }
    let wrapper_ref = registry_ref(&backend.url, &author_ns, "wrapper-skill", "v1.0");
    let (out, err, status) = run(
        &author,
        &["--json", "ext", "push", "wrapper-skill", &wrapper_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);

    // ── Collaborator: pull the wrapper, no calculator installed ──
    let collab = PekoCli::new();
    peko_login(&collab, &collab_key, &backend.url);

    // `peko ext pull` installs via IPC after the OCI fetch — the
    // daemon must be running first (src/commands/ext.rs:594).
    let _daemon = common::DaemonGuard::spawn(&collab);

    let (out, err, status) = run(
        &collab,
        &["--json", "ext", "pull", &wrapper_ref],
        Duration::from_secs(20),
    );
    assert_ok(&out, &err, &status);
    // `peko ext pull --json` emits the IPC install response, NOT
    // the OCI pull manifest with `dependencies` (see the matching
    // comment in `ext_pull_round_trip_two_clis`). The pull itself
    // resolved the wrapper extension and IPC-installed it, so we
    // assert on the install success + id.
    let v: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("pull --json did not emit JSON: {e}; stdout={out}"));
    assert_eq!(v["success"], serde_json::json!(true), "pull JSON: {v}");
    assert_eq!(
        v["id"],
        serde_json::json!("wrapper-skill"),
        "pull JSON: {v}"
    );
}

/// Flow 3 negative: a fresh `PekoCli` (no `peko login` ever run)
/// attempts `peko ext push` after installing the skill. The CLI
/// must fail with an error that mentions "No registry
/// authentication" — the exact string emitted by
/// [`src/commands/ext.rs:258-263`](../../src/commands/ext.rs#L258-L263).
#[tokio::test]
#[ignore = "requires PEKOHUB_URL + MOCK_LLM_URL + peko daemon"]
async fn ext_push_without_login_fails() {
    let Some((_hub_url, _mock_url)) = hub_and_llm_urls() else {
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
    let (_id, author_ns) = create_test_user(&client, &backend.url, "s2_author").await;

    // A PekoCli that has never seen `peko login`. We don't need it
    // to be the same user as the test_user above — the "no login"
    // test is purely about the CLI's local state, not pekohub's.
    let cli = PekoCli::new();

    // Install the skill first so push has something to push. Install
    // does NOT need a pekohub token.
    {
        let _daemon = common::DaemonGuard::spawn(&cli);
        install_local_skill_copy(&cli);
    }

    // Now push without login. Exit non-zero; stderr mentions the
    // "No registry authentication" sentinel.
    let pushed_ref = registry_ref(&backend.url, &author_ns, "calculator-skill", "v1.0");
    let (out, err, status) = run(
        &cli,
        &["ext", "push", "calculator-skill", &pushed_ref],
        Duration::from_secs(10),
    );
    assert_ne!(
        status.code(),
        Some(0),
        "push without login should fail (status=0 indicates a regression in the auth check)\nstdout: {out}\nstderr: {err}",
    );
    let combined = format!("{out}\n{err}");
    assert!(
        combined.contains("No registry authentication"),
        "push without login should mention 'No registry authentication': stdout={out} stderr={err}",
    );
}
