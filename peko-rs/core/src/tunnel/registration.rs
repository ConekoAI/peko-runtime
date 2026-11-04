//! Registering this runtime with PekoHub (ADR-081 D2, D3).
//!
//! A runtime works fully without Hub. Registration links three things to
//! one Hub account at once: the runtime itself, known to Hub by its one
//! identity (`runtime/identity.toml`, vault-held `did:key`), and the local
//! user, whose Hub alias becomes that account. Registration always proves
//! possession of the runtime key; there is no unproven fallback.

use std::sync::Arc;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use peko_channel::{ChannelConfig, ChannelStore};

use crate::common::paths::PathResolver;
use crate::common::vault::Vault;
use crate::tunnel::PekoHubCredential;

/// A completed registration.
#[derive(Debug, Clone)]
pub struct RuntimeRegistration {
    /// The Hub HTTP origin.
    pub origin: String,
    /// The runtime identity DID Hub knows this runtime by.
    pub runtime_did: String,
    /// The Hub account the runtime and its local user are registered under.
    pub account: String,
}

/// Normalize a user-typed Hub location into its HTTP(S) origin.
pub fn hub_http_origin(host: &str) -> Result<String> {
    let url = reqwest::Url::parse(&if host.contains("://") {
        host.to_string()
    } else {
        format!("https://{host}")
    })?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "Use a PekoHub HTTP(S) origin"
    );
    Ok(url.origin().ascii_serialization())
}

/// The tunnel URL for a Hub origin.
#[must_use]
pub fn tunnel_url(origin: &str) -> String {
    let ws = origin
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{ws}/v1/tunnel")
}

/// A sign-in waiting for the user to approve it on the website (RFC 8628).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DeviceSignIn {
    device_code: String,
    /// The code the user confirms, `ABCD-EFGH`.
    pub user_code: String,
    /// Where to approve it (the website's `/device`).
    pub verification_uri: String,
    /// The same page with the code filled in.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires.
    pub expires_in: u64,
    #[serde(default = "default_poll_interval")]
    interval: u64,
}

fn default_poll_interval() -> u64 {
    5
}

/// The OAuth client the CLI signs in as (pekohub ADR-007).
const CLI_CLIENT_ID: &str = "peko-cli";

/// Start signing the CLI in to the Hub at `origin`: the user approves the
/// returned code on the website, while [`finish_device_sign_in`] waits.
///
/// # Errors
/// When the Hub can't be reached or refuses the request.
pub async fn start_device_sign_in(origin: &str) -> Result<DeviceSignIn> {
    let origin = crate::tunnel::credential::official_hub_url(&hub_http_origin(origin)?);
    let response = reqwest::Client::new()
        .post(format!("{origin}/oauth/device/code"))
        .form(&[("client_id", CLI_CLIENT_ID)])
        .send()
        .await
        .context("could not reach PekoHub")?;
    let status = response.status();
    anyhow::ensure!(
        status.is_success(),
        "PekoHub refused to start sign-in: HTTP {status}"
    );
    response
        .json()
        .await
        .context("PekoHub sent an unreadable sign-in code")
}

/// Wait until the user approves (or denies) `sign_in`, polling at the
/// Hub's pace. Returns an access token for the account that approved.
///
/// # Errors
/// When the user denies it, the code expires, or the Hub fails.
pub async fn finish_device_sign_in(origin: &str, sign_in: &DeviceSignIn) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct TokenAnswer {
        access_token: Option<String>,
        error: Option<String>,
    }
    let origin = crate::tunnel::credential::official_hub_url(&hub_http_origin(origin)?);
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(sign_in.expires_in);
    let mut interval = sign_in.interval.max(1);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "The sign-in code expired. Run the command again for a new one."
        );
        let answer: TokenAnswer = client
            .post(format!("{origin}/oauth/token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", CLI_CLIENT_ID),
                ("device_code", sign_in.device_code.as_str()),
            ])
            .send()
            .await
            .context("could not reach PekoHub")?
            .json()
            .await
            .context("PekoHub sent an unreadable answer")?;
        match (answer.access_token, answer.error.as_deref()) {
            (Some(token), _) => return Ok(token),
            (None, Some("authorization_pending")) => {}
            // RFC 8628 §3.5: back off by five seconds.
            (None, Some("slow_down")) => interval += 5,
            (None, Some("access_denied")) => anyhow::bail!("Sign-in was denied on PekoHub."),
            (None, Some("expired_token")) => {
                anyhow::bail!("The sign-in code expired. Run the command again for a new one.")
            }
            (None, other) => anyhow::bail!(
                "PekoHub refused the sign-in: {}",
                other.unwrap_or("no reason given")
            ),
        }
    }
}

