//! The owner's other computer lists and decides a peko's waiting approval
//! requests on the peko's host (ADR-099). A child module of
//! `tunnel_channel_port` so it shares the port's store and tunnel context.
//!
//! - The person's side asks ([`TunnelChannelPort::peko_approvals`],
//!   [`TunnelChannelPort::decide_peko_approval`]) in its DM with the peko
//!   and accepts only the answer it is waiting for.
//! - The host checks the request ([`TunnelChannelPort::verified_peko_approvals_request`],
//!   [`TunnelChannelPort::verified_peko_approval_decide`]); the dispatcher
//!   asks the daemon, which answers only the peko's owner, and the port
//!   signs and sends the answer.

use super::peko_models::OwnDmPeko;
use super::TunnelChannelPort;
use crate::tunnel::{
    did_key_to_verifying_key, sign_history, verify_peko_approval_decide,
    verify_peko_approval_decided, verify_peko_approvals, verify_peko_approvals_request,
    PekoApproval, PekoApprovalDecidePayload, PekoApprovalDecidedPayload, PekoApprovalsPayload,
    PekoApprovalsRequestPayload, TunnelHandle, TunnelMessage, PEKO_APPROVALS_REQUEST_VERSION,
    PEKO_APPROVALS_VERSION, PEKO_APPROVAL_DECIDED_VERSION, PEKO_APPROVAL_DECIDE_VERSION,
};
use peko_channel::ChannelId;
use std::time::Duration;
use tracing::{debug, warn};

/// How long the person's side waits for the host's answer.
const APPROVALS_WAIT: Duration = Duration::from_secs(15);

/// What the host answered.
pub(super) enum ApprovalAnswer {
    List(Vec<PekoApproval>),
    Decided { decided: bool, message: String },
}

/// A request waiting for its answer.
pub(super) struct PendingApproval {
    host: String,
    principal_did: String,
    /// Set for a decision: the request it decides.
    approval_id: Option<String>,
    reply: tokio::sync::oneshot::Sender<ApprovalAnswer>,
}

fn approvals_b64u(approvals: &[PekoApproval]) -> String {
    PekoApprovalsPayload::approvals_b64u_of(&serde_json::to_vec(approvals).unwrap_or_default())
}

impl TunnelChannelPort {
    /// The approval requests waiting for you, the owner of the peko in
    /// `channel` — this account's DM with a peko on another runtime —
    /// oldest first, from its host.
    ///
    /// # Errors
    ///
    /// The channel is not this account's DM with a peko elsewhere, this
    /// runtime has no PekoHub connection, or the host does not answer.
    pub async fn peko_approvals(&self, channel: &ChannelId) -> anyhow::Result<Vec<PekoApproval>> {
        let peko = self.own_dm_peko(channel).await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoApprovalsRequestPayload {
            v: PEKO_APPROVALS_REQUEST_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: peko.ctx.caller_runtime_id.clone(),
            recipient_runtime_id: peko.runtime_id.clone(),
            source_principal_did: format!("user:hub:{}", peko.account),
            principal_did: peko.principal_did.clone(),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&peko.ctx.signing_key, &payload);
        let message = TunnelMessage::TunnelPekoApprovalsRequest {
            request_id: request_id.clone(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            source_principal_did: payload.source_principal_did,
            principal_did: payload.principal_did,
            signature,
        };
        match self.ask_host(&peko, request_id, None, message).await? {
            ApprovalAnswer::List(approvals) => Ok(approvals),
            ApprovalAnswer::Decided { .. } => anyhow::bail!("the host answered something else"),
        }
    }

    /// Decide approval request `approval_id` of the peko in `channel` on
    /// its host: approve (with `always`, also remember it there) or deny.
    /// Resolves to what the host did ("Approved apr_… Running it …").
    ///
    /// # Errors
    ///
    /// As [`Self::peko_approvals`], or the host did not apply it (with its
    /// reason: already decided, expired, not the owner …).
    pub async fn decide_peko_approval(
        &self,
        channel: &ChannelId,
        approval_id: &str,
        approve: bool,
        always: bool,
    ) -> anyhow::Result<String> {
        let peko = self.own_dm_peko(channel).await?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoApprovalDecidePayload {
            v: PEKO_APPROVAL_DECIDE_VERSION.into(),
            request_id: request_id.clone(),
            source_runtime_id: peko.ctx.caller_runtime_id.clone(),
            recipient_runtime_id: peko.runtime_id.clone(),
            source_principal_did: format!("user:hub:{}", peko.account),
            principal_did: peko.principal_did.clone(),
            approval_id: approval_id.to_string(),
            approve,
            always: approve && always,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&peko.ctx.signing_key, &payload);
        let message = TunnelMessage::TunnelPekoApprovalDecide {
            request_id: request_id.clone(),
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            source_principal_did: payload.source_principal_did,
            principal_did: payload.principal_did,
            approval_id: payload.approval_id,
            approve: payload.approve,
            always: payload.always,
            signature,
        };
        match self
            .ask_host(&peko, request_id, Some(approval_id.to_string()), message)
            .await?
        {
            ApprovalAnswer::Decided {
                decided: true,
                message,
            } => Ok(message),
            ApprovalAnswer::Decided { message, .. } => anyhow::bail!("{message}"),
            ApprovalAnswer::List(_) => anyhow::bail!("the host answered something else"),
        }
    }

    /// Send `message` (request `request_id`) to the peko's host and wait
    /// for its answer.
    async fn ask_host(
        &self,
        peko: &OwnDmPeko,
        request_id: String,
        approval_id: Option<String>,
        message: TunnelMessage,
    ) -> anyhow::Result<ApprovalAnswer> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.approvals_pending
            .lock()
            .expect("approvals pending")
            .insert(
                request_id.clone(),
                PendingApproval {
                    host: peko.runtime_id.clone(),
                    principal_did: peko.principal_did.clone(),
                    approval_id,
                    reply,
                },
            );
        let forget = || {
            self.approvals_pending
                .lock()
                .expect("approvals pending")
                .remove(&request_id);
        };
        if let Err(e) = peko.handle.send(message) {
            forget();
            anyhow::bail!("Not connected to PekoHub: {e}");
        }
        match tokio::time::timeout(APPROVALS_WAIT, answer).await {
            Ok(Ok(answer)) => Ok(answer),
            _ => {
                forget();
                anyhow::bail!("The computer hosting this peko did not answer; it may be offline")
            }
        }
    }

