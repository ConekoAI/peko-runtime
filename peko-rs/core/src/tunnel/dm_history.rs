//! A person's history with a remote peko comes from its host (ADR-091).
//! A child module of `tunnel_channel_port` so it shares the port's store
//! and tunnel context.
//!
//! - The person's side asks ([`TunnelChannelPort::sync_history`] on
//!   opening and catching up, [`TunnelChannelPort::history_older`] for
//!   scroll-back) and accepts only answers it is waiting for.
//! - The host answers ([`TunnelChannelPort::answer_history`]) once the
//!   dispatcher's D3 gate has passed, with host-local fields removed.

use super::TunnelChannelPort;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_history, ChannelHistoryPayload,
    ChannelHistoryRequestPayload, TunnelHandle, TunnelMessage, CHANNEL_HISTORY_REQUEST_VERSION,
    CHANNEL_HISTORY_VERSION,
};
use peko_channel::{ChannelId, ChannelPort, Checkpoint};
use peko_protocol::channel::ChannelEvent;
use peko_subject::ActorRef;
use std::time::Duration;
use tracing::{debug, warn};

/// Most posts in one page.
pub const HISTORY_PAGE_LIMIT: u32 = 100;
/// How long the person's side waits for the host's answer.
const HISTORY_WAIT: Duration = Duration::from_secs(15);
/// Most pages one catch-up fetches.
const CATCH_UP_PAGES: usize = 20;

/// One page of a DM from its host: posts, oldest first.
#[derive(Debug, Clone, Default)]
pub struct HistoryPage {
    pub events: Vec<ChannelEvent>,
    /// More posts lie beyond the page in the direction asked.
    pub has_more: bool,
}

/// A request waiting for its answer.
pub(super) struct PendingHistory {
    channel: ChannelId,
    host: String,
    reply: tokio::sync::oneshot::Sender<HistoryPage>,
}

impl TunnelChannelPort {
    /// The account this runtime's local user is registered under.
    async fn current_account(&self) -> Option<String> {
        self.local
            .actor_info(&ActorRef::User("local".into()))
            .await
            .hub_id
    }

    /// Who asks for `channel`'s history, and which runtime holds it: this
    /// runtime's person for their DM with a remote peko (its host) or a
    /// group they joined (its home, ADR-092 D3); else a peko hosted here
    /// that is a member of a group whose home is elsewhere, by its signing
    /// DID.
    async fn history_requester(
        &self,
        ctx: &crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx,
        channel: &ChannelId,
    ) -> anyhow::Result<(String, String)> {
        let home = self.local.group_home(channel).await?.map(|h| h.runtime);
        let account = self.current_account().await;
        let owner = self.local.remote_dm_account(channel).await?;
        if owner.is_some() {
            let account = account
                .filter(|a| owner.as_deref() == Some(a.as_str()))
                .ok_or_else(|| anyhow::anyhow!("{channel} is not your conversation"))?;
            let host = match home {
                Some(home) => home,
                None => self
                    .local
                    .list_remote_members(channel)
                    .await?
                    .into_iter()
                    .next()
                    .map(|m| m.runtime_id)
                    .ok_or_else(|| anyhow::anyhow!("No runtime is known to host this peko"))?,
            };
            return Ok((format!("user:hub:{account}"), host));
        }
        let home = home
            .filter(|h| h != &ctx.caller_runtime_id)
            .ok_or_else(|| {
                anyhow::anyhow!("{channel} has no history to ask another runtime for")
            })?;
        for member in self.local.list_members(channel).await? {
            if let ActorRef::Principal(id) = &member {
                let pid = peko_subject::PrincipalId::from_did(id);
                if let Some(did) = ctx.principal_keys.did_for_principal(&pid).await {
                    return Ok((did, home));
                }
            }
        }
        anyhow::bail!("no member here can ask for {channel}'s history")
    }

