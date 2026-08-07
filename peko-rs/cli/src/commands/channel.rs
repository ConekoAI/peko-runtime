//! `peko channel` — multi-principal chat primitive CLI surface (PR-2d).
//!
//! Each subcommand has two paths:
//!
//! 1. **Daemon-first.** Connect via `peko_core::ipc::DaemonClient` and
//!    send the corresponding `RequestPacket::Channel*` variant. This
//!    is the production path — the daemon owns the on-disk store and
//!    can publish events to live subscribers.
//!
//! 2. **In-process fallback.** When the daemon is unreachable, build a
//!    fresh `ChannelStore` rooted at `paths.runtime_dir()` and
//!    invoke `ChannelCliRouter` directly. This mirrors
//!    `peko tunnel status` (`commands/tunnel.rs:223`) — the manual
//!    smoke test works without a live daemon.
//!
//! Why dual-path: PR-1 promised the trait surface is fully reachable
//! from the binary without the daemon; PR-2 keeps that promise. The
//! daemon path is preferred (subscribers, audit, quota) but not
//! required (manual smoke, CI snapshots, scripting).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use peko_channel::port::{CreateOpts, PostMsg};
use peko_channel::{ChannelCliRouter, ChannelConfig, ChannelStore};
use peko_core::ipc::packet::{RequestPacket, ResponsePacket};
use peko_core::ipc::DaemonClient;
use peko_protocol::channel::{ChannelEvent, ChannelId, ChannelMembership};
use peko_subject::PrincipalId;

use crate::commands::GlobalPaths;

