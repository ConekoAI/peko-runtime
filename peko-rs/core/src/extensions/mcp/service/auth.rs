//! OAuth credentials and consent are bound to the principal, server, and resource.
use super::{
    config::{Authentication, Transport},
    digest, now, PrincipalMcpRuntime, Server,
};
use crate::{
    common::vault::{Credential, CredentialFilter, CredentialKind, OAuthTokenEntry},
    extensions::mcp::protocol::{config::McpAuthConfig, oauth::OAuthFlow},
};
use anyhow::{bail, ensure, Context, Result};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

impl PrincipalMcpRuntime {
    fn oauth_binding(&self, server: &Server) -> Result<String> {
        Ok(digest(
            &json!({"transport":serde_json::to_value(&server.definition.transport)?,"auth":serde_json::to_value(&server.definition.auth)?}),
        ))
    }
    fn oauth_record(&self, server: &Server) -> Result<Option<Credential>> {
        let vault = self
            .vault
            .as_ref()
            .context("MCP credential vault unavailable")?;
        Ok(vault
            .list_credentials(&CredentialFilter {
                namespace: Some(self.namespace(&server.id)),
                include_system: true,
                ..Default::default()
            })
            .iter()
            .find(|credential| credential.name == "oauth_token")
            .and_then(|credential| vault.get_credential(&credential.id)))
    }
    fn store_token(&self, server: &Server, token: OAuthTokenEntry, client_id: &str) -> Result<()> {
        ensure!(
            server.current.load(Ordering::Acquire) && !self.stopped.is_cancelled(),
            "OAuth configuration changed before completion"
        );
        let vault = self
            .vault
            .as_ref()
            .context("MCP credential vault unavailable")?;
        let old = self.oauth_record(server)?;
        let mut credential = Credential::now(
            self.namespace(&server.id),
            "oauth_token",
            CredentialKind::OAuthToken,
            SecretString::new(token.access_token.into()),
        );
        if let Some(old) = old {
            credential.id = old.id;
        }
        credential.metadata = json!({"refresh_token":token.refresh_token,"expires_at":token.expires_at,"client_id":client_id,"binding":self.oauth_binding(server)?});
        vault.set_credential(&credential)?;
        Ok(())
    }
    pub(super) async fn bearer(&self, server: &Arc<Server>) -> Result<Option<String>> {
        match &server.definition.auth {
            Authentication::None => Ok(None),
            Authentication::Bearer { credential } => Ok(Some(self.secret(&server.id, credential)?)),
            Authentication::OAuth { .. } => {
                let _refresh = server.oauth_refresh.lock().await;
                let record = self
                    .oauth_record(server)?
                    .context("MCP authentication required; use MCP authenticate")?;
                ensure!(
                    record.metadata["binding"] == self.oauth_binding(server)?,
                    "MCP reauthentication required for changed endpoint/resource"
                );
                if record.metadata["expires_at"]
                    .as_i64()
                    .is_some_and(|expires| expires <= now() as i64 + 30)
                {
                    let refresh = record.metadata["refresh_token"]
                        .as_str()
                        .context("MCP authentication expired; use MCP authenticate")?;
                    let (mut config, resource) = self.oauth_config(server, None).await?;
                    config.oauth_client_id =
                        record.metadata["client_id"].as_str().map(str::to_string);
                    let mut token = OAuthFlow::refresh_token_for_resource(
                        &config,
                        refresh,
                        resource.as_deref(),
                    )
                    .await
                    .context("MCP authentication refresh failed")?;
                    if token.refresh_token.is_none() {
                        token.refresh_token = Some(refresh.into());
                    }
                    let bearer = token.access_token.clone();
                    self.store_token(
                        server,
                        token,
                        config
                            .oauth_client_id
                            .as_deref()
                            .context("OAuth client ID missing")?,
                    )?;
                    Ok(Some(bearer))
                } else {
                    Ok(Some(record.material.expose_secret().into()))
                }
            }
        }
    }

