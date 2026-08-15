//! Shared HTTP client for all providers
//!
//! Handles authentication, retries, timeouts, and request/response formatting.

use super::retry::{RetryExecutor, RetryPolicy, SharedRetryBudget};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde::{de::DeserializeOwned, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tracing::debug;

// Phase 9b.N.5b.5 — `is_context_window_exceeded` is a pure bool helper
// over `&anyhow::Error` lifted into `peko_provider_api` so the agentic
// loop can use it without depending on the root-only transport layer.
// Keep a re-export alias here so existing call sites that import via
// `crate::transport::client::is_context_window_exceeded`
// (notably `src/engine/agentic_loop.rs:2097`) keep compiling.
pub use peko_provider_api::is_context_window_exceeded;

/// Build an `anyhow::Error` that carries the upstream `Retry-After`
/// hint AND the parsed rate-limit snapshot.
///
/// F40a Phase 2A: when the server replies with a non-success status
/// and includes `Retry-After: <seconds>` (RFC 7231 §7.1.3), the value
/// is appended to the message in a parseable `(retry_after=Ns)` form
/// so [`super::retry::RetryableError::retry_after`] can pull it back
/// out and let [`RetryExecutor`] wait the server-suggested interval
/// instead of guessing with exponential backoff. Also embeds the
/// parsed `RateLimitSnapshot` (`rate_limit_kind=...`,
/// `rate_limit_*=...`) so string-only retry sites can recover
/// structured rate-limit metadata without coupling to
/// `reqwest` / `hyper` types.
///
/// `retry_after: Some(Duration::ZERO)` is treated as "no hint" so we
/// don't pay the parse cost on every 4xx response — only well-formed
/// positive-second hints are propagated.
fn classify_http_error(
    status: u16,
    error_text: String,
    retry_after: Option<Duration>,
    snapshot: Option<&peko_provider_api::RateLimitSnapshot>,
) -> anyhow::Error {
    let retry_after = retry_after.filter(|d| !d.is_zero());
    let mut tokens: Vec<String> = Vec::new();
    if let Some(d) = retry_after {
        tokens.push(format!("retry_after={}s", d.as_secs()));
    }
    if let Some(s) = snapshot {
        // Embed the snapshot block; the parser
        // (`peko_provider_api::parse_snapshot_metadata`) will round-trip
        // it back out for callers that want structured access.
        tokens.push(peko_provider_api::format_snapshot(s));
    }
    let prefix = if tokens.is_empty() {
        format!("HTTP error {status}")
    } else {
        format!("HTTP error {status} ({})", tokens.join("; "))
    };
    anyhow::anyhow!("{prefix}: {error_text}")
}

// Re-exported from `peko_provider_api::is_context_window_exceeded`
// (Phase 9b.N.5b.5). See `crates/provider-api/src/context_window_error.rs`
// for the docstring — it covers the matching rules, the F22 front-evict
// + retry consumer, and the inline-substring rationale.
// Re-exported from `peko_provider_api::is_context_window_exceeded`
// (Phase 9b.N.5b.5). See `crates/provider-api/src/context_window_error.rs`
// for the matching rules, the F22 front-evict + retry consumer, and the
// inline-substring rationale.

/// Parse the `Retry-After` response header (seconds form OR HTTP-date
/// form per RFC 7231 §7.1.3).
///
/// F40a: lifted to `peko_provider_api::parse_retry_after_header`. The
/// pre-fix version was seconds-only; the new helper accepts the
/// HTTP-date form too. The HTTP-date form is rare from major LLM
/// providers but spec-compliant; accepting it surfaces more precise
/// retry intervals when providers do emit one. Returns `None` for
/// missing / malformed / zero-second / past values (a past HTTP-date
/// would be meaningless — the predicate strips it).
fn parse_retry_after_header(value: &str) -> Option<Duration> {
    peko_provider_api::parse_retry_after_header(value, SystemTime::now())
}

/// Authentication configuration
#[derive(Debug, Clone)]
pub enum AuthConfig {
    Bearer { token: String },
    Header { name: String, value: String },
}

/// Shared HTTP client for provider API calls
#[derive(Clone)]
pub struct HttpClient {
    inner: Client,
    base_url: String,
    auth: AuthConfig,
    extra_headers: Vec<(String, String)>,
    retry_policy: Option<RetryPolicy>,
    /// F40: shared retry budget drawn from by both transport and
    /// the agentic-loop mid-stream retry site. `None` is the
    /// pre-F40 default — transport retries governed by
    /// `retry_policy.max_retries` alone.
    shared_budget: Option<Arc<SharedRetryBudget>>,
    /// F40a: vendored `RateLimitParser` for snapshot capture at the
    /// moment of 429/503/etc. The default `StandardRateLimitParser`
    /// covers both OpenAI and Anthropic header families — adapters
    /// with non-standard headers can swap in a custom parser.
    rate_limit_parser: Option<Arc<dyn peko_provider_api::RateLimitParser>>,
    /// F40a: most-recent rate-limit snapshot captured on a failed
    /// call. Held under a `Mutex` so multiple concurrent callers see
    /// a coherent view; the agentic loop reads it after a 429 to
    /// surface `kind` + remaining counters via
    /// `RateLimitSnapshot` instead of string-parsing. Wrapped in
    /// `Arc` so the `Clone` impl is cheap.
    last_snapshot: Arc<Mutex<Option<peko_provider_api::RateLimitSnapshot>>>,
}

impl HttpClient {
    /// Create a new HTTP client
    pub fn new(
        base_url: impl Into<String>,
        auth: AuthConfig,
        timeout_secs: u64,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_mins(1))
            .http1_only() // Force HTTP/1.1 to avoid HTTP/2 issues with some providers
            .build()?;

        let base_url = base_url.into();
        // Remove trailing slash for consistency
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            inner: client,
            base_url,
            auth,
            extra_headers: vec![],
            retry_policy: None,
            shared_budget: None,
            rate_limit_parser: None,
            last_snapshot: Arc::new(Mutex::new(None)),
        })
    }

    /// Create a new HTTP client with extra headers
    pub fn with_headers(
        base_url: impl Into<String>,
        auth: AuthConfig,
        timeout_secs: u64,
        extra_headers: Vec<(String, String)>,
    ) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_keepalive(Duration::from_mins(1))
            .http1_only()
            .build()?;

        let base_url = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();

        Ok(Self {
            inner: client,
            base_url,
            auth,
            extra_headers,
            retry_policy: None,
            shared_budget: None,
            rate_limit_parser: None,
            last_snapshot: Arc::new(Mutex::new(None)),
        })
    }

    /// Set retry policy for this client
    #[must_use]
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// F40: set the shared retry budget. The agentic loop's
    /// mid-stream retry site passes the same `Arc<SharedRetryBudget>`
    /// so transport + engine draw from one counter (collapsing the
    /// pre-F40 stacked-budget anti-pattern). When no budget is
    /// configured, each layer uses its own ceiling
    /// (`retry_policy.max_retries` for transport,
    /// `stream_max_retries` for engine) — pre-F40 behavior.
    #[must_use]
    pub fn with_shared_budget(mut self, budget: Arc<SharedRetryBudget>) -> Self {
        self.shared_budget = Some(budget);
        self
    }

    /// F40 accessor: the shared budget if one was wired in. Used
    /// by `HttpClient::post_json` / `post_stream` to thread the
    /// budget into `RetryExecutor::execute_with_classifier_and_budget`.
    #[must_use]
    pub fn shared_budget(&self) -> Option<&Arc<SharedRetryBudget>> {
        self.shared_budget.as_ref()
    }

    /// F40a: install a rate-limit parser. When set, `post_json` /
    /// `post_stream` / `get` run the parser over the response
    /// headers + body on every non-success status and stash the
    /// resulting snapshot in `last_snapshot` + embed it in the
    /// error message. The default `None` means "don't parse";
    /// adapters that don't care about rate-limit introspection
    /// keep the pre-F40a path.
    #[must_use]
    pub fn with_rate_limit_parser(
        mut self,
        parser: Arc<dyn peko_provider_api::RateLimitParser>,
    ) -> Self {
        self.rate_limit_parser = Some(parser);
        self
    }

    /// F40a accessor: clone the most-recent rate-limit snapshot.
    /// Returns `None` if no failed call has populated one. The
    /// snapshot is cloned (not moved) so callers don't have to
    /// worry about concurrent updates.
    #[must_use]
    pub fn last_rate_limit_snapshot(&self) -> Option<peko_provider_api::RateLimitSnapshot> {
        self.last_snapshot
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// F40a: process a non-success `reqwest::Response` — pull the
    /// `Retry-After` header (seconds or HTTP-date), run the body
    /// through the body-delay extractor, capture a `RateLimitSnapshot`
    /// via the configured parser, and emit the canonical
    /// `anyhow::Error` with all metadata embedded.
    ///
    /// Centralized so `post_json` / `post_stream` / `get` stay short
    /// and the snapshot-capture policy is uniform across the three.
    async fn process_failed_response(&self, response: reqwest::Response) -> anyhow::Error {
        let status_u16 = response.status().as_u16();
        // Collect headers up-front so we can both parse the snapshot
        // and feed them to a future rate-limit-parser enhancement.
        let header_entries: Vec<peko_provider_api::HeaderEntry> = response
            .headers()
            .iter()
            .map(|(k, v)| peko_provider_api::HeaderEntry::new(k.as_str(), v.to_str().unwrap_or("")))
            .collect();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_retry_after_header);
        let body_text = response.text().await.unwrap_or_default();
        let body_delay = peko_provider_api::extract_body_delay(&body_text);
        // Run the parser once. We then enrich the returned snapshot
        // with the body-delay + retry-after (in case the parser
        // didn't surface them) before stashing and embedding.
        let snapshot_owned = self
            .rate_limit_parser
            .as_ref()
            .and_then(|parser| parser.parse(&header_entries, &body_text, SystemTime::now()))
            .map(|mut s| {
                if s.body_delay.is_none() {
                    s.body_delay = body_delay;
                }
                if s.retry_after.is_none() {
                    s.retry_after = retry_after;
                }
                s
            });
        if let Some(s) = &snapshot_owned {
            if let Ok(mut guard) = self.last_snapshot.lock() {
                *guard = Some(s.clone());
            }
        }
        debug!(
            "HTTP error {}: snapshot_kind={:?}",
            status_u16,
            snapshot_owned.as_ref().map(|s| s.kind)
        );
        classify_http_error(status_u16, body_text, retry_after, snapshot_owned.as_ref())
    }

    /// Build request with authentication headers
    fn build_request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            format!("{}{}", self.base_url, path)
        };

        let mut request = self.inner.request(method, &url);

        // Add authentication
        match &self.auth {
            AuthConfig::Bearer { token } => {
                request = request.header("Authorization", format!("Bearer {token}"));
            }
            AuthConfig::Header { name, value } => {
                request = request.header(name, value);
            }
        }

        // Add extra headers
        for (name, value) in &self.extra_headers {
            request = request.header(name, value);
        }

        request
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
    }

    /// Send a POST request with JSON body and parse JSON response
    ///
    /// `per_request_headers` (F25) carries dynamic headers that depend
    /// on the caller's `ChatOptions` (e.g. `anthropic-beta:
    /// interleaved-thinking-2025-05-08`). They override static
    /// `extra_headers` on name collision.
    pub async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
        per_request_headers: &[(String, String)],
    ) -> anyhow::Result<R> {
        let body_json = serde_json::to_value(body)?;
        let operation = || async {
            let mut request = self
                .build_request(reqwest::Method::POST, path)
                .json(&body_json);
            // F25: per-request headers override the static set.
            for (name, value) in per_request_headers {
                request = request.header(name.as_str(), value.as_str());
            }

            debug!("Sending POST request to {}{}", self.base_url, path);

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                return Err(self.process_failed_response(response).await);
            }

            let result: R = response.json().await?;
            Ok(result)
        };

        match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute_with_classifier_and_budget(
                    policy,
                    &format!("POST {path}"),
                    self.shared_budget.as_deref(),
                    &peko_provider_api::BodyStringClassifier,
                    operation,
                )
                .await
            }
            None => operation().await,
        }
    }

    /// Send a POST request with JSON body and return streaming response
    ///
    /// See `post_json` for the `per_request_headers` parameter.
    pub async fn post_stream(
        &self,
        path: &str,
        body: &impl Serialize,
        per_request_headers: &[(String, String)],
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<Bytes>>> {
        let body_json = serde_json::to_value(body)?;
        let operation = || async {
            let mut request = self
                .build_request(reqwest::Method::POST, path)
                .json(&body_json)
                .header("Accept", "text/event-stream");
            // F25: per-request headers override the static set.
            for (name, value) in per_request_headers {
                request = request.header(name.as_str(), value.as_str());
            }

            debug!(
                "Sending streaming POST request to {}{}",
                self.base_url, path
            );

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                return Err(self.process_failed_response(response).await);
            }

            Ok(response)
        };

        // Retry the initial request if configured
        let response = match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute_with_classifier_and_budget(
                    policy,
                    &format!("POST {path}"),
                    self.shared_budget.as_deref(),
                    &peko_provider_api::BodyStringClassifier,
                    operation,
                )
                .await?
            }
            None => operation().await?,
        };

        // Convert the byte stream to a stream of anyhow::Result<Bytes>
        let stream = response.bytes_stream().map(|result| match result {
            Ok(bytes) => Ok(bytes),
            Err(e) => Err(anyhow::anyhow!("Stream error: {e}")),
        });

        Ok(stream)
    }

    /// Send a simple GET request
    pub async fn get<R: DeserializeOwned>(&self, path: &str) -> anyhow::Result<R> {
        let operation = || async {
            let request = self.build_request(reqwest::Method::GET, path);

            debug!("Sending GET request to {}{}", self.base_url, path);

            let response = request.send().await?;
            let status = response.status();

            if !status.is_success() {
                return Err(self.process_failed_response(response).await);
            }

            let result: R = response.json().await?;
            Ok(result)
        };

        match &self.retry_policy {
            Some(policy) => {
                RetryExecutor::execute_with_classifier_and_budget(
                    policy,
                    &format!("GET {path}"),
                    self.shared_budget.as_deref(),
                    &peko_provider_api::BodyStringClassifier,
                    operation,
                )
                .await
            }
            None => operation().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_config_bearer() {
        let auth = AuthConfig::Bearer {
            token: "test_token".to_string(),
        };
        match auth {
            AuthConfig::Bearer { token } => assert_eq!(token, "test_token"),
            _ => panic!("Expected Bearer auth"),
        }
    }

    #[test]
    fn test_client_creation() {
        let auth = AuthConfig::Bearer {
            token: "test".to_string(),
        };
        let client = HttpClient::new("https://api.example.com", auth, 30);
        assert!(client.is_ok());
    }

    /// F40a: `classify_http_error` embeds both the `Retry-After`
    /// value and the parsed snapshot block, separated by `;` so the
    /// retry executor can recover them via substring scan.
    #[test]
    fn classify_http_error_embeds_retry_after_and_snapshot_tokens() {
        let snap = peko_provider_api::RateLimitSnapshot {
            kind: peko_provider_api::RateLimitKind::Anthropic,
            requests_remaining: Some(2),
            ..Default::default()
        };
        let err = classify_http_error(
            429,
            "rate limit".to_string(),
            Some(Duration::from_secs(7)),
            Some(&snap),
        );
        let msg = err.to_string();
        assert!(msg.contains("retry_after=7s"), "msg: {msg}");
        assert!(msg.contains("rate_limit_kind=Anthropic"), "msg: {msg}");
        assert!(
            msg.contains("rate_limit_requests_remaining=2"),
            "msg: {msg}"
        );
    }

    /// F40a: when the snapshot is `None`, no `rate_limit_*` tokens are
    /// emitted. The Retry-After still propagates.
    #[test]
    fn classify_http_error_omits_snapshot_tokens_when_none() {
        let err = classify_http_error(
            503,
            "backend".to_string(),
            Some(Duration::from_secs(3)),
            None,
        );
        let msg = err.to_string();
        assert!(msg.contains("retry_after=3s"), "msg: {msg}");
        assert!(
            !msg.contains("rate_limit_"),
            "msg should not embed snapshot tokens: {msg}"
        );
    }

    /// F40a: `parse_retry_after_header` accepts integer-seconds form
    /// via the lifted helper (the test ensures the thin wrapper
    /// around `peko_provider_api::parse_retry_after_header` is
    /// wired correctly).
    #[test]
    fn parse_retry_after_header_accepts_integer_seconds() {
        let d = parse_retry_after_header("12").unwrap();
        assert_eq!(d, Duration::from_secs(12));
    }

    // `test_is_context_window_exceeded_*` tests moved to
    // `crates/provider-api/src/context_window_error.rs` (Phase 9b.N.5b.5)
    // alongside the function definition.
}
