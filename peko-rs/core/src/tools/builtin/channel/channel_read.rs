//! `ChannelRead` — `peko_channel_read` tool impl.
//!
//! Mirrors the shape of `PlanGetAction` (`plan/get.rs`):
//!   `pub struct X { port: Arc<dyn ...> }` with `execute_with_context`
//!   pulling `PrincipalId` out of the `ToolContext`. The principal
//!   membership boundary is enforced by ChannelPort; this tool is a thin
//!   wrapper around `ChannelPort::peek`.

use std::sync::Arc;

use async_trait::async_trait;
use peko_channel::{ChannelError, ChannelId, ChannelPort, Checkpoint};
use peko_tools_core::{Tool, ToolContext};
use serde_json::json;

/// Wire name registered with the ToolingRuntime.
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
         - limit:   int    (optional) — max events returned (default 50, cap 1000)\n\
         - query:   string (optional) — search mode: case-insensitive substring \
         match on message text\n\
         - author:  string (optional) — search mode: exact author match \
         (e.g. 'user:alice' or a principal id)\n\
         - before:  string (optional) — event id; in read mode return the page \
         of events OLDER than it; in search mode, only search older history\n\
         - since:   string (optional) — event id; return events strictly NEWER \
         than it (catch up after a prior read). Overrides `before`. Ignored in \
         search mode.\n\n\
         With no cursor and no query, returns the NEWEST `limit` events. Events \
         are always oldest→newest; each carries an `id` field (its line number \
         in the channel log). The response object is {channel, events, \
         has_more, next_cursor}: when `has_more` is true, pass `next_cursor` \
         back as `before` (or `since` after a forward read) to continue. \
         Search mode scans backward from `before` (or the tip) and returns \
         only matching `posted` events. The kind tag matches \
         peko_protocol::channel::ChannelEvent (created / posted / member_joined / \
         member_left)."
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
                "limit": {
                    "type": "integer",
                    "description": "Max events returned (default 50, cap 1000; search mode caps at 200)",
                    "minimum": 1
                },
                "query": {
                    "type": "string",
                    "description": "Search mode: case-insensitive substring match on message text"
                },
                "author": {
                    "type": "string",
                    "description": "Search mode: exact author match (e.g. 'user:alice' or a principal id)"
                },
                "before": {
                    "type": "string",
                    "description": "Event id; return/search the page of events older than it"
                },
                "since": {
                    "type": "string",
                    "description": "Event id; return events strictly newer than it (read mode only)"
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
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let before = params
            .get("before")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let query_text = params
            .get("query")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let author = params
            .get("author")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, 1000))
            .unwrap_or(50);

        // Pull the principal id out of the ToolContext. The F37 funnel
        // supplies this; bare `execute` callers (none in production)
        // get a hard error.
        let principal_str = ctx
            .principal_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ChannelRead requires a principal context"))?;

        // Resolve membership. We surface a soft-error JSON when the
        // caller isn't a member so the LLM can react, mirroring the
        // Plan action get `not_found_error` pattern. `ChannelError::NotMember`
        // is the only "soft" we accept here — adapter-level errors
        // propagate as hard Err so the framework surfaces them as
        // `success=false`.
        match self.port.list_members(&channel_id).await {
            Ok(members) => {
                // ADR-049 Phase 1: members are Subject-typed. Compare
                // against the caller's principal Subject exactly (no
                // cross-kind match on the bare id string).
                let caller =
                    peko_subject::Subject::from(&peko_subject::PrincipalId(principal_str.clone()));
                let is_member = members.contains(&caller);
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

        // Fetch. Search mode (query/author set) runs the store's
        // bounded backward scan; `since` is a forward catch-up walk;
        // the default path is the tail read — the newest `limit`
        // events, parsed at the store without decoding the whole log.
        let (events, has_more, next_cursor) = if query_text.is_some() || author.is_some() {
            let q = peko_channel::ChannelQuery {
                text: query_text,
                author,
                before,
                limit,
            };
            let page = self
                .port
                .search(&channel_id, &q)
                .await
                .map_err(|e| anyhow::anyhow!("ChannelRead search: {e}"))?;
            (page.events, page.has_more, page.resume_before)
        } else {
            match &since {
                Some(s) => {
                    // The store's checkpoint is an inclusive line offset;
                    // `since` names the last event the caller has seen,
                    // so its own line is dropped to return strictly newer
                    // events (a fed-back `next_cursor` must not repeat).
                    let items: Vec<_> = self
                        .port
                        .peek_with_ids(&channel_id, &Checkpoint(s.clone()))
                        .await
                        .map_err(|e| anyhow::anyhow!("ChannelRead peek: {e}"))?
                        .into_iter()
                        .filter(|(id, _)| id != s)
                        .collect();
                    let has_more = items.len() > limit;
                    let page: Vec<_> = items.into_iter().take(limit).collect();
                    let next_cursor = page.last().map(|(id, _)| id.clone());
                    (page, has_more, next_cursor)
                }
                None => {
                    let page = self
                        .port
                        .peek_tail(&channel_id, limit, before.as_ref())
                        .await
                        .map_err(|e| anyhow::anyhow!("ChannelRead peek_tail: {e}"))?;
                    let next_cursor = if page.has_more {
                        page.events.first().map(|(id, _)| id.clone())
                    } else {
                        None
                    };
                    (page.events, page.has_more, next_cursor)
                }
            }
        };

        // Reading marks everything up to the newest returned line as
        // seen by this session (2026-09-12): the `{{session_context}}`
        // digest advances its read mark on the same (channel, session)
        // pair, so a post the agent just read here never re-reports as
        // "new". Captured before the events are consumed below; the
        // store's monotonic guard keeps a backward page (`before`)
        // from rewinding the mark.
        let max_seen = events.last().map(|(id, _)| id.clone());

        // Serialize each event with its line-number id merged in, so
        // the caller can thread replies (`parent`) and page cursors.
        let events = events
            .into_iter()
            .map(|(id, ev)| {
                let mut v = serde_json::to_value(&ev)?;
                if let serde_json::Value::Object(map) = &mut v {
                    map.insert("id".into(), serde_json::Value::String(id));
                }
                Ok(v)
            })
            .collect::<serde_json::Result<Vec<_>>>()?;

        // Advance the session's digest read mark to the newest returned
        // line — only on success and only when the page was non-empty.
        // Best-effort: a persistence failure must not fail the read.
        if let (Some(session_key), Some(mark)) =
            (ctx.session_id.clone().filter(|s| !s.is_empty()), max_seen)
        {
            if let Err(e) = self
                .port
                .advance_read_mark(&channel_id, &session_key, mark)
                .await
            {
                tracing::debug!("ChannelRead: advance_read_mark failed: {e}");
            }
        }

        Ok(json!({
            "channel": channel_id.as_str(),
            "events": events,
            "has_more": has_more,
            "next_cursor": next_cursor,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, CreateOpts, PostMsg};
    use peko_subject::{PrincipalId, Subject};
    use serde_json::json;

    /// A real file-backed channel store holding one channel created by
    /// alice, with bob (a user) invited.
    struct Fixture {
        _dir: tempfile::TempDir,
        store: Arc<ChannelStore>,
        tool: ChannelReadTool,
        alice: PrincipalId,
        channel: ChannelId,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ChannelStore::new(ChannelConfig {
                runtime_dir: dir.path().to_path_buf(),
                shared_dir: None,
            }));
            let alice = PrincipalId::generate();
            let channel = store
                .create(
                    &alice,
                    CreateOpts {
                        name: "team".into(),
                        ..CreateOpts::default()
                    },
                )
                .await
                .unwrap();
            store
                .invite(&channel, &alice, &Subject::User("bob".into()))
                .await
                .unwrap();
            Self {
                tool: ChannelReadTool::new(store.clone()),
                _dir: dir,
                store,
                alice,
                channel,
            }
        }

        async fn post(&self, author: &Subject, text: &str) -> String {
            self.store
                .post(&self.channel, author, PostMsg::root(text))
                .await
                .unwrap()
        }

        fn ctx(&self, session: Option<&str>) -> ToolContext {
            let ctx = ToolContext::for_hook_run("run", "tc", CHANNEL_READ_TOOL_NAME)
                .with_principal_id(self.alice.0.clone());
            match session {
                Some(s) => ctx.with_session_id(s),
                None => ctx,
            }
        }

        async fn read(&self, mut params: serde_json::Value) -> serde_json::Value {
            params["channel"] = json!(self.channel.as_str());
            self.tool
                .execute_with_context(params, &self.ctx(None))
                .await
                .unwrap()
        }
    }

    fn texts(page: &serde_json::Value) -> Vec<String> {
        page["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| match e["kind"].as_str().unwrap() {
                "posted" => e["text"].as_str().unwrap().to_string(),
                other => format!("<{other}>"),
            })
            .collect()
    }

    /// Without a cursor the newest `limit` events come back; `next_cursor`
    /// pages backward through `before` until the channel's start.
    #[tokio::test]
    async fn tail_read_pages_backward_to_the_start() {
        let fx = Fixture::new().await;
        let alice = Subject::from(&fx.alice);
        let first = fx.post(&alice, "first").await;
        fx.post(&alice, "second").await;

        let page = fx.read(json!({ "limit": 2 })).await;
        assert_eq!(texts(&page), ["first", "second"]);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["next_cursor"], first.as_str());
        assert_eq!(
            page["events"][0]["id"],
            first.as_str(),
            "events carry their ids"
        );

        let mut seen = Vec::new();
        let mut before = page["next_cursor"].clone();
        while !before.is_null() {
            let older = fx.read(json!({ "limit": 1, "before": before })).await;
            seen.extend(texts(&older));
            before = older["next_cursor"].clone();
        }
        assert_eq!(
            seen.last().map(String::as_str),
            Some("<created>"),
            "{seen:?}"
        );
        assert!(
            !seen.iter().any(|t| t == "first" || t == "second"),
            "{seen:?}"
        );
    }

    /// `since` returns only events strictly after the cursor, oldest first,
    /// and `next_cursor` continues the forward walk.
    #[tokio::test]
    async fn since_walks_forward_from_the_cursor() {
        let fx = Fixture::new().await;
        let alice = Subject::from(&fx.alice);
        let first = fx.post(&alice, "first").await;
        let second = fx.post(&alice, "second").await;
        fx.post(&alice, "third").await;

        let page = fx.read(json!({ "since": first, "limit": 1 })).await;
        assert_eq!(texts(&page), ["second"]);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["next_cursor"], second.as_str());

        let page = fx.read(json!({ "since": second })).await;
        assert_eq!(texts(&page), ["third"]);
        assert_eq!(page["has_more"], false);
    }

    #[tokio::test]
    async fn missing_channel_and_non_member_are_soft_errors() {
        let fx = Fixture::new().await;
        let got = fx
            .tool
            .execute_with_context(json!({ "channel": "chan_zzzzzzzz" }), &fx.ctx(None))
            .await
            .unwrap();
        assert_eq!(got["error"], "channel not found");
        assert_eq!(got["channel"], "chan_zzzzzzzz");

        let outsider = ToolContext::for_hook_run("run", "tc", CHANNEL_READ_TOOL_NAME)
            .with_principal_id(PrincipalId::generate().0);
        let got = fx
            .tool
            .execute_with_context(json!({ "channel": fx.channel.as_str() }), &outsider)
            .await
            .unwrap();
        assert_eq!(got["error"], "caller is not a member of this channel");
    }

    #[tokio::test]
    async fn malformed_calls_are_rejected() {
        let fx = Fixture::new().await;
        for params in [json!({}), json!({ "channel": "not-a-chan-id" })] {
            assert!(
                fx.tool
                    .execute_with_context(params.clone(), &fx.ctx(None))
                    .await
                    .is_err(),
                "{params}"
            );
        }
        let no_principal = ToolContext::for_hook_run("run", "tc", CHANNEL_READ_TOOL_NAME);
        let error = fx
            .tool
            .execute_with_context(json!({ "channel": fx.channel.as_str() }), &no_principal)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("principal context"), "{error}");
        assert!(fx
            .tool
            .execute(json!({ "channel": fx.channel.as_str() }))
            .await
            .is_err());
    }

    /// Search mode matches text case-insensitively and narrows by author.
    #[tokio::test]
    async fn search_filters_by_text_and_author() {
        let fx = Fixture::new().await;
        let bob = Subject::User("bob".into());
        let bobs = fx.post(&bob, "deploy the release").await;
        fx.post(&Subject::from(&fx.alice), "release notes drafted")
            .await;
        fx.post(&bob, "unrelated").await;

        let got = fx.read(json!({ "query": "RELEASE" })).await;
        let mut found = texts(&got);
        found.sort();
        assert_eq!(found, ["deploy the release", "release notes drafted"]);

        let got = fx
            .read(json!({ "query": "release", "author": "user:bob" }))
            .await;
        assert_eq!(texts(&got), ["deploy the release"]);
        assert_eq!(
            got["events"][0]["id"],
            bobs.as_str(),
            "the match carries its id"
        );
    }

    /// Reading advances the session's digest read mark to the newest
    /// returned line; paging backward never rewinds it, and a read without
    /// a session leaves marks alone.
    #[tokio::test]
    async fn reads_advance_the_session_read_mark_monotonically() {
        let fx = Fixture::new().await;
        let alice = Subject::from(&fx.alice);
        let first = fx.post(&alice, "one").await;
        let newest = fx.post(&alice, "two").await;

        fx.tool
            .execute_with_context(json!({ "channel": fx.channel.as_str() }), &fx.ctx(None))
            .await
            .unwrap();
        assert_eq!(
            fx.store.read_mark(&fx.channel, "sess-xyz").await.unwrap(),
            None
        );

        fx.tool
            .execute_with_context(
                json!({ "channel": fx.channel.as_str() }),
                &fx.ctx(Some("sess-xyz")),
            )
            .await
            .unwrap();
        assert_eq!(
            fx.store.read_mark(&fx.channel, "sess-xyz").await.unwrap(),
            Some(newest.clone())
        );

        fx.tool
            .execute_with_context(
                json!({ "channel": fx.channel.as_str(), "before": newest, "limit": 1 }),
                &fx.ctx(Some("sess-xyz")),
            )
            .await
            .unwrap();
        assert_eq!(
            fx.store.read_mark(&fx.channel, "sess-xyz").await.unwrap(),
            Some(newest),
            "a backward page (ending at {first}) must not rewind the mark"
        );
    }
}