    /// Ask `channel`'s host for one page; the receiver yields the answer.
    async fn ask_history(
        &self,
        channel: &ChannelId,
        before: Option<&str>,
        after: Option<&str>,
    ) -> anyhow::Result<(tokio::sync::oneshot::Receiver<HistoryPage>, String)> {
        let ctx = self
            .ctx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This runtime has no PekoHub connection"))?;
        let (person, host) = self.history_requester(&ctx, channel).await?;
        let handle = ctx
            .tunnel
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to PekoHub"))?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = ChannelHistoryRequestPayload {
            v: CHANNEL_HISTORY_REQUEST_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: host.clone(),
            source_principal_did: person.clone(),
            channel_id: channel.as_str().to_string(),
            before: before.map(str::to_string),
            after: after.map(str::to_string),
            limit: HISTORY_PAGE_LIMIT,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.history_pending
            .lock()
            .expect("history pending")
            .insert(
                request_id.clone(),
                PendingHistory {
                    channel: channel.clone(),
                    host: host.clone(),
                    reply,
                },
            );
        let sent = handle.send(TunnelMessage::TunnelChannelHistoryRequest {
            request_id: request_id.clone(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: host.clone(),
            source_principal_did: person,
            channel_id: payload.channel_id,
            before: payload.before,
            after: payload.after,
            limit: payload.limit,
            signature,
        });
        if let Err(e) = sent {
            self.history_pending
                .lock()
                .expect("history pending")
                .remove(&request_id);
            anyhow::bail!("Not connected to PekoHub: {e}");
        }
        Ok((answer, host))
    }

    /// Wait for an answer, forgetting the request when none comes.
    async fn await_history(
        &self,
        answer: tokio::sync::oneshot::Receiver<HistoryPage>,
    ) -> anyhow::Result<HistoryPage> {
        match tokio::time::timeout(HISTORY_WAIT, answer).await {
            Ok(Ok(page)) => Ok(page),
            _ => {
                self.history_pending
                    .lock()
                    .expect("history pending")
                    .retain(|_, p| !p.reply.is_closed());
                anyhow::bail!("The computer hosting this peko did not answer; it may be offline")
            }
        }
    }

    /// Whether `channel` is a DM with a remote peko opened under another
    /// Hub account than this runtime's (ADR-091 D5).
    pub async fn belongs_to_another_account(&self, channel: &ChannelId) -> bool {
        match self.local.remote_dm_account(channel).await {
            Ok(Some(owner)) => self.current_account().await.as_deref() != Some(owner.as_str()),
            _ => false,
        }
    }

    /// Scroll-back (ADR-091 D4): the page of posts before `before`, from
    /// the host. Not stored: the device keeps only what it caught up on.
    ///
    /// # Errors
    ///
    /// The DM is not this account's, or the host does not answer.
    pub async fn history_older(
        &self,
        channel: &ChannelId,
        before: &str,
    ) -> anyhow::Result<HistoryPage> {
        let (answer, _) = self.ask_history(channel, Some(before), None).await?;
        let page = self.await_history(answer).await?;
        Ok(HistoryPage {
            events: page
                .events
                .into_iter()
                .map(|ev| self.local.normalized(ev))
                .collect(),
            has_more: page.has_more,
        })
    }

    /// Catch up (ADR-091 D4): store what the host has that this copy
    /// lacks. With no post here yet, the newest page; otherwise every
    /// page after the newest post held. Returns how many were added.
    ///
    /// # Errors
    ///
    /// The DM is not this account's, or the host does not answer.
    pub async fn sync_history(&self, channel: &ChannelId) -> anyhow::Result<usize> {
        let mut after = self
            .local
            .peek(channel, &Checkpoint::zero())
            .await?
            .iter()
            .rev()
            .find_map(|ev| match ev {
                ChannelEvent::Posted { msg_id, .. } => msg_id.clone(),
                _ => None,
            });
        let mut added = 0;
        for _ in 0..CATCH_UP_PAGES {
            let (answer, host) = self.ask_history(channel, None, after.as_deref()).await?;
            let page = self.await_history(answer).await?;
            let last = page.events.iter().rev().find_map(|ev| match ev {
                ChannelEvent::Posted { msg_id, .. } => msg_id.clone(),
                _ => None,
            });
            // ADR-094: the files these posts name come from the host too.
            let files: Vec<_> = page
                .events
                .iter()
                .filter_map(|ev| match ev {
                    ChannelEvent::Posted { attachments, .. }
                        if attachments.iter().any(|a| a.sha256.is_some()) =>
                    {
                        Some(attachments.clone())
                    }
                    _ => None,
                })
                .collect();
            if !files.is_empty() {
                let port = self.clone();
                let channel = channel.clone();
                tokio::spawn(async move {
                    for attachments in files {
                        port.fetch_attachments(&channel, &attachments, &host).await;
                    }
                });
            }
            added += self
                .local
                .append_synced_events(channel, page.events)
                .await?;
            // The newest page answers a fresh copy; older pages are
            // scroll-back. After a cursor, keep reading forward.
            if after.is_none() || !page.has_more || last.is_none() {
                break;
            }
            after = last;
        }
        Ok(added)
    }

    /// Catch up every remote DM of this account in the background, or
    /// only those whose peko `host` serves (ADR-091 D4).
    pub(crate) async fn sync_remote_dms(self: &std::sync::Arc<Self>, host: Option<&str>) {
        let Some(account) = self.current_account().await else {
            return;
        };
        let Ok(mut channels) = self.local.remote_dms(&account).await else {
            return;
        };
        // ADR-092 D3: the groups this person joined catch up from their homes.
        if let Ok(groups) = self.local.person_groups(&account).await {
            channels.extend(groups.into_iter().map(|(channel, _, _)| channel));
        }
        for channel in channels {
            if let Some(host) = host {
                let hosted = self
                    .local
                    .list_remote_members(&channel)
                    .await
                    .is_ok_and(|members| members.iter().any(|m| m.runtime_id == host));
                if !hosted {
                    continue;
                }
            }
            let port = std::sync::Arc::clone(self);
            tokio::spawn(async move {
                match port.sync_history(&channel).await {
                    Ok(added) if added > 0 => debug!(%channel, added, "remote DM caught up"),
                    Ok(_) => {}
                    Err(e) => debug!(%channel, "remote DM catch-up skipped: {e}"),
                }
            });
        }
    }

    /// The person's side receives a page: accept it only as the answer to
    /// a request of ours, from the runtime asked, for the channel asked.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_history(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        channel_id: String,
        events: Vec<ChannelEvent>,
        has_more: bool,
        signature: String,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let waiting = {
            let mut pending = self.history_pending.lock().expect("history pending");
            let matches = pending
                .get(&request_id)
                .is_some_and(|p| p.host == source_runtime_id && p.channel.as_str() == channel_id);
            if matches {
                pending.remove(&request_id)
            } else {
                None
            }
        };
        let Some(waiting) = waiting else {
            debug!(request_id, "unrequested history page dropped");
            return;
        };
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_history(&key, &signature));
        let events_bytes = serde_json::to_vec(&events).unwrap_or_default();
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.channel_id == channel_id
                && p.has_more == has_more
                && p.events_b64u == ChannelHistoryPayload::events_b64u_of(&events_bytes)
        });
        if !agrees {
            warn!(request_id, "history page failed verification; dropped");
            return;
        }
        let events = events
            .into_iter()
            .filter(|ev| matches!(ev, ChannelEvent::Posted { channel, .. } if channel.as_str() == channel_id))
            .collect();
        let _ = waiting.reply.send(HistoryPage { events, has_more });
    }

    /// The host answers a request its D3 gate passed (or refused, with an
    /// empty page): posts without host-local fields, signed.
    pub(crate) async fn answer_history(
        &self,
        handle: &TunnelHandle,
        request: &ChannelHistoryRequestPayload,
        page: HistoryPage,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let mut events = Vec::with_capacity(page.events.len());
        for event in page.events {
            events.push(self.served(event).await);
        }
        let events_bytes = serde_json::to_vec(&events).unwrap_or_default();
        let iat = chrono::Utc::now().timestamp();
        let payload = ChannelHistoryPayload {
            v: CHANNEL_HISTORY_VERSION.into(),
            request_id: request.request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: request.source_runtime_id.clone(),
            channel_id: request.channel_id.clone(),
            events_b64u: ChannelHistoryPayload::events_b64u_of(&events_bytes),
            has_more: page.has_more,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let _ = handle.send(TunnelMessage::TunnelChannelHistory {
            request_id: payload.request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            channel_id: payload.channel_id,
            events,
            has_more: payload.has_more,
            signature,
        });
    }

    /// One page of `channel` as the host serves it, for a client without
    /// a runtime (ADR-091 D6): host-local fields removed.
    pub(crate) async fn served_history_page(
        &self,
        channel: &ChannelId,
        before: Option<&str>,
        after: Option<&str>,
        limit: u32,
    ) -> HistoryPage {
        let page = self.history_page(channel, before, after, limit).await;
        let mut events = Vec::with_capacity(page.events.len());
        for event in page.events {
            events.push(self.served(event).await);
        }
        HistoryPage {
            events,
            has_more: page.has_more,
        }
    }

    /// One page of a group at its home (ADR-092 D3), from `since` when the
    /// member may read only what followed their joining.
    pub(crate) async fn group_history_page(
        &self,
        channel: &ChannelId,
        before: Option<&str>,
        after: Option<&str>,
        limit: u32,
        since: Option<&str>,
    ) -> HistoryPage {
        let Some(since) = since else {
            return self.history_page(channel, before, after, limit).await;
        };
        let limit = limit.clamp(1, HISTORY_PAGE_LIMIT) as usize;
        match self
            .local
            .history_page_since(channel, before, after, limit, since)
            .await
        {
            Ok((events, has_more)) => HistoryPage { events, has_more },
            Err(_) => HistoryPage::default(),
        }
    }

    /// One page of `channel` on this host (ADR-091 D2).
    pub(crate) async fn history_page(
        &self,
        channel: &ChannelId,
        before: Option<&str>,
        after: Option<&str>,
        limit: u32,
    ) -> HistoryPage {
        let limit = limit.clamp(1, HISTORY_PAGE_LIMIT) as usize;
        match self.local.history_page(channel, before, after, limit).await {
            Ok((events, has_more)) => HistoryPage { events, has_more },
            Err(_) => HistoryPage::default(),
        }
    }
}

