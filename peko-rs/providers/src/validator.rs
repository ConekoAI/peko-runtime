//! Live model validation.
//!
//! `LlmResolver::test_key` only checks the shape of a stored key — it
//! doesn't talk to the network. This module closes that gap.
//! `Validator::test` makes the cheapest possible authenticated call
//! against the model's actual API and returns a [`CredentialTestOutcome`]
//! with the HTTP status, latency, human-readable reason, and (for
//! Anthropic-format models) the model id that was used. It's the live
//! counterpart to the shape check and is what `peko model test` and the
//! desktop's Test button call.
//!
//! Per-format dispatch mirrors [`crate::factory::create_provider_for_model`]:
//!
//! | `config.api_format` | Method | Path | Body |
//! |---|---|---|---|
//! | `OpenaiCompletions`   | `GET`  | `/models` (relative to `config.base_url`) | — |
//! | `AnthropicMessages`   | `POST` | `/v1/messages` | `{"model": "<config.model_id>", "messages": [{"role":"user","content":"ping"}], "max_tokens": 1}` |
//!
//! The validator deliberately does NOT go through the metered provider
//! wrappers: a user-initiated model ping must not charge quota, and the
//! cheap list-models call doesn't consume tokens anyway. For
//! Anthropic-format, the `max_tokens: 1` body makes the messages call
//! billable but small — the outcome's `model_used` field surfaces which
//! model was used so the UI can warn the user.

use std::time::Instant;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use crate::adapters::{AnthropicAdapter, AnyAdapter, ApiAdapter, OpenAiAdapter};
use crate::catalog::{ApiFormat, ModelConfig};
use crate::transport::client::{AuthConfig, HttpClient};

/// How long to wait for the ping before giving up.
///
/// 5 seconds is enough for every supported provider on a healthy
/// connection; Ollama on a cold start is the slowest realistic case.
/// Anything longer means the user's network or the provider is
/// genuinely broken — we want to surface that fast.
const VALIDATOR_TIMEOUT_SECS: u64 = 5;

/// The result of a single live credential ping.
///
/// All fields are populated by the validator itself; callers should
/// not assume any are non-None — `http_status` is `None` for
/// connection-level failures (DNS, TCP refused, timeout) and
/// `model_used` is `None` for `OpenaiCompletions` (the ping doesn't
/// pick a model).
#[derive(Debug, Clone)]
pub struct CredentialTestOutcome {
    /// True iff the provider returned a 2xx response. False for any
    /// HTTP error or transport-level failure.
    pub ok: bool,
    /// One-line human-readable verdict. Always populated; safe to
    /// surface directly in the UI or CLI without further mapping.
    pub message: String,
    /// Wall-clock latency of the ping, in milliseconds. Measured
    /// around the HTTP call (excluding credential lookup and
    /// `HttpClient` construction).
    pub latency_ms: u32,
    /// HTTP status code from the provider, when one was returned.
    /// `None` for connection-level failures (DNS / TCP / timeout).
    pub http_status: Option<u16>,
    /// For `AnthropicMessages` pings, the model id we sent in the
    /// `POST /v1/messages` body. `None` for `OpenaiCompletions`
    /// (no model is implied by `GET /models`).
    pub model_used: Option<String>,
}

/// Live validator entry point. Stateless — every call constructs a
/// fresh `HttpClient` so there's no auth state leaking between pings.
pub struct Validator;

