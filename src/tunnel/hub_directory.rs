//! Hub agent-directory HTTP client — Slice B of issue #29.
//!
//! Resolves a `TargetSpec` (`RemoteByDid` / `RemoteByHandle`) into a
//! concrete `{ runtime_id, instance_id, agent_did, owner_principal,
//! exposure }` host before the outbound a2a path can sign and dispatch
//! over the tunnel. The hub-side surface is shipped via pekohub#14
//! (merged Jun 19 2026, commit `2995164`):
//!
//!   * `GET /v1/agents/by-did/:did`
//!   * `GET /v1/agents/by-handle/:owner/:agent_name`
//!
//! ## Authentication
//!
//! Slice B's v1 hits the hub **anonymously**, which limits cross-runtime
//! a2a to agents with `exposure: "public"` — the hub's
//! `principalCanAccess` short-circuits on public exposure but otherwise
//! requires a `Principal` to gate. There is no shared HTTP credential
//! between the runtime and the hub today: the runtime authenticates
//! over the WebSocket tunnel via Ed25519 + nonce challenge, not over
//! HTTP. Slice B documents this and surfaces a clean `Forbidden` error
//! when a private-exposure target is resolved anonymously, so a
//! follow-up (cross-repo, gated on a runtime-attested JWT — see
//! pekohub PR #15's discussion) can lift the limitation without
//! changing this client's call sites.
//!
//! ## Abstraction
//!
//! The [`AgentDirectory`] trait keeps the outbound a2a wiring
//! independent of `reqwest`: production wires [`HubAgentDirectoryClient`],
//! tests and the Slice E E2E harness wire [`FakeAgentDirectory`] or an
//! in-process server. The trait is small on purpose — adding methods
//! is a follow-up, not an upfront design (YAGNI; the existing surface
//! is exactly what the outbound `a2a_send` path needs).

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::Deserialize;
use std::time::Duration;
use thiserror::Error;

use crate::auth::principal::Principal;

/// Hit payload returned by the hub's `/v1/agents/by-did/:did` and
/// `/v1/agents/by-handle/:owner/:agent_name` endpoints.
///
/// Field names match pekohub's `AgentTargetResolution` (camelCase on the
/// wire); see `pekohub/backend/src/services/instances.ts:679-690` for
/// the source of truth. The pekohub merge commit is `2995164`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResolution {
    /// did:key form of the runtime hosting the target. Used by the
    /// outbound a2a path to address the right tunnel.
    pub runtime_id: String,
    /// Hub-side instance row id. Useful for audit correlation
    /// (`AuditEvent.target_instance_id`) but not strictly required to
    /// dispatch.
    pub instance_id: String,
    /// Per-agent stable DID. Pekohub returns the empty string for the
    /// by-handle path when the target row predates the `agent_did`
    /// column (pre-#34 runtime); the by-did path never returns empty
    /// because the lookup key is the DID. Callers MUST treat
    /// empty-string as "no DID known".
    pub agent_did: String,
    /// Resolved owner principal (User / Agent / Team / Public). The
    /// outbound path doesn't currently consume this, but it's part of
    /// the response contract (the hub mirrors it for client-side
    /// trust display) and audit code wants it.
    pub owner_principal: Principal,
    /// Visibility of the target instance. Drives the local-side check
    /// before issuing the outbound a2a (an unexposed agent shouldn't
    /// be addressable even if the directory leaks it).
    pub exposure: ResolvedExposure,
}

/// Mirror of pekohub's `instance.exposure` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedExposure {
    Public,
    Private,
    Unexposed,
}

/// Errors the directory client surfaces. The variants are deliberately
/// shaped around the hub's HTTP semantics — `NotFound` for 404,
/// `Forbidden` for 403 — so the outbound a2a path can branch on them
/// directly and bubble structured errors back to the calling agent
/// rather than a generic "remote a2a failed" string.
#[derive(Debug, Error)]
pub enum DirectoryError {
    /// 404 from the hub: the target spec resolves to nothing.
    #[error("agent directory: target not found")]
    NotFound,
    /// 403 from the hub: the caller (anonymous today, Agent-attested
    /// later) does not pass `principalCanAccess` on the target.
    #[error("agent directory: caller not permitted to resolve target")]
    Forbidden,
    /// 400 from the hub: malformed target spec. This indicates a bug
    /// in the runtime client, not user input — pin it as a distinct
    /// variant so it's loud in logs.
    #[error("agent directory: hub rejected the request as malformed: {0}")]
    BadRequest(String),
    /// 5xx, network failure, body decode failure, etc. Whatever the
    /// underlying cause, the outbound a2a path will retry (Slice B
    /// adds the retry policy in the call-site, not the client) and
    /// then surface a structured "directory unavailable" error to
    /// the caller.
    #[error("agent directory: transport or decode error: {0}")]
    Transport(String),
}