    async fn oauth_config(
        &self,
        server: &Server,
        redirect: Option<&str>,
    ) -> Result<(McpAuthConfig, Option<String>)> {
        let Authentication::OAuth {
            client_id,
            authorization_endpoint,
            token_endpoint,
            scopes,
            resource,
        } = &server.definition.auth
        else {
            bail!("server does not configure OAuth");
        };
        let endpoint = match &server.definition.transport {
            Transport::StreamableHttp { url } | Transport::Sse { endpoint: url } => url,
            _ => bail!("OAuth requires HTTP transport"),
        };
        let resource = resource.clone().or_else(|| Some(endpoint.clone()));
        let mut config = McpAuthConfig {
            oauth_client_id: client_id.clone(),
            authorization_endpoint: authorization_endpoint.clone(),
            token_endpoint: token_endpoint.clone(),
            scopes: scopes.clone(),
            ..Default::default()
        };
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut registration = None;
        if config.authorization_endpoint.is_none()
            || config.token_endpoint.is_none()
            || config.oauth_client_id.is_none()
        {
            let endpoint = url::Url::parse(endpoint)?;
            let mut metadata_url = endpoint.clone();
            metadata_url.set_path(&format!(
                "/.well-known/oauth-protected-resource{}",
                endpoint.path().trim_end_matches('/')
            ));
            metadata_url.set_query(None);
            let protected: Value = http
                .get(metadata_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            ensure!(
                protected["resource"].as_str() == resource.as_deref(),
                "OAuth protected resource metadata disagrees with configured resource"
            );
            let issuer = protected["authorization_servers"]
                .as_array()
                .and_then(|servers| servers.first())
                .and_then(Value::as_str)
                .context("OAuth metadata missing authorization server")?;
            let mut metadata_url = url::Url::parse(issuer)?;
            let suffix = metadata_url.path().trim_end_matches('/').to_string();
            metadata_url.set_path(&format!("/.well-known/oauth-authorization-server{suffix}"));
            metadata_url.set_query(None);
            let metadata: Value = http
                .get(metadata_url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            ensure!(
                metadata["issuer"].as_str() == Some(issuer),
                "OAuth issuer metadata mismatch"
            );
            config.authorization_endpoint = config.authorization_endpoint.or_else(|| {
                metadata["authorization_endpoint"]
                    .as_str()
                    .map(str::to_string)
            });
            config.token_endpoint = config
                .token_endpoint
                .or_else(|| metadata["token_endpoint"].as_str().map(str::to_string));
            registration = metadata["registration_endpoint"]
                .as_str()
                .map(str::to_string);
        }
        if config.oauth_client_id.is_none() {
            if let Some(redirect) = redirect {
                let registration = registration.context(
                    "OAuth requires a configured client_id or dynamic registration endpoint",
                )?;
                let response: Value = http.post(registration).json(&json!({"client_name":"Peko","redirect_uris":[redirect],"grant_types":["authorization_code","refresh_token"],"response_types":["code"],"token_endpoint_auth_method":"none"})).send().await?.error_for_status()?.json().await?;
                config.oauth_client_id = Some(
                    response["client_id"]
                        .as_str()
                        .context("OAuth registration returned no client_id")?
                        .into(),
                );
            }
        }
        for endpoint in [&config.authorization_endpoint, &config.token_endpoint] {
            let url = url::Url::parse(endpoint.as_deref().context("OAuth endpoint missing")?)?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.username().is_empty()
                    && url.password().is_none(),
                "invalid OAuth endpoint"
            );
        }
        Ok((config, resource))
    }

    pub(super) async fn authenticate(self: &Arc<Self>, server: Arc<Server>) -> Result<Value> {
        ensure!(
            matches!(server.definition.auth, Authentication::OAuth { .. }),
            "server does not configure OAuth"
        );
        ensure!(self.vault.is_some(), "MCP credential vault unavailable");
        let mut active = self.authenticating.lock().await;
        ensure!(
            !active.contains(&server.id),
            "OAuth authorization already in progress; inspect MCP status"
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr()?.port()
        );
        let (config, resource) = tokio::time::timeout(
            Duration::from_secs(30),
            self.oauth_config(&server, Some(&redirect)),
        )
        .await
        .context("OAuth metadata discovery timed out")??;
        let (verifier, challenge) = OAuthFlow::generate_pkce();
        let state = uuid::Uuid::new_v4().to_string();
        let mut url = url::Url::parse(
            config
                .authorization_endpoint
                .as_deref()
                .context("OAuth authorization endpoint missing")?,
        )?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair(
                    "client_id",
                    config
                        .oauth_client_id
                        .as_deref()
                        .context("OAuth client ID missing")?,
                )
                .append_pair("redirect_uri", &redirect)
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256")
                .append_pair("state", &state);
            if !config.scopes.is_empty() {
                query.append_pair("scope", &config.scopes.join(" "));
            }
            if let Some(resource) = &resource {
                query.append_pair("resource", resource);
            }
        }
        active.insert(server.id.clone());
        drop(active);
        let runtime = self.clone();
        let id = server.id.clone();
        let response_id = id.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                () = runtime.stopped.cancelled() => Err(anyhow::anyhow!("principal stopped during authentication")),
                () = server.revoked.cancelled() => Err(anyhow::anyhow!("server changed during authentication")),
                result = tokio::time::timeout(Duration::from_mins(5),async {
                    let code = OAuthFlow::wait_for_callback(listener,&state).await?;
                    let token = OAuthFlow::authorize_with_code_for_resource(&config,&code,&verifier,&redirect,resource.as_deref()).await?;
                    runtime.store_token(&server,token,config.oauth_client_id.as_deref().context("OAuth client ID missing")?)
                }) => result.context("OAuth authorization timed out").and_then(|result|result),
            };
            if let Err(error) = result {
                *server.diagnostic.write().await =
                    Some(runtime.sanitize(&id, format!("{error:#}")));
            }
            runtime.authenticating.lock().await.remove(&id);
        });
        Ok(
            json!({"server":response_id,"state":"authentication_pending","authorization_url":url.as_str()}),
        )
    }
}
