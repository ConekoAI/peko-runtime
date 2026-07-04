//! OAuth PKCE flow for remote SSE MCP servers.
//!
//! Implements the authorization-code grant with PKCE so a user can authenticate
//! once with a remote MCP vendor. Tokens are stored in the encrypted vault and
//! refreshed automatically by `SseTransport` when requests return 401.

use crate::common::vault::OAuthTokenEntry;
use crate::extensions::mcp::protocol::config::McpAuthConfig;
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{info, warn};

/// OAuth helper functions.
pub struct OAuthFlow;

impl OAuthFlow {
    /// Generate a PKCE verifier/challenge pair.
    ///
    /// Returns `(verifier, challenge)` where the challenge is the base64url
    /// SHA-256 hash of the verifier.
    #[must_use]
    pub fn generate_pkce() -> (String, String) {
        let mut verifier_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);

        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());

        (verifier, challenge)
    }

    /// Run the full PKCE authorization flow for the given server.
    ///
    /// 1. Spawns a temporary localhost callback listener.
    /// 2. Prints/opens the authorization URL.
    /// 3. Waits for the browser to redirect back with the authorization code.
    /// 4. Exchanges the code for tokens at `token_endpoint`.
    pub async fn authorize(config: &McpAuthConfig, server_name: &str) -> Result<OAuthTokenEntry> {
        let auth_endpoint = config
            .authorization_endpoint
            .as_ref()
            .ok_or_else(|| anyhow!("Missing authorization_endpoint"))?;
        let client_id = config
            .oauth_client_id
            .as_ref()
            .ok_or_else(|| anyhow!("Missing oauth_client_id"))?;

        let (verifier, challenge) = Self::generate_pkce();
        let state = Self::random_state();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind OAuth callback listener")?;
        let port = listener
            .local_addr()
            .context("failed to get callback listener address")?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");

        let scope = if config.scopes.is_empty() {
            None
        } else {
            Some(config.scopes.join(" "))
        };

        let mut url = url::Url::parse(auth_endpoint)
            .with_context(|| format!("invalid authorization endpoint: {auth_endpoint}"))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", client_id);
            query.append_pair("redirect_uri", &redirect_uri);
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("state", &state);
            if let Some(ref scope) = scope {
                query.append_pair("scope", scope);
            }
        }

        info!(
            "Please authenticate '{}' by visiting:\n{}",
            server_name, url
        );
        if let Err(e) = Self::open_browser(url.as_str()) {
            warn!("Could not open browser automatically: {}", e);
        }

        // Accept the callback request and extract the authorization code.
        let code = Self::wait_for_callback(listener, &state)
            .await
            .context("OAuth callback failed")?;

        Self::authorize_with_code(config, &code, &verifier, &redirect_uri).await
    }

    /// Exchange an authorization code (plus PKCE verifier) for tokens.
    pub async fn authorize_with_code(
        config: &McpAuthConfig,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokenEntry> {
        let token_endpoint = config
            .token_endpoint
            .as_ref()
            .ok_or_else(|| anyhow!("Missing token_endpoint"))?;
        let client_id = config
            .oauth_client_id
            .as_ref()
            .ok_or_else(|| anyhow!("Missing oauth_client_id"))?;

        let mut params = HashMap::new();
        params.insert("grant_type", "authorization_code");
        params.insert("code", code);
        params.insert("redirect_uri", redirect_uri);
        params.insert("client_id", client_id);
        params.insert("code_verifier", verifier);

        Self::post_token_request(token_endpoint, params, config).await
    }

    /// Refresh an access token using a refresh token.
    pub async fn refresh_token(
        config: &McpAuthConfig,
        refresh_token: &str,
    ) -> Result<OAuthTokenEntry> {
        let token_endpoint = config
            .token_endpoint
            .as_ref()
            .ok_or_else(|| anyhow!("Missing token_endpoint"))?;
        let client_id = config
            .oauth_client_id
            .as_ref()
            .ok_or_else(|| anyhow!("Missing oauth_client_id"))?;

        let mut params = HashMap::new();
        params.insert("grant_type", "refresh_token");
        params.insert("refresh_token", refresh_token);
        params.insert("client_id", client_id);

        Self::post_token_request(token_endpoint, params, config).await
    }

    /// POST to the token endpoint and parse the response into an entry.
    async fn post_token_request(
        token_endpoint: &str,
        params: HashMap<&str, &str>,
        config: &McpAuthConfig,
    ) -> Result<OAuthTokenEntry> {
        let client = reqwest::Client::new();
        let response = client
            .post(token_endpoint)
            .form(&params)
            .send()
            .await
            .with_context(|| format!("token endpoint request failed: {token_endpoint}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("token endpoint error: {}", body));
        }

        let token: TokenResponse = response
            .json()
            .await
            .context("failed to parse token response")?;

        let expires_at = token.expires_in.and_then(|secs| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs() as i64 + secs)
        });

        Ok(OAuthTokenEntry {
            server: config.oauth_client_id.clone().unwrap_or_default(),
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at,
        })
    }

    /// Wait for a single HTTP callback request and validate the state parameter.
    async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String> {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("failed to accept OAuth callback")?;

        let mut buf = [0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .context("failed to read OAuth callback request")?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line for the query string.
        let request_line = request.lines().next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/callback");

        let response_html = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<html><body><h1>Authentication complete</h1><p>You can close this window.</p></body></html>";
        let _ = stream.write_all(response_html).await;

        let query = path.splitn(2, '?').nth(1).unwrap_or("");
        let params: HashMap<String, String> = query
            .split('&')
            .filter_map(|pair| {
                let mut it = pair.splitn(2, '=');
                Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
            })
            .collect();

        if params.get("state").map(String::as_str) != Some(expected_state) {
            return Err(anyhow!("OAuth callback state mismatch"));
        }

        params
            .get("code")
            .cloned()
            .ok_or_else(|| anyhow!("OAuth callback missing code"))
    }

    /// Best-effort attempt to open the user's browser.
    fn open_browser(url: &str) -> std::io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("open").arg(url).spawn()?;
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("xdg-open").arg(url).spawn()?;
        }
        #[cfg(target_os = "windows")]
        {
            std::process::Command::new("cmd")
                .args([&"/c", &"start", &"\"\"", url])
                .spawn()?;
        }
        Ok(())
    }

    fn random_state() -> String {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pkce_verifier_format() {
        let (verifier, challenge) = OAuthFlow::generate_pkce();
        assert!(!verifier.is_empty());
        assert!(!challenge.is_empty());
        assert_ne!(verifier, challenge);

        // Verify challenge is SHA-256 of verifier.
        let mut hasher = Sha256::new();
        hasher.update(verifier.as_bytes());
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hasher.finalize());
        assert_eq!(challenge, expected);
    }

    #[tokio::test]
    async fn test_refresh_token_request_body() {
        // Spin up a tiny HTTP server that captures the form body.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();

            let response = br#"HTTP/1.1 200 OK
Content-Type: application/json
Connection: close

{"access_token":"new-token","refresh_token":"new-refresh","expires_in":3600}"#;
            stream.write_all(response).await.unwrap();
            request
        });

        let config = McpAuthConfig {
            bearer_token: None,
            oauth_client_id: Some("client".to_string()),
            authorization_endpoint: None,
            token_endpoint: Some(format!("http://127.0.0.1:{port}/token")),
            scopes: vec![],
            headers: HashMap::new(),
        };

        let entry = OAuthFlow::refresh_token(&config, "old-refresh")
            .await
            .unwrap();

        let request = server.await.unwrap();
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("refresh_token=old-refresh"));
        assert!(request.contains("client_id=client"));
        assert_eq!(entry.access_token, "new-token");
        assert_eq!(entry.refresh_token, Some("new-refresh".to_string()));
        assert!(entry.expires_at.is_some());
    }
}