/// The minimal interface the outbound a2a path needs. Hold this trait
/// behind `Arc<dyn AgentDirectory>` (it's `Send + Sync`) so the same
/// `A2aSendTool` instance can be cheaply cloned across the tool
/// registry without re-allocating the underlying HTTP client.
#[async_trait]
pub trait AgentDirectory: Send + Sync {
    /// Resolve a `did:peko:agent:...` to its host.
    async fn resolve_by_did(&self, did: &str) -> Result<AgentResolution, DirectoryError>;

    /// Resolve a `{owner, agent_name}` handle to its host. The `owner`
    /// segment is a user namespace today; team-handle resolution is
    /// gated on pekohub#8.
    async fn resolve_by_handle(
        &self,
        owner: &str,
        agent_name: &str,
    ) -> Result<AgentResolution, DirectoryError>;
}

/// Default HTTP client implementation. Hits the hub directly via
/// `reqwest`. Timeouts and retries are policy-layered — this client
/// is a thin transport, the caller adds the policy.
pub struct HubAgentDirectoryClient {
    /// Base URL of the hub's HTTP surface, e.g. `https://pekohub.org`.
    /// Constructed from the `wss://...` URL in `PekoHubCredential`
    /// (see `From<&PekoHubCredential>`).
    base_url: String,
    http: reqwest::Client,
}

impl HubAgentDirectoryClient {
    /// Build a client bound to an explicit base URL. Used by tests
    /// that want a stubbed host; production callers prefer
    /// `from_credential`.
    ///
    /// `base_url` should be the HTTPS base, e.g. `https://pekohub.org`,
    /// not the WebSocket URL. The two endpoints are appended as
    /// `/v1/agents/by-did/...` / `/v1/agents/by-handle/...`.
    #[must_use]
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            // Reasonable default; the call-site adds per-request
            // timeouts via the outbound a2a policy layer.
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client builder must not fail with default settings"),
        }
    }

    /// Build a client from the runtime's stored PekoHub credential by
    /// flipping `ws[s]://...` → `http[s]://...` and stripping any path
    /// (the tunnel URL is `wss://host/v1/tunnel`; the HTTP base is
    /// just `https://host`).
    ///
    /// # Errors
    ///
    /// Returns a `DirectoryError::Transport` if the credential URL
    /// doesn't parse as a valid `ws://` / `wss://` URL.
    pub fn from_credential(
        cred: &crate::tunnel::PekoHubCredential,
    ) -> Result<Self, DirectoryError> {
        let parsed = reqwest::Url::parse(&cred.url)
            .map_err(|e| DirectoryError::Transport(format!("PekoHub URL is not a valid URL: {e}")))?;
        let scheme = match parsed.scheme() {
            "wss" => "https",
            "ws" => "http",
            // If the runtime is ever configured with a literal HTTP
            // URL (e.g. an integration test), just take it verbatim.
            other if other == "http" || other == "https" => other,
            other => {
                return Err(DirectoryError::Transport(format!(
                    "PekoHub URL has unsupported scheme `{other}://`"
                )));
            }
        };
        let host = parsed
            .host_str()
            .ok_or_else(|| DirectoryError::Transport("PekoHub URL has no host".to_string()))?;
        let port = parsed
            .port()
            .map_or_else(String::new, |p| format!(":{p}"));
        let base_url = format!("{scheme}://{host}{port}");
        Ok(Self::new(base_url))
    }

    /// Map an HTTP response status + body into our `DirectoryError`
    /// shape. Factored out so the by-did and by-handle paths share
    /// the contract.
    async fn handle_response(
        resp: reqwest::Response,
    ) -> Result<AgentResolution, DirectoryError> {
        let status = resp.status();
        match status {
            StatusCode::OK => resp
                .json::<AgentResolution>()
                .await
                .map_err(|e| DirectoryError::Transport(format!("response body decode failed: {e}"))),
            StatusCode::NOT_FOUND => Err(DirectoryError::NotFound),
            StatusCode::FORBIDDEN => Err(DirectoryError::Forbidden),
            StatusCode::BAD_REQUEST => {
                let body = resp.text().await.unwrap_or_default();
                Err(DirectoryError::BadRequest(body))
            }
            other => {
                let body = resp.text().await.unwrap_or_default();
                Err(DirectoryError::Transport(format!(
                    "hub returned HTTP {other}: {body}"
                )))
            }
        }
    }
}

