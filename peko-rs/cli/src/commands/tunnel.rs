//! Tunnel Management Commands (ADR-035)
//!
//! Provides CLI commands to start, stop, and check the status of the
//! PekoHub tunnel connection.

use crate::commands::GlobalPaths;
use anyhow::Context;
use clap::Subcommand;
use peko_core::tunnel::{load_pekohub_credential, TunnelClient};
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

/// Tunnel management subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum TunnelCommands {
    /// Set up PekoHub tunnel credentials
    Setup {
        /// PekoHub URL (default: wss://pekohub.org/v1/tunnel)
        #[arg(short, long)]
        url: Option<String>,
        /// API key from PekoHub (can also be set via PEKOHUB_API_KEY env var)
        #[arg(short, long, env = "PEKOHUB_API_KEY")]
        api_key: Option<String>,
    },

    /// Start the tunnel connection to PekoHub
    Start {
        /// Path to PekoHub credential file (default: ~/.peko/runtime/pekohub.toml)
        #[arg(short, long)]
        credential: Option<PathBuf>,
    },

    /// Stop the tunnel connection
    Stop,

    /// Show tunnel status
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// ADR-058 D4: build the `pop` object for the register body —
/// `{nonce, jws}` where `jws` is a compact EdDSA JWS over
/// `{"nonce":..,"runtimeDid":..,"owner":..,"iat":..,"exp":..}` signed
/// with the runtime identity key. `exp` and `owner` pass through
/// verbatim from the hub's register-challenge response.
fn build_register_pop(
    nonce: &str,
    exp: &serde_json::Value,
    owner: &serde_json::Value,
    runtime_did: &str,
    keypair: &peko_identity::keys::KeyPair,
) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    let payload = serde_json::json!({
        "nonce": nonce,
        "runtimeDid": runtime_did,
        "owner": owner,
        "iat": now,
        "exp": exp,
    });
    let jws = peko_core::tunnel::tunnel_channel_signature::jws_sign(
        &keypair.signing_key,
        payload.to_string().as_bytes(),
    );
    serde_json::json!({
        "nonce": nonce,
        "jws": jws,
    })
}