impl Validator {
    /// Ping the model identified by `config` with `api_key` (or no key at
    /// all for local models like Ollama) and report what happened.
    ///
    /// `api_key` is `Some` for models with `requires_key = true` and
    /// `None` otherwise. The model config controls everything else:
    /// `base_url`, `api_format`, and `model_id` (for the Anthropic-format
    /// messages body).
    pub async fn test(
        config: &ModelConfig,
        api_key: Option<&SecretString>,
    ) -> CredentialTestOutcome {
        // Pre-flight: empty base URL means the user never configured
        // one (e.g. azure-openai's default template). Don't even try
        // to dial — surface a clear "go set one in Settings" error.
        if config.base_url.trim().is_empty() {
            return CredentialTestOutcome {
                ok: false,
                message: "No base URL configured; set one in Settings → Models".to_string(),
                latency_ms: 0,
                http_status: None,
                model_used: None,
            };
        }

        let started = Instant::now();
        // We pick a stable adapter per format so we can read its
        // `auth_config` / `extra_headers` for the `HttpClient`. The
        // adapter is otherwise unused — we build our own `HttpClient`
        // so we can suppress retries (the default policy would mask
        // 429s by waiting and retrying, which is wrong for a ping).
        let adapter = build_adapter(config);

        let mut extra_headers = adapter.extra_headers();
        // Merge catalog-level headers on top of adapter defaults;
        // same precedence rule as `Provider::new` so a Test ping
        // reaches the endpoint with the same headers a real call
        // would.
        for (name, value) in &config.headers {
            let needle = name.to_ascii_lowercase();
            if let Some(existing) = extra_headers
                .iter_mut()
                .find(|(n, _)| n.to_ascii_lowercase() == needle)
            {
                existing.1 = value.clone();
            } else {
                extra_headers.push((name.clone(), value.clone()));
            }
        }

        let auth = match (&api_key, config.requires_key) {
            (Some(key), _) => adapter.auth_config(key.expose_secret()),
            // Local / keyless model (Ollama). The HTTP request still
            // goes out; we just don't attach an Authorization header.
            (None, false) => AuthConfig::Bearer {
                token: String::new(),
            },
            // Vault says "no key" but the model requires one — that's
            // a misconfiguration on the user's side. Bail without
            // making the network call.
            (None, true) => {
                return CredentialTestOutcome {
                    ok: false,
                    message: format!("No key stored for '{}'", config.id),
                    latency_ms: started.elapsed().as_millis() as u32,
                    http_status: None,
                    model_used: None,
                };
            }
        };

        let client = match HttpClient::with_headers(
            config.base_url.clone(),
            auth,
            VALIDATOR_TIMEOUT_SECS,
            extra_headers,
        ) {
            Ok(c) => c,
            Err(e) => {
                return CredentialTestOutcome {
                    ok: false,
                    message: format!("Failed to construct HTTP client: {e}"),
                    latency_ms: started.elapsed().as_millis() as u32,
                    http_status: None,
                    model_used: None,
                };
            }
        };

        let outcome = match config.api_format {
            ApiFormat::OpenaiCompletions | ApiFormat::OpenAiResponses => {
                ping_openai_compat(&client).await
            }
            ApiFormat::AnthropicMessages => {
                ping_anthropic_messages(&client, &config.model_id).await
            }
        };

        let elapsed_ms = started.elapsed().as_millis() as u32;
        CredentialTestOutcome {
            latency_ms: elapsed_ms,
            ..outcome
        }
    }
}

