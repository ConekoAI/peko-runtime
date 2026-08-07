//! `ChannelSend` — `peko_channel_send` tool impl.
//!
//! Symmetric counterpart of `ChannelReadTool`
//! (`tools/builtin/channel/channel_read.rs`):
//!   `pub struct X { port: Arc<dyn ...> }` with `execute_with_context`
//!   pulling `PrincipalId` out of the `ToolContext`. The principal
//!   boundary is enforced by the F37 funnel + capability gate; this
//!   tool itself is a thin wrapper around `ChannelPort::post`.
//!
//! ## Why a symmetric tool (vs an internal-only post path)
//!
//! `ChannelRead` exists so a principal's agentic loop can pull
//! channel events on demand. If the principal can only READ but not
//! POST, every reactive loop has to fall through `peko channel post`
//! CLI subprocesses — which leaks process boundaries, breaks the
//! audit ring buffer's per-tool attribution, and forces the agentic
//! loop to know about the CLI surface. Symmetry preserves the
//! principal boundary (`ctx.principal_id` is the `ChannelPort::post`
//! `sender` argument) and keeps the audit trail uniform.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{ChannelError, ChannelId, ChannelPort, PostMsg};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

/// Wire name registered with the ExtensionCore.
pub const CHANNEL_SEND_TOOL_NAME: &str = "ChannelSend";

/// Post a message to a channel the calling principal is a member of.
///
/// Constructed with an [`Arc<dyn ChannelPort>`] (the same handle the
/// daemon holds on `AppState`). The principal boundary is preserved —
/// this tool only ever posts as the principal that the funnel surfaces
/// in the [`ToolContext`]. Membership is enforced inside
/// [`ChannelPort::post`] itself (the F37 funnel does not pre-check),
/// which means a hard `NotMember` error surfaces as `success=false` to
/// the LLM rather than a soft JSON.
pub struct ChannelSendTool {
    port: Arc<dyn ChannelPort>,
}

impl ChannelSendTool {
    /// Build a new tool backed by `port`.
    #[must_use]
    pub fn new(port: Arc<dyn ChannelPort>) -> Self {
        Self { port }
    }
}

