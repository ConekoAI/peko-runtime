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
use peko_subject::{ActorRef, PrincipalId};
use std::str::FromStr;

use crate::commands::GlobalPaths;

/// `peko channel` subcommands.
#[derive(Subcommand)]
#[command(disable_version_flag = true)]
pub enum ChannelCommands {
    /// Create a new channel owned by `creator`.
    Create {
        /// Creator peko name (must exist on disk).
        creator: String,
        /// Channel display name (free-form string).
        name: String,
        /// Optional explicit channel id (ADR-049 Phase 2, D5), e.g.
        /// `group:<slug>` for a named group channel. Omit to mint a
        /// fresh `chan_<8 base36>` id.
        #[arg(long, value_name = "CHANNEL_ID")]
        id: Option<String>,
        /// Output the new channel id as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Invite `invitee` to `channel` (invited by `inviter`).
    Invite {
        /// Channel id (`chan_xxxxxxxx`).
        channel: String,
        /// Inviter peko name (must already be a member).
        inviter: String,
        /// Invitee peko name (must exist on disk), or a user's actor
        /// address such as `user:hub:<id>` (ADR-049 Phase 1 — validated
        /// by form only, no lookup).
        invitee: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Post a message to `channel` from `sender`.
    Post {
        /// Channel id.
        channel: String,
        /// Sender peko name (must be a member), or `me` to post
        /// as your own identity (ADR-057: derived from the
        /// connection — hub user when logged into pekohub, else
        /// `user:local`). A `user:…` sender is refused: user
        /// identities are never specified on the wire.
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
        /// Peko name.
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
        /// Peko name (must already be a member).
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

    /// Open your DM with a peko on another computer (ADR-090 D2), by its
    /// ID or its PekoHub ID; with a message, post it too. Read replies
    /// with `peko channel peek <dm>`, post with `peko channel post <dm> me`.
    Dm {
        /// `@owner/name` (a public peko), `principal:hub:<UUID>`, or the
        /// bare PekoHub ID.
        peko: String,
        /// Post this message right away.
        message: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Groups on other computers that invited you (ADR-092 D2).
    Invitations {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Join a group: accept its invitation, or ask its home with an invite
    /// link (or a public group's ID) from PekoHub (ADR-092 D5).
    Join {
        /// Channel id from `peko channel invitations`, an invite link, or a
        /// public group's ID.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Decline a group's invitation.
    Decline {
        /// Channel id from `peko channel invitations`.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Give a group an ID on PekoHub, or change who may find it (ADR-092 D5).
    Register {
        /// The group's channel id (its home is this computer).
        channel: String,
        /// Anyone with the ID may join; otherwise only with an invite link.
        #[arg(long)]
        public: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Make an invite link to a registered group, or stop them all.
    Link {
        /// The group's channel id.
        channel: String,
        /// How long the link works (1–30 days).
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Stop every link to the group from working instead.
        #[arg(long)]
        revoke: bool,
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
            | ChannelCommands::PinToShared { json, .. }
            | ChannelCommands::Dm { json, .. }
            | ChannelCommands::Invitations { json }
            | ChannelCommands::Join { json, .. }
            | ChannelCommands::Decline { json, .. }
            | ChannelCommands::Register { json, .. }
            | ChannelCommands::Link { json, .. } => *json,
        }
    }
}

/// Dispatch `peko channel` subcommands.
pub async fn handle_channel(cmd: ChannelCommands, paths: &GlobalPaths) -> Result<()> {
    let json = cmd.json_flag();
    match cmd {
        ChannelCommands::Create {
            creator, name, id, ..
        } => {
            let packet = RequestPacket::ChannelCreate {
                request_id: 0,
                creator_name: creator.clone(),
                name: name.clone(),
                id: id.clone(),
            };
            let ch = run_daemon_or(
                paths,
                packet,
                "channel_create_failed",
                move |port, paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let creator_id = paths
                            .resolver()
                            .lookup_principal_id_by_name(&creator)
                            .with_context(|| {
                                format!("Creator peko '{creator}' not found on disk")
                            })?;
                        // ADR-049 Phase 2 (D5): validate the explicit id
                        // up front; `None` keeps the chan_* mint.
                        let id = id.map(|raw| parse_channel_id(&raw)).transpose()?;
                        let resp = router.handle_create(&creator_id, &name, id).await?;
                        Ok(resp.channel)
                    })
                },
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
                move |port, paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        let inviter_id = paths
                            .resolver()
                            .lookup_principal_id_by_name(&inviter)
                            .with_context(|| {
                                format!("Inviter peko '{inviter}' not found on disk")
                            })?;
                        // ADR-049 Phase 1: a `user:<id>` invitee is taken
                        // verbatim (validated by wire form only); anything
                        // else resolves as a principal name.
                        let invitee_subject = match ActorRef::from_str(&invitee) {
                            Ok(s @ ActorRef::User(_)) => s,
                            _ => {
                                let id = paths
                                    .resolver()
                                    .lookup_principal_id_by_name(&invitee)
                                    .with_context(|| {
                                        format!("Invitee peko '{invitee}' not found on disk")
                                    })?;
                                ActorRef::from(&id)
                            }
                        };
                        let resp = router
                            .handle_invite(&ch, &inviter_id, &invitee_subject)
                            .await?;
                        Ok((resp.channel, resp.invitee))
                    })
                },
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
            // ADR-057: `me` posts as the caller's own derived identity
            // (`sender_name: None`); anything else must be a principal
            // name. A `user:<id>` sender is refused client-side —
            // user identities are derived from the connection, never
            // specified.
            let sender_name: Option<String> = if sender == "me" {
                None
            } else {
                if let Ok(s) = ActorRef::from_str(&sender) {
                    if matches!(s, ActorRef::User(_)) {
                        anyhow::bail!(
                            "user identities cannot be specified — the daemon derives yours \
                             from the connection. Use `me` to post as yourself, or a \
                             principal name to post as a runtime-hosted actor."
                        );
                    }
                }
                Some(sender.clone())
            };
            let packet = RequestPacket::ChannelPost {
                request_id: 0,
                channel: channel.clone(),
                sender_name,
                text: text.clone(),
                parent: parent.clone(),
            };
            let task_id =
                run_daemon_or(paths, packet, "channel_post_failed", move |port, paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        // In-process fallback (no daemon): `me`
                        // speaks as the CLI's derived identity; the
                        // daemon path resolves it server-side.
                        let sender_subject = if sender == "me" {
                            ActorRef::User(paths.user().into())
                        } else {
                            let id = paths
                                .resolver()
                                .lookup_principal_id_by_name(&sender)
                                .with_context(|| {
                                    format!("Sender peko '{sender}' not found on disk")
                                })?;
                            ActorRef::from(&id)
                        };
                        let resp = router
                            .handle_post(&ch, &sender_subject, &text, parent)
                            .await?;
                        Ok(resp.task_id)
                    })
                })
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
                // ADR-057: the reader's identity is derived
                // server-side from the connection; no requester on
                // the wire.
                tail: None,
                before: None,
                query: None,
                author: None,
            };
            let events =
                run_daemon_or(paths, packet, "channel_peek_failed", move |port, _paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        let resp = router.handle_peek(&ch, since).await?;
                        Ok(resp.events)
                    })
                })
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
                move |port, _paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        let resp = router.handle_members(&ch).await?;
                        Ok(resp.members)
                    })
                },
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
            let channels =
                run_daemon_or(paths, packet, "channel_list_failed", move |port, paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let p_id = paths
                            .resolver()
                            .lookup_principal_id_by_name(&principal)
                            .with_context(|| format!("Peko '{principal}' not found on disk"))?;
                        let resp = router.handle_list(&p_id).await?;
                        Ok(resp.channels)
                    })
                })
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
            let members =
                run_daemon_or(paths, packet, "channel_show_failed", move |port, _paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        let resp = router.handle_show(&ch).await?;
                        Ok(resp.members)
                    })
                })
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
            channel, principal, ..
        } => {
            let packet = RequestPacket::ChannelLeave {
                request_id: 0,
                channel: channel.clone(),
                principal_name: Some(principal.clone()),
            };
            let (ch, principal_id) =
                run_daemon_or(paths, packet, "channel_leave_failed", move |port, paths| {
                    Box::pin(async move {
                        let router = ChannelCliRouter::new(port);
                        let ch = parse_channel_id(&channel)?;
                        let principal_id = paths
                            .resolver()
                            .lookup_principal_id_by_name(&principal)
                            .with_context(|| format!("Peko '{principal}' not found on disk"))?;
                        let resp = router.handle_leave(&ch, &principal_id).await?;
                        Ok((resp.channel, resp.principal))
                    })
                })
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

        ChannelCommands::Dm { peko, message, .. } => {
            let hub_id = remote_peko_hub_id(paths, &peko).await?;
            let resp = dm_request(RequestPacket::ChannelOpenRemoteDm {
                request_id: 0,
                hub_id,
                token: None,
            })
            .await?;
            let ResponsePacket::ChannelRemoteDmOpened {
                channel,
                peer,
                name,
                created,
                ..
            } = resp
            else {
                anyhow::bail!("unexpected reply: {resp:?}");
            };
            let channel = channel.to_string();
            let posted = match message {
                Some(text) => {
                    dm_request(RequestPacket::ChannelPost {
                        request_id: 0,
                        channel: channel.clone(),
                        sender_name: None,
                        text,
                        parent: None,
                    })
                    .await?;
                    true
                }
                None => false,
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "channel": channel,
                        "peer": peer,
                        "name": name,
                        "created": created,
                        "posted": posted,
                    })
                );
            } else {
                let who = name.unwrap_or_else(|| peer.clone());
                if created {
                    println!("Opened your DM with {who}: {channel}");
                } else {
                    println!("Your DM with {who}: {channel}");
                }
                if posted {
                    println!("Posted. Replies: peko channel peek {channel}");
                } else {
                    println!("Post with: peko channel post {channel} me \"…\"");
                }
            }
            Ok(())
        }

        ChannelCommands::Invitations { .. } => {
            let client = DaemonClient::connect()
                .await
                .context("Group invitations need the daemon: start it with `peko daemon start`")?;
            let resp = client
                .request_response(RequestPacket::ChannelInvitations { request_id: 0 })
                .await?;
            let invitations = match resp {
                ResponsePacket::ChannelInvitationsResult { invitations, .. } => invitations,
                ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
                other => anyhow::bail!("unexpected reply: {other:?}"),
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&invitations)?);
            } else if invitations.is_empty() {
                println!("No group invitations.");
            } else {
                for i in &invitations {
                    println!(
                        "{}  \"{}\"  from {}  ({} members)",
                        i.channel,
                        i.name,
                        i.inviter,
                        i.initial_members.len()
                    );
                }
                println!("\nJoin with `peko channel join <channel>`, or `peko channel decline <channel>`.");
            }
            Ok(())
        }

        ChannelCommands::Join { channel, .. }
            if channel.starts_with("chan_") || channel.starts_with("group:") =>
        {
            answer_invitation(&channel, true, json).await
        }
        ChannelCommands::Join { channel: link, .. } => {
            let resp = group_request(RequestPacket::ChannelGroupJoin {
                request_id: 0,
                link,
            })
            .await?;
            let ResponsePacket::ChannelGroupJoined { channel, name, .. } = resp else {
                anyhow::bail!("unexpected reply: {resp:?}");
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "channel": channel, "name": name, "requested": true })
                );
            } else {
                println!(
                    "Asked to join \"{name}\" ({channel}). It appears here when its home lets you in."
                );
            }
            Ok(())
        }
        ChannelCommands::Register {
            channel, public, ..
        } => {
            let resp = group_request(RequestPacket::ChannelGroupRegister {
                request_id: 0,
                channel,
                visibility: if public { "public" } else { "invite_only" }.into(),
            })
            .await?;
            let ResponsePacket::ChannelGroupRegistered {
                channel,
                id,
                page,
                visibility,
                ..
            } = resp
            else {
                anyhow::bail!("unexpected reply: {resp:?}");
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "channel": channel, "id": id, "page": page, "visibility": visibility })
                );
            } else {
                println!("{channel} is group {id} on PekoHub ({visibility}): {page}");
            }
            Ok(())
        }
        ChannelCommands::Link {
            channel,
            days,
            revoke,
            ..
        } => {
            let resp = group_request(RequestPacket::ChannelGroupLink {
                request_id: 0,
                channel,
                days: Some(days),
                revoke,
            })
            .await?;
            let ResponsePacket::ChannelGroupLinkResult {
                channel,
                link,
                expires,
                revoked,
                ..
            } = resp
            else {
                anyhow::bail!("unexpected reply: {resp:?}");
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "channel": channel, "link": link, "expires": expires, "revoked": revoked })
                );
            } else if let Some(n) = revoked {
                println!("Stopped {n} invite link(s) to {channel}.");
            } else {
                println!(
                    "{}\nWorks until {}.",
                    link.unwrap_or_default(),
                    expires.unwrap_or_default()
                );
            }
            Ok(())
        }
        ChannelCommands::Decline { channel, .. } => answer_invitation(&channel, false, json).await,
    }
}

