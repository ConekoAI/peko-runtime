//! Integration tests for the mock LLM's `MOCK_LLM_SCRIPT` list-of-responses
//! (sequence) feature. The mock is a single FastAPI shared by every peko
//! test subprocess, so the tests in this file MUST be isolated from each
//! other and from neighbouring `tests/*.rs` files that also hit the mock.
//!
//! Isolation strategy (two layers):
//!
//! 1. **Per-test unique substring** — every test picks a substring that no
//!    other test or file uses. The mock's per-substring counter is keyed
//!    by that substring, so even if state leaks between tests, the
//!    relevant counter is untouched.
//!
//! 2. **Explicit reset via `/_test/configure`** — each test starts by
//!    POSTing the new script to the mock. That endpoint also clears
//!    the per-substring counter map, so a test that REUSES a substring
//!    across iterations (the last-element-default test) starts from a
//!    known-good baseline.
//!
//! Tier: mock-LLM. Requires `MOCK_LLM_URL` (the docker-compose stack
//! brings it up). Tests early-return if unset so `cargo test` still
//! passes on a bare checkout.
//!
//! Spec lives in `docs/integration/TESTING.md` §3 ("Mock LLM vs Real LLM
//! Rule") and §7 Phase C.

mod common;
use common::{write_mock_agent, DaemonGuard, PekoCli, run_with_timeout};
use serial_test::serial;
use std::process::Stdio;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Read `MOCK_LLM_URL` and return the base URL. Returns `None` if unset —
/// tests early-return so `cargo test` still passes on a bare checkout.
fn mock_llm_url() -> Option<String> {
    let url = std::env::var("MOCK_LLM_URL").ok()?;
    if url.is_empty() {
        return None;
    }
    Some(url)
}

/// Build the base URL of the `/v1/chat/completions` endpoint (reqwest
/// needs the trailing slash or it drops the last path segment).
fn completions_url(base: &str) -> String {
    format!("{}/v1/chat/completions", base.trim_end_matches('/'))
}

/// Build the URL of the test-only `/configure` endpoint.
fn configure_url(base: &str) -> String {
    format!("{}/_test/configure", base.trim_end_matches('/'))
}

/// Install `MOCK_LLM_SCRIPT` on the mock and reset its per-substring
/// counters. The script is passed as a JSON-encoded string (the same
/// shape as the env var the docker-compose entrypoint sets), which lets
/// the helper install list-OR-string values uniformly.
///
/// Panics with a clear message if the mock is unreachable or returns
/// a non-OK status, so a misconfigured environment fails this test
/// rather than cascading into confusing LLM-side mismatches.
async fn configure_mock(base: &str, mock_llm_script_json: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");
    let url = configure_url(base);
    let body = serde_json::json!({ "MOCK_LLM_SCRIPT": mock_llm_script_json });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .unwrap_or_else(|e| panic!("POST {url} failed: {e}"));
    assert!(
        resp.status().is_success(),
        "POST {url} returned {} (body: {})",
        resp.status(),
        resp.text().await.unwrap_or_default(),
    );
}