/// This runtime's identity and signing key, created on first use.
pub fn runtime_key(
    resolver: &PathResolver,
    vault: Arc<Vault>,
) -> Result<(peko_identity::runtime::RuntimeIdentity, Arc<SigningKey>)> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    let vault_port = crate::identity_compat::identity_vault_arc(vault);
    let identity = peko_identity::runtime::RuntimeIdentity::generate_or_load(
        crate::identity_compat::runtime_paths_arc(resolver).as_ref(),
        vault_port.as_ref(),
    )?;
    let encoded = identity
        .load_private_key(vault_port.as_ref())?
        .context("runtime private key not found in vault")?;
    let bytes: [u8; 32] = BASE64
        .decode(encoded.trim())
        .context("runtime private key is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("runtime private key must be 32 bytes"))?;
    Ok((identity, Arc::new(SigningKey::from_bytes(&bytes))))
}

/// The registration proof (ADR-058 D4): a compact EdDSA JWS by the runtime
/// key over the Hub's challenge, binding the runtime DID to the account.
#[must_use]
pub fn register_pop(
    nonce: &str,
    exp: &serde_json::Value,
    owner: &serde_json::Value,
    runtime_did: &str,
    signing_key: &SigningKey,
) -> serde_json::Value {
    let payload = serde_json::json!({
        "nonce": nonce,
        "runtimeDid": runtime_did,
        "owner": owner,
        "iat": chrono::Utc::now().timestamp(),
        "exp": exp,
    });
    let jws = crate::tunnel::tunnel_channel_signature::jws_sign(
        signing_key,
        payload.to_string().as_bytes(),
    );
    serde_json::json!({ "nonce": nonce, "jws": jws })
}

fn actor_store(resolver: &PathResolver) -> ChannelStore {
    ChannelStore::new(ChannelConfig {
        runtime_dir: resolver.runtime_dir(),
        shared_dir: None,
    })
}

/// Register this runtime and its local user with the Hub at `origin`
/// under the account `api_key` belongs to.
pub async fn register_runtime(
    resolver: &PathResolver,
    vault: Arc<Vault>,
    origin: &str,
    api_key: &str,
) -> Result<RuntimeRegistration> {
    let origin = crate::tunnel::credential::official_hub_url(&hub_http_origin(origin)?);
    let (identity, signing_key) = runtime_key(resolver, vault)?;
    let runtime_did = identity.runtime_did.clone();
    let client = reqwest::Client::new();

    let me = client
        .get(format!("{origin}/v1/auth/me"))
        .bearer_auth(api_key)
        .send()
        .await
        .context("could not reach PekoHub")?;
    anyhow::ensure!(
        me.status().is_success(),
        "PekoHub rejected the API key: HTTP {}",
        me.status()
    );
    let account = me
        .json::<serde_json::Value>()
        .await?
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .context("PekoHub returned no account ID")?;
    uuid::Uuid::parse_str(&account).context("PekoHub account IDs are UUIDs")?;

    let challenge = client
        .post(format!("{origin}/v1/runtimes/register-challenge"))
        .bearer_auth(api_key)
        .send()
        .await?;
    anyhow::ensure!(
        challenge.status().is_success(),
        "PekoHub refused a registration challenge: HTTP {}",
        challenge.status()
    );
    let challenge: serde_json::Value = challenge.json().await?;
    let nonce = challenge
        .get("nonce")
        .and_then(|v| v.as_str())
        .context("registration challenge carried no nonce")?;
    let exp = challenge
        .get("exp")
        .cloned()
        .context("registration challenge carried no expiry")?;
    let owner = challenge
        .get("owner")
        .cloned()
        .unwrap_or_else(|| serde_json::json!(account));
    let pop = register_pop(nonce, &exp, &owner, &runtime_did, &signing_key);

    let registered = client
        .post(format!("{origin}/v1/runtimes/register"))
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "runtime_did": runtime_did,
            "display_name": "peko-runtime",
            "pop": pop,
        }))
        .send()
        .await?;
    let status = registered.status();
    if !status.is_success() {
        let body = registered.text().await.unwrap_or_default();
        anyhow::bail!("PekoHub rejected runtime registration: HTTP {status}. {body}");
    }
    let row: serde_json::Value = registered.json().await?;
    anyhow::ensure!(
        row.get("ownerId").and_then(|v| v.as_str()) == Some(account.as_str()),
        "PekoHub registered the runtime under a different account"
    );

    let store = actor_store(resolver);
    store.link_local_user(&runtime_did, &account).await?;
    // ADR-091 D5: another person's conversations do not survive a change
    // of hands, whatever path removed the old registration.
    forget_remote_dms(resolver, &store, Some(&account)).await?;
    PekoHubCredential {
        url: tunnel_url(&origin),
        runtime_id: runtime_did.clone(),
        tls: None,
    }
    .save_to_file(&resolver.pekohub_config())?;

    Ok(RuntimeRegistration {
        origin,
        runtime_did,
        account,
    })
}

