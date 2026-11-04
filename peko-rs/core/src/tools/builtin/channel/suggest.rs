//! `Suggest` — offer suggested replies with the run's reply (ADR-093 D3).
//!
//! A conversation's reply is the run's final text, posted after the run,
//! so a tool cannot post it. `Suggest` stages a few short answers the
//! person may send with a tap; the reply post carries them
//! ([`crate::principal::reply_options`]). A tap is just the person's
//! message — the options carry no authority.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{ChannelId, ChannelPort, MAX_OPTIONS, MAX_OPTION_CHARS, MIN_OPTIONS};
use peko_subject::PrincipalId;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

/// Wire name registered with the ToolingRuntime.
pub const SUGGEST_TOOL_NAME: &str = "Suggest";

/// Stage suggested replies for the calling run's reply.
pub struct SuggestTool {
    port: Arc<dyn ChannelPort>,
}

impl SuggestTool {
    #[must_use]
    pub fn new(port: Arc<dyn ChannelPort>) -> Self {
        Self { port }
    }

    /// Only the conversation's own session replies there; a run the
    /// conversation delegated to (an `Agent` worker) shares its
    /// compartment but reports back instead.
    async fn ensure_conversation_session(
        &self,
        channel: &ChannelId,
        principal: &PrincipalId,
        ctx: &ToolContext,
    ) -> anyhow::Result<()> {
        let binding = self.port.binding_for(channel, principal).await?;
        let bound = match (binding, ctx.session_id.as_deref()) {
            (Some(binding), Some(session)) if binding.session == session => true,
            (Some(binding), Some(_)) => {
                let via = super::channel_send::via_for_ctx(ctx).await;
                via.as_deref()
                    .and_then(|v| v.strip_prefix(peko_session::path::SCHEME_PREFIX))
                    == Some(binding.session.as_str())
            }
            _ => false,
        };
        anyhow::ensure!(
            bound,
            "Suggest: only the conversation's own reply can carry suggested replies; \
             this run reports back to the one that started it — include the choices in \
             your result instead"
        );
        Ok(())
    }
}