/// Run `peko send` once and return (stdout, stderr, status).
fn send_once(cli: &PekoCli, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let (out, _, _) = run_with_timeout(
        || {
            let mut c = cli.cmd();
            c.stdout(Stdio::piped()).stderr(Stdio::piped());
            c
        },
        args,
        Duration::from_secs(20),
    )
    .expect("run peko send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (stdout, stderr, out.status)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// 1st / 2nd / 3rd LLM call gets the i-th element of the list.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn mock_llm_script_list_returns_ith_element_per_call() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    // Script with a 3-element list keyed on a test-unique substring.
    let needle = "seq-needle-abc123";
    let script = serde_json::json!({ needle: ["FIRST_TURN", "SECOND_TURN", "THIRD_TURN"] })
        .to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "seq-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    // Three sends, each prompt contains the needle so the substring
    // matcher in the mock picks the script entry. Each send hits the
    // mock exactly once, advancing the per-substring counter.
    for (i, expected) in ["FIRST_TURN", "SECOND_TURN", "THIRD_TURN"]
        .iter()
        .enumerate()
    {
        let prompt = format!("please react to {needle} turn {i}");
        let (stdout, stderr, status) =
            send_once(&cli, &["send", "seq-agent", &prompt, "--no-stream"]);
        assert_eq!(
            status.code(),
            Some(0),
            "peko send #{i} exited non-zero (status={status:?})\n\
             stdout: {stdout}\nstderr: {stderr}",
        );
        assert!(
            stdout.contains(expected),
            "call #{i} did not return {expected}\nstdout: {stdout}\nstderr: {stderr}",
        );
    }
}

/// After the list is exhausted, every further call returns the last
/// element — so a stray N+1 call in a test that scripted N turns
/// doesn't crash.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
#[serial]
async fn mock_llm_script_list_clamps_to_last_element_after_exhaustion() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    // Same script, 2-element list, send 4 times. Calls 3 and 4 should
    // both return the last element.
    let needle = "seq-needle-def456";
    let script = serde_json::json!({ needle: ["R1", "R2"] }).to_string();
    configure_mock(&mock_url, &script).await;

    let cli = PekoCli::new();
    write_mock_agent(cli.home(), "clamp-agent", &mock_url).expect("write mock agent");
    let _daemon = DaemonGuard::spawn(&cli);

    for (i, expected) in ["R1", "R2", "R2", "R2"].iter().enumerate() {
        let prompt = format!("needle {needle} call {i}");
        let (stdout, stderr, status) =
            send_once(&cli, &["send", "clamp-agent", &prompt, "--no-stream"]);
        assert_eq!(
            status.code(),
            Some(0),
            "peko send #{i} exited non-zero (status={status:?})\n\
             stdout: {stdout}\nstderr: {stderr}",
        );
        assert!(
            stdout.contains(expected),
            "call #{i} did not return {expected}\nstdout: {stdout}\nstderr: {stderr}",
        );
    }
}

/// A list element can be a `tool_call` dict — mixed text/tool sequences
/// are the use case that unblocks `cron_agent_tool.ps1` (see
/// `docs/integration/TESTING.md` §7 Phase B coverage gap).
///
/// We can't directly observe a tool call from `peko send` output
/// (the CLI prints the final text), so this test exercises the mock
/// directly: two calls with the same needle, the first returning a
/// tool_call, the second returning text.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL"]
#[serial]
async fn mock_llm_script_list_supports_mixed_text_and_tool_call() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let needle = "seq-needle-ghi789";
    let tool_args = r#"{"sub_command":"at","at":"2099-01-01T00:00:00Z","agent_id":"x"}"#;
    let script = serde_json::json!({
        needle: [
            { "tool_call": { "name": "cron", "arguments": tool_args } },
            "TOOL_SUCCESS",
        ]
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");
    let url = completions_url(&mock_url);

    // Call 1: expect a tool_call. We only assert the SSE stream
    // contains the tool name + the arguments JSON — the streamed
    // chunking is word-based for text but tool-call chunks are
    // emitted as two deltas (id+name, then arguments). Reading the
    // whole body and substring-searching is sufficient.
    let body = serde_json::json!({
        "model": "default",
        "messages": [{ "role": "user", "content": format!("first turn with {needle}") }],
    });
    let resp1 = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("call 1 send");
    assert!(resp1.status().is_success(), "call 1 status {}", resp1.status());
    let body1 = resp1.text().await.expect("read call 1 body");
    assert!(
        body1.contains("\"cron\""),
        "call 1 did not include tool name 'cron' in its stream\n{body1}",
    );
    // The tool-call chunk is emitted as a JSON string value, so the
    // inner quotes of `tool_args` are escaped to `\"` when the mock
    // serializes the SSE chunk. Assert on values inside the args
    // (a key, a value, and the timestamp) so the check is robust to
    // JSON escaping.
    for needle in ["sub_command", "2099-01-01T00:00:00Z", "agent_id"] {
        assert!(
            body1.contains(needle),
            "call 1 stream missing expected args substring {needle:?}\n{body1}",
        );
    }

    // Call 2: expect the text sentinel. Send the same needle again
    // so the per-substring counter advances to element 1.
    let body = serde_json::json!({
        "model": "default",
        "messages": [{ "role": "user", "content": format!("second turn with {needle}") }],
    });
    let resp2 = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .expect("call 2 send");
    assert!(resp2.status().is_success(), "call 2 status {}", resp2.status());
    let body2 = resp2.text().await.expect("read call 2 body");
    assert!(
        body2.contains("TOOL_SUCCESS"),
        "call 2 did not return the text sentinel 'TOOL_SUCCESS'\n{body2}",
    );
}

/// Backward compat: a single-string value still behaves as one-shot,
/// AND the per-substring counter does NOT advance (so each call returns
/// the same string forever — same as the pre-sequence behavior).
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL"]
#[serial]
async fn mock_llm_script_string_value_unchanged_single_shot() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let needle = "seq-needle-jkl012";
    let script = serde_json::json!({ needle: "ALWAYS_THIS" }).to_string();
    configure_mock(&mock_url, &script).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");
    let url = completions_url(&mock_url);
    let body = serde_json::json!({
        "model": "default",
        "messages": [{ "role": "user", "content": format!("hello {needle}") }],
    });

    // Three calls: every one should return the same string.
    for i in 0..3 {
        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("call {i} send: {e}"));
        assert!(resp.status().is_success(), "call {i} status {}", resp.status());
        let text = resp.text().await.expect("read body");
        assert!(
            text.contains("ALWAYS_THIS"),
            "call {i} did not return 'ALWAYS_THIS'\n{text}",
        );
    }
}