/// The outbox directory of this runtime's cross-runtime channels.
fn outbox_dir(resolver: &PathResolver) -> std::path::PathBuf {
    resolver
        .runtime_dir()
        .join("channel_delivery")
        .join("outbox")
}

/// Delete the DMs with remote pekos that do not belong to `keep`, and
/// what waits to be sent in them (ADR-091 D5).
async fn forget_remote_dms(
    resolver: &PathResolver,
    store: &ChannelStore,
    keep: Option<&str>,
) -> Result<()> {
    store.purge_invitations(keep).await?;
    // ADR-095 D6: requests to DM the previous person, too; their DMs with
    // people go with their remote DMs (both are kept under their account).
    store.purge_dm_requests(keep).await?;
    let purged = store.purge_remote_dms(keep).await?;
    if !purged.is_empty() {
        tracing::info!(
            count = purged.len(),
            "deleted another account's remote conversations"
        );
        crate::tunnel::channel_outbox::forget_channels(&outbox_dir(resolver), &purged).await;
    }
    Ok(())
}

/// Remove this runtime's Hub registration: Hub forgets the runtime, the
/// local user loses its Hub alias, and the registration file is deleted.
/// Registered principals must be unregistered first. The person's
/// conversations with remote pekos are deleted (their hosts keep them,
/// ADR-091 D5); with posts in them still unsent, this refuses unless
/// `discard_unsent`.
pub async fn unregister_runtime(
    resolver: &PathResolver,
    vault: Arc<Vault>,
    api_key: &str,
    discard_unsent: bool,
) -> Result<()> {
    use peko_channel::ChannelPort;
    let store = actor_store(resolver);
    if let Some(account) = store
        .actor_info(&peko_subject::ActorRef::User("local".into()))
        .await
        .hub_id
    {
        let remote = store.remote_dms(&account).await?;
        let unsent =
            crate::tunnel::channel_outbox::unsent_posts_in(&outbox_dir(resolver), &remote).await;
        anyhow::ensure!(
            unsent == 0 || discard_unsent,
            "{unsent} message(s) to pekos on other computers have not been delivered yet. \
             Unregistering deletes this runtime's copy of those conversations (their hosts keep \
             the rest), so the undelivered messages would be lost. Wait until they are \
             delivered, or unregister with --discard-unsent."
        );
    }
    let registered: Vec<String> = actor_store(resolver)
        .known_actors()
        .await?
        .into_iter()
        .filter(|a| a.kind == "principal" && a.local_id.is_some() && a.hub_id.is_some())
        .map(|a| a.name.unwrap_or(a.address))
        .collect();
    anyhow::ensure!(
        registered.is_empty(),
        "Unregister these principals first (peko unregister <name>): {}",
        registered.join(", ")
    );
    let path = resolver.pekohub_config();
    let credential = PekoHubCredential::from_file(&path)?;
    let origin = crate::tunnel::hub_origin(&credential.url)
        .context("the registration names no Hub origin")?;
    let (identity, _) = runtime_key(resolver, vault)?;
    let response = reqwest::Client::new()
        .delete(format!(
            "{origin}/v1/runtimes/{}",
            urlencoding::encode(&identity.runtime_did)
        ))
        .bearer_auth(api_key)
        .send()
        .await
        .context("could not reach PekoHub")?;
    let status = response.status();
    anyhow::ensure!(
        status.is_success() || status == reqwest::StatusCode::NOT_FOUND,
        "PekoHub refused to unregister the runtime: HTTP {status}"
    );
    store.unlink_local_user().await?;
    forget_remote_dms(resolver, &store, None).await?;
    std::fs::remove_file(&path)?;
    Ok(())
}