impl TunnelChannelPort {
    /// A post as the host serves it (ADR-091 D3): its author by Hub
    /// address, as live envelopes name it; no session path (`via`), no
    /// line-number `parent` (it is the host's own); attachments without
    /// their path on the host's disk.
    async fn served(&self, event: ChannelEvent) -> ChannelEvent {
        let ChannelEvent::Posted { author, .. } = &event else {
            return event;
        };
        let subject = author
            .parse::<ActorRef>()
            .unwrap_or_else(|_| ActorRef::Principal(author.clone().into()));
        let wire_author = self
            .local
            .actor_info(&subject)
            .await
            .hub_address()
            .unwrap_or_else(|| author.clone());
        served(event, wire_author)
    }
}

fn served(event: ChannelEvent, wire_author: String) -> ChannelEvent {
    match event {
        ChannelEvent::Posted {
            channel,
            text,
            at,
            msg_id,
            reply_to,
            attachments,
            options,
            ..
        } => ChannelEvent::Posted {
            channel,
            author: wire_author,
            parent: None,
            text,
            at,
            via: None,
            msg_id,
            reply_to,
            attachments: attachments
                .into_iter()
                .map(|a| peko_protocol::channel::Attachment {
                    path: String::new(),
                    ..a
                })
                .collect(),
            options,
        },
        other => other,
    }
}