/// `/_test/configure` resets the per-substring counter map: after a
/// configure call, a fresh script with a previously-used substring
/// starts from element 0 again (NOT element N).
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL"]
#[serial]
async fn mock_test_configure_resets_sequence_counter() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");
    let url = completions_url(&mock_url);
    let needle = "seq-needle-mno345";

    // Phase 1: install a 2-element script and burn through it.
    let script_a = serde_json::json!({ needle: ["PHASE1_A", "PHASE1_B"] }).to_string();
    configure_mock(&mock_url, &script_a).await;
    let body = serde_json::json!({
        "model": "default",
        "messages": [{ "role": "user", "content": format!("{needle} burn") }],
    });
    for expected in ["PHASE1_A", "PHASE1_B", "PHASE1_B"] {
        let resp = client.post(&url).json(&body).send().await.expect("send");
        let text = resp.text().await.expect("read");
        assert!(text.contains(expected), "phase 1 missed {expected}\n{text}");
    }

    // Phase 2: reconfigure with a NEW script (same substring) and
    // verify the counter was reset — first call should get PHASE2_A,
    // not PHASE1_B.
    let script_b = serde_json::json!({ needle: ["PHASE2_A", "PHASE2_B"] }).to_string();
    configure_mock(&mock_url, &script_b).await;
    for expected in ["PHASE2_A", "PHASE2_B"] {
        let resp = client.post(&url).json(&body).send().await.expect("send");
        let text = resp.text().await.expect("read");
        assert!(
            text.contains(expected),
            "phase 2 missed {expected} (configure did not reset the counter)\n{text}",
        );
    }
}

/// Two different substrings have independent counters. Sequencing
/// `a` does not advance the counter for `b`.
#[tokio::test]
#[ignore = "requires MOCK_LLM_URL"]
#[serial]
async fn mock_llm_script_counters_are_per_substring() {
    let Some(mock_url) = mock_llm_url() else {
        eprintln!("MOCK_LLM_URL not set; skipping");
        return;
    };

    let needle_a = "seq-needle-pqr678";
    let needle_b = "seq-needle-stu901";
    let script = serde_json::json!({
        needle_a: ["A1", "A2"],
        needle_b: ["B1", "B2"],
    })
    .to_string();
    configure_mock(&mock_url, &script).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("build reqwest client");
    let url = completions_url(&mock_url);

    // The future captures `client` + `url` by move so the closure can be
    // called multiple times without each call consuming the closure. The
    // JSON body is built fresh inside the future.
    let call = move |content: String| {
        let client = client.clone();
        let url = url.clone();
        async move {
            let body = serde_json::json!({
                "model": "default",
                "messages": [{ "role": "user", "content": content }],
            });
            client
                .post(&url)
                .json(&body)
                .send()
                .await
                .expect("send")
                .text()
                .await
                .expect("read")
        }
    };

    // Interleave: A1, B1, A2, B2.
    let expectations = [
        (format!("first {needle_a}"), "A1"),
        (format!("first {needle_b}"), "B1"),
        (format!("second {needle_a}"), "A2"),
        (format!("second {needle_b}"), "B2"),
    ];
    for (i, (prompt, expected)) in expectations.iter().enumerate() {
        let body = call(prompt.clone()).await;
        let msg = format!(
            "interleaved call #{i} (prompt={prompt:?}) did not return {expected}\n{body}"
        );
        assert!(body.contains(expected), "{msg}");
    }
}