/// Build an `AnyAdapter` purely so we can call its `auth_config` /
/// `extra_headers`. The adapter's `base_url` is unused — the
/// `HttpClient` we build carries the real base URL. Mirrors the
/// construction in
/// [`crate::factory::create_provider_for_model`].
fn build_adapter(config: &ModelConfig) -> AnyAdapter {
    match config.api_format {
        ApiFormat::OpenaiCompletions => {
            // Even when `config.base_url` is empty (we already bailed
            // on that above), construct the adapter so we can read its
            // default auth/headers — the URL never reaches the wire
            // because the HttpClient is the one that actually dials.
            let adapter = if config.base_url.is_empty() {
                OpenAiAdapter::new()
            } else {
                OpenAiAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::OpenAi(adapter)
        }
        ApiFormat::AnthropicMessages => {
            let adapter = if config.base_url.is_empty() {
                AnthropicAdapter::new()
            } else {
                AnthropicAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::Anthropic(adapter)
        }
        ApiFormat::OpenAiResponses => {
            // Same as Chat Completions: `GET /models` is the canonical
            // readiness ping for any OpenAI-compatible surface.
            let adapter = if config.base_url.is_empty() {
                crate::adapters::OpenAiResponsesAdapter::new()
            } else {
                crate::adapters::OpenAiResponsesAdapter::new().with_base_url(&config.base_url)
            };
            AnyAdapter::OpenAiResponses(adapter)
        }
    }
}

/// `GET /models` ping for OpenAI-compatible providers.
///
/// We deserialize the body as a generic `{"data": [...]}` envelope
/// because every OpenAI-compatible provider agrees on this shape (the
/// actual model objects differ slightly between vendors but we only
/// need the count for the success message). If the body shape is
/// unexpected we still report success — the important signal is that
/// the server returned 2xx and accepted the key.
async fn ping_openai_compat(client: &HttpClient) -> CredentialTestOutcome {
    #[derive(Deserialize)]
    struct ListModels {
        data: Option<Vec<serde_json::Value>>,
    }

    match client.get::<ListModels>("/models").await {
        Ok(resp) => {
            let count = resp.data.as_ref().map(|d| d.len()).unwrap_or(0);
            CredentialTestOutcome {
                ok: true,
                message: if count > 0 {
                    format!("Connection successful ({count} models)")
                } else {
                    "Connection successful".to_string()
                },
                latency_ms: 0,
                http_status: Some(200),
                model_used: None,
            }
        }
        Err(e) => map_error(e),
    }
}

/// 1-token `POST /v1/messages` ping for Anthropic-format providers.
///
/// The cheapest valid Anthropic call: a single user message with
/// `max_tokens: 1`. This costs ~$0.0001 per click on Claude — we
/// surface the model id used in `model_used` so the UI can warn the
/// user about the charge.
async fn ping_anthropic_messages(client: &HttpClient, model: &str) -> CredentialTestOutcome {
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
    });
    let model_owned = model.to_string();

    match client
        .post_json::<serde_json::Value, serde_json::Value>(
            "/v1/messages",
            &body,
            &[], // no per-request headers for the ping
        )
        .await
    {
        Ok(_) => CredentialTestOutcome {
            ok: true,
            message: format!("Connection successful (1 token billed via {model})"),
            latency_ms: 0,
            http_status: Some(200),
            model_used: Some(model_owned),
        },
        Err(e) => map_error(e),
    }
}

/// Translate an `HttpClient` error into a structured outcome.
///
/// The transport layer wraps every HTTP error as
/// `"HTTP error {code}: {body}"` and connection-level failures
/// surface as plain `anyhow` chains. We do best-effort status
/// extraction so the UI can show "HTTP 401" inline.
fn map_error(err: anyhow::Error) -> CredentialTestOutcome {
    let msg = err.to_string();
    let http_status = extract_http_status(&msg);

    let message = match http_status {
        Some(401) => format!(
            "HTTP 401: invalid api key — {tail}",
            tail = tail_after(&msg, 401)
        ),
        Some(403) => format!(
            "HTTP 403: forbidden — key lacks access. {tail}",
            tail = tail_after(&msg, 403)
        ),
        Some(404) => format!(
            "HTTP 404: endpoint not found; verify the base URL is correct. {tail}",
            tail = tail_after(&msg, 404)
        ),
        Some(429) => format!(
            "HTTP 429: rate limited — try again later. {tail}",
            tail = tail_after(&msg, 429)
        ),
        Some(code) if (500..600).contains(&code) => {
            format!("HTTP {code}: upstream error")
        }
        Some(code) => format!("HTTP {code}: {msg}"),
        // No status code — connection refused, DNS failure, timeout,
        // or TLS handshake. Surface the underlying message verbatim
        // because that's exactly what the user needs to debug it.
        None => msg,
    };

    CredentialTestOutcome {
        ok: false,
        message,
        latency_ms: 0,
        http_status,
        model_used: None,
    }
}

