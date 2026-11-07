//! The owner's other computer asks a peko's host which models it can
//! answer with (ADR-097 D1). A child module of `tunnel_channel_port` so it
//! shares the port's store and tunnel context.
//!
//! - The person's side asks ([`TunnelChannelPort::peko_models`], for the
//!   desktop's per-message model picker) and accepts only the answer it is
//!   waiting for.
//! - The host checks the request ([`TunnelChannelPort::verified_peko_models_request`]);
//!   the dispatcher asks the daemon for the models the peko's owner may
//!   pick, and the port signs and sends them
//!   ([`TunnelChannelPort::answer_peko_models`]).

use super::TunnelChannelPort;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_peko_models, verify_peko_models_request,
    PekoModel, PekoModelsPayload, PekoModelsRequestPayload, TunnelHandle, TunnelMessage,
    PEKO_MODELS_REQUEST_VERSION, PEKO_MODELS_VERSION,
};
use peko_channel::{ChannelId, ChannelPort, ConversationKind};
use peko_subject::ActorRef;
use std::time::Duration;
use tracing::{debug, warn};

/// How long the person's side waits for the host's answer.
const MODELS_WAIT: Duration = Duration::from_secs(15);

/// A request waiting for its answer.
pub(super) struct PendingModels {
    host: String,
    principal_did: String,
    reply: tokio::sync::oneshot::Sender<Vec<PekoModel>>,
}

fn models_b64u(models: &[PekoModel]) -> String {
    PekoModelsPayload::models_b64u_of(&serde_json::to_vec(models).unwrap_or_default())
}

/// The account's own peko on another runtime that a DM here talks to.
pub(super) struct OwnDmPeko {
    pub ctx: std::sync::Arc<crate::tunnel::CrossRuntimeChannelCtx>,
    /// This runtime's Hub account (`<account>`, no prefix).
    pub account: String,
    /// The peko's host.
    pub runtime_id: String,
    /// The peko's signing DID.
    pub principal_did: String,
    pub handle: TunnelHandle,
}

