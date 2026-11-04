//! `Attach` — share files in the run's conversation (ADR-088 D7).
//!
//! The counterpart of a user's attachments: the peko hands files to the
//! person it is talking to. Each file is copied into the workspace
//! (`attachments/<YYYY-MM-DD>/<8 hex>-<name>`, the same place and limits
//! as inbound attachments) and posted on the run's conversation channel
//! as the principal's own message, so clients show it beside the reply.
//!
//! ADR-094: in a conversation with anyone on another runtime or the Hub,
//! the files also go into the channel's file store under their digest,
//! within the remote limits, and the receiving runtime pulls the bytes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use peko_auth::ActorRef;
use peko_channel::{ChannelId, ChannelPort, PostMsg};
use peko_subject::PrincipalId;
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

use crate::principal::attachments;
use crate::security::floor::{check_path, PathDecision};

/// Wire name registered with the ToolingRuntime.
pub const ATTACH_TOOL_NAME: &str = "Attach";

/// Post files to the calling run's conversation.
pub struct AttachTool {
    port: Arc<dyn ChannelPort>,
}

impl AttachTool {
    #[must_use]
    pub fn new(port: Arc<dyn ChannelPort>) -> Self {
        Self { port }
    }
}

/// `raw` as an absolute path: `~` expanded, relative paths taken from
/// the workspace (as the file tools do).
fn resolve(raw: &str, workspace: &Path) -> PathBuf {
    let path = crate::tools::builtin::paths::expand_tilde(raw);
    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

#[async_trait]
impl Tool for AttachTool {
    fn name(&self) -> &'static str {
        ATTACH_TOOL_NAME
    }

    fn description(&self) -> String {
        r#"Send files to the person you are talking to. Each file is copied into your workspace's `attachments/` folder and posted in this conversation as a message from you, with `text` as an optional caption; it appears beside your reply (images and SVG inline, audio and video playable, anything else opens in its app). Use it for something you made or found that they should have — a chart, a report, a spreadsheet, a recording. Don't paste a file's contents or path in your reply instead.

Up to 8 files per call, each up to 20 MB — or, when someone in the conversation is on another computer, each up to 10 MB and 25 MB in all. Only in a conversation (not in cron or internal work).

## Result
`{success, channel, id, attachments: [{name, path, mime, size}]}` — `path` is your copy."#
            .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "maxItems": attachments::MAX_FILES,
                    "description": "Files to send: absolute, `~/…`, or relative to your workspace."
                },
                "text": {
                    "type": "string",
                    "description": "Optional caption posted with the files."
                }
            },
            "required": ["paths"]
        })
    }

    fn parallelizable(&self) -> bool {
        // A channel post: appends ordered lines, like ChannelSend.
        false
    }

    async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        Err(anyhow::anyhow!(
            "Attach requires a ToolContext (use execute_with_context)"
        ))
    }

    async fn execute_with_context(
        &self,
        params: serde_json::Value,
        ctx: &ToolContext,
    ) -> anyhow::Result<serde_json::Value> {
        let raw_paths: Vec<String> = params
            .get("paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow::anyhow!("Attach requires 'paths' (array of strings)"))?
            .iter()
            .map(|p| {
                p.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("Attach: every path must be a string"))
            })
            .collect::<anyhow::Result<_>>()?;
        anyhow::ensure!(!raw_paths.is_empty(), "Attach requires at least one path");
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();

        let principal = ctx
            .principal_id
            .as_deref()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Attach requires a principal"))?;
        let workspace = ctx
            .workspace
            .as_deref()
            .filter(|w| !w.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("Attach requires the principal's workspace"))?;
        let stamp = ctx.acting_for();
        let channel = stamp
            .compartment
            .channel()
            .and_then(ChannelId::parse)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Attach: this run is not in a conversation, so there is no one to send \
                     files to; mention the paths in your result instead"
                )
            })?;
        let remote = attachments::crosses_runtimes(self.port.as_ref(), &channel).await?;

        // ADR-078: the same reach as `Read` — untrusted runs stay inside
        // the workspace, and no non-owner run reads another principal's.
        let mut paths = Vec::with_capacity(raw_paths.len());
        for raw in &raw_paths {
            let path = resolve(raw, &workspace);
            if let PathDecision::Deny(reason) = check_path(&stamp, Some(&workspace), &path, false) {
                anyhow::bail!("Attach: {reason}");
            }
            paths.push(path.display().to_string());
        }
        if remote {
            attachments::validate(&paths, attachments::Limits::REMOTE)
                .await
                .map_err(|e| anyhow::anyhow!("Attach: {e}"))?;
        }
        let copies = attachments::ingest(&workspace, &paths)
            .await
            .map_err(|e| anyhow::anyhow!("Attach: {e}"))?;
        // ADR-094: the bytes go to the channel's file store, where the
        // other runtimes pull them from.
        let posted = if remote {
            let files_dir = self.port.files_dir(&channel).await?;
            attachments::share_with_channel(&files_dir, &workspace, copies.clone())
                .await
                .map_err(|e| anyhow::anyhow!("Attach: {e}"))?
        } else {
            copies.clone()
        };

        let mut msg = PostMsg::root(text).with_attachments(posted);
        if let Some(via) = crate::tools::builtin::channel::channel_send::via_for_ctx(ctx).await {
            msg = msg.with_via(via);
        }
        let id = self
            .port
            .post(
                &channel,
                &ActorRef::from(&PrincipalId(principal.into())),
                msg,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Attach to {} failed: {e}", channel.as_str()))?;

        Ok(json!({
            "success": true,
            "channel": channel.as_str(),
            "id": id,
            "attachments": copies
                .iter()
                .map(|a| json!({
                    "name": a.name,
                    "path": attachments::absolute(&workspace, a).display().to_string(),
                    "mime": a.mime,
                    "size": a.size,
                }))
                .collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::builtin::test_harness::{Caller, ToolHarness, PRINCIPAL};
    use peko_channel::{ChannelEvent, Checkpoint};
    use peko_tools_core::{ActingFor, Compartment, Origin, Trust};
    use serde_json::json;

    fn in_conversation(channel: &str, trust: Trust) -> Caller {
        Caller::acting_for(ActingFor {
            actor: "user:local:local".into(),
            name: None,
            trust,
            compartment: Compartment::Conversation {
                channel: channel.into(),
            },
            origin: Origin::Ingress { channel: None },
        })
    }

    /// The files are copied into the workspace and posted on the run's
    /// conversation as one message from the principal, caption included.
    #[tokio::test]
    async fn files_are_copied_and_posted_in_the_conversation() {
        let h = ToolHarness::new().await;
        let channel = h.channel(PRINCIPAL, "chat").await;
        h.write("out/chart.svg", "<svg xmlns='http://www.w3.org/2000/svg'/>");
        h.write("out/report.md", "# Q3");

        let value = h
            .call_as(
                &in_conversation(channel.as_str(), Trust::Owner),
                "Attach",
                json!({"paths": ["out/chart.svg", "out/report.md"], "text": "Here you go"}),
            )
            .await
            .ok();
        assert_eq!(value["channel"], channel.as_str());
        let sent = value["attachments"].as_array().unwrap();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0]["name"], "chart.svg");
        assert_eq!(sent[0]["mime"], "image/svg+xml");
        let copy = std::path::Path::new(sent[0]["path"].as_str().unwrap());
        assert!(copy.starts_with(h.workspace().join("attachments")));
        assert!(std::fs::read_to_string(copy).unwrap().starts_with("<svg"));

        let posts: Vec<_> = h
            .channels
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap()
            .into_iter()
            .filter_map(|event| match event {
                ChannelEvent::Posted {
                    author,
                    text,
                    attachments,
                    ..
                } => Some((author, text, attachments)),
                _ => None,
            })
            .collect();
        assert_eq!(posts.len(), 1);
        let (author, text, attachments) = &posts[0];
        assert!(author.contains(PRINCIPAL), "{author}");
        assert_eq!(text, "Here you go");
        assert_eq!(attachments.len(), 2);
        assert!(attachments[1].path.starts_with("attachments/"));
        assert!(attachments[1].path.ends_with("-report.md"));
    }

    /// ADR-078: like `Read`, a non-owner run's call goes to the policy
    /// reviewer first (none is configured here, so it is refused).
    #[tokio::test]
    async fn non_owner_runs_go_to_the_policy_reviewer() {
        let h = ToolHarness::new().await;
        let channel = h.channel(PRINCIPAL, "chat").await;
        h.write("ok.txt", "ok");
        h.call_as(
            &in_conversation(channel.as_str(), Trust::Trusted),
            "Attach",
            json!({"paths": ["ok.txt"]}),
        )
        .await
        .err("policy");
        assert!(!h.workspace().join("attachments").exists());
    }

    /// Past review, an untrusted run still sends only workspace files.
    #[tokio::test]
    async fn untrusted_runs_send_only_workspace_files() {
        let h = ToolHarness::new().await;
        let channel = h.channel(PRINCIPAL, "chat").await;
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "s").unwrap();
        h.write("inside.txt", "fine");

        let tool = super::AttachTool::new(h.channels.clone());
        let ctx = peko_tools_core::ToolContext::default_for_tool("Attach")
            .with_principal_id(PRINCIPAL)
            .with_workspace(h.workspace().display().to_string())
            .with_acting_for(in_conversation(channel.as_str(), Trust::Untrusted).acting_for);
        let refused = peko_tools_core::Tool::execute_with_context(
            &tool,
            json!({"paths": [secret.display().to_string()]}),
            &ctx,
        )
        .await
        .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("outside this principal's workspace"),
            "{refused}"
        );
        assert!(!h.workspace().join("attachments").exists());

        peko_tools_core::Tool::execute_with_context(&tool, json!({"paths": ["inside.txt"]}), &ctx)
            .await
            .unwrap();
        assert!(h.workspace().join("attachments").exists());
    }

    /// ADR-094: a conversation with a Hub account gets the files too —
    /// stored under their digest for the other runtime to pull, within
    /// the remote limits.
    #[tokio::test]
    async fn conversations_with_remote_members_get_files_by_digest() {
        let h = ToolHarness::new().await;
        let channel = h.channel(PRINCIPAL, "chat").await;
        h.channels
            .invite(
                &channel,
                &peko_subject::PrincipalId(PRINCIPAL.into()),
                &peko_auth::ActorRef::User(peko_subject::Route::hub("acct")),
            )
            .await
            .unwrap();
        h.write("ok.txt", "ok");
        let caller = in_conversation(channel.as_str(), Trust::Owner);
        h.call_as(&caller, "Attach", json!({"paths": ["ok.txt"]}))
            .await
            .ok();
        let posted = h
            .channels
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap()
            .into_iter()
            .find_map(|e| match e {
                ChannelEvent::Posted { attachments, .. } if !attachments.is_empty() => {
                    Some(attachments)
                }
                _ => None,
            })
            .unwrap();
        let digest = posted[0].sha256.clone().expect("a digest");
        let stored = h
            .channels
            .files_dir(&channel)
            .await
            .unwrap()
            .join(&digest)
            .join("ok.txt");
        assert_eq!(std::fs::read_to_string(stored).unwrap(), "ok");
        // The peko keeps its own copy as before.
        assert!(posted[0].path.starts_with("attachments/"));

        // Over the remote per-file limit: refused, nothing posted.
        let big = h.workspace().join("big.bin");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(crate::principal::attachments::REMOTE_MAX_FILE_BYTES + 1)
            .unwrap();
        h.call_as(&caller, "Attach", json!({"paths": ["big.bin"]}))
            .await
            .err("another computer");
    }

    /// A missing file refuses the whole call before anything is posted.
    #[tokio::test]
    async fn a_bad_path_posts_nothing() {
        let h = ToolHarness::new().await;
        let channel = h.channel(PRINCIPAL, "chat").await;
        h.write("ok.txt", "ok");
        h.call_as(
            &in_conversation(channel.as_str(), Trust::Owner),
            "Attach",
            json!({"paths": ["ok.txt", "missing.txt"]}),
        )
        .await
        .err("cannot read attachment");
        let events = h
            .channels
            .peek(&channel, &Checkpoint::default())
            .await
            .unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, ChannelEvent::Posted { .. })));
    }
}
