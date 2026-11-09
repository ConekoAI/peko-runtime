//! The local user's DM with a principal hosted on another runtime
//! (ADR-090). A child module of `tunnel_channel_port` so it shares the
//! port's outbox and routing state.
//!
//! - [`TunnelChannelPort::open_hub_dm`] (D2) resolves the peko, find-or-
//!   creates the DM on this runtime, and queues the invite.
//! - The invite (D3) is the user's: runtime-signed, no author signature,
//!   creator `user:hub:<account>`. The receiver accepts it only in that
//!   shape (`dispatcher::is_human_dm_invite`).

use super::{OutboxItem, TunnelChannelPort};
use crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx;
use crate::tunnel::hub_directory::{DirectoryError, ResolvedExposure};
use peko_channel::{ActorInfo, ChannelId, ChannelPort, ConversationKind, Registration};
use peko_subject::{ActorRef, PrincipalId, Route};
use std::sync::Arc;

/// An open DM between the local user and a remote principal.
#[derive(Debug, Clone)]
pub struct HubDm {
    /// The pair channel (`dm_<sha256>`) on this runtime.
    pub channel: ChannelId,
    /// The remote principal, as this runtime's registry knows it.
    pub peer: ActorInfo,
    /// Whether this call created the channel.
    pub created: bool,
}

/// A peko on another runtime, as far as opening a DM with it needs.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPeko {
    pub hub_id: String,
    pub signing_did: String,
    pub identity_key: String,
    pub name: Option<String>,
    pub runtime_id: String,
    pub public: bool,
}

/// The pair id of a DM between Hub user `user_address`
/// (`user:hub:<account>`) and a principal with immutable key
/// `principal_key` (ADR-090 D1). Both runtimes derive it from the wire.
#[must_use]
pub fn human_dm_channel_id(principal_key: &str, user_address: &str) -> ChannelId {
    peko_channel::dm_channel_id(principal_key, user_address)
}

/// `@owner/name` as `(owner, name)`, the owner lowercased (PekoHub
/// handles are lowercase). `None` for anything else.
fn parse_handle(target: &str) -> Option<(String, &str)> {
    let (owner, name) = target.strip_prefix('@')?.split_once('/')?;
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(name)).then(|| (owner.to_ascii_lowercase(), name))
}