/// Send a group request to the daemon (ADR-092 D5), failing on its error.
/// One request to the daemon for a DM with a remote peko.
async fn dm_request(packet: RequestPacket) -> Result<ResponsePacket> {
    let client = DaemonClient::connect().await.context(
        "DMs with pekos on other computers need the daemon: start it with `peko daemon start`",
    )?;
    let resp = client.request_response(packet).await?;
    if let ResponsePacket::Error { message, .. } = resp {
        anyhow::bail!("{message}");
    }
    Ok(resp)
}

/// The PekoHub ID of the peko `target` names: `principal:hub:<UUID>`, a
/// bare UUID, or `@owner/name` — a public peko, looked up on the Hub this
/// runtime is registered with (`GET /v1/public/pekos/:owner/:name`).
async fn remote_peko_hub_id(paths: &GlobalPaths, target: &str) -> Result<String> {
    let target = target.trim();
    let bare = target.strip_prefix("principal:hub:").unwrap_or(target);
    if uuid::Uuid::parse_str(bare).is_ok() {
        return Ok(bare.to_string());
    }
    let Some((owner, name)) = target
        .strip_prefix('@')
        .and_then(|rest| rest.split_once('/'))
        .filter(|(owner, name)| !owner.is_empty() && !name.is_empty())
    else {
        anyhow::bail!("Name the peko as @owner/name or by its PekoHub ID, not {target:?}");
    };
    let credential =
        peko_core::tunnel::PekoHubCredential::from_file(&paths.resolver().pekohub_config())
            .map_err(|_| {
                anyhow::anyhow!(
                    "This runtime isn't registered with PekoHub: `peko runtime register` first"
                )
            })?;
    let origin = peko_core::tunnel::hub_origin(&credential.url)
        .ok_or_else(|| anyhow::anyhow!("the registration names no Hub origin"))?;
    let url = format!(
        "{}/v1/public/pekos/{}/{}",
        origin.trim_end_matches('/'),
        urlencoding::encode(&owner.to_lowercase()),
        urlencoding::encode(name)
    );
    let resp = reqwest::get(&url)
        .await
        .with_context(|| format!("Couldn't reach PekoHub at {origin}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "No public peko @{owner}/{name} on PekoHub. A private one is named by its PekoHub ID."
        );
    }
    let body: serde_json::Value = resp
        .error_for_status()
        .context("PekoHub didn't answer the lookup")?
        .json()
        .await?;
    body.pointer("/liveInstance/hubId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("@{owner}/{name} isn't online on PekoHub"))
}

async fn group_request(packet: RequestPacket) -> Result<ResponsePacket> {
    let client = DaemonClient::connect()
        .await
        .context("Groups on PekoHub need the daemon: start it with `peko daemon start`")?;
    let resp = client.request_response(packet).await?;
    if let ResponsePacket::Error { message, .. } = resp {
        anyhow::bail!("{message}");
    }
    Ok(resp)
}

/// Accept or decline a group's invitation through the daemon (ADR-092 D2).
async fn answer_invitation(channel: &str, accept: bool, json: bool) -> Result<()> {
    let client = DaemonClient::connect()
        .await
        .context("Group invitations need the daemon: start it with `peko daemon start`")?;
    let resp = client
        .request_response(RequestPacket::ChannelInvitationAnswer {
            request_id: 0,
            channel: channel.to_string(),
            accept,
        })
        .await?;
    if let ResponsePacket::Error { message, .. } = resp {
        anyhow::bail!("{message}");
    }
    if json {
        println!(
            "{}",
            serde_json::json!({ "channel": channel, "joined": accept })
        );
    } else if accept {
        println!(
            "Joined {channel}. Its home is told; the group's messages arrive here from now on."
        );
    } else {
        println!("Declined {channel}.");
    }
    Ok(())
}

/// Use the daemon when reachable, otherwise the in-process router. Once a
/// request reaches the daemon, errors must not repeat a mutation locally.
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
        let resp = client.request_response(packet).await.context(err_label)?;
        return decode_daemon_response::<T>(resp).context(err_label);
    }
    let port: Arc<dyn peko_channel::ChannelPort> = Arc::new(ChannelStore::new(ChannelConfig {
        runtime_dir: paths.runtime_dir(),
        // PR-3d: in-process fallback mirrors the daemon's
        // `principals_root_dir()` so `peko channel pin-to-shared`
        // works without a running daemon. The same authority
        // gate that the daemon enforces runs upstream of
        // `ChannelCliRouter::handle_pin_to_shared` in production.
        shared_dir: Some(paths.principals_root_dir()),
    }));
    local(port, paths).await.context(err_label)
}