impl TunnelChannelPort {
    /// The peko in `channel` — this account's DM with one of its pekos on
    /// another runtime — with what it takes to ask its host something.
    ///
    /// # Errors
    ///
    /// The channel is not this account's DM with a peko elsewhere, or this
    /// runtime has no PekoHub connection.
    pub(super) async fn own_dm_peko(&self, channel: &ChannelId) -> anyhow::Result<OwnDmPeko> {
        let ctx = self
            .ctx
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("This runtime has no PekoHub connection"))?;
        let account = self
            .local
            .actor_info(&ActorRef::User("local".into()))
            .await
            .hub_id
            .ok_or_else(|| anyhow::anyhow!("This runtime is not registered with PekoHub"))?;
        let mine = self.local.remote_dm_account(channel).await?.as_deref() == Some(&account)
            && self.local.conversation_kind(channel).await? == ConversationKind::Dm;
        if !mine {
            anyhow::bail!("{channel} is not your conversation with a peko elsewhere");
        }
        let peko = self
            .local
            .list_remote_members(channel)
            .await?
            .into_iter()
            .find(|m| m.principal_id.starts_with("did:key:"))
            .filter(|m| m.runtime_id != ctx.caller_runtime_id)
            .ok_or_else(|| anyhow::anyhow!("{channel} has no peko on another runtime"))?;
        let handle = ctx
            .tunnel
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Not connected to PekoHub"))?;
        Ok(OwnDmPeko {
            ctx,
            account,
            runtime_id: peko.runtime_id,
            principal_did: peko.principal_id,
            handle,
        })
    }

    /// The models the peko in `channel` — this account's DM with a peko on
    /// another runtime — can answer its owner with, from its host.
    ///
    /// # Errors
    ///
    /// The channel is not this account's DM with a peko elsewhere, this
    /// runtime has no PekoHub connection, or the host does not answer.
    pub async fn peko_models(&self, channel: &ChannelId) -> anyhow::Result<Vec<PekoModel>> {
        let OwnDmPeko {
            ctx,
            account,
            runtime_id,
            principal_did,
            handle,
        } = self.own_dm_peko(channel).await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoModelsRequestPayload {
            v: PEKO_MODELS_REQUEST_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: runtime_id.clone(),
            source_principal_did: format!("user:hub:{account}"),
            principal_did: principal_did.clone(),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.models_pending.lock().expect("models pending").insert(
            request_id.clone(),
            PendingModels {
                host: runtime_id.clone(),
                principal_did: principal_did.clone(),
                reply,
            },
        );
        let sent = handle.send(TunnelMessage::TunnelPekoModelsRequest {
            request_id: request_id.clone(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            source_principal_did: payload.source_principal_did,
            principal_did: payload.principal_did,
            signature,
        });
        if let Err(e) = sent {
            self.models_pending
                .lock()
                .expect("models pending")
                .remove(&request_id);
            anyhow::bail!("Not connected to PekoHub: {e}");
        }
        match tokio::time::timeout(MODELS_WAIT, answer).await {
            Ok(Ok(models)) => Ok(models),
            _ => {
                self.models_pending
                    .lock()
                    .expect("models pending")
                    .remove(&request_id);
                anyhow::bail!("The computer hosting this peko did not answer; it may be offline")
            }
        }
    }

    /// The person's side receives an answer: accept it only for a request
    /// of ours, from the runtime asked, about the peko asked about.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_peko_models(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        principal_did: String,
        models: Vec<PekoModel>,
        signature: String,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        if recipient_runtime_id != ctx.caller_runtime_id {
            return;
        }
        let verified = did_key_to_verifying_key(&source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_peko_models(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.principal_did == principal_did
                && p.models_b64u == models_b64u(&models)
        });
        if !agrees {
            warn!(request_id, "peko models failed verification; dropped");
            return;
        }
        // Taken only once the answer holds, so a bad one cannot end the wait.
        let waiting = {
            let mut pending = self.models_pending.lock().expect("models pending");
            let matches = pending
                .get(&request_id)
                .is_some_and(|p| p.host == source_runtime_id && p.principal_did == principal_did);
            if matches {
                pending.remove(&request_id)
            } else {
                None
            }
        };
        let Some(waiting) = waiting else {
            debug!(request_id, "unrequested peko models dropped");
            return;
        };
        let _ = waiting.reply.send(models);
    }

    /// The host checks a request: addressed here, signed by the runtime it
    /// names, and asked by a person (`user:hub:<account>`). Whether that
    /// person owns the peko is the daemon's to decide.
    pub(crate) async fn verified_peko_models_request(
        &self,
        envelope: PekoModelsRequestPayload,
        signature: &str,
    ) -> Option<PekoModelsRequestPayload> {
        let ctx = self.ctx.read().await.clone()?;
        if envelope.recipient_runtime_id != ctx.caller_runtime_id
            || !envelope.source_principal_did.starts_with("user:hub:")
        {
            return None;
        }
        let verified = match did_key_to_verifying_key(&envelope.source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_peko_models_request(&key, signature))
        {
            Ok(payload) => payload,
            Err(e) => {
                warn!(request_id = %envelope.request_id, "peko models request failed verification: {e}");
                return None;
            }
        };
        let same = PekoModelsRequestPayload {
            iat: verified.iat,
            exp: verified.exp,
            ..envelope
        };
        if same != verified {
            warn!(request_id = %verified.request_id, "peko models request disagrees with its signature");
            return None;
        }
        Some(verified)
    }

    /// The host answers `request` with `models` (empty for a refusal).
    pub(crate) async fn answer_peko_models(
        &self,
        handle: &TunnelHandle,
        request: &PekoModelsRequestPayload,
        models: Vec<PekoModel>,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoModelsPayload {
            v: PEKO_MODELS_VERSION.into(),
            request_id: request.request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: request.source_runtime_id.clone(),
            principal_did: request.principal_did.clone(),
            models_b64u: models_b64u(&models),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let _ = handle.send(TunnelMessage::TunnelPekoModels {
            request_id: payload.request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            principal_did: payload.principal_did,
            models,
            signature,
        });
    }
}
