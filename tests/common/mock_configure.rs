//! Helpers for scripting the mock LLM via its `/_test/configure` endpoint.
//!
//! `mock_llm_server.py` exposes a test-only `POST /_test/configure` that
//! swaps the `MOCK_LLM_SCRIPT` / `DEFAULT_RESPONSE` env vars in place
//! and clears the per-substring counter map (see §3 *Sequence* in
//! `docs/integration/TESTING.md`). Tests that need to script multi-turn
//! dialogs (e.g. the cron-tool agent flows) call [`configure_mock`]
//! at the start of the test to install their script and start from a
//! known-good baseline, then issue chat calls that hit the same
//! substring counter.
//!
//! All tests in this crate that drive the mock directly — or that
//! script a daemon to call the mock in a particular shape — share this
//! helper to keep the wire format in one place.

#![allow(dead_code)]

use std::time::Duration;

/// Build the URL of the test-only `/configure` endpoint.
pub fn configure_url(base: &str) -> String {
    format!("{}/_test/configure", base.trim_end_matches('/'))
}

/// Install `MOCK_LLM_SCRIPT` on the mock and reset its per-substring
/// counters. The script is passed as a JSON-encoded string (the same
/// shape as the env var the docker-compose entrypoint sets), which lets
/// the helper install list-OR-string values uniformly.
///
/// Panics with a clear message if the mock is unreachable or returns
/// a non-OK status, so a misconfigured environment fails the test
/// rather than cascading into confusing LLM-side mismatches.
pub async fn configure_mock(base: &str, mock_llm_script_json: &str) {
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