impl TunnelChannelPort {
    /// Open, or find, the local user's DM with a principal hosted on
    /// another runtime (ADR-090 D2), named by its Hub ID or as
    /// `@owner/name`. `bearer` is the user's PekoHub access token, used only
    /// to resolve a private peko and never stored. Without one, the
    /// runtime's own proof stands in for its person. By handle, PekoHub
    /// resolves a private peko only for its owner.
    ///
    /// # Errors
    ///
    /// The target is neither form; the runtime is unregistered or has no
    /// Hub connection set up; the directory refuses or does not know the
    /// peko; the peko is hosted here or local-only; or the directory
    /// cannot be reached and the DM was never opened (by handle, the DM
    /// can't be found offline).
    pub async fn open_hub_dm(&self, target: &str, bearer: Option<&str>) -> anyhow::Result<HubDm> {
        let target = target.trim();
        let handle = parse_handle(target);
        anyhow::ensure!(
            handle.is_some() || uuid::Uuid::parse_str(target).is_ok(),
            "Expected a peko's PekoHub ID (a UUID) or @owner/name, got {target:?}"
        );
        let user = self.local.actor_info(&ActorRef::User("local".into())).await;
        let user_address = user.hub_address().ok_or_else(|| {
            anyhow::anyhow!("Register this runtime with PekoHub first (peko runtime register)")
        })?;
        let ctx = self
            .ctx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This runtime has no PekoHub connection"))?;

        let lookup = match &handle {
            Some((owner, name)) => {
                ctx.directory
                    .resolve_by_handle_for_user(owner, name, bearer)
                    .await
            }
            None => {
                ctx.directory
                    .resolve_by_hub_id_for_user(target, bearer)
                    .await
            }
        };
        let resolved = match lookup {
            Ok(resolved) => resolved,
            // D2 step 4: offline, a DM opened before is still there.
            Err(DirectoryError::Transport(e)) if handle.is_none() => {
                tracing::warn!(hub_id = target, "PekoHub directory unreachable: {e}");
                let peer_subject = ActorRef::Principal(Route::hub(target.to_string()));
                return self
                    .known_hub_dm(&peer_subject, &user_address)
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "PekoHub cannot be reached, and no conversation with this peko \
                             was opened before"
                        )
                    });
            }
            Err(DirectoryError::Transport(e)) => {
                anyhow::bail!("PekoHub cannot be reached to look up {target}: {e}")
            }
            Err(DirectoryError::NotFound) if handle.is_some() => anyhow::bail!(
                "No peko {target} that you can reach on PekoHub. By name you find public pekos \
                 and your own; name anyone else's private peko by its PekoHub ID"
            ),
            Err(DirectoryError::NotFound) => anyhow::bail!("No peko has PekoHub ID {target}"),
            Err(DirectoryError::Forbidden) => {
                anyhow::bail!(
                    "PekoHub does not let you reach this peko (sign in, or ask its owner)"
                )
            }
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            handle.is_some() || resolved.hub_id == target,
            "Directory substituted a different Hub identity"
        );
        anyhow::ensure!(
            uuid::Uuid::parse_str(&resolved.hub_id).is_ok(),
            "Directory returned no PekoHub ID for {target}"
        );
        let hub_id = resolved.hub_id.clone();
        anyhow::ensure!(
            !resolved.agent_did.is_empty() && !resolved.identity_key.is_empty(),
            "Directory returned no signing identity for this peko"
        );
        anyhow::ensure!(
            resolved.runtime_id != ctx.caller_runtime_id,
            "This peko is hosted on this runtime; talk to it directly"
        );
        anyhow::ensure!(
            resolved.exposure != ResolvedExposure::LocalOnly,
            "This peko is local-only"
        );
        self.open_resolved_hub_dm(
            &ctx,
            &user,
            &ResolvedPeko {
                hub_id,
                signing_did: resolved.agent_did,
                identity_key: resolved.identity_key,
                name: resolved.name,
                runtime_id: resolved.runtime_id,
                public: resolved.exposure == ResolvedExposure::Public,
            },
        )
        .await
    }

    /// Open, or find, the local user's DM with a peko already resolved —
    /// by the directory (D2), or by another of the person's runtimes
    /// (ADR-095 D5) — and queue the invite where none reached its runtime.
    pub(crate) async fn open_resolved_hub_dm(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        user: &ActorInfo,
        resolved: &ResolvedPeko,
    ) -> anyhow::Result<HubDm> {
        let hub_id = resolved.hub_id.as_str();
        let peer_subject = ActorRef::Principal(Route::hub(hub_id.to_string()));
        let user_address = user.hub_address().ok_or_else(|| {
            anyhow::anyhow!("Register this runtime with PekoHub first (peko runtime register)")
        })?;
        self.local
            .register_actor(
                &peer_subject,
                Some(hub_id),
                Some(&resolved.signing_did),
                Some(&resolved.identity_key),
                resolved.name.as_deref(),
                if resolved.public {
                    Registration::Public
                } else {
                    Registration::Private
                },
            )
            .await?;

        let channel = human_dm_channel_id(&resolved.identity_key, &user_address);
        let name = format!("dm-{}", resolved.name.as_deref().unwrap_or(hub_id));
        let account = user
            .hub_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Register this runtime with PekoHub first"))?;
        let created = self
            .local
            .ensure_human_dm(
                &ActorRef::User("local".into()),
                &peer_subject,
                &channel,
                &name,
                &account,
            )
            .await?;
        // Invite on first contact, and again when the peko's runtime
        // changed (ADR-081 D7): its new runtime has no mirror yet.
        let routed = self
            .local
            .list_remote_members(&channel)
            .await?
            .iter()
            .any(|m| m.principal_id == resolved.signing_did && m.runtime_id == resolved.runtime_id);
        if !routed {
            self.fanout_user_dm_invite(
                ctx,
                &channel,
                &PrincipalId(resolved.signing_did.clone()),
                &resolved.runtime_id,
            )
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        }
        Ok(HubDm {
            channel,
            peer: self.local.actor_info(&peer_subject).await,
            created,
        })
    }

    /// The DM opened before with `peer`, from the registry alone.
    async fn known_hub_dm(&self, peer: &ActorRef, user_address: &str) -> Option<HubDm> {
        let known = self.local.actor_info(peer).await;
        if known.identity_key == known.address {
            return None;
        }
        let channel = human_dm_channel_id(&known.identity_key, user_address);
        (self.local.conversation_kind(&channel).await.ok()? == ConversationKind::Dm).then_some(
            HubDm {
                channel,
                peer: known,
                created: false,
            },
        )
    }

    /// Route `channel` to `invitee` on `runtime`, moving envelopes still
    /// pending for its old runtime, and queue the user's invite there.
    async fn fanout_user_dm_invite(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        channel: &ChannelId,
        invitee: &PrincipalId,
        runtime: &str,
    ) -> Result<(), String> {
        let old = self
            .local
            .replace_peer_route(channel, &invitee.0, runtime)
            .await
            .map_err(|e| e.to_string())?;
        for previous in old {
            self.outbox
                .retarget(channel, &previous, runtime)
                .await
                .map_err(|e| e.to_string())?;
        }
        self.send_queued(
            ctx,
            runtime,
            OutboxItem::UserInvite {
                channel: channel.clone(),
                invitee: invitee.clone(),
            },
        )
        .await
    }

    /// Build and sign the user's `TunnelChannelInvite` for `invitee`
    /// (ADR-090 D3): creator, creator DID, and source are the user's Hub
    /// address; two members; no author signature.
    pub(super) async fn user_invite_envelope(
        &self,
        ctx: &Arc<CrossRuntimeChannelCtx>,
        request_id: &str,
        channel: &ChannelId,
        invitee: &PrincipalId,
        invitee_runtime_id: &str,
    ) -> Result<crate::tunnel::TunnelMessage, String> {
        let user = self.local.actor_info(&ActorRef::User("local".into())).await;
        let creator = user
            .hub_address()
            .ok_or_else(|| "Human sender has no Hub identity".to_string())?;
        let kind = self
            .local
            .conversation_kind(channel)
            .await
            .map_err(|e| format!("read channel kind (channel={channel}): {e}"))?;
        if kind != ConversationKind::Dm {
            return Err(format!("{channel} is not a DM"));
        }
        let name = self
            .local
            .channel_name(channel)
            .await
            .map_err(|e| format!("read channel name (channel={channel}): {e}"))?;
        let peer = self
            .local
            .actor_info(&ActorRef::Principal(invitee.0.clone().into()))
            .await;
        if peer.hub_id.is_none() {
            return Err("The invited peko has no Hub identity".into());
        }
        let initial_members = vec![
            peko_protocol::channel::InitialMember {
                principal_did: creator.clone(),
                runtime_id: Some(ctx.caller_runtime_id.clone()),
                hub_id: user.hub_id.clone(),
                identity_key: Some(creator.clone()),
                name: None,
            },
            peko_protocol::channel::InitialMember {
                principal_did: invitee.0.clone(),
                runtime_id: None,
                hub_id: peer.hub_id.clone(),
                identity_key: Some(peer.identity_key.clone()),
                name: peer.name.clone(),
            },
        ];
        let initial_members_bytes = serde_json::to_vec(&initial_members)
            .map_err(|e| format!("serialize initial_members for invite envelope: {e}"))?;
        let signed = crate::tunnel::ChannelInviteSignedFields {
            request_id,
            source_runtime_id: &ctx.caller_runtime_id,
            recipient_runtime_id: invitee_runtime_id,
            source_principal_did: &creator,
            channel_id: channel.as_str(),
            creator: &creator,
            creator_did: &creator,
            name: &name,
            kind: kind.as_str(),
            initial_members_bytes: &initial_members_bytes,
            created_at: None,
        };
        let iat = chrono::Utc::now().timestamp();
        let (signature, author_signature) =
            crate::tunnel::sign_channel_invite(&ctx.signing_key, None, signed, iat, iat + 300);
        crate::tunnel::emit_forwarded_outbound(
            request_id,
            &ctx.caller_runtime_id,
            &creator,
            channel.as_str(),
            "channel_invite",
            &crate::tunnel::tunnel_channel_audit::preview_event_payload(&String::from_utf8_lossy(
                &initial_members_bytes,
            )),
        );
        Ok(crate::tunnel::TunnelMessage::TunnelChannelInvite {
            request_id: request_id.to_string(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: invitee_runtime_id.to_string(),
            source_principal_did: creator.clone(),
            channel_id: channel.as_str().to_string(),
            creator: creator.clone(),
            creator_did: creator,
            name,
            kind,
            initial_members,
            signature,
            author_signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunnel::hub_directory::{AgentResolution, DirectoryErrorKind, FakeAgentDirectory};
    use crate::tunnel::{TunnelHandle, TunnelMessage};
    use peko_channel::{ChannelConfig, ChannelStore};

    const RUNTIME: &str = "did:key:zUserRuntime";

    struct Fixture {
        _dir: tempfile::TempDir,
        port: TunnelChannelPort,
        directory: Arc<FakeAgentDirectory>,
        sent: tokio::sync::mpsc::Receiver<TunnelMessage>,
        hub_id: String,
        account: String,
    }

    fn resolution(hub_id: &str, runtime: &str) -> AgentResolution {
        AgentResolution {
            runtime_id: runtime.into(),
            instance_id: "instance".into(),
            name: Some("peko".into()),
            hub_id: hub_id.into(),
            local_id: "prin_peko".into(),
            identity_key: "peko-key".into(),
            agent_did: "did:key:zPeko".into(),
            owner_principal: ActorRef::Public,
            owner_handle: None,
            exposure: ResolvedExposure::Public,
        }
    }

    async fn fixture(registered: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().into(),
            shared_dir: None,
        }));
        let account = uuid::Uuid::new_v4().to_string();
        store.ensure_local_user(RUNTIME).await.unwrap();
        if registered {
            store.link_local_user(RUNTIME, &account).await.unwrap();
        }
        let port = TunnelChannelPort::new(store);
        let directory = Arc::new(FakeAgentDirectory::new());
        let hub_id = uuid::Uuid::new_v4().to_string();
        directory.register_hub_id(&hub_id, resolution(&hub_id, "did:key:zPekoRuntime"));
        let (tx, sent) = tokio::sync::mpsc::channel(16);
        port.set_ctx(Arc::new(CrossRuntimeChannelCtx {
            directory: directory.clone(),
            signing_key: Arc::new(ed25519_dalek::SigningKey::from_bytes(&[7; 32])),
            caller_runtime_id: RUNTIME.into(),
            tunnel: Arc::new(tokio::sync::RwLock::new(Some(TunnelHandle::new(tx)))),
            principal_keys: Arc::new(crate::tunnel::cross_runtime_channel::NoPrincipalKeys),
        }))
        .await;
        Fixture {
            _dir: dir,
            port,
            directory,
            sent,
            hub_id,
            account,
        }
    }

    fn invites(rx: &mut tokio::sync::mpsc::Receiver<TunnelMessage>) -> Vec<TunnelMessage> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter(|m| matches!(m, TunnelMessage::TunnelChannelInvite { .. }))
            .collect()
    }

    /// D1–D3: the DM has the user and the peko as members, its pair id
    /// is the wire's, and the invite is the user's — two members, no
    /// author signature.
    #[tokio::test]
    async fn opening_invites_the_peko_as_the_user() {
        let mut f = fixture(true).await;
        let dm = f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        let user = format!("user:hub:{}", f.account);
        assert!(dm.created);
        assert_eq!(dm.channel, human_dm_channel_id("peko-key", &user));
        assert_eq!(
            f.port.local().conversation_kind(&dm.channel).await.unwrap(),
            ConversationKind::Dm
        );
        let members = f.port.local().list_members(&dm.channel).await.unwrap();
        assert!(members.contains(&ActorRef::User("local".into())));
        assert!(members.contains(&ActorRef::Principal(Route::hub(f.hub_id.clone()))));
        let sent = invites(&mut f.sent);
        let [TunnelMessage::TunnelChannelInvite {
            recipient_runtime_id,
            source_principal_did,
            creator,
            creator_did,
            kind,
            initial_members,
            author_signature,
            ..
        }] = sent.as_slice()
        else {
            panic!("expected one invite, got {sent:?}");
        };
        assert_eq!(recipient_runtime_id, "did:key:zPekoRuntime");
        assert!(creator == &user && creator_did == &user && source_principal_did == &user);
        assert_eq!(*kind, ConversationKind::Dm);
        assert!(author_signature.is_empty());
        assert_eq!(initial_members.len(), 2);
        assert_eq!(initial_members[0].runtime_id.as_deref(), Some(RUNTIME));
        assert_eq!(initial_members[1].principal_did, "did:key:zPeko");
        assert_eq!(initial_members[1].runtime_id, None);
    }

    /// D2 step 3: reopening sends nothing new, unless the peko moved —
    /// then its route follows and its new runtime is invited.
    #[tokio::test]
    async fn a_moved_peko_is_invited_on_its_new_runtime() {
        let mut f = fixture(true).await;
        let first = f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        invites(&mut f.sent);
        let again = f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        assert!(again.channel == first.channel && !again.created);
        assert!(invites(&mut f.sent).is_empty());

        f.directory
            .register_hub_id(&f.hub_id, resolution(&f.hub_id, "did:key:zNewHome"));
        f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        let sent = invites(&mut f.sent);
        assert!(matches!(
            sent.as_slice(),
            [TunnelMessage::TunnelChannelInvite { recipient_runtime_id, .. }]
                if recipient_runtime_id == "did:key:zNewHome"
        ));
        let rows = f
            .port
            .local()
            .list_remote_members(&first.channel)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].runtime_id, "did:key:zNewHome");
    }

    #[tokio::test]
    async fn refusals_say_what_is_missing() {
        let f = fixture(false).await;
        let error = f.port.open_hub_dm(&f.hub_id, None).await.unwrap_err();
        assert!(
            error.to_string().contains("peko runtime register"),
            "{error}"
        );

        let f = fixture(true).await;
        let error = f.port.open_hub_dm("not-a-uuid", None).await.unwrap_err();
        assert!(error.to_string().contains("UUID"), "{error}");

        // Hosted here.
        f.directory
            .register_hub_id(&f.hub_id, resolution(&f.hub_id, RUNTIME));
        let error = f.port.open_hub_dm(&f.hub_id, None).await.unwrap_err();
        assert!(
            error.to_string().contains("hosted on this runtime"),
            "{error}"
        );

        // Private and not reachable for this user.
        f.directory
            .register_hub_id_err(&f.hub_id, DirectoryErrorKind::Forbidden);
        let error = f.port.open_hub_dm(&f.hub_id, None).await.unwrap_err();
        assert!(error.to_string().contains("does not let you"), "{error}");

        // D2 step 4: offline, a DM never opened cannot be.
        f.directory
            .register_hub_id_err(&f.hub_id, DirectoryErrorKind::Transport("down".into()));
        let error = f.port.open_hub_dm(&f.hub_id, None).await.unwrap_err();
        assert!(error.to_string().contains("cannot be reached"), "{error}");
    }

    /// A post sent with a DM's invite may reach the peko's runtime before
    /// the invite is applied, and go unanswered there. When the invite is
    /// confirmed, the channel's waiting posts go again at once, not after
    /// their backoff.
    #[tokio::test]
    async fn a_confirmed_invite_sends_its_channels_posts_again() {
        use peko_channel::port::PostMsg;
        let mut f = fixture(true).await;
        let dm = f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        f.port
            .post(
                &dm.channel,
                &ActorRef::User("local".into()),
                PostMsg::root("hi"),
            )
            .await
            .unwrap();
        let sent: Vec<TunnelMessage> = std::iter::from_fn(|| f.sent.try_recv().ok()).collect();
        let invite = sent
            .iter()
            .find_map(|m| match m {
                TunnelMessage::TunnelChannelInvite { request_id, .. } => Some(request_id.clone()),
                _ => None,
            })
            .expect("the invite went out");
        let post_sends = |sent: &[TunnelMessage]| {
            sent.iter()
                .filter(|m| matches!(m, TunnelMessage::TunnelChannelEvent { .. }))
                .count()
        };
        assert_eq!(
            post_sends(&sent),
            1,
            "the post went out once with the invite"
        );

        // A receipt for something else resends nothing.
        f.port
            .confirm_delivery("did:key:zPekoRuntime", &["unrelated".to_string()])
            .await;
        assert!(f.sent.try_recv().is_err());

        f.port
            .confirm_delivery("did:key:zPekoRuntime", std::slice::from_ref(&invite))
            .await;
        let again: Vec<TunnelMessage> = std::iter::from_fn(|| f.sent.try_recv().ok()).collect();
        assert_eq!(
            post_sends(&again),
            1,
            "the waiting post goes again: {again:?}"
        );
        assert!(invites(&mut f.sent).is_empty());
    }

    /// `@owner/name` resolves by handle (the owner lowercased), opens the
    /// same DM the Hub ID does, and refuses what PekoHub doesn't resolve.
    #[tokio::test]
    async fn a_peko_is_named_by_its_owner_and_name() {
        let mut f = fixture(true).await;
        f.directory.register_handle(
            "maya",
            "juno",
            resolution(&f.hub_id, "did:key:zPekoRuntime"),
        );
        let by_name = f.port.open_hub_dm("@Maya/juno", None).await.unwrap();
        assert!(by_name.created);
        assert_eq!(by_name.peer.hub_id.as_deref(), Some(f.hub_id.as_str()));
        assert_eq!(invites(&mut f.sent).len(), 1);
        let by_id = f.port.open_hub_dm(&f.hub_id, None).await.unwrap();
        assert_eq!(by_id.channel, by_name.channel);
        assert!(!by_id.created);

        // Not resolvable for this person (someone else's private peko).
        let error = f.port.open_hub_dm("@noah/otto", None).await.unwrap_err();
        assert!(error.to_string().contains("by its PekoHub ID"), "{error}");
        // Offline there is no ID to find a DM by.
        f.directory.register_handle_err(
            "maya",
            "juno",
            DirectoryErrorKind::Transport("down".into()),
        );
        let error = f.port.open_hub_dm("@maya/juno", None).await.unwrap_err();
        assert!(error.to_string().contains("cannot be reached"), "{error}");
        for bad in ["@maya", "@/juno", "@maya/", "@ma ya/juno"] {
            let error = f.port.open_hub_dm(bad, None).await.unwrap_err();
            assert!(error.to_string().contains("@owner/name"), "{bad}: {error}");
        }
    }
}