/// Handle tunnel setup
async fn handle_tunnel_setup(
    url: Option<String>,
    api_key: Option<String>,
    _json: bool,
    paths: &GlobalPaths,
) -> anyhow::Result<()> {
    let cred_path = peko_core::tunnel::PekoHubCredential::path_for_config_dir(&paths.config_dir);
    let vault = peko_core::common::vault::Vault::load(paths.resolver().vault())
        .context("Failed to load credential vault")?;

    // Check if credential already exists
    if cred_path.exists() {
        let existing = peko_core::tunnel::PekoHubCredential::from_file(&cred_path)?;
        anyhow::bail!(
            "PekoHub credential already exists at: {}\n\
             Runtime ID: {}\n\
             Use `peko tunnel start` to connect, or delete the file to reconfigure.",
            cred_path.display(),
            existing.runtime_id
        );
    }

    // Generate new ed25519 keypair
    let keypair = peko_identity::keys::KeyPair::generate();
    let public_key_bytes = keypair.public_key_bytes();
    let private_key_bytes = keypair.private_key_bytes();

    // Create did:key from public key
    let runtime_did = peko_identity::runtime::public_key_to_did_key(&public_key_bytes);

    // Determine hub URL
    let hub_url = url.unwrap_or_else(|| "wss://pekohub.org/v1/tunnel".to_string());

    // API key is required for setup
    let api_key = api_key.ok_or_else(|| {
        anyhow::anyhow!(
            "API key is required. Provide it with --api-key or set the PEKOHUB_API_KEY environment variable."
        )
    })?;

    // Validate the API key with a lightweight HTTP ping before saving credentials
    let http_url = hub_url
        .replace("wss://", "https://")
        .replace("ws://", "http://")
        .replace("/v1/tunnel", "");
    let validate_url = format!("{}/v1/ping", http_url);
    let client = reqwest::Client::new();
    let resp = client
        .get(&validate_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            println!("   API key validated successfully.");
        }
        Ok(r) => {
            anyhow::bail!(
                "API key validation failed: HTTP {}. Please check your API key and hub URL.",
                r.status()
            );
        }
        Err(e) => {
            anyhow::bail!(
                "Failed to validate API key: {}. Please check your network connection and hub URL.",
                e
            );
        }
    }

    // Register this runtime's DID with PekoHub's allowlist before we
    // save the credential. PekoHub's tunnel handshake (issue #1) now
    // requires a row in the `runtimes` table or it will close the
    // WebSocket with 1008. Registration is idempotent (`upsert`) so
    // re-running setup is safe.
    //
    // ADR-057: the register response carries the owning hub user's id
    // (`ownerId`); it is persisted into the credential so the daemon
    // can derive the local terminal's attribution identity from it
    // while logged in.
    //
    // ADR-058 D4: proof of possession. Before registering, fetch a
    // server-issued challenge (`POST /v1/runtimes/register-challenge`
    // → `{nonce, exp}`), then include `pop: {nonce, jws}` in the
    // register body. The JWS is a compact EdDSA JWS over
    // `{"nonce":..,"runtimeDid":..,"owner":..,"iat":..,"exp":..}`
    // signed with the RUNTIME identity key — the same key behind
    // `runtime_did` and the tunnel hello — so the directory row
    // becomes a verified claim rather than a first-registrant squat.
    // Hubs that predate D4 (challenge endpoint missing/failing) get
    // the legacy register body without `pop`.
    let challenge_url = format!("{}/v1/runtimes/register-challenge", http_url);
    let mut pop_field: Option<serde_json::Value> = None;
    match client
        .post(&challenge_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(body) => {
                match body.get("nonce").and_then(|v| v.as_str()) {
                    Some(nonce) => {
                        let now = chrono::Utc::now().timestamp();
                        // `exp` / `owner` pass through VERBATIM from the
                        // challenge so the signed payload matches what
                        // the hub issued; `owner` is null when the hub
                        // does not disclose it at challenge time.
                        let exp = body
                            .get("exp")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!(now + 300));
                        let owner = body
                            .get("owner")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let pop = build_register_pop(nonce, &exp, &owner, &runtime_did, &keypair);
                        pop_field = Some(pop);
                        println!("   Registration PoP minted (ADR-058 D4).");
                    }
                    None => eprintln!(
                        "   ⚠️  register-challenge response carried no nonce; \
                         registering without PoP."
                    ),
                }
            }
            Err(e) => eprintln!(
                "   ⚠️  Could not parse the register-challenge response ({e}); \
                 registering without PoP."
            ),
        },
        Ok(r) => eprintln!(
            "   ⚠️  register-challenge returned HTTP {}; registering without PoP \
             (hub predates ADR-058 D4?).",
            r.status()
        ),
        Err(e) => {
            eprintln!("   ⚠️  Could not reach register-challenge ({e}); registering without PoP.")
        }
    }
    let mut register_body = serde_json::json!({
        "runtime_did": runtime_did,
        "display_name": "peko-runtime",
    });
    if let Some(pop) = pop_field {
        register_body["pop"] = pop;
    }
    let register_url = format!("{}/v1/runtimes/register", http_url);
    let register_resp = client
        .post(&register_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&register_body)
        .send()
        .await;
    let mut hub_owner_id: Option<String> = None;
    match register_resp {
        Ok(r) if r.status().is_success() => {
            hub_owner_id = r.json::<serde_json::Value>().await.ok().and_then(|row| {
                row.get("ownerId")
                    .and_then(|v| v.as_str().map(str::to_string))
            });
            match &hub_owner_id {
                Some(owner) => {
                    println!("   Runtime registered with PekoHub allowlist (owner {owner}).")
                }
                None => eprintln!(
                    "   ⚠️  Runtime registered, but the hub owner id was not in the response. \
                     Local messages will attribute as user:local until re-setup."
                ),
            }
        }
        Ok(r) => {
            // 4xx is fatal — PekoHub explicitly rejected the registration
            // (e.g. API key lacks the right scope, or runtime DID is
            // already owned by another user). 5xx is treated as a soft
            // warning so setup still succeeds and the operator can
            // retry registration manually.
            if r.status().is_client_error() {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                anyhow::bail!("PekoHub rejected runtime registration: HTTP {status}. {body}");
            }
            eprintln!(
                "   ⚠️  Runtime registration failed: HTTP {}. The credential was saved; \
                 re-run `peko tunnel setup` once PekoHub is reachable to complete registration.",
                r.status()
            );
        }
        Err(e) => {
            eprintln!(
                "   ⚠️  Could not reach PekoHub to register the runtime: {}. \
                 The credential was saved; re-run `peko tunnel setup` once PekoHub is reachable \
                 to complete registration.",
                e
            );
        }
    }

    // Store the private key in the encrypted vault.
    vault
        .set_tunnel_private_key(&runtime_did, &BASE64.encode(private_key_bytes))
        .context("Failed to store tunnel private key in vault")?;
    println!("   Private key stored securely in vault.");

    // Create credential (no raw private_key)
    let credential = peko_core::tunnel::PekoHubCredential {
        url: hub_url.clone(),
        runtime_id: runtime_did.clone(),
        owner_id: hub_owner_id,
        tls: None,
    };

    // Save credential to file
    credential.save_to_file(&cred_path)?;

    println!("✅ PekoHub tunnel configured successfully.");
    println!("   Credential file: {}", cred_path.display());
    println!("   Hub URL:         {}", hub_url);
    println!("   Runtime DID:     {}", runtime_did);
    println!();
    println!("   Start the tunnel with: peko tunnel start");
    println!("   Or start the daemon:   peko daemon start");

    Ok(())
}