#[async_trait]
impl AgentDirectory for HubAgentDirectoryClient {
    async fn resolve_by_did(&self, did: &str) -> Result<AgentResolution, DirectoryError> {
        // The hub regex accepts `[\w:.\-]{1,512}` for the :did segment;
        // bad shapes round-trip to 400. We do NOT pre-validate here:
        // surfacing the hub's 400 unmodified gives the calling agent
        // a single source of truth for "what's a valid DID".
        let url = format!("{base}/v1/agents/by-did/{did}", base = self.base_url);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DirectoryError::Transport(format!("GET {url} failed: {e}")))?;
        Self::handle_response(resp).await
    }

    async fn resolve_by_handle(
        &self,
        owner: &str,
        agent_name: &str,
    ) -> Result<AgentResolution, DirectoryError> {
        // No client-side encoding — the hub's path regex pins both
        // segments and round-trips bad shapes as 400. Keeping the
        // client thin means the test surface is the contract.
        let url = format!(
            "{base}/v1/agents/by-handle/{owner}/{agent_name}",
            base = self.base_url
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| DirectoryError::Transport(format!("GET {url} failed: {e}")))?;
        Self::handle_response(resp).await
    }
}

/// Test-only fake. Holds canned responses by `(kind, key)` and
/// returns them verbatim. Pulled out into `tunnel::hub_directory`
/// rather than a separate `#[cfg(test)]` mod so the Slice E E2E
/// test (in `tests/`) can reuse the same fake without re-declaring
/// it.
#[cfg(any(test, feature = "test-utils"))]
pub mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory directory used by unit tests and the Slice E
    /// E2E harness. `register_*` populates the response table;
    /// `resolve_*` returns either the registered value or
    /// `NotFound`.
    pub struct FakeAgentDirectory {
        by_did: Mutex<HashMap<String, Result<AgentResolution, DirectoryErrorKind>>>,
        by_handle: Mutex<HashMap<(String, String), Result<AgentResolution, DirectoryErrorKind>>>,
    }

    /// Cloneable error tag for the fake. The real `DirectoryError`
    /// has non-`Clone` `Box<dyn Error>`-style variants
    /// (`#[from] reqwest::Error` etc.), so the fake stores a
    /// simpler enum and converts on resolve.
    #[derive(Debug, Clone)]
    pub enum DirectoryErrorKind {
        NotFound,
        Forbidden,
        BadRequest(String),
        Transport(String),
    }

    impl From<DirectoryErrorKind> for DirectoryError {
        fn from(k: DirectoryErrorKind) -> Self {
            match k {
                DirectoryErrorKind::NotFound => DirectoryError::NotFound,
                DirectoryErrorKind::Forbidden => DirectoryError::Forbidden,
                DirectoryErrorKind::BadRequest(s) => DirectoryError::BadRequest(s),
                DirectoryErrorKind::Transport(s) => DirectoryError::Transport(s),
            }
        }
    }

    impl FakeAgentDirectory {
        #[must_use]
        pub fn new() -> Self {
            Self {
                by_did: Mutex::new(HashMap::new()),
                by_handle: Mutex::new(HashMap::new()),
            }
        }

        pub fn register_did(&self, did: impl Into<String>, resolution: AgentResolution) {
            self.by_did.lock().unwrap().insert(did.into(), Ok(resolution));
        }

        pub fn register_did_err(&self, did: impl Into<String>, err: DirectoryErrorKind) {
            self.by_did.lock().unwrap().insert(did.into(), Err(err));
        }

        pub fn register_handle(
            &self,
            owner: impl Into<String>,
            name: impl Into<String>,
            resolution: AgentResolution,
        ) {
            self.by_handle
                .lock()
                .unwrap()
                .insert((owner.into(), name.into()), Ok(resolution));
        }

        pub fn register_handle_err(
            &self,
            owner: impl Into<String>,
            name: impl Into<String>,
            err: DirectoryErrorKind,
        ) {
            self.by_handle
                .lock()
                .unwrap()
                .insert((owner.into(), name.into()), Err(err));
        }
    }

    impl Default for FakeAgentDirectory {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl AgentDirectory for FakeAgentDirectory {
        async fn resolve_by_did(&self, did: &str) -> Result<AgentResolution, DirectoryError> {
            self.by_did
                .lock()
                .unwrap()
                .get(did)
                .cloned()
                .ok_or(DirectoryError::NotFound)?
                .map_err(Into::into)
        }

        async fn resolve_by_handle(
            &self,
            owner: &str,
            agent_name: &str,
        ) -> Result<AgentResolution, DirectoryError> {
            self.by_handle
                .lock()
                .unwrap()
                .get(&(owner.to_string(), agent_name.to_string()))
                .cloned()
                .ok_or(DirectoryError::NotFound)?
                .map_err(Into::into)
        }
    }
}