/// Extract each reply's payload into the local router's return shape. The
/// tagged IPC envelope is not itself a ChannelId, tuple, or event array.
fn decode_daemon_response<T: serde::de::DeserializeOwned>(resp: ResponsePacket) -> Result<T> {
    let value = match resp {
        ResponsePacket::ChannelCreated { channel, .. } => serde_json::to_value(channel)?,
        ResponsePacket::ChannelInvited {
            channel, invitee, ..
        } => serde_json::to_value((channel, invitee))?,
        ResponsePacket::ChannelPosted { task_id, .. } => serde_json::to_value(task_id)?,
        ResponsePacket::ChannelPeekResult { events, .. } => serde_json::to_value(events)?,
        ResponsePacket::ChannelMembersResult { members, .. } => serde_json::to_value(members)?,
        ResponsePacket::ChannelListResult { channels, .. } => serde_json::to_value(channels)?,
        ResponsePacket::ChannelLeft {
            channel, principal, ..
        } => serde_json::to_value((channel, principal))?,
        ResponsePacket::ChannelPinnedToShared { shared_path, .. } => {
            serde_json::to_value(shared_path)?
        }
        ResponsePacket::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected channel response: {other:?}"),
    };
    serde_json::from_value(value).context("decode channel reply payload")
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
fn _unused(_: ChannelEvent, _: PostMsg, _: CreateOpts, _: ChannelMembership, _: PrincipalId) {}

#[cfg(test)]
mod response_tests {
    use super::*;

    fn test_paths(tmp: &tempfile::TempDir) -> GlobalPaths {
        GlobalPaths::new(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
            "default".to_string(),
        )
    }

    /// `peko channel dm` names a peko by its PekoHub ID in either form;
    /// `@owner/name` needs a registered runtime to look it up.
    #[tokio::test]
    async fn a_dm_target_is_a_hub_id_or_an_owner_and_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = test_paths(&tmp);
        let id = "704f080d-1453-408d-8349-f8657364c6f5";
        assert_eq!(remote_peko_hub_id(&paths, id).await.unwrap(), id);
        assert_eq!(
            remote_peko_hub_id(&paths, &format!("principal:hub:{id}"))
                .await
                .unwrap(),
            id
        );
        let unregistered = remote_peko_hub_id(&paths, "@alice/ada").await.unwrap_err();
        assert!(unregistered
            .to_string()
            .contains("isn't registered with PekoHub"));
        for bad in ["ada", "@alice", "@/ada", "alice/ada"] {
            assert!(remote_peko_hub_id(&paths, bad).await.is_err(), "{bad}");
        }
    }

    #[test]
    fn channel_mutation_replies_extract_scalar_and_tuple_payloads() {
        let channel = ChannelId::for_group("review");
        let created: ChannelId = decode_daemon_response(ResponsePacket::ChannelCreated {
            request_id: 1,
            channel: channel.clone(),
        })
        .unwrap();
        assert_eq!(created, channel);
        let user = ActorRef::from_str("user:local").unwrap();
        let invited: (ChannelId, ActorRef) =
            decode_daemon_response(ResponsePacket::ChannelInvited {
                request_id: 2,
                channel: channel.clone(),
                invitee: user.clone(),
            })
            .unwrap();
        assert_eq!(invited, (channel.clone(), user));
        let task: String = decode_daemon_response(ResponsePacket::ChannelPosted {
            request_id: 3,
            channel: channel.clone(),
            task_id: "task-test".into(),
        })
        .unwrap();
        assert_eq!(task, "task-test");
        let principal = PrincipalId::generate();
        let left: (ChannelId, PrincipalId) = decode_daemon_response(ResponsePacket::ChannelLeft {
            request_id: 4,
            channel: channel.clone(),
            principal: principal.clone(),
        })
        .unwrap();
        assert_eq!(left, (channel.clone(), principal));
        let path: std::path::PathBuf =
            decode_daemon_response(ResponsePacket::ChannelPinnedToShared {
                request_id: 5,
                channel,
                shared_path: "/tmp/shared".into(),
            })
            .unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp/shared"));
    }

    #[test]
    fn daemon_denial_and_unexpected_replies_are_errors() {
        let denied = decode_daemon_response::<ChannelId>(ResponsePacket::Error {
            request_id: 1,
            message: "not authorized".into(),
        })
        .unwrap_err();
        assert!(denied.to_string().contains("not authorized"));
        assert!(decode_daemon_response::<ChannelId>(ResponsePacket::Pong {
            request_id: 1,
            uptime_secs: 0,
            version: "test".into(),
        })
        .is_err());
    }

    #[test]
    fn channel_read_replies_extract_arrays() {
        let channel = ChannelId::for_group("review");
        let principal = PrincipalId::generate();
        let channels: Vec<ChannelId> = decode_daemon_response(ResponsePacket::ChannelListResult {
            request_id: 1,
            principal,
            channels: vec![channel.clone()],
        })
        .unwrap();
        assert_eq!(channels, vec![channel.clone()]);
        let reply: ResponsePacket = serde_json::from_value(serde_json::json!({
            "type": "channel_members_result", "request_id": 2,
            "channel": channel.to_string(), "members": [ActorRef::from_str("user:local").unwrap()]
        }))
        .unwrap();
        let members: Vec<ActorRef> = decode_daemon_response(reply).unwrap();
        assert_eq!(members, vec![ActorRef::from_str("user:local").unwrap()]);
        let reply: ResponsePacket = serde_json::from_value(serde_json::json!({
            "type": "channel_peek_result", "request_id": 3,
            "channel": channel.to_string(), "events": []
        }))
        .unwrap();
        let events: Vec<peko_protocol::channel::ChannelEvent> =
            decode_daemon_response(reply).unwrap();
        assert!(events.is_empty());
    }
}