#[async_trait]
impl Tool for ChannelSendTool {
    fn name(&self) -> &'static str {
        CHANNEL_SEND_TOOL_NAME
    }

    fn description(&self) -> String {
        "Post a message to a peko channel the calling principal is a member of.\n\n\
         Parameters:\n\
         - channel: string (required) — channel id, e.g. 'chan_a1b2c3d4'\n\
         - text:    string (required) — message body\n\
         - parent:  string (optional) — TaskId of the message being \
         replied to; omit to post a root message\n\n\
         Returns { task_id: string } where task_id is the line number the \
         new message was assigned in the channel's append-only event log. \
         Pass it as `parent` for a reply, or to `ChannelRead`'s `since` \
         cursor to fetch everything after it."
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Channel id (e.g. 'chan_a1b2c3d4')"
                },
                "text": {
                    "type": "string",
                    "description": "Message body"
                },
                "parent": {
                    "type": "string",
                    "description": "TaskId of the message being replied to; omit to post a root message"
                }
            },
            "required": ["channel", "text"]
        })
    }

    fn parallelizable(&self) -> bool {
        // Posts are NOT parallel-safe for the same channel: each post
        // appends a JSONL line, and the line-number TaskId assigned
        // depends on the order in which writes hit disk. Two parallel
        // posts in the same channel can race the lock and force one
        // caller to retry (or, worse, observe an ordering the channel
        // reader won't reconstruct). Different channels are fine.
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        // ChannelSend requires a principal context (the F37 funnel
        // surfaces this), so the bare `execute` path is never hit in
        // production — surface a clear error if it is.
        Err(anyhow::anyhow!(
            "ChannelSend requires a ToolContext (use execute_with_context)"
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
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires 'channel' (string)"))?;

        let channel_id = ChannelId::parse(channel_str).ok_or_else(|| {
            anyhow::anyhow!(
                "ChannelSend: '{channel_str}' is not a valid channel id \
                 (expected 'chan_<8 base36 chars>')"
            )
        })?;

        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires 'text' (string)"))?;
        if text.is_empty() {
            return Err(anyhow::anyhow!("ChannelSend requires non-empty 'text'"));
        }

        let parent = params
            .get("parent")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Pull the principal id out of the ToolContext. The F37 funnel
        // supplies this; bare `execute` callers (none in production)
        // get a hard error.
        let principal_str = ctx
            .principal_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ChannelSend requires a principal context"))?;

        let msg = match parent {
            Some(p) => PostMsg::reply(p, text.to_string()),
            None => PostMsg::root(text.to_string()),
        };

        let sender = peko_subject::PrincipalId(principal_str);
        match self.port.post(&channel_id, &sender, msg).await {
            Ok(task_id) => Ok(serde_json::json!({
                "task_id": task_id,
                "channel": channel_id.as_str(),
            })),
            Err(ChannelError::NotMember) => Ok(serde_json::json!({
                "error": "caller is not a member of this channel",
                "channel": channel_id.as_str(),
            })),
            Err(ChannelError::NotFound(_)) => Ok(serde_json::json!({
                "error": "channel not found",
                "channel": channel_id.as_str(),
            })),
            Err(e) => Err(anyhow::anyhow!("ChannelSend post: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelError, ChannelEvent, Checkpoint, CreateOpts};
    use peko_subject::PrincipalId;
    use peko_tools_core::ToolContext;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tokio::sync::Mutex as AsyncMutex;

    /// In-memory `ChannelPort` for unit tests. Local to this tool's
    /// tests so the tool can be tested without depending on a real
    /// `ChannelStore` (file-backed) — that integration path is covered
    /// by `peko-channel`'s own `tests/integration.rs`.
    #[derive(Default)]
    struct TestChannelPort {
        events: Mutex<HashMap<ChannelId, Vec<ChannelEvent>>>,
        members: AsyncMutex<HashMap<ChannelId, Vec<PrincipalId>>>,
        next_line: AsyncMutex<HashMap<ChannelId, u64>>,
    }

    impl TestChannelPort {
        fn is_member(&self, channel: &ChannelId, principal: &PrincipalId) -> bool {
            // Sync lookup against the snapshot taken under the async
            // lock — fine for tests, since TestChannelPort is only
            // exercised from a single-threaded tokio runtime here.
            if let Ok(g) = self.members.try_lock() {
                g.get(channel)
                    .map(|m| m.iter().any(|p| p == principal))
                    .unwrap_or(false)
            } else {
                false
            }
        }
    }

    #[async_trait]
    impl ChannelPort for TestChannelPort {
        async fn create(
            &self,
            _creator: &PrincipalId,
            _opts: CreateOpts,
        ) -> peko_channel::Result<ChannelId> {
            Err(ChannelError::Adapter("create not implemented in test".into()))
        }

        async fn invite(
            &self,
            _channel: &ChannelId,
            _inviter: &PrincipalId,
            _invitee: &PrincipalId,
        ) -> peko_channel::Result<()> {
            Ok(())
        }

        async fn post(
            &self,
            channel: &ChannelId,
            sender: &PrincipalId,
            msg: PostMsg,
        ) -> peko_channel::Result<String> {
            if !self.is_member(channel, sender) {
                return Err(ChannelError::NotMember);
            }
            let mut lines = self.next_line.lock().await;
            let line = lines.get(channel).copied().unwrap_or(0);
            lines.insert(channel.clone(), line + 1);
            drop(lines);
            let ev = ChannelEvent::Posted {
                channel: channel.clone(),
                author: sender.to_string(),
                parent: msg.parent,
                text: msg.text,
                at: "2026-08-06T12:00:00Z".into(),
            };
            let mut events = self.events.lock().unwrap();
            events.entry(channel.clone()).or_default().push(ev);
            Ok(line.to_string())
        }

        async fn peek(
            &self,
            channel: &ChannelId,
            _since: &Checkpoint,
        ) -> peko_channel::Result<Vec<ChannelEvent>> {
            let events = self.events.lock().unwrap();
            Ok(events.get(channel).cloned().unwrap_or_default())
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
        ) -> peko_channel::Result<Vec<PrincipalId>> {
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
            Err(ChannelError::Adapter("pin_to_shared not implemented in test".into()))
        }
    }

    fn ctx_with(principal: &PrincipalId) -> ToolContext {
        ToolContext::for_hook_run("run", "tc", CHANNEL_SEND_TOOL_NAME)
            .with_principal_id(principal.0.clone())
    }

    fn chan_id() -> ChannelId {
        ChannelId::generate()
    }

    #[tokio::test]
    async fn send_returns_task_id_for_root_post() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![alice.clone()]);
        }

        let tool = ChannelSendTool::new(port.clone());
        let got = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "hello world" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["channel"], channel.as_str());
        assert_eq!(got["task_id"], "0", "first post in a channel is line 0");
    }

    #[tokio::test]
    async fn send_advances_task_id_per_post() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![alice.clone()]);
        }

        let tool = ChannelSendTool::new(port.clone());
        let first = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "first" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        let second = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "second" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(first["task_id"], "0");
        assert_eq!(second["task_id"], "1");
    }

    #[tokio::test]
    async fn send_accepts_parent_for_reply() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![alice.clone()]);
        }

        let tool = ChannelSendTool::new(port.clone());
        let root = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "root" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        let reply = tool
            .execute_with_context(
                json!({
                    "channel": channel.as_str(),
                    "text": "reply",
                    "parent": root["task_id"].as_str().unwrap(),
                }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(reply["task_id"], "1");
        // Confirm the reply actually wired the parent into the event
        // log.
        let events = port.events.lock().unwrap();
        let reply_event = events
            .get(&channel)
            .and_then(|v| v.last())
            .expect("reply event");
        match reply_event {
            ChannelEvent::Posted { parent, text, .. } => {
                assert_eq!(parent.as_deref(), Some("0"));
                assert_eq!(text, "reply");
            }
            other => panic!("expected Posted event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_soft_errors_on_non_member() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        let bob = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![bob.clone()]);
        }
        let tool = ChannelSendTool::new(port.clone());

        let got = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "hi" }),
                &ctx_with(&alice),
            )
            .await
            .unwrap();
        assert_eq!(got["error"], "caller is not a member of this channel");
        assert_eq!(got["channel"], channel.as_str());
    }

    #[tokio::test]
    async fn send_rejects_empty_text() {
        let port = Arc::new(TestChannelPort::default());
        let channel = chan_id();
        let alice = PrincipalId::generate();
        {
            let mut members = port.members.lock().await;
            members.insert(channel.clone(), vec![alice.clone()]);
        }
        let tool = ChannelSendTool::new(port.clone());

        let res = tool
            .execute_with_context(
                json!({ "channel": channel.as_str(), "text": "" }),
                &ctx_with(&alice),
            )
            .await;
        assert!(res.is_err(), "empty text must be rejected as Err");
    }

    #[tokio::test]
    async fn send_rejects_invalid_channel_id() {
        let port = Arc::new(TestChannelPort::default());
        let alice = PrincipalId::generate();
        let tool = ChannelSendTool::new(port.clone());

        let res = tool
            .execute_with_context(
                json!({ "channel": "not-a-chan-id", "text": "hi" }),
                &ctx_with(&alice),
            )
            .await;
        assert!(res.is_err(), "invalid channel id must propagate as Err");
    }

    #[tokio::test]
    async fn send_requires_principal_context() {
        let port = Arc::new(TestChannelPort::default());
        let tool = ChannelSendTool::new(port);
        // Bare `execute` — no principal context.
        let res = tool
            .execute(json!({ "channel": "chan_a1b2c3d4", "text": "hi" }))
            .await;
        assert!(res.is_err(), "bare execute must surface principal-missing error");
    }
}