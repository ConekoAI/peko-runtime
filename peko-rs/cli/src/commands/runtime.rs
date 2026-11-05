//! Runtime identity commands (ADR-032).
//!
//! The legacy `KnownRuntimes` trust registry commands (`list`,
//! `register`, `trust`, `remove`) were removed in the ADR-057
//! cleanup: `TrustLevel` was never consulted by any access decision,
//! and the direct-transport fields the registry carried were dead.

use crate::commands::GlobalPaths;
use clap::Subcommand;

/// Runtime identity subcommands
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum RuntimeCommands {
    /// Show this computer's runtime identity (its DID)
    Id,
    /// Show runtime metadata
    Info,
    /// Link this computer, and you as its user, to your PekoHub account.
    /// Needed before any peko can go online. Without `--api-key`, shows a
    /// code to approve on the PekoHub website.
    Register {
        /// PekoHub API origin (`pekohub.ai` and `pekohub.org` are read as
        /// `api.pekohub.ai`)
        #[arg(long, default_value = "https://api.pekohub.ai")]
        hub: String,
        /// API key of the Hub account to register under (for scripts;
        /// otherwise you approve a sign-in code on the website)
        #[arg(long, env = "PEKOHUB_API_KEY")]
        api_key: Option<String>,
    },
    /// Remove this runtime's PekoHub registration. Its copies of your
    /// conversations with pekos on other computers are deleted; their
    /// hosts keep them
    Unregister {
        /// API key of the Hub account the runtime is registered under
        /// (otherwise you approve a sign-in code on the website)
        #[arg(long, env = "PEKOHUB_API_KEY")]
        api_key: Option<String>,
        /// Unregister even though some messages to pekos on other
        /// computers have not been delivered yet (they are lost)
        #[arg(long)]
        discard_unsent: bool,
    },
}

/// Sign the CLI in to the Hub: show a code, wait for the user to approve
/// it on the website, and return an access token for their account.
async fn sign_in(hub: &str, json: bool) -> anyhow::Result<String> {
    use peko_core::tunnel::registration::{finish_device_sign_in, start_device_sign_in};
    let pending = start_device_sign_in(hub).await?;
    let link = pending
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| pending.verification_uri.clone());
    if json {
        eprintln!(
            "{}",
            serde_json::json!({
                "user_code": pending.user_code,
                "verification_uri": pending.verification_uri,
                "verification_uri_complete": pending.verification_uri_complete,
            })
        );
    } else {
        println!("To sign in, open {link}");
        println!("and confirm the code {}", pending.user_code);
        println!("Waiting for you to approve it…");
    }
    finish_device_sign_in(hub, &pending).await
}

/// Handle runtime commands
pub async fn handle_runtime(
    cmd: RuntimeCommands,
    paths: &GlobalPaths,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        RuntimeCommands::Register { hub, api_key } => {
            let vault = std::sync::Arc::new(peko_core::common::vault::Vault::load(
                paths.resolver().vault(),
            )?);
            let api_key = match api_key {
                Some(key) => key,
                None => sign_in(&hub, json).await?,
            };
            let registered = peko_core::tunnel::registration::register_runtime(
                paths.resolver(),
                vault,
                &hub,
                &api_key,
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "hub": registered.origin,
                        "runtime_did": registered.runtime_did,
                        "account": registered.account,
                    })
                );
            } else {
                println!("✓ Registered with {}", registered.origin);
                println!("  Runtime:    {}", registered.runtime_did);
                println!(
                    "  Local user: user:local:local = user:hub:{}",
                    registered.account
                );
                println!("  Restart the daemon to connect: peko daemon restart");
            }
            Ok(())
        }
        RuntimeCommands::Unregister {
            api_key,
            discard_unsent,
        } => {
            let vault = std::sync::Arc::new(peko_core::common::vault::Vault::load(
                paths.resolver().vault(),
            )?);
            let api_key = match api_key {
                Some(key) => key,
                None => {
                    let credential = peko_core::tunnel::PekoHubCredential::from_file(
                        &paths.resolver().pekohub_config(),
                    )
                    .map_err(|_| anyhow::anyhow!("This runtime isn't registered with PekoHub."))?;
                    let origin = peko_core::tunnel::hub_origin(&credential.url)
                        .ok_or_else(|| anyhow::anyhow!("the registration names no Hub origin"))?;
                    sign_in(&origin, json).await?
                }
            };
            peko_core::tunnel::registration::unregister_runtime(
                paths.resolver(),
                vault,
                &api_key,
                discard_unsent,
            )
            .await?;
            println!("✓ Runtime unregistered; the local user is offline-only.");
            Ok(())
        }
        RuntimeCommands::Id => {
            let client = peko_core::ipc::DaemonClient::connect().await?;
            let packet = peko_core::ipc::RequestPacket::RuntimeId { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                peko_core::ipc::ResponsePacket::RuntimeId { did, .. } => {
                    if json {
                        println!("{}", serde_json::json!({"did": did}));
                    } else {
                        println!("{}", did);
                    }
                    Ok(())
                }
                peko_core::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
        RuntimeCommands::Info => {
            let client = peko_core::ipc::DaemonClient::connect().await?;
            let packet = peko_core::ipc::RequestPacket::RuntimeInfo { request_id: 1 };
            let response = client.request_response(packet).await?;
            match response {
                peko_core::ipc::ResponsePacket::RuntimeInfo { metadata, .. } => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&metadata)?);
                    } else {
                        println!("Runtime ID: {}", metadata.runtime_id);
                        println!("Display Name: {}", metadata.display_name);
                        println!("Version: {}", metadata.version);
                        println!("Created: {}", metadata.created_at);
                        println!("Last Seen: {}", metadata.last_seen_at);
                        println!("Capabilities: {}", metadata.capabilities.join(", "));
                        println!("Host OS: {}", metadata.host_info.os);
                        println!("Host Arch: {}", metadata.host_info.arch);
                        println!("Hostname: {}", metadata.host_info.hostname);
                    }
                    Ok(())
                }
                peko_core::ipc::ResponsePacket::Error { message, .. } => {
                    anyhow::bail!("{}", message)
                }
                _ => anyhow::bail!("Unexpected response"),
            }
        }
    }
}