/// `peko channel` subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ChannelCommands {
    /// Create a new channel owned by `creator`.
    Create {
        /// Creator principal name (must exist on disk).
        creator: String,
        /// Channel display name (free-form string).
        name: String,
        /// Output the new channel id as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Invite `invitee` to `channel` (invited by `inviter`).
    Invite {
        /// Channel id (`chan_xxxxxxxx`).
        channel: String,
        /// Inviter principal name (must already be a member).
        inviter: String,
        /// Invitee principal name (must exist on disk).
        invitee: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Post a message to `channel` from `sender`.
    Post {
        /// Channel id.
        channel: String,
        /// Sender principal name (must be a member).
        sender: String,
        /// Message text.
        text: String,
        /// Optional parent task id (reply target).
        #[arg(long)]
        parent: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List events on `channel` since `since` (None = from start).
    Peek {
        /// Channel id.
        channel: String,
        /// Optional cursor (`task_xxxxxxxx`).
        #[arg(long)]
        since: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List members of `channel`.
    Members {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List channels where `principal` is a member.
    Ls {
        /// Principal name.
        principal: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Membership snapshot for `channel` (mirrors `peko channel members`
    /// today; preserved as a separate verb because the hub frontend
    /// uses it).
    Show {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Remove `principal` from `channel`. PR-3a: closes the
    /// missing IPC variant — PR-1 had `handle_leave` only on the
    /// in-process path.
    Leave {
        /// Channel id.
        channel: String,
        /// Principal name (must already be a member).
        principal: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Copy a Runtime-tier channel into the Shared tier (PR-3d).
    /// COPY semantics — the Runtime source remains so `peko channel
    /// show` still resolves the channel. The shared root lives at
    /// `<shared_dir>/channels/<channel_id>/`. Production requires
    /// `channel:write_shared`; the in-process fallback path inherits
    /// the same gate via the daemon.
    PinToShared {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
}

impl ChannelCommands {
    /// Read the `--json` flag from any variant.
    fn json_flag(&self) -> bool {
        match self {
            ChannelCommands::Create { json, .. }
            | ChannelCommands::Invite { json, .. }
            | ChannelCommands::Post { json, .. }
            | ChannelCommands::Peek { json, .. }
            | ChannelCommands::Members { json, .. }
            | ChannelCommands::Ls { json, .. }
            | ChannelCommands::Show { json, .. }
            | ChannelCommands::Leave { json, .. }
            | ChannelCommands::PinToShared { json, .. } => *json,
        }
    }
}

/// Dispatch `peko channel` subcommands.
pub async fn handle_channel(cmd: ChannelCommands, paths: &GlobalPaths) -> Result<()> {
    let json = cmd.json_flag();
    match cmd {
        ChannelCommands::Create { creator, name, .. } => {
            let packet = RequestPacket::ChannelCreate {
                request_id: 0,
                creator_name: creator.clone(),
                name: name.clone(),
            };
            let ch = run_daemon_or(
                paths,
                packet,
                "channel_create_failed",
                move |port, paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let creator_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&creator)
                        .with_context(|| {
                            format!("Creator principal '{creator}' not found on disk")
                        })?;
                    let resp = router.handle_create(&creator_id, &name).await?;
                    Ok(resp.channel)
                })
            )
            .await?;
            print_channel_id(ch, json)
        }
        ChannelCommands::Invite {
            channel,
            inviter,
            invitee,
            ..
        } => {
            let packet = RequestPacket::ChannelInvite {
                request_id: 0,
                channel: channel.clone(),
                inviter_name: inviter.clone(),
                invitee_name: invitee.clone(),
            };
            let (ch, invitee_id) = run_daemon_or(
                paths,
                packet,
                "channel_invite_failed",
                move |port, paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let inviter_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&inviter)
                        .with_context(|| {
                            format!("Inviter principal '{inviter}' not found on disk")
                        })?;
                    let invitee_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&invitee)
                        .with_context(|| {
                            format!("Invitee principal '{invitee}' not found on disk")
                        })?;
                    let resp = router.handle_invite(&ch, &inviter_id, &invitee_id).await?;
                    Ok((resp.channel, resp.invitee))
                })
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "channel": ch.to_string(),
                        "invitee": invitee_id.to_string(),
                    })
                );
            } else {
                println!("invited {} to {}", invitee_id, ch);
            }
            Ok(())
        }
        ChannelCommands::Post {
            channel,
            sender,
            text,
            parent,
            ..
        } => {
            let packet = RequestPacket::ChannelPost {
                request_id: 0,
                channel: channel.clone(),
                sender_name: sender.clone(),
                text: text.clone(),
                parent: parent.clone(),
            };
            let task_id = run_daemon_or(
                paths,
                packet,
                "channel_post_failed",
                move |port, paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let sender_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&sender)
                        .with_context(|| {
                            format!("Sender principal '{sender}' not found on disk")
                        })?;
                    let resp = router
                        .handle_post(&ch, &sender_id, &text, parent)
                        .await?;
                    Ok(resp.task_id)
                })
            )
            .await?;
            if json {
                println!("{}", serde_json::json!({ "task_id": task_id }));
            } else {
                println!("posted → {task_id}");
            }
            Ok(())
        }
        ChannelCommands::Peek { channel, since, .. } => {
            let packet = RequestPacket::ChannelPeek {
                request_id: 0,
                channel: channel.clone(),
                since: since.clone(),
            };
            let events = run_daemon_or(
                paths,
                packet,
                "channel_peek_failed",
                move |port, _paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let resp = router.handle_peek(&ch, since).await?;
                    Ok(resp.events)
                })
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&events).context("serialize events")?
                );
            } else {
                println!("{} events:", events.len());
                for ev in &events {
                    println!("  {ev:?}");
                }
            }
            Ok(())
        }
        ChannelCommands::Members { channel, .. } => {
            let packet = RequestPacket::ChannelMembers {
                request_id: 0,
                channel: channel.clone(),
            };
            let members = run_daemon_or(
                paths,
                packet,
                "channel_members_failed",
                move |port, _paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let resp = router.handle_members(&ch).await?;
                    Ok(resp.members)
                })
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&members).context("serialize members")?
                );
            } else {
                println!("{} member(s):", members.len());
                for m in &members {
                    println!("  {m}");
                }
            }
            Ok(())
        }
        ChannelCommands::Ls { principal, .. } => {
            let packet = RequestPacket::ChannelList {
                request_id: 0,
                principal_name: principal.clone(),
            };
            let channels = run_daemon_or(
                paths,
                packet,
                "channel_list_failed",
                move |port, paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let p_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&principal)
                        .with_context(|| {
                            format!("Principal '{principal}' not found on disk")
                        })?;
                    let resp = router.handle_list(&p_id).await?;
                    Ok(resp.channels)
                })
            )
            .await?;
            if json {
                let s: Vec<String> = channels.iter().map(|c| c.to_string()).collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&s).context("serialize channels")?
                );
            } else {
                println!("{} channel(s):", channels.len());
                for c in &channels {
                    println!("  {c}");
                }
            }
            Ok(())
        }
        ChannelCommands::Show { channel, .. } => {
            let packet = RequestPacket::ChannelMembers {
                request_id: 0,
                channel: channel.clone(),
            };
            let members = run_daemon_or(
                paths,
                packet,
                "channel_show_failed",
                move |port, _paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let resp = router.handle_show(&ch).await?;
                    Ok(resp.members)
                })
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&members).context("serialize members")?
                );
            } else {
                println!("{} member(s):", members.len());
                for m in &members {
                    println!("  {m}");
                }
            }
            Ok(())
        }
        ChannelCommands::Leave {
            channel,
            principal,
            ..
        } => {
            let packet = RequestPacket::ChannelLeave {
                request_id: 0,
                channel: channel.clone(),
                principal_name: principal.clone(),
            };
            let (ch, principal_id) = run_daemon_or(
                paths,
                packet,
                "channel_leave_failed",
                move |port, paths| Box::pin(async move {
                    let router = ChannelCliRouter::new(port);
                    let ch = parse_channel_id(&channel)?;
                    let principal_id = paths
                        .resolver()
                        .lookup_principal_id_by_name(&principal)
                        .with_context(|| {
                            format!("Principal '{principal}' not found on disk")
                        })?;
                    let resp = router.handle_leave(&ch, &principal_id).await?;
                    Ok((resp.channel, resp.principal))
                })
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "channel": ch.to_string(),
                        "principal": principal_id.to_string(),
                    })
                );
            } else {
                println!("{principal_id} left {ch}");
            }
            Ok(())
        }

        ChannelCommands::PinToShared { channel, .. } => {
            let channel_label = channel.clone();
            let channel_for_closure = channel.clone();
            let packet = RequestPacket::ChannelPinToShared {
                request_id: 0,
                channel: channel.clone(),
            };
            let shared_path = run_daemon_or(
                paths,
                packet,
                "channel_pin_to_shared_failed",
                move |port, _paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel_for_closure)?;
                        Ok(router.handle_pin_to_shared(&ch).await?)
                    })
                },
            )
            .await?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "channel": channel_label,
                        "shared_path": shared_path.to_string_lossy(),
                    }))
                    .context("serialize pin-to-shared result")?
                );
            } else {
                println!("pinned to shared: {}", shared_path.display());
            }
            Ok(())
        }
    }
}

