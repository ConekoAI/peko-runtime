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
    let origin = hub_http_origin(origin)?;
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

    actor_store(resolver)
        .link_local_user(&runtime_did, &account)
        .await?;
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

/// Remove this runtime's Hub registration: Hub forgets the runtime, the
/// local user loses its Hub alias, and the registration file is deleted.
/// Registered principals must be unregistered first.
pub async fn unregister_runtime(
    resolver: &PathResolver,
    vault: Arc<Vault>,
    api_key: &str,
) -> Result<()> {
    use peko_channel::ChannelPort;
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
    actor_store(resolver).unlink_local_user().await?;
    std::fs::remove_file(&path)?;
    Ok(())
}

/// The local user's actor record, for status displays.
pub async fn local_user(resolver: &PathResolver) -> peko_channel::ActorInfo {
    use peko_channel::ChannelPort;
    actor_store(resolver)
        .actor_info(&peko_subject::Subject::User("local".into()))
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

    /// ADR-081 D3: the local user keeps one key; registering adds the
    /// account as an alias, a second account is refused, and unlinking
    /// stops the account resolving to the local user.
    #[tokio::test]
    async fn the_local_user_links_one_account_at_a_time() {
        use peko_channel::ChannelPort;
        use peko_subject::Subject;
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
        let hub = Subject::User(format!("hub:{account}"));
        let local_subject = Subject::User("local".into());
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