/// Handle tunnel commands
pub async fn handle_tunnel(
    cmd: TunnelCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        TunnelCommands::Setup { url, api_key } => {
            handle_tunnel_setup(url, api_key, json, paths).await
        }
        TunnelCommands::Start { credential } => {
            let cred_path = credential.as_deref();
            let cred = match load_pekohub_credential(cred_path)? {
                Some(c) => c,
                None => {
                    let path = cred_path.map_or_else(
                        peko_core::tunnel::PekoHubCredential::default_path,
                        PathBuf::from,
                    );
                    anyhow::bail!(
                        "No PekoHub credential found at: {}\n\
                         Run `peko tunnel setup` to configure, or provide --credential",
                        path.display()
                    );
                }
            };

            println!("🔗 Starting PekoHub tunnel...");
            println!("   URL: {}", cred.url);
            println!("   Runtime ID: {}", cred.runtime_id);

            // Try to connect to the daemon and use its AppState for dispatch.
            // If the daemon is not running, fall back to a limited mode.
            let daemon_running = peko_core::ipc::DaemonClient::connect().await.is_ok();

            if daemon_running {
                println!("   Mode: daemon-integrated (full service dispatch)");
                // The tunnel is already running in the daemon if credentials exist.
                // This foreground command is mainly for debugging / manual override.
                println!("   Note: Daemon already manages the tunnel. Use `peko tunnel status` to check.");
                println!("   Forcing foreground tunnel connection...");
            } else {
                println!("   Mode: standalone (daemon not running)");
                println!("   Warning: Chat requests will not be dispatched without the daemon.");
            }

            let vault = peko_core::common::vault::Vault::load(paths.resolver().vault())
                .context("Failed to load credential vault")?;
            let mut client = TunnelClient::new(cred).with_vault(std::sync::Arc::new(vault));
            client.on_request(|msg, _handle| async move {
                tracing::info!("Received tunnel message (no dispatcher): {:?}", msg);
            });

            println!("✅ Tunnel connected (Ctrl+C to disconnect)");
            client.run().await;

            Ok(())
        }
        TunnelCommands::Stop => {
            // In daemon mode, stop the tunnel via IPC
            match peko_core::ipc::DaemonClient::connect().await {
                Ok(client) => match client.tunnel_stop().await {
                    Ok(peko_core::ipc::ResponsePacket::Done { success, .. }) => {
                        if success {
                            println!("🛑 Tunnel stopped.");
                        } else {
                            println!("⚠️  Tunnel stop returned unsuccessful.");
                        }
                    }
                    Ok(other) => {
                        println!(
                            "⚠️  Unexpected response from daemon: {}",
                            other.variant_name()
                        );
                    }
                    Err(e) => {
                        println!("❌ Failed to stop tunnel: {}", e);
                    }
                },
                Err(_) => {
                    println!("🛑 No daemon running. Tunnel is not active.");
                }
            }
            Ok(())
        }
        TunnelCommands::Status { json: json_flag } => {
            let has_cred = peko_core::tunnel::credential::has_pekohub_credential();
            let cred_path = peko_core::tunnel::PekoHubCredential::default_path();

            // Try to check daemon tunnel status
            match peko_core::ipc::DaemonClient::connect().await {
                Ok(client) => match client.tunnel_status().await {
                    Ok(peko_core::ipc::ResponsePacket::TunnelStatus {
                        configured,
                        daemon_running,
                        connected,
                        ..
                    }) => {
                        if json_flag || json {
                            let output = serde_json::json!({
                                "configured": configured,
                                "credential_path": cred_path.to_string_lossy().to_string(),
                                "daemon_running": daemon_running,
                                "connected": connected,
                            });
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        } else {
                            println!("📡 Tunnel Status:");
                            if configured {
                                println!("  Credential: ✅ Found at {}", cred_path.display());
                            } else {
                                println!("  Credential: ❌ Not found at {}", cred_path.display());
                            }
                            if daemon_running {
                                println!("  Daemon:     ✅ Running");
                            } else {
                                println!("  Daemon:     ❌ Not running");
                            }
                            if connected {
                                println!("  Tunnel:     ✅ Connected");
                            } else {
                                println!("  Tunnel:     ❌ Not connected");
                            }
                            println!();
                            println!("  Start with: peko daemon start  (auto-starts tunnel if cred exists)");
                            println!("  Or:         peko tunnel start   (foreground mode)");
                        }
                    }
                    Ok(other) => {
                        if json_flag || json {
                            let output = serde_json::json!({
                                "configured": has_cred,
                                "credential_path": cred_path.to_string_lossy().to_string(),
                                "daemon_running": true,
                                "connected": false,
                                "warning": format!("Unexpected response: {}", other.variant_name()),
                            });
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        } else {
                            println!("📡 Tunnel Status:");
                            println!("  Daemon:     ✅ Running");
                            println!("  Warning:    Unexpected response from daemon");
                        }
                    }
                    Err(e) => {
                        if json_flag || json {
                            let output = serde_json::json!({
                                "configured": has_cred,
                                "credential_path": cred_path.to_string_lossy().to_string(),
                                "daemon_running": true,
                                "connected": false,
                                "error": format!("Failed to get status: {}", e),
                            });
                            println!("{}", serde_json::to_string_pretty(&output)?);
                        } else {
                            println!("📡 Tunnel Status:");
                            println!("  Daemon:     ✅ Running");
                            println!("  Error:      Failed to get status: {}", e);
                        }
                    }
                },
                Err(_) => {
                    if json_flag || json {
                        let output = serde_json::json!({
                            "configured": has_cred,
                            "credential_path": cred_path.to_string_lossy().to_string(),
                            "daemon_running": false,
                            "connected": false,
                        });
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    } else {
                        println!("📡 Tunnel Status:");
                        if has_cred {
                            println!("  Credential: ✅ Found at {}", cred_path.display());
                        } else {
                            println!("  Credential: ❌ Not found at {}", cred_path.display());
                        }
                        println!("  Daemon:     ❌ Not running");
                        println!("  Tunnel:     ❌ Not connected");
                        println!();
                        println!(
                            "  Start with: peko daemon start  (auto-starts tunnel if cred exists)"
                        );
                        println!("  Or:         peko tunnel start   (foreground mode)");
                    }
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_core::ipc::packet::{RequestPacket, ResponsePacket};

    /// ADR-058 D4: the register PoP is a compact JWS over the
    /// canonical claim set, verifiable against the runtime identity
    /// key (the key behind `runtime_did`).
    #[test]
    fn test_build_register_pop_signs_canonical_claims() {
        let keypair = peko_identity::keys::KeyPair::generate();
        let runtime_did =
            peko_identity::runtime::public_key_to_did_key(&keypair.public_key_bytes());

        let pop = build_register_pop(
            "nonce-abc",
            &serde_json::json!(1_900_000_000i64),
            &serde_json::json!("user-42"),
            &runtime_did,
            &keypair,
        );
        assert_eq!(pop["nonce"].as_str(), Some("nonce-abc"));
        let jws = pop["jws"].as_str().expect("jws present");

        // Verifies against the key embedded in the claimed runtime DID.
        let key = peko_core::tunnel::did_key_to_verifying_key(&runtime_did).unwrap();
        let (_seg, payload) = peko_core::tunnel::tunnel_channel_signature::jws_verify(&key, jws)
            .expect("PoP JWS must verify against the runtime DID key");
        let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(payload["nonce"].as_str(), Some("nonce-abc"));
        assert_eq!(payload["runtimeDid"].as_str(), Some(runtime_did.as_str()));
        assert_eq!(payload["owner"].as_str(), Some("user-42"));
        assert_eq!(payload["exp"].as_i64(), Some(1_900_000_000));
        assert!(payload["iat"].as_i64().is_some(), "iat present");

        // A different runtime DID's key must NOT verify the PoP.
        let other = peko_identity::keys::KeyPair::generate();
        let other_did = peko_identity::runtime::public_key_to_did_key(&other.public_key_bytes());
        let other_key = peko_core::tunnel::did_key_to_verifying_key(&other_did).unwrap();
        assert!(
            peko_core::tunnel::tunnel_channel_signature::jws_verify(&other_key, jws).is_err(),
            "PoP must not verify against a different runtime key"
        );
    }

    #[test]
    fn test_tunnel_commands_enum() {
        let _cmd = TunnelCommands::Setup {
            url: None,
            api_key: None,
        };
        let _cmd = TunnelCommands::Start { credential: None };
        let _cmd = TunnelCommands::Stop;
        let _cmd = TunnelCommands::Status { json: true };
    }

    #[test]
    fn test_tunnel_stop_request_serialization() {
        let req = RequestPacket::TunnelStop { request_id: 700 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TunnelStop { request_id } => {
                assert_eq!(request_id, 700);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_status_request_serialization() {
        let req = RequestPacket::TunnelStatus { request_id: 701 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TunnelStatus { request_id } => {
                assert_eq!(request_id, 701);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_status_response_serialization() {
        let resp = ResponsePacket::TunnelStatus {
            request_id: 702,
            configured: true,
            daemon_running: true,
            connected: false,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TunnelStatus {
                request_id,
                configured,
                daemon_running,
                connected,
            } => {
                assert_eq!(request_id, 702);
                assert!(configured);
                assert!(daemon_running);
                assert!(!connected);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_tunnel_request_ids() {
        let req_stop = RequestPacket::TunnelStop { request_id: 1 };
        assert_eq!(req_stop.request_id(), 1);

        let req_status = RequestPacket::TunnelStatus { request_id: 2 };
        assert_eq!(req_status.request_id(), 2);

        let resp = ResponsePacket::TunnelStatus {
            request_id: 3,
            configured: false,
            daemon_running: true,
            connected: false,
        };
        assert_eq!(resp.request_id(), 3);
    }
}
