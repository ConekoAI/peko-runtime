//! A person's DMs follow them to every runtime of their account (ADR-095
//! D5). A child module of `tunnel_channel_port` so it shares the port's
//! store, tunnel context, and DM opening.
//!
//! - A runtime asks ([`TunnelChannelPort::sync_account_dms`], on
//!   `tunnel_ready`): PekoHub delivers the request to the account's other
//!   runtimes, and only from a runtime registered to that account.
//! - Each of them answers ([`TunnelChannelPort::on_account_dms_request`])
//!   with the person's DMs it holds: their DMs with pekos elsewhere, and
//!   with pekos hosted there.
//! - The asker ([`TunnelChannelPort::on_account_dms`]) opens each DM it
//!   lacks, as if the person had opened it here, and catches it up from
//!   the peko's host. PekoHub keeps no list of who a person talks to.

use super::user_dm::ResolvedPeko;
use super::TunnelChannelPort;
use crate::tunnel::{
    did_key_to_verifying_key, human_dm_channel_id, sign_history, verify_account_dms,
    verify_account_dms_request, AccountDm, AccountDmsPayload, AccountDmsRequestPayload,
    AccountPersonDm, DmBlock, TunnelHandle, TunnelMessage, ACCOUNT_DMS_REQUEST_VERSION,
    ACCOUNT_DMS_VERSION,
};
use peko_channel::{BlockDecision, ChannelId, ChannelPort, Registration};
use peko_subject::{ActorRef, PrincipalId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Requests sent, by id, with when.
pub(super) type AccountDmRequests = Arc<std::sync::Mutex<HashMap<String, Instant>>>;

/// How long answers to a request are taken.
const ANSWERS_FOR: Duration = Duration::from_mins(10);

impl TunnelChannelPort {
    /// This runtime's person, when it is registered: their actor and Hub
    /// account.
    async fn registered_person(&self) -> Option<(peko_channel::ActorInfo, String)> {
        let user = self.local.actor_info(&ActorRef::User("local".into())).await;
        let account = user.hub_id.clone()?;
        Some((user, account))
    }

    /// Ask the account's other runtimes for the person's DMs (ADR-095 D5).
    pub(crate) async fn sync_account_dms(&self) {
        let (Some((_, account)), Some(ctx)) = (
            self.registered_person().await,
            self.ctx.read().await.clone(),
        ) else {
            return;
        };
        let Some(handle) = ctx.tunnel.read().await.clone() else {
            return;
        };
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = AccountDmsRequestPayload {
            v: ACCOUNT_DMS_REQUEST_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_account: format!("user:hub:{account}"),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        {
            let mut pending = self
                .account_dm_requests
                .lock()
                .expect("account DM requests");
            pending.retain(|_, at| at.elapsed() < ANSWERS_FOR);
            pending.insert(request_id.clone(), Instant::now());
        }
        if let Err(e) = handle.send(TunnelMessage::TunnelAccountDmsRequest {
            request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_account: payload.recipient_account,
            signature,
        }) {
            debug!("account DM request not sent: {e}");
        }
    }

    /// Another runtime of this account asks for the person's DMs: answer
    /// with those held here. PekoHub delivered it only from a runtime
    /// registered to the account.
    pub(crate) async fn on_account_dms_request(
        &self,
        handle: &TunnelHandle,
        request_id: String,
        source_runtime_id: String,
        recipient_account: String,
        signature: String,
    ) {
        let (Some((_, account)), Some(ctx)) = (
            self.registered_person().await,
            self.ctx.read().await.clone(),
        ) else {
            return;
        };
        if recipient_account != format!("user:hub:{account}")
            || source_runtime_id == ctx.caller_runtime_id
        {
            return;
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_account_dms_request(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_account == recipient_account
        });
        if !agrees {
            warn!(
                request_id,
                "account DM request failed verification; dropped"
            );
            return;
        }
        let dms = self.account_dm_list(&ctx, &account).await;
        let people = self.person_dm_list(&account).await;
        let blocks: Vec<DmBlock> = self
            .local
            .block_decisions(&account)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|d| DmBlock {
                person: d.person,
                blocked: d.blocked,
                at: d.at.to_rfc3339(),
            })
            .collect();
        let iat = chrono::Utc::now().timestamp();
        let payload = AccountDmsPayload {
            v: ACCOUNT_DMS_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: source_runtime_id.clone(),
            dms_b64u: AccountDmsPayload::dms_b64u_of(&serde_json::to_vec(&dms).unwrap_or_default()),
            people_b64u: AccountDmsPayload::dms_b64u_of(
                &serde_json::to_vec(&people).unwrap_or_default(),
            ),
            blocks_b64u: AccountDmsPayload::dms_b64u_of(
                &serde_json::to_vec(&blocks).unwrap_or_default(),
            ),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let _ = handle.send(TunnelMessage::TunnelAccountDms {
            request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: source_runtime_id,
            dms,
            people,
            blocks,
            signature,
        });
    }

    /// The person's DMs held here, as another of their runtimes needs them.
    async fn account_dm_list(
        &self,
        ctx: &crate::tunnel::cross_runtime_channel::CrossRuntimeChannelCtx,
        account: &str,
    ) -> Vec<AccountDm> {
        let person = format!("user:hub:{account}");
        let mut out = Vec::new();
        // Their DMs with pekos on other runtimes (ADR-090).
        for channel in self.local.remote_dms(account).await.unwrap_or_default() {
            let Some(row) = self
                .local
                .list_remote_members(&channel)
                .await
                .unwrap_or_default()
                .into_iter()
                .find(|m| m.principal_id.starts_with("did:key:"))
            else {
                continue;
            };
            let peer = self
                .local
                .actor_info(&ActorRef::Principal(row.principal_id.clone().into()))
                .await;
            if let Some(dm) = account_dm(&channel, &peer, &row.principal_id, &row.runtime_id) {
                out.push(dm);
            }
        }
        // Their DMs with pekos hosted here, keyed by their account (D7).
        for (channel, peer) in self.local.local_user_dm_peers().await.unwrap_or_default() {
            let Some(local_id) = peer.local_id.clone().filter(|_| peer.kind == "principal") else {
                continue;
            };
            if peer.registration == Registration::LocalOnly
                || channel != human_dm_channel_id(&peer.identity_key, &person)
            {
                continue;
            }
            let Some(did) = ctx
                .principal_keys
                .did_for_principal(&PrincipalId(local_id))
                .await
            else {
                continue;
            };
            if let Some(dm) = account_dm(&channel, &peer, &did, &ctx.caller_runtime_id) {
                out.push(dm);
            }
        }
        out
    }

    /// One of this account's runtimes answers: open each DM this runtime
    /// lacks and catch it up from the peko's host.
    pub(crate) async fn on_account_dms(
        self: &Arc<Self>,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        dms: Vec<AccountDm>,
        people: Vec<AccountPersonDm>,
        blocks: Vec<DmBlock>,
        signature: String,
    ) {
        let (Some((user, account)), Some(ctx)) = (
            self.registered_person().await,
            self.ctx.read().await.clone(),
        ) else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let asked = self
            .account_dm_requests
            .lock()
            .expect("account DM requests")
            .get(&request_id)
            .is_some_and(|at| at.elapsed() < ANSWERS_FOR);
        if !asked {
            debug!(request_id, "unrequested account DM list dropped");
            return;
        }
        fn b64u<T: serde::Serialize>(value: &T) -> String {
            AccountDmsPayload::dms_b64u_of(&serde_json::to_vec(value).unwrap_or_default())
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_account_dms(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.dms_b64u == b64u(&dms)
                && p.people_b64u == b64u(&people)
                && p.blocks_b64u == b64u(&blocks)
        });
        if !agrees {
            warn!(request_id, "account DM list failed verification; dropped");
            return;
        }
        // D6: the person's block decisions (latest per person wins) and
        // their DMs with other people.
        let decisions: Vec<BlockDecision> = blocks
            .iter()
            .filter_map(|b| {
                Some(BlockDecision {
                    person: b.person.clone(),
                    blocked: b.blocked,
                    at: chrono::DateTime::parse_from_rfc3339(&b.at)
                        .ok()?
                        .with_timezone(&chrono::Utc),
                })
            })
            .collect();
        if !decisions.is_empty() {
            if let Err(e) = self.local.merge_block_decisions(&account, &decisions).await {
                warn!("block decisions from another device not kept: {e}");
            }
            // A request from someone now blocked goes, even one that
            // arrived here before the block.
            for request in self.local.dm_requests(&account).await.unwrap_or_default() {
                if self.local.is_blocked(&account, &request.from).await {
                    let _ = self.local.take_dm_request(&request.channel).await;
                }
            }
        }
        for dm in people {
            let channel = dm.channel_id.clone();
            let Some(id) = ChannelId::parse(&channel) else {
                continue;
            };
            if self.local.conversation_kind(&id).await.is_ok() {
                continue;
            }
            if let Err(e) = self.adopt_person_dm(&account, dm).await {
                debug!(
                    channel,
                    "DM with a person from another device not opened: {e}"
                );
            }
        }
        let person = format!("user:hub:{account}");
        for dm in dms {
            let Some(channel) = ChannelId::parse(&dm.channel_id) else {
                continue;
            };
            // Only this person's pair with that peko, and only one this
            // runtime lacks; a peko hosted here is talked to directly.
            if channel != human_dm_channel_id(&dm.peer_identity_key, &person)
                || dm.peer_runtime_id == ctx.caller_runtime_id
                || self.local.conversation_kind(&channel).await.is_ok()
            {
                continue;
            }
            let resolved = ResolvedPeko {
                hub_id: dm.peer_hub_id,
                signing_did: dm.peer_signing_did,
                identity_key: dm.peer_identity_key,
                name: dm.peer_name,
                runtime_id: dm.peer_runtime_id,
                public: dm.peer_exposure == "public",
            };
            match self.open_resolved_hub_dm(&ctx, &user, &resolved).await {
                Ok(opened) => {
                    let port = Arc::clone(self);
                    tokio::spawn(async move {
                        if let Err(e) = port.sync_history(&opened.channel).await {
                            debug!(channel = %opened.channel, "DM from another device not caught up yet: {e}");
                        }
                    });
                }
                Err(e) => debug!(%channel, "DM from another device not opened: {e}"),
            }
        }
    }
}

/// `channel` as the DM list carries it, when the peko has a Hub ID.
fn account_dm(
    channel: &ChannelId,
    peer: &peko_channel::ActorInfo,
    signing_did: &str,
    runtime: &str,
) -> Option<AccountDm> {
    Some(AccountDm {
        channel_id: channel.as_str().to_string(),
        peer_hub_id: peer.hub_id.clone()?,
        peer_signing_did: signing_did.to_string(),
        peer_identity_key: peer.identity_key.clone(),
        peer_name: peer.name.clone(),
        peer_runtime_id: runtime.to_string(),
        peer_exposure: if peer.registration == Registration::Public {
            "public".into()
        } else {
            "private".into()
        },
    })
}