/// Try the daemon first; on any error, fall back to the in-process
/// `ChannelCliRouter`. Returns the typed result either way.
///
/// Type `T` is the caller's local-path return shape. For the daemon
/// path we decode the wire `ResponsePacket` by serializing it and
/// deserializing into T — the response shapes in
/// `peko_channel::cli_handlers` and `peko_core::ipc::packet::ResponsePacket`
/// overlap structurally (ChannelId ↔ ChannelId, Vec<PrincipalId> ↔
/// Vec<PrincipalId>, etc.).
async fn run_daemon_or<'a, T, F>(
    paths: &'a GlobalPaths,
    packet: RequestPacket,
    err_label: &'static str,
    local: F,
) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(
        Arc<dyn peko_channel::ChannelPort>,
        &'a GlobalPaths,
    ) -> Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>,
{
    if let Ok(client) = DaemonClient::connect().await {
        if let Ok(resp) = client.request_response(packet).await {
            if let Ok(v) = decode_daemon_response::<T>(resp) {
                return Ok(v);
            }
        }
    }
    let port: Arc<dyn peko_channel::ChannelPort> = Arc::new(ChannelStore::new(
        ChannelConfig {
            runtime_dir: paths.runtime_dir(),
            // PR-3d: in-process fallback mirrors the daemon's
            // `principals_root_dir()` so `peko channel pin-to-shared`
            // works without a running daemon. The same authority
            // gate that the daemon enforces runs upstream of
            // `ChannelCliRouter::handle_pin_to_shared` in production.
            shared_dir: Some(paths.principals_root_dir()),
        },
    ));
    local(port, paths).await.context(err_label)
}

/// Decode a `ResponsePacket` returned by the daemon into the
/// caller's expected `T`. We do this by re-serializing the response
/// and deserializing into T — both `ChannelCliRouter::handle_*`
/// responses and the IPC `ResponsePacket::Channel*` variants carry
/// the same wire fields.
fn decode_daemon_response<T: serde::de::DeserializeOwned>(resp: ResponsePacket) -> Result<T> {
    let bytes = serde_json::to_vec(&resp).context("encode ResponsePacket for decode")?;
    serde_json::from_slice(&bytes).context("decode ResponsePacket into T")
}

fn parse_channel_id(s: &str) -> Result<ChannelId> {
    ChannelId::parse(s).with_context(|| format!("invalid ChannelId: {s}"))
}

fn print_channel_id(ch: ChannelId, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::json!({ "channel": ch.to_string() }));
    } else {
        println!("{ch}");
    }
    Ok(())
}

// Silence unused-import lints for types referenced only in patterns we
// may exercise in future subcommands (e.g. `PinToShared`).
#[allow(dead_code)]
fn _unused(
    _: ChannelEvent,
    _: PostMsg,
    _: CreateOpts,
    _: ChannelMembership,
    _: PrincipalId,
) {
}