/// Pull the `NNN` out of an `"HTTP error NNN: ..."` chain. Returns
/// the first matching code only — the chain always has at most one.
///
/// The transport also injects `(retry_after=Ns)` between the status
/// and the colon for 429/503 responses with a `Retry-After` header
/// (see `classify_http_error` in `client.rs`), so we walk through
/// any number of space-separated tokens before the colon instead of
/// blindly splitting on `:`.
fn extract_http_status(msg: &str) -> Option<u16> {
    let rest = msg.strip_prefix("HTTP error ")?;
    // Walk until we hit `:` or run out of tokens. Each token is
    // either the bare status (`"500"`) or a parenthetical like
    // `"(retry_after=3s)"` that we ignore for status extraction.
    for token in rest.split_whitespace() {
        if let Some(stripped) = token.strip_suffix(':') {
            return stripped.parse().ok();
        }
        // Bare numeric token (no trailing colon) — accept it
        // defensively even though the standard format doesn't emit
        // this. Skip anything that starts with `(` (the retry_after
        // suffix).
        if !token.starts_with('(') {
            if let Ok(code) = token.parse::<u16>() {
                return Some(code);
            }
        }
    }
    None
}

/// Short tail after the status line for the common 401/403/404/429
/// cases — gives the user the upstream's body text without the
/// prefix noise. Truncated to keep the UI line reasonable.
fn tail_after(msg: &str, code: u16) -> String {
    let prefix = format!("HTTP error {code}:");
    let Some(rest) = msg.strip_prefix(&prefix) else {
        return String::new();
    };
    let trimmed = rest.trim();
    if trimmed.len() > 120 {
        format!("{}…", &trimmed[..120])
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// Spawn a one-shot HTTP server on a random localhost port. The
    /// returned URL (e.g. `"http://127.0.0.1:54321"`) is what tests
    /// hand to `ProviderCatalogEntry.base_url`.
    ///
    /// `handler` runs once per request and is expected to write a
    /// complete HTTP/1.1 response (status line + headers + body).
    /// After the first request the listener is closed — these tests
    /// are single-shot pings, not load tests.
    async fn spawn_once<F>(handler: F) -> String
    where
        F: Fn(&str) -> String + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind localhost");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Read the request — we don't care about the body for
                // these tests, but we do need to consume it before
                // writing the response or some servers will RST.
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let request = String::from_utf8_lossy(&buf);
                let response = handler(&request);
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        format!("http://{addr}")
    }

    fn make_entry(
        id: &str,
        api_format: ApiFormat,
        base_url: String,
        model_id: &str,
        requires_key: bool,
    ) -> ModelConfig {
        ModelConfig {
            id: id.to_string(),
            display_name: id.to_string(),
            template_id: None,
            api_format,
            base_url,
            model_id: model_id.to_string(),
            context_window: None,
            max_output_tokens: None,
            headers: Default::default(),
            credential_id: None,
            requires_key,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            compat: None,
        }
    }

    fn ok_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn status_response(code: u16, reason: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    }

    #[tokio::test]
    async fn openai_compat_200_with_models_reports_success_and_count() {
        let base = spawn_once(|_| ok_response(r#"{"data":[{"id":"a"},{"id":"b"}]}"#)).await;
        let entry = make_entry("openai", ApiFormat::OpenaiCompletions, base, "gpt-5", true);
        let key = SecretString::new("sk-test".to_string().into());

        let outcome = Validator::test(&entry, Some(&key)).await;

        assert!(outcome.ok);
        assert_eq!(outcome.http_status, Some(200));
        assert!(
            outcome.message.contains("2 models"),
            "msg: {}",
            outcome.message
        );
        assert!(outcome.latency_ms < 5_000);
        assert!(outcome.model_used.is_none());
    }

    #[tokio::test]
    async fn openai_compat_401_maps_to_invalid_api_key_message() {
        let base =
            spawn_once(|_| status_response(401, "Unauthorized", r#"{"error":"invalid api key"}"#))
                .await;
        let entry = make_entry("openai", ApiFormat::OpenaiCompletions, base, "gpt-5", true);
        let key = SecretString::new("sk-bogus".to_string().into());

        let outcome = Validator::test(&entry, Some(&key)).await;

        assert!(!outcome.ok);
        assert_eq!(outcome.http_status, Some(401));
        assert!(outcome.message.contains("401"), "msg: {}", outcome.message);
        assert!(
            outcome.message.contains("invalid"),
            "msg should map to invalid-key copy, got: {}",
            outcome.message
        );
    }

    #[tokio::test]
    async fn anthropic_format_200_surfaces_model_used() {
        let base = spawn_once(|_| {
            ok_response(r#"{"id":"msg_01","content":[{"type":"text","text":"."}]}"#)
        })
        .await;
        let entry = make_entry(
            "anthropic",
            ApiFormat::AnthropicMessages,
            base,
            "claude-haiku-4-5",
            true,
        );
        let key = SecretString::new("sk-ant-test".to_string().into());

        let outcome = Validator::test(&entry, Some(&key)).await;

        assert!(outcome.ok);
        assert_eq!(outcome.http_status, Some(200));
        assert_eq!(outcome.model_used.as_deref(), Some("claude-haiku-4-5"));
        assert!(
            outcome.message.contains("claude-haiku-4-5"),
            "msg: {}",
            outcome.message
        );
    }

    #[tokio::test]
    async fn empty_base_url_returns_error_without_dialing() {
        // We can't easily prove "no network call was made" from a
        // unit test, but we can prove the fast path: the outcome is
        // populated, ok=false, latency is effectively zero, and the
        // message names the configuration gap.
        let entry = make_entry(
            "azure-openai",
            ApiFormat::OpenaiCompletions,
            String::new(),
            "gpt-5",
            true,
        );
        let key = SecretString::new("sk-test".to_string().into());

        let outcome = Validator::test(&entry, Some(&key)).await;

        assert!(!outcome.ok);
        assert_eq!(outcome.http_status, None);
        assert!(outcome.latency_ms < 100);
        assert!(
            outcome.message.contains("base URL"),
            "msg: {}",
            outcome.message
        );
    }

    #[tokio::test]
    async fn ollama_local_with_no_key_pings_models_endpoint() {
        // Ollama doesn't require a key — the validator should still
        // issue a `GET /models` against the base URL.
        let base = spawn_once(|_| ok_response(r#"{"data":[{"id":"llama3"}]}"#)).await;
        let entry = make_entry(
            "ollama",
            ApiFormat::OpenaiCompletions,
            base,
            "llama3",
            false,
        );

        let outcome = Validator::test(&entry, None).await;

        assert!(outcome.ok);
        assert!(outcome.message.contains("1 model"));
    }

    #[tokio::test]
    async fn connection_refused_surfaces_underlying_error() {
        // Bind a listener and immediately drop it so the port is
        // closed — reqwest should get ECONNREFUSED.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let entry = make_entry(
            "openai",
            ApiFormat::OpenaiCompletions,
            format!("http://{addr}"),
            "gpt-5",
            true,
        );
        let key = SecretString::new("sk-test".to_string().into());

        let outcome = Validator::test(&entry, Some(&key)).await;

        assert!(!outcome.ok);
        assert_eq!(outcome.http_status, None);
        assert!(!outcome.message.is_empty());
    }

    #[tokio::test]
    async fn requires_key_true_with_no_key_says_so_without_dialing() {
        let base = "http://127.0.0.1:1".to_string(); // would fail if dialed
        let entry = make_entry("openai", ApiFormat::OpenaiCompletions, base, "gpt-5", true);

        let outcome = Validator::test(&entry, None).await;

        assert!(!outcome.ok);
        assert!(outcome.message.contains("No key"));
    }

    #[tokio::test]
    async fn extract_http_status_parses_well_formed_error_chains() {
        assert_eq!(extract_http_status("HTTP error 401: bad"), Some(401));
        assert_eq!(
            extract_http_status("HTTP error 500 (retry_after=3s): boom"),
            Some(500)
        );
        assert_eq!(extract_http_status("HTTP error 429: too many"), Some(429));
        assert_eq!(extract_http_status("connection refused"), None);
        assert_eq!(extract_http_status("HTTP error abc: bad"), None);
    }

    // Silence the unused-import lint for items only used in
    // feature-gated paths (the `oneshot`/`Arc` imports are kept
    // available for future tests).
    #[allow(dead_code)]
    fn _unused_witness() -> (Arc<()>, oneshot::Sender<()>) {
        (Arc::new(()), oneshot::channel().0)
    }
}
