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
    /// Create a group chat started by one of your pekos.
    Create {
        /// The peko that starts the group.
        creator: String,
        /// The group's display name.
        name: String,
        /// Choose the group's id, e.g. `group:<slug>` for a named group.
        /// Omit for a fresh `chan_…` id.
        #[arg(long, value_name = "CHANNEL_ID")]
        id: Option<String>,
        /// Output the new channel id as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Add a peko or a person to a group.
    Invite {
        /// Channel id (`chan_xxxxxxxx`).
        channel: String,
        /// The peko doing the inviting (already a member).
        inviter: String,
        /// A peko on this computer by name, or a person's address such
        /// as `user:hub:<id>`.
        invitee: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Post a message to a group or DM.
    Post {
        /// Channel id.
        channel: String,
        /// A peko that is a member, or `me` to post as yourself (your
        /// PekoHub account when this computer is registered, else
        /// `user:local`). You can't post as another person.
        sender: String,
        /// Message text.
        text: String,
        /// Reply to this message id.
        #[arg(long)]
        parent: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Read a group or DM, from the start or after a message.
    Peek {
        /// Channel id.
        channel: String,
        /// Only messages after this one (`task_…`).
        #[arg(long)]
        since: Option<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List a group's members.
    Members {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// List the groups and DMs a peko is in.
    Ls {
        /// Peko name.
        #[arg(value_name = "PEKO")]
        principal: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Show a group: its name and members.
    Show {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Take one of your pekos out of a group.
    Leave {
        /// Channel id.
        channel: String,
        /// Peko name (must already be a member).
        #[arg(value_name = "PEKO")]
        principal: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Copy a channel into shared storage (maintenance).
    #[command(hide = true)]
    PinToShared {
        /// Channel id.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Open a DM with a peko on another computer, or with a person
    ///
    /// Name the peko by its ID or PekoHub ID, or a person by
    /// `user:hub:<account>` (they get a message request first). Add a
    /// message to send it right away. Read replies with
    /// `peko channel peek <dm>`; post with `peko channel post <dm> me`.
    Dm {
        /// `@owner/name` (a public peko), `peko:hub:<UUID>`, the bare
        /// PekoHub ID, or a person as `user:hub:<account>`.
        peko: String,
        /// Post this message right away.
        message: Option<String>,
        /// Attach a file (repeatable): at most 10 MB a file and 25 MB in
        /// all, since it travels through PekoHub.
        #[arg(long = "attach", value_name = "PATH")]
        attach: Vec<String>,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Groups on other computers that invited you.
    Invitations {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Join a group: accept its invitation, or use an invite link (or a
    /// public group's ID) from PekoHub.
    Join {
        /// Channel id from `peko channel invitations`, an invite link, or a
        /// public group's ID.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Decline a group's invitation, or a person's message request.
    Decline {
        /// Channel id from `peko channel invitations` or `peko channel requests`.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// People asking to DM you.
    Requests {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Accept a person's message request: the DM opens here and they are
    /// told; what they wrote arrives.
    Accept {
        /// Channel id from `peko channel requests`.
        channel: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Block a person: their requests, messages, and history requests are
    /// refused on every computer of your account.
    Block {
        /// A request's channel id, `user:hub:<account>`, or the account ID.
        who: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Unblock a person.
    Unblock {
        /// `user:hub:<account>` or the account ID.
        who: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// The people you blocked.
    Blocked {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Give a group an ID on PekoHub, or change who may find it.
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
            | ChannelCommands::Requests { json }
            | ChannelCommands::Accept { json, .. }
            | ChannelCommands::Block { json, .. }
            | ChannelCommands::Unblock { json, .. }
            | ChannelCommands::Blocked { json }
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
                println!(
                    "invited {} to {}",
                    super::shown_address(&invitee_id.to_string()),
                    ch
                );
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
                             from the connection. Use `me` to post as yourself, or the \
                             name of one of your pekos to post as it."
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
                attachments: Vec::new(),
                model: None,
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
                    println!("  {}", super::shown_channel_event(ev));
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
                    println!("  {}", super::shown_address(&m.to_string()));
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
            // The daemon knows the group's name, kind, roster, and each
            // local peko's wake setting; without one, read the roster
            // from disk.
            if let Ok(client) = DaemonClient::connect().await {
                let packet = RequestPacket::ChannelInfo {
                    request_id: 0,
                    channel: channel.clone(),
                };
                let resp = client
                    .request_response(packet)
                    .await
                    .context("channel_show_failed")?;
                return print_channel_info(resp, json);
            }
            let port: Arc<dyn peko_channel::ChannelPort> =
                Arc::new(ChannelStore::new(ChannelConfig {
                    runtime_dir: paths.runtime_dir(),
                    shared_dir: Some(paths.principals_root_dir()),
                }));
            let ch = parse_channel_id(&channel)?;
            let membership = ChannelCliRouter::new(port)
                .handle_show(&ch)
                .await
                .context("channel_show_failed")?;
            if json {
                println!("{}", serde_json::to_string_pretty(&membership)?);
            } else {
                println!("{} ({})", membership.name, membership.channel);
                println!("{} member(s):", membership.members.len());
                for m in &membership.members {
                    println!("  {}", super::shown_address(m));
                }
            }
            Ok(())
        }
        ChannelCommands::Leave {
            channel, principal, ..
        } => {
            let name = principal.clone();
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
                println!("{name} left {ch}");
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

        ChannelCommands::Dm {
            peko,
            message,
            attach,
            ..
        } => {
            let attachments = crate::commands::send::absolute_attachments(&attach)?;
            let (channel, peer, name, created) =
                if let Some(account) = peko.strip_prefix("user:hub:") {
                    // ADR-095 D6: a person; they get a message request.
                    let resp = dm_request(RequestPacket::ChannelOpenPersonDm {
                        request_id: 0,
                        account: account.to_string(),
                        name: None,
                    })
                    .await?;
                    let ResponsePacket::ChannelPersonDmOpened {
                        channel,
                        peer,
                        created,
                        ..
                    } = resp
                    else {
                        anyhow::bail!("unexpected reply: {resp:?}");
                    };
                    (channel, peer, None, created)
                } else {
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
                    (channel, peer, name, created)
                };
            let channel = channel.to_string();
            let message = match message {
                None if !attachments.is_empty() => Some(String::new()),
                other => other,
            };
            let posted = match message {
                Some(text) => {
                    dm_request(RequestPacket::ChannelPost {
                        request_id: 0,
                        channel: channel.clone(),
                        sender_name: None,
                        text,
                        parent: None,
                        attachments,
                        model: None,
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
        ChannelCommands::Decline { channel, .. } if channel.starts_with("dm_") => {
            answer_dm_request(&channel, "decline", json).await
        }
        ChannelCommands::Decline { channel, .. } => answer_invitation(&channel, false, json).await,
        ChannelCommands::Accept { channel, .. } => {
            answer_dm_request(&channel, "accept", json).await
        }
        ChannelCommands::Requests { .. } => {
            let resp = dm_request(RequestPacket::ChannelDmRequests { request_id: 0 }).await?;
            let ResponsePacket::ChannelDmRequestsResult { requests, .. } = resp else {
                anyhow::bail!("unexpected reply: {resp:?}");
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&requests)?);
            } else if requests.is_empty() {
                println!("No message requests.");
            } else {
                for r in &requests {
                    match &r.name {
                        Some(name) => println!("{}  from {name}  ({})", r.channel, r.from),
                        None => println!("{}  from {}", r.channel, r.from),
                    }
                }
                println!(
                    "\nAccept with `peko channel accept <channel>`, or `peko channel decline <channel>` \
                     / `peko channel block <channel>`."
                );
            }
            Ok(())
        }
        ChannelCommands::Block { who, .. } if who.starts_with("dm_") => {
            answer_dm_request(&who, "block", json).await
        }
        ChannelCommands::Block { who, .. } => set_blocked(&who, true, json).await,
        ChannelCommands::Unblock { who, .. } => set_blocked(&who, false, json).await,
        ChannelCommands::Blocked { .. } => {
            let resp = dm_request(RequestPacket::ChannelBlocked { request_id: 0 }).await?;
            print_blocked(resp, json)
        }
    }
}

/// Answer a person's message request (ADR-095 D6).
async fn answer_dm_request(channel: &str, answer: &str, json: bool) -> Result<()> {
    let resp = dm_request(RequestPacket::ChannelDmRequestAnswer {
        request_id: 0,
        channel: channel.to_string(),
        answer: answer.to_string(),
    })
    .await?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "channel": channel, "answer": answer })
        );
        return Ok(());
    }
    match (answer, resp) {
        ("accept", ResponsePacket::ChannelPersonDmOpened { channel, peer, .. }) => println!(
            "Accepted. Your DM with {peer}: {channel}. What they wrote arrives shortly; \
             post with `peko channel post {channel} me \"…\"`."
        ),
        ("block", _) => println!("Blocked. Nothing from them reaches you now."),
        _ => println!("Declined {channel}."),
    }
    Ok(())
}

/// Block or unblock a person by account (ADR-095 D6).
async fn set_blocked(who: &str, blocked: bool, json: bool) -> Result<()> {
    let account = who.strip_prefix("user:hub:").unwrap_or(who);
    let resp = dm_request(RequestPacket::ChannelBlockPerson {
        request_id: 0,
        account: account.to_string(),
        blocked,
    })
    .await?;
    print_blocked(resp, json)
}

fn print_blocked(resp: ResponsePacket, json: bool) -> Result<()> {
    let ResponsePacket::ChannelBlockedPeople { people, names, .. } = resp else {
        anyhow::bail!("unexpected reply: {resp:?}");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&people)?);
    } else if people.is_empty() {
        println!("You have blocked no one.");
    } else {
        println!("Blocked:");
        for person in &people {
            match names.get(person) {
                Some(name) => println!("  {name}  ({person})"),
                None => println!("  {person}"),
            }
        }
    }
    Ok(())
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

/// The PekoHub ID of the peko `target` names: `peko:hub:<UUID>` (or the
/// wire spelling `principal:hub:<UUID>`), a
/// bare UUID, or `@owner/name` — a public peko, looked up on the Hub this
/// runtime is registered with (`GET /v1/public/pekos/:owner/:name`).
async fn remote_peko_hub_id(paths: &GlobalPaths, target: &str) -> Result<String> {
    let target = target.trim();
    let bare = target
        .strip_prefix("peko:hub:")
        .or_else(|| target.strip_prefix("principal:hub:"))
        .unwrap_or(target);
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

/// Print a `ChannelInfo` reply for `peko channel show`.
fn print_channel_info(resp: ResponsePacket, json: bool) -> Result<()> {
    match resp {
        ResponsePacket::ChannelInfoResult {
            channel,
            kind,
            name,
            members,
            pekos,
            ..
        } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "channel": channel,
                        "kind": kind,
                        "name": name,
                        "members": members,
                        "pekos": pekos,
                    }))?
                );
                return Ok(());
            }
            let kind = match kind {
                peko_protocol::channel::ConversationKind::Dm => "DM",
                peko_protocol::channel::ConversationKind::Group => "group",
            };
            println!("{name} ({kind}, {channel})");
            println!("{} member(s):", members.len());
            for m in &members {
                println!("  {}", super::shown_address(&m.to_string()));
            }
            for peko in &pekos {
                let wake = match peko.wake {
                    peko_channel::WakePolicy::Human => "wakes on every post from a person",
                    peko_channel::WakePolicy::Mention => "wakes when @mentioned",
                    peko_channel::WakePolicy::Off => "does not wake",
                };
                println!("  {}: {wake}", peko.name);
            }
            Ok(())
        }
        ResponsePacket::Error { message, .. } => {
            anyhow::bail!("channel_show_failed: {message}")
        }
        other => anyhow::bail!(
            "channel_show_failed: unexpected reply {}",
            other.variant_name()
        ),
    }
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