    /// Hand `answer` to the request `request_id` waits for, when it came
    /// from the runtime asked, about the peko asked about (and, for a
    /// decision, the request decided). Taken only once the answer holds,
    /// so a bad one cannot end the wait.
    fn settle_approval_answer(
        &self,
        request_id: &str,
        host: &str,
        principal_did: &str,
        approval_id: Option<&str>,
        answer: ApprovalAnswer,
    ) {
        let waiting = {
            let mut pending = self.approvals_pending.lock().expect("approvals pending");
            let matches = pending.get(request_id).is_some_and(|p| {
                p.host == host
                    && p.principal_did == principal_did
                    && p.approval_id.as_deref() == approval_id
            });
            if matches {
                pending.remove(request_id)
            } else {
                None
            }
        };
        let Some(waiting) = waiting else {
            debug!(request_id, "unrequested peko approvals answer dropped");
            return;
        };
        let _ = waiting.reply.send(answer);
    }

    /// The person's side receives the host's list.
    pub(crate) async fn on_peko_approvals(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        principal_did: String,
        approvals: Vec<PekoApproval>,
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
            .and_then(|key| verify_peko_approvals(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.principal_did == principal_did
                && p.approvals_b64u == approvals_b64u(&approvals)
        });
        if !agrees {
            warn!(request_id, "peko approvals failed verification; dropped");
            return;
        }
        self.settle_approval_answer(
            &request_id,
            &source_runtime_id,
            &principal_did,
            None,
            ApprovalAnswer::List(approvals),
        );
    }