/// The local user's actor record, for status displays.
pub async fn local_user(resolver: &PathResolver) -> peko_channel::ActorInfo {
    use peko_channel::ChannelPort;
    actor_store(resolver)
        .actor_info(&peko_subject::ActorRef::User("local".into()))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_and_tunnel_urls() {
        assert_eq!(
            hub_http_origin("pekohub.org").unwrap(),
            "https://pekohub.org"
        );
        assert_eq!(
            hub_http_origin("http://localhost:4000/x").unwrap(),
            "http://localhost:4000"
        );
        assert!(hub_http_origin("ftp://hub").is_err());
        assert_eq!(
            tunnel_url("https://pekohub.org"),
            "wss://pekohub.org/v1/tunnel"
        );
        assert_eq!(
            tunnel_url("http://localhost:4000"),
            "ws://localhost:4000/v1/tunnel"
        );
    }

    /// A one-route-per-call HTTP stand-in for the Hub's OAuth endpoints:
    /// answers each request with the next canned body for its path.
    async fn fake_hub(answers: Vec<(&'static str, &'static str)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let mut answers = std::collections::VecDeque::from(answers);
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let Some((path, body)) = answers.pop_front() else {
                    break;
                };
                assert!(
                    request.starts_with(&format!("POST {path} ")),
                    "unexpected {request}"
                );
                assert!(
                    request.contains("client_id=peko-cli"),
                    "CLI client id missing"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        origin
    }

    /// pekohub ADR-007: the CLI shows a code, waits through
    /// `authorization_pending`, and returns the token once approved.
    #[tokio::test]
    async fn device_sign_in_waits_for_approval() {
        let origin = fake_hub(vec![
            (
                "/oauth/device/code",
                r#"{"device_code":"dc","user_code":"BCDF-GHJK","verification_uri":"https://hub/device","verification_uri_complete":"https://hub/device?code=BCDF-GHJK","expires_in":60,"interval":1}"#,
            ),
            ("/oauth/token", r#"{"error":"authorization_pending"}"#),
            ("/oauth/token", r#"{"access_token":"tok","token_type":"Bearer","expires_in":3600}"#),
        ])
        .await;
        let pending = start_device_sign_in(&origin).await.unwrap();
        assert_eq!(pending.user_code, "BCDF-GHJK");
        assert_eq!(pending.verification_uri, "https://hub/device");
        assert_eq!(
            finish_device_sign_in(&origin, &pending).await.unwrap(),
            "tok"
        );
    }

    #[tokio::test]
    async fn a_denied_device_sign_in_says_so() {
        let origin = fake_hub(vec![
            (
                "/oauth/device/code",
                r#"{"device_code":"dc","user_code":"BCDF-GHJK","verification_uri":"https://hub/device","expires_in":60,"interval":1}"#,
            ),
            ("/oauth/token", r#"{"error":"access_denied"}"#),
        ])
        .await;
        let pending = start_device_sign_in(&origin).await.unwrap();
        let error = finish_device_sign_in(&origin, &pending).await.unwrap_err();
        assert!(error.to_string().contains("denied"), "{error}");
    }

    /// ADR-081 D3: the local user keeps one key; registering adds the
    /// account as an alias, a second account is refused, and unlinking
    /// stops the account resolving to the local user.
    #[tokio::test]
    async fn the_local_user_links_one_account_at_a_time() {
        use peko_channel::ChannelPort;
        use peko_subject::ActorRef;
        let dir = tempfile::tempdir().unwrap();
        let store = ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().into(),
            shared_dir: None,
        });
        let local = store.ensure_local_user("did:key:zRuntime").await.unwrap();
        assert_eq!(local.identity_key, "local@did:key:zRuntime");
        let account = uuid::Uuid::new_v4().to_string();
        store
            .link_local_user("did:key:zRuntime", &account)
            .await
            .unwrap();
        let hub = ActorRef::User(peko_subject::Route::hub(account.clone()));
        let local_subject = ActorRef::User("local".into());
        assert_eq!(store.key_of(&hub), store.key_of(&local_subject));
        assert_eq!(
            store.key_of(&local_subject).0,
            "local@did:key:zRuntime",
            "registration never re-keys the local user"
        );
        let other = uuid::Uuid::new_v4().to_string();
        assert!(store
            .link_local_user("did:key:zRuntime", &other)
            .await
            .is_err());
        store.unlink_local_user().await.unwrap();
        assert_ne!(store.key_of(&hub), store.key_of(&local_subject));
    }
}
