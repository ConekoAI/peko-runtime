//! `ChannelRead` — `peko_channel_read` tool impl.
//!
//! Mirrors the shape of `PlanGetTool` (`plan/get.rs`):
//!   `pub struct X { port: Arc<dyn ...> }` with `execute_with_context`
//!   pulling `PrincipalId` out of the `ToolContext`. The principal
//!   boundary is enforced by the caller (the F37 funnel + capability
//!   gate); this tool itself is a thin wrapper around `ChannelPort::peek`.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{ChannelError, ChannelId, ChannelPort, Checkpoint};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

/// Wire name registered with the ExtensionCore.
pub const CHANNEL_READ_TOOL_NAME: &str = "ChannelRead";

/// Read events from a channel the calling principal is a member of.
///
/// Constructed with an [`Arc<dyn ChannelPort>`] (the same handle the
/// daemon holds on `AppState`). The principal boundary is preserved —
/// this tool only ever reads events for the channel the LLM asks
/// about; the F37 funnel at execute-time is what enforces that the
/// caller is a member.
pub struct ChannelReadTool {
    port: Arc<dyn ChannelPort>,
}

impl ChannelReadTool {
    /// Build a new tool backed by `port`.
    #[must_use]
    pub fn new(port: Arc<dyn ChannelPort>) -> Self {
        Self { port }
    }
}