// Re-export the fake under the parent module path so callers don't
// have to type `hub_directory::fake::FakeAgentDirectory`. Gated on the
// same `cfg` as the inner module so it's a no-op in production builds.
#[cfg(any(test, feature = "test-utils"))]
pub use fake::{DirectoryErrorKind, FakeAgentDirectory};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_resolution() -> AgentResolution {
        AgentResolution {
            runtime_id: "did:key:zHostingRuntime".to_string(),
            instance_id: "inst-abc-123".to_string(),
            agent_did: "did:peko:agent:target-keyhash".to_string(),
            owner_principal: Principal::User("alice".to_string()),
            exposure: ResolvedExposure::Public,
        }
    }

    /// `AgentResolution` decodes from the exact JSON pekohub returns
    /// on a hit. The shape is pinned by pekohub's
    /// `AgentTargetResolution` in `backend/src/services/instances.ts`;
    /// this test catches the case where the hub team renames a field
    /// (e.g. `ownerPrincipal` → `owner`) and the runtime decoder
    /// silently starts erroring.
    #[test]
    fn test_agent_resolution_decodes_pekohub_payload() {
        let body = r#"{
            "runtimeId": "did:key:zHostingRuntime",
            "instanceId": "inst-abc-123",
            "agentDid": "did:peko:agent:target-keyhash",
            "ownerPrincipal": { "kind": "user", "id": "alice" },
            "exposure": "public"
        }"#;
        let decoded: AgentResolution = serde_json::from_str(body).unwrap();
        assert_eq!(decoded, sample_resolution());
    }

    /// `Principal::Agent` and `Principal::Public` also decode — the
    /// hub returns these on the by-did path for agent-owned or
    /// publicly-owned instances respectively.
    #[test]
    fn test_agent_resolution_decodes_each_principal_kind() {
        for (kind_json, expected) in [
            (
                r#"{ "kind": "user", "id": "alice" }"#,
                Principal::User("alice".to_string()),
            ),
            (
                r#"{ "kind": "agent", "id": "did:peko:agent:abc" }"#,
                Principal::Agent("did:peko:agent:abc".to_string()),
            ),
            (
                r#"{ "kind": "team", "id": "eng" }"#,
                Principal::Team("eng".to_string()),
            ),
            (r#"{ "kind": "public" }"#, Principal::Public),
        ] {
            let body = format!(
                r#"{{
                    "runtimeId": "r",
                    "instanceId": "i",
                    "agentDid": "did:peko:agent:x",
                    "ownerPrincipal": {kind_json},
                    "exposure": "private"
                }}"#
            );
            let decoded: AgentResolution =
                serde_json::from_str(&body).expect("must decode every Principal kind");
            assert_eq!(decoded.owner_principal, expected);
        }
    }

    /// Each exposure value (`public` / `private` / `unexposed`)
    /// decodes to the right variant. Pekohub returns these in the
    /// `exposure` field; the outbound a2a path branches on them.
    #[test]
    fn test_resolved_exposure_decodes_each_variant() {
        for (json, expected) in [
            ("\"public\"", ResolvedExposure::Public),
            ("\"private\"", ResolvedExposure::Private),
            ("\"unexposed\"", ResolvedExposure::Unexposed),
        ] {
            let decoded: ResolvedExposure = serde_json::from_str(json).unwrap();
            assert_eq!(decoded, expected);
        }
    }

    /// `from_credential` accepts the runtime's `wss://...` tunnel URL
    /// and produces the matching HTTPS base. This is the production
    /// path — if it regresses, the runtime falls back to `localhost`
    /// or worse.
    #[test]
    fn test_from_credential_flips_wss_to_https() {
        let cred = crate::tunnel::PekoHubCredential {
            url: "wss://pekohub.org/v1/tunnel".to_string(),
            runtime_id: "did:key:zRuntime".to_string(),
            keyring_entry: None,
            private_key: None,
        };
        let client = HubAgentDirectoryClient::from_credential(&cred).unwrap();
        assert_eq!(client.base_url, "https://pekohub.org");
    }

    #[test]
    fn test_from_credential_flips_ws_to_http_and_preserves_port() {
        let cred = crate::tunnel::PekoHubCredential {
            url: "ws://localhost:4000/v1/tunnel".to_string(),
            runtime_id: "did:key:zRuntime".to_string(),
            keyring_entry: None,
            private_key: None,
        };
        let client = HubAgentDirectoryClient::from_credential(&cred).unwrap();
        assert_eq!(client.base_url, "http://localhost:4000");
    }

    #[test]
    fn test_from_credential_rejects_unsupported_scheme() {
        let cred = crate::tunnel::PekoHubCredential {
            url: "ftp://pekohub.org/".to_string(),
            runtime_id: "did:key:zRuntime".to_string(),
            keyring_entry: None,
            private_key: None,
        };
        assert!(matches!(
            HubAgentDirectoryClient::from_credential(&cred),
            Err(DirectoryError::Transport(_))
        ));
    }

    /// `FakeAgentDirectory` round-trips a registered hit. Pins the
    /// fake's contract so Slice B's outbound-path tests can rely on
    /// it.
    #[tokio::test]
    async fn test_fake_directory_returns_registered_hit() {
        let fake = FakeAgentDirectory::new();
        fake.register_did("did:peko:agent:x", sample_resolution());

        let res = fake.resolve_by_did("did:peko:agent:x").await.unwrap();
        assert_eq!(res, sample_resolution());
    }

    /// Unregistered targets return `NotFound`, mirroring the hub's
    /// 404.
    #[tokio::test]
    async fn test_fake_directory_unknown_did_returns_not_found() {
        let fake = FakeAgentDirectory::new();
        assert!(matches!(
            fake.resolve_by_did("did:peko:agent:nope").await,
            Err(DirectoryError::NotFound)
        ));
    }

    /// `register_did_err` lets tests inject the denied / bad-request
    /// / transport branches.
    #[tokio::test]
    async fn test_fake_directory_propagates_registered_error_kind() {
        let fake = FakeAgentDirectory::new();
        fake.register_did_err("did:peko:agent:locked", DirectoryErrorKind::Forbidden);
        fake.register_did_err(
            "did:peko:agent:malformed",
            DirectoryErrorKind::BadRequest("bad shape".to_string()),
        );

        assert!(matches!(
            fake.resolve_by_did("did:peko:agent:locked").await,
            Err(DirectoryError::Forbidden)
        ));
        let err = fake
            .resolve_by_did("did:peko:agent:malformed")
            .await
            .unwrap_err();
        match err {
            DirectoryError::BadRequest(body) => assert_eq!(body, "bad shape"),
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    /// `register_handle` populates the by-handle path.
    #[tokio::test]
    async fn test_fake_directory_resolves_by_handle() {
        let fake = FakeAgentDirectory::new();
        fake.register_handle("alice", "helper", sample_resolution());

        let res = fake.resolve_by_handle("alice", "helper").await.unwrap();
        assert_eq!(res, sample_resolution());
    }
}