#[async_trait]
impl Tool for SuggestTool {
    fn name(&self) -> &'static str {
        SUGGEST_TOOL_NAME
    }

    fn description(&self) -> String {
        format!(
            r#"Offer the person you are talking to a few short replies they can send with one tap, shown under your reply (for example "Ship it" / "Hold off", or "Tuesday" / "Thursday"). A tap sends that text as their own message, exactly as if they typed it; they can always write something else instead. Call it during your turn, then reply as usual — the options go with your reply. A later call replaces the earlier one; an empty `replies` clears them.

Use it only when your reply asks something the person will most likely answer with one of a few short choices. Not for open questions, not on every reply, and never to ask for permission: the runtime asks the owner itself, and a tapped "Approve" decides nothing.

{MIN_OPTIONS}–{MAX_OPTIONS} replies, one line each, up to {MAX_OPTION_CHARS} characters, no duplicates. Only in a conversation (not in cron, internal work, or a delegated task).

## Result
`{{success, channel, replies}}`."#
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "replies": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": MAX_OPTIONS,
                    "description": "The suggested replies, in the order to show them; [] clears."
                }
            },
            "required": ["replies"]
        })
    }

    fn parallelizable(&self) -> bool {
        // Replaces the run's staged replies: calls apply in order.
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "Suggest requires a ToolContext (use execute_with_context)"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(
            params
                .get("replies")
                .is_some_and(serde_json::Value::is_array),
            "Suggest requires 'replies' (array of strings)"
        );
        let replies = params["replies"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow::anyhow!("Suggest: every reply must be a string"))?;
        let replies = peko_channel::validate_options(&replies)
            .map_err(|e| anyhow::anyhow!("Suggest: {e}"))?;
        let principal = ctx
            .principal_id
            .as_deref()
            .filter(|p| !p.is_empty())
            .map(|p| PrincipalId(p.to_string()))
            .ok_or_else(|| anyhow::anyhow!("Suggest requires a principal"))?;
        let channel = ctx
            .acting_for()
            .compartment
            .channel()
            .and_then(ChannelId::parse)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Suggest: this run is not in a conversation, so there is no reply to \
                     put suggestions on"
                )
            })?;
        self.ensure_conversation_session(&channel, &principal, ctx)
            .await?;
        crate::principal::reply_options::stage(
            self.port.as_ref(),
            &channel,
            &principal,
            replies.clone(),
        )
        .await;
        Ok(json!({
            "success": true,
            "channel": channel.as_str(),
            "replies": replies,
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::principal::reply_options;
    use crate::tools::builtin::test_harness::{Caller, ToolHarness, PRINCIPAL};
    use peko_channel::ChannelId;
    use peko_subject::PrincipalId;
    use peko_tools_core::{ActingFor, Compartment, Origin, Trust};
    use serde_json::json;

    fn in_conversation(channel: &str, trust: Trust, session: &str) -> Caller {
        let mut caller = Caller::acting_for(ActingFor {
            actor: "user:local:local".into(),
            name: None,
            trust,
            compartment: Compartment::Conversation {
                channel: channel.into(),
            },
            origin: Origin::Ingress { channel: None },
        });
        caller.session = session.into();
        caller
    }

    /// A conversation bound to session `session`.
    async fn conversation(h: &ToolHarness, session: &str) -> ChannelId {
        let channel = h.channel(PRINCIPAL, "chat").await;
        h.channels
            .bind(&channel, &PrincipalId(PRINCIPAL.into()), session)
            .await
            .unwrap();
        channel
    }

    const SESSION: &str = "0b6f2b5e-6b8a-4b7a-9c1e-2f4a5d6e7f80";

    /// Any run in the conversation — the untrusted one too — stages
    /// replies for its reply without review: they are only text.
    #[tokio::test]
    async fn the_conversation_stages_replies_for_its_reply() {
        let h = ToolHarness::new().await;
        let channel = conversation(&h, SESSION).await;
        for trust in [Trust::Owner, Trust::Untrusted] {
            let value = h
                .call_as(
                    &in_conversation(channel.as_str(), trust, SESSION),
                    "Suggest",
                    json!({ "replies": [" Ship it", "Hold off "] }),
                )
                .await
                .ok();
            assert_eq!(value["replies"], json!(["Ship it", "Hold off"]));
            assert_eq!(value["channel"], channel.as_str());
        }
        let staged = reply_options::take(
            h.channels.as_ref(),
            &channel,
            &PrincipalId(PRINCIPAL.into()),
        )
        .await;
        assert_eq!(staged, ["Ship it", "Hold off"]);
    }

    #[tokio::test]
    async fn empty_replies_clear() {
        let h = ToolHarness::new().await;
        let channel = conversation(&h, SESSION).await;
        let caller = in_conversation(channel.as_str(), Trust::Owner, SESSION);
        h.call_as(&caller, "Suggest", json!({ "replies": ["A", "B"] }))
            .await
            .ok();
        h.call_as(&caller, "Suggest", json!({ "replies": [] }))
            .await
            .ok();
        let staged = reply_options::take(
            h.channels.as_ref(),
            &channel,
            &PrincipalId(PRINCIPAL.into()),
        )
        .await;
        assert!(staged.is_empty());
    }

    #[tokio::test]
    async fn bad_calls_are_refused() {
        let h = ToolHarness::new().await;
        let channel = conversation(&h, SESSION).await;
        let caller = in_conversation(channel.as_str(), Trust::Owner, SESSION);
        for (params, expected) in [
            // The dispatcher's schema check answers these first.
            (json!({}), "'replies'"),
            (json!({ "replies": "Yes" }), "replies"),
            (json!({ "replies": ["only"] }), "2 to 4 options"),
            (json!({ "replies": ["same", "same"] }), "twice"),
        ] {
            h.call_as(&caller, "Suggest", params).await.err(expected);
        }

        // Cron and internal work have no conversation to reply in.
        h.call_as(
            &Caller::default(),
            "Suggest",
            json!({ "replies": ["A", "B"] }),
        )
        .await
        .err("not in a conversation");

        // A worker the conversation delegated to shares the compartment
        // but not the session.
        h.call_as(
            &in_conversation(
                channel.as_str(),
                Trust::Owner,
                "9a1c2b3d-0000-4000-8000-000000000001",
            ),
            "Suggest",
            json!({ "replies": ["A", "B"] }),
        )
        .await
        .err("only the conversation's own reply");
    }
}