#[async_trait]
impl Tool for ChannelReadTool {
    fn name(&self) -> &'static str {
        CHANNEL_READ_TOOL_NAME
    }

    fn description(&self) -> String {
        "Read events from a peko channel the calling principal is a member of.\n\n\
         Parameters:\n\
         - channel: string (required) — channel id, e.g. 'chan_a1b2c3d4' or a \
         named group 'group:<slug>'\n\
         - since:   string (optional) — opaque cursor from a previous read; \
         omit to start from the beginning\n\
         - limit:   int    (optional) — cap the number of events returned\n\n\
         Returns an array of {kind, ...} event objects in causal order. \
         The kind tag matches peko_protocol::channel::ChannelEvent \
         (created / posted / member_joined / member_left)."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel id (e.g. 'chan_a1b2c3d4' or a named group 'group:<slug>')"
                },
                "since": {
                    "type": "string",
                    "description": "Opaque cursor from a prior read; omit to start from the beginning"
                },
                "limit": {
                    "type": "integer",
                    "description": "Cap the number of events returned",
                    "minimum": 1
                }
            },
            "required": ["channel"]
        })
    }

    fn parallelizable(&self) -> bool {
        // Reads are pure + idempotent; multiple ChannelRead calls can
        // safely overlap.
        true
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // ChannelRead requires a principal context (the F37 funnel
        // surfaces this), so the bare `execute` path is never hit in
        // production — surface a clear error if it is.
        Err(anyhow::anyhow!(
            "ChannelRead requires a ToolContext (use execute_with_context)"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        // Parse + validate arguments.
        let channel_str = params
            .get("channel")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ChannelRead requires 'channel' (string)"))?;

        let channel_id = ChannelId::parse(channel_str).ok_or_else(|| {
            anyhow::anyhow!(
                "ChannelRead: '{channel_str}' is not a valid channel id \
                 (expected 'chan_<8 base36 chars>' or 'group:<slug>')"
            )
        })?;

        let since = params
            .get("since")
            .and_then(|v| v.as_str())
            .map_or_else(Checkpoint::zero, |s| Checkpoint(s.to_string()));

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        // Pull the principal id out of the ToolContext. The F37 funnel
        // supplies this; bare `execute` callers (none in production)
        // get a hard error.
        let principal_str = ctx
            .principal_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ChannelRead requires a principal context"))?;

        // Resolve membership. We surface a soft-error JSON when the
        // caller isn't a member so the LLM can react, mirroring the
        // PlanGet `not_found_error` pattern. `ChannelError::NotMember`
        // is the only "soft" we accept here — adapter-level errors
        // propagate as hard Err so the framework surfaces them as
        // `success=false`.
        match self.port.list_members(&channel_id).await {
            Ok(members) => {
                // ADR-049 Phase 1: members are Subject-typed. Compare
                // against the caller's principal Subject exactly (no
                // cross-kind match on the bare id string).
                let caller = peko_subject::Subject::from(&peko_subject::PrincipalId(
                    principal_str.clone(),
                ));
                let is_member = members.iter().any(|m| *m == caller);
                if !is_member {
                    return Ok(serde_json::json!({
                        "error": "caller is not a member of this channel",
                        "channel": channel_id.as_str(),
                    }));
                }
            }
            Err(ChannelError::NotFound(_)) => {
                return Ok(serde_json::json!({
                    "error": "channel not found",
                    "channel": channel_id.as_str(),
                }));
            }
            Err(e) => return Err(anyhow::anyhow!("ChannelRead list_members: {e}")),
        }

        // Peek + trim.
        let mut events = self
            .port
            .peek(&channel_id, &since)
            .await
            .map_err(|e| anyhow::anyhow!("ChannelRead peek: {e}"))?;

        if let Some(n) = limit {
            events.truncate(n);
        }

        // Return the events as a JSON array. Each event serializes via
        // its derived `Serialize` impl (peko_protocol::channel::ChannelEvent).
        let value = serde_json::to_value(&events)?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelError, ChannelEvent};
    use peko_protocol::channel::ChannelId as ProtoChannelId;
    use peko_subject::{PrincipalId, Subject};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::Mutex as AsyncMutex;

    /// In-memory `ChannelPort` for unit tests. Local to this tool's
    /// tests so the tool can be tested without depending on the
    /// engine's mock fixture (PR-5a deleted `engine::channel_responder`).
    #[derive(Default)]
    struct TestChannelPort {
        events: Mutex<HashMap<ChannelId, Vec<ChannelEvent>>>,
        members: AsyncMutex<HashMap<ChannelId, Vec<Subject>>>,
    }

    #[async_trait]
    impl ChannelPort for TestChannelPort {
        async fn create(
            &self,
            _creator: &PrincipalId,
            _opts: peko_channel::CreateOpts,
        ) -> peko_channel::Result<ChannelId> {
            Err(ChannelError::Adapter(
                "create not implemented in test".into(),
            ))
        }

        async fn invite(
            &self,
            _channel: &ChannelId,
            _inviter: &PrincipalId,
            _invitee: &Subject,
        ) -> peko_channel::Result<()> {
            Ok(())
        }

        async fn post(
            &self,
            _channel: &ChannelId,
            _sender: &Subject,
            _msg: peko_channel::PostMsg,
        ) -> peko_channel::Result<String> {
            Err(ChannelError::Adapter("post not implemented in test".into()))
        }

        async fn peek(
            &self,
            channel: &ChannelId,
            since: &Checkpoint,
        ) -> peko_channel::Result<Vec<ChannelEvent>> {
            let events = self.events.lock().unwrap();
            let all = events.get(channel).cloned().unwrap_or_default();
            // Trim everything at-or-before `since`. For the test fixture
            // we don't have real opaque cursor comparison, so we use a
            // crude approach: if `since` is empty, return everything.
            if since.0.is_empty() {
                Ok(all)
            } else {
                Ok(all)
            }
        }

        async fn peek_with_ids(
            &self,
            channel: &ChannelId,
            since: &Checkpoint,
        ) -> peko_channel::Result<Vec<(String, ChannelEvent)>> {
            let events = self.peek(channel, since).await?;
            // Test fixture: assign line numbers by event index; the
            // subscription loop only needs strictly-increasing ids.
            Ok(events
                .into_iter()
                .enumerate()
                .map(|(i, ev)| ((i + 1).to_string(), ev))
                .collect())
        }

        async fn leave(
            &self,
            _channel: &ChannelId,
            _principal: &PrincipalId,
        ) -> peko_channel::Result<()> {
            Ok(())
        }

        async fn list_members(
            &self,
            channel: &ChannelId,
        ) -> peko_channel::Result<Vec<Subject>> {
            let g = self.members.lock().await;
            Ok(g.get(channel).cloned().unwrap_or_default())
        }

        async fn list_for_principal(
            &self,
            _principal: &PrincipalId,
        ) -> peko_channel::Result<Vec<ChannelId>> {
            Ok(Vec::new())
        }

        async fn pin_to_shared(
            &self,
            _channel: &ChannelId,
        ) -> peko_channel::Result<std::path::PathBuf> {
            Err(ChannelError::Adapter(
                "pin_to_shared not implemented in test".into(),
            ))
        }
    }

    fn ctx_with(principal: PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", CHANNEL_READ_TOOL_NAME)
            .with_principal_id(principal.0)
    }

    fn chan_id() -> ChannelId {
        ProtoChannelId::generate()
    }

    #[tokio::test]
    async fn read_returns_events_for_existing_channel() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }
        {
            let mut events = port.events.lock().unwrap();
            events.insert(
                channel.clone(),
                vec![ChannelEvent::Created {
                    channel: channel.clone(),
                    creator: alice.0.clone(),
                    name: "team".into(),
                    at: "2026-08-05T12:00:00Z".into(),
                }],
            );
        }

        let tool = ChannelReadTool::new(port);
        let got = tool
            .execute_with_context(json!({ "channel": channel.as_str() }), &ctx_with(alice))
            .await
            .unwrap();
        let arr = got.as_array().expect("events array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], "created");
    }

    #[tokio::test]
    async fn read_respects_since_checkpoint_and_limit() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&alice)]);
        }
        // 3 events.
        {
            let mut events = port.events.lock().unwrap();
            events.insert(
                channel.clone(),
                vec![
                    ChannelEvent::Created {
                        channel: channel.clone(),
                        creator: alice.0.clone(),
                        name: "team".into(),
                        at: "2026-08-05T12:00:00Z".into(),
                    },
                    ChannelEvent::Posted {
                        channel: channel.clone(),
                        author: alice.0.clone(),
                        parent: None,
                        text: "first".into(),
                        at: "2026-08-05T12:00:30Z".into(),
                    },
                    ChannelEvent::Posted {
                        channel: channel.clone(),
                        author: alice.0.clone(),
                        parent: None,
                        text: "second".into(),
                        at: "2026-08-05T12:01:00Z".into(),
                    },
                ],
            );
        }

        let tool = ChannelReadTool::new(port);
        let got = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "since": "node_abc", "limit": 2 }),
                &ctx_with(alice),
            )
            .await
            .unwrap();
        let arr = got.as_array().expect("events array");
        // TestChannelPort doesn't actually filter by `since`, so the
        // `limit` cap is the only truncation we exercise here. The
        // production `ChannelStore` does the real filtering.
        assert_eq!(arr.len(), 2, "limit should truncate to 2 events");
    }

    #[tokio::test]
    async fn read_soft_errors_on_unknown_channel() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelReadTool::new(port);

        // Use a syntactically valid channel id (matches
        // `ChannelId::parse`'s prefix + 8-base36-char rule) but which
        // isn't in the port's member/event maps. The fixture returns
        // an empty member list, so we hit the soft-error branch.
        let got = tool
            .execute_with_context(json!({ "channel": "chan_zzzzzzzz" }), &ctx_with(alice))
            .await
            .unwrap();
        assert_eq!(got["error"], "caller is not a member of this channel");
        assert_eq!(got["channel"], "chan_zzzzzzzz");
    }

    #[tokio::test]
    async fn read_soft_errors_on_non_member() {
        // Channel exists + alice is not a member → soft error, not Err.
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        let bob = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![Subject::from(&bob)]);
        }
        let tool = ChannelReadTool::new(port);

        let got = tool
            .execute_with_context(json!({ "channel": channel.as_str() }), &ctx_with(alice))
            .await
            .unwrap();
        assert_eq!(got["error"], "caller is not a member of this channel");
    }

    #[tokio::test]
    async fn read_rejects_invalid_channel_id() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelReadTool::new(port);

        let res = tool
            .execute_with_context(json!({ "channel": "not-a-chan-id" }), &ctx_with(alice))
            .await;
        assert!(res.is_err(), "invalid channel id must propagate as Err");
    }

    #[tokio::test]
    async fn read_requires_principal_context() {
        let port = Arc::new(TestChannelPort::default());
        let tool = ChannelReadTool::new(port);
        // Bare `execute` — no principal context.
        let res = tool.execute(json!({ "channel": "chan_a1b2c3d4" })).await;
        assert!(
            res.is_err(),
            "bare execute must surface principal-missing error"
        );
    }
}