    /// The person's side receives what the host did with a decision.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn on_peko_approval_decided(
        &self,
        request_id: String,
        source_runtime_id: String,
        recipient_runtime_id: String,
        principal_did: String,
        approval_id: String,
        decided: bool,
        message: String,
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
            .and_then(|key| verify_peko_approval_decided(&key, &signature));
        let agrees = verified.as_ref().is_ok_and(|p| {
            p.request_id == request_id
                && p.source_runtime_id == source_runtime_id
                && p.recipient_runtime_id == recipient_runtime_id
                && p.principal_did == principal_did
                && p.approval_id == approval_id
                && p.decided == decided
                && p.message == message
        });
        if !agrees {
            warn!(
                request_id,
                "peko approval outcome failed verification; dropped"
            );
            return;
        }
        self.settle_approval_answer(
            &request_id,
            &source_runtime_id,
            &principal_did,
            Some(&approval_id),
            ApprovalAnswer::Decided { decided, message },
        );
    }

    /// Whether a request may be looked at here at all: addressed to this
    /// runtime and asked by a person (`user:hub:<account>`). Whether that
    /// person owns the peko is the daemon's to decide.
    async fn approvals_request_for_us(
        &self,
        recipient_runtime_id: &str,
        source_principal_did: &str,
    ) -> bool {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return false;
        };
        recipient_runtime_id == ctx.caller_runtime_id
            && source_principal_did.starts_with("user:hub:")
    }

    /// The host checks a list request: addressed here, signed by the
    /// runtime it names, asked by a person.
    pub(crate) async fn verified_peko_approvals_request(
        &self,
        envelope: PekoApprovalsRequestPayload,
        signature: &str,
    ) -> Option<PekoApprovalsRequestPayload> {
        if !self
            .approvals_request_for_us(
                &envelope.recipient_runtime_id,
                &envelope.source_principal_did,
            )
            .await
        {
            return None;
        }
        let verified = match did_key_to_verifying_key(&envelope.source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_peko_approvals_request(&key, signature))
        {
            Ok(payload) => payload,
            Err(e) => {
                warn!(request_id = %envelope.request_id, "peko approvals request failed verification: {e}");
                return None;
            }
        };
        let same = PekoApprovalsRequestPayload {
            iat: verified.iat,
            exp: verified.exp,
            ..envelope
        };
        if same != verified {
            warn!(request_id = %verified.request_id, "peko approvals request disagrees with its signature");
            return None;
        }
        Some(verified)
    }

    /// The host checks a decision: addressed here, signed by the runtime
    /// it names, made by a person.
    pub(crate) async fn verified_peko_approval_decide(
        &self,
        envelope: PekoApprovalDecidePayload,
        signature: &str,
    ) -> Option<PekoApprovalDecidePayload> {
        if !self
            .approvals_request_for_us(
                &envelope.recipient_runtime_id,
                &envelope.source_principal_did,
            )
            .await
        {
            return None;
        }
        let verified = match did_key_to_verifying_key(&envelope.source_runtime_id)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .and_then(|key| verify_peko_approval_decide(&key, signature))
        {
            Ok(payload) => payload,
            Err(e) => {
                warn!(request_id = %envelope.request_id, "peko approval decision failed verification: {e}");
                return None;
            }
        };
        let same = PekoApprovalDecidePayload {
            iat: verified.iat,
            exp: verified.exp,
            ..envelope
        };
        if same != verified {
            warn!(request_id = %verified.request_id, "peko approval decision disagrees with its signature");
            return None;
        }
        Some(verified)
    }

    /// The host answers `request` with `approvals` (empty for a refusal).
    pub(crate) async fn answer_peko_approvals(
        &self,
        handle: &TunnelHandle,
        request: &PekoApprovalsRequestPayload,
        approvals: Vec<PekoApproval>,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoApprovalsPayload {
            v: PEKO_APPROVALS_VERSION.into(),
            request_id: request.request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: request.source_runtime_id.clone(),
            principal_did: request.principal_did.clone(),
            approvals_b64u: approvals_b64u(&approvals),
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let _ = handle.send(TunnelMessage::TunnelPekoApprovals {
            request_id: payload.request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            principal_did: payload.principal_did,
            approvals,
            signature,
        });
    }

    /// The host answers a decision with what it did.
    pub(crate) async fn answer_peko_approval_decide(
        &self,
        handle: &TunnelHandle,
        request: &PekoApprovalDecidePayload,
        outcome: Result<String, String>,
    ) {
        let Some(ctx) = self.ctx.read().await.clone() else {
            return;
        };
        let (decided, message) = match outcome {
            Ok(message) => (true, message),
            Err(message) => (false, message),
        };
        let iat = chrono::Utc::now().timestamp();
        let payload = PekoApprovalDecidedPayload {
            v: PEKO_APPROVAL_DECIDED_VERSION.into(),
            request_id: request.request_id.clone(),
            source_runtime_id: ctx.caller_runtime_id.clone(),
            recipient_runtime_id: request.source_runtime_id.clone(),
            principal_did: request.principal_did.clone(),
            approval_id: request.approval_id.clone(),
            decided,
            message,
            iat,
            exp: iat + 300,
        };
        let signature = sign_history(&ctx.signing_key, &payload);
        let _ = handle.send(TunnelMessage::TunnelPekoApprovalDecided {
            request_id: payload.request_id,
            source_runtime_id: payload.source_runtime_id,
            recipient_runtime_id: payload.recipient_runtime_id,
            principal_did: payload.principal_did,
            approval_id: payload.approval_id,
            decided: payload.decided,
            message: payload.message,
            signature,
        });
    }
}
