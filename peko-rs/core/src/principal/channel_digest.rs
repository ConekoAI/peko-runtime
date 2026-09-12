//! Channel-activity digest for the `{{session_context}}` tail section.
//!
//! While an agent's loop runs, posts land on channels bound to its
//! session that are NOT delivered as wakes: same-principal posts by
//! other agents, threaded replies, other principals' group posts,
//! human CLI posts. Without this handler the bound agent never learns
//! about them. [`ChannelDigestSessionContextHandler`] renders a
//! per-iteration "N new messages on #channel" digest with previews,
//! tracking per-(session, channel) read positions in the channel's
//! `read_marks.json` (see [`peko_channel::read_marks`]).
//!
//! It rides the existing ADR-052 machinery: the `session_context`
//! slot's change-detection (`RuntimeContextState::take_changed`)
//! re-injects only when the rendered text changes, so each batch of
//! posts is shown once and unchanged iterations pay nothing.
//!
//! ## Deliberate render-time side effect
//!
//! The handler advances the read mark as it renders (to the max line
//! id it observed, *even when every event was filtered*). The mark
//! means "observed up to", not "shown to the model" — a filtered event
//! must not resurface on the next iteration just because the digest
//! chose not to print it. This is the one place the prompt path writes
//! to disk, which is why it is called out here and at the call site.

use std::sync::Arc;

use peko_channel::{ChannelPort, Checkpoint};
use peko_protocol::channel::ChannelEvent;

use crate::extensions::framework::core::{HookContext, HookHandler, HookPoint};
use crate::extensions::framework::types::{HookInput, HookOutput, HookResult};

/// Priority of the digest handler in the `SessionContextBuild`
/// aggregation. Below `SelfPositionSessionContextHandler` (110) and
/// `PeersSessionContextHandler` (100) — the digest is the most
/// volatile line in the section, so it lands last.
pub const CHANNEL_DIGEST_HOOK_PRIORITY: i32 = 90;

/// Max preview lines rendered per channel before the `… +M more`
/// overflow line takes over.
const DIGEST_MAX_PREVIEWS_PER_CHANNEL: usize = 5;

/// Max characters of a message body in a preview line (~80 chars,
/// matching the task's "~80-char truncation").
const DIGEST_PREVIEW_MAX_CHARS: usize = 80;

/// Hard cap on the whole digest. Mirrors the skills-catalog cap style
/// (`extensions/skill/prompt.rs`): on overflow, whole lines are
/// dropped from the end and a pointer line is appended instead.
const DIGEST_MAX_BYTES: usize = 2 * 1024;

/// Render the bound channels' unread activity into the
/// `{{session_context}}` section.
///
/// Registered once on the daemon-global core; per-principal state is
/// resolved per invoke from the hook context's workspace + principal
/// id, like the sibling `SessionContextBuild` handlers in
/// `principal::child_turns`. Renders nothing when the session has no
/// bound channels, no unread digest-worthy posts, or the workspace
/// doesn't resolve.
#[derive(Debug)]
pub(crate) struct ChannelDigestSessionContextHandler;

#[async_trait::async_trait]
impl HookHandler for ChannelDigestSessionContextHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let session_id = match &ctx.input {
            HookInput::SessionState(snapshot) => {
                peko_session::id::SessionId::parse(&snapshot.session_id)
            }
            _ => None,
        };
        let Some(session_id) = session_id else {
            return HookResult::PassThrough;
        };
        let tool_ctx =
            ctx.get_state::<crate::extensions::framework::types::ToolRuntimeContext>("tool_context");
        let (workspace, principal_id) = match tool_ctx {
            Some(rtc) => (rtc.workspace.clone(), rtc.principal_id.clone()),
            None => (None, None),
        };
        let (Some(workspace), Some(principal_id)) = (workspace, principal_id) else {
            return HookResult::PassThrough;
        };
        if workspace.is_empty() || principal_id.is_empty() {
            return HookResult::PassThrough;
        }
        let Some(sessions_dir) =
            crate::principal::child_turns::sessions_dir_for_workspace(&workspace)
        else {
            return HookResult::PassThrough;
        };
        let metas = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .list_all_sessions(false)
            .await
            .unwrap_or_default();
        let Some(port) = ctx.services.channel_port() else {
            return HookResult::PassThrough;
        };

        match render_channel_digest(&port, &metas, session_id, &principal_id).await {
            Some(text) => HookResult::Continue(HookOutput::Text(text)),
            None => HookResult::PassThrough,
        }
    }

    fn hook_point(&self) -> HookPoint {
        HookPoint::SessionContextBuild
    }

    fn priority(&self) -> i32 {
        CHANNEL_DIGEST_HOOK_PRIORITY
    }

    fn name(&self) -> String {
        "ChannelDigestSessionContextHandler".to_string()
    }
}

/// Core digest logic, split out from [`ChannelDigestSessionContextHandler::handle`]
/// so it is testable without the workspace/env-dependent session-dir
/// resolution: discovers the session's bound channels, applies
/// first-observation marking, filters to digest-worthy posts, advances
/// the read marks, and renders.
async fn render_channel_digest(
    port: &Arc<dyn ChannelPort>,
    metas: &[peko_session::metadata::SessionMetadata],
    session_id: peko_session::id::SessionId,
    principal_id: &str,
) -> Option<String> {
    let own_meta = metas.iter().find(|m| m.session_id == session_id)?;
    let own_path = peko_session::path::compute_path(metas, session_id);
    let session_key = session_id.as_str();

    // Group-bound standing child sessions are stamped with
    // `peer = Subject::User(<channel wire id>)` (peer_children.rs:200);
    // a group channel has no `passive_binding`, so that stamp is the
    // only binding evidence.
    let group_bound_channel = match (own_meta.peer_type.as_deref(), own_meta.peer_id.as_deref()) {
        (Some("user"), Some(id)) => Some(id),
        _ => None,
    };

    let principal = peko_subject::PrincipalId(principal_id.to_string());
    let channels = match port.list_for_principal(&principal).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("channel digest: list_for_principal failed: {e}");
            return None;
        }
    };

    let mut blocks: Vec<String> = Vec::new();
    for channel in channels {
        // Bound-channel discovery: DM channels carry a
        // `passive_binding` equal to the bound session's `/slug` path
        // or raw id; group channels have no binding and bind via the
        // session's peer stamp instead.
        let is_dm = match port.passive_binding(&channel).await.unwrap_or(None) {
            Some(b) => {
                if b != own_path && b != session_key {
                    continue;
                }
                true
            }
            None => {
                if group_bound_channel != Some(channel.as_str()) {
                    continue;
                }
                false
            }
        };

        // First observation: adopt the current tip as the mark and
        // emit nothing — never dump history into the prompt.
        let mark = match port.read_mark(&channel, &session_key).await {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("channel digest: read_mark({channel}) failed: {e}");
                continue;
            }
        };
        let Some(mark) = mark else {
            match port.peek_tail(&channel, 1, None).await {
                Ok(tail) => {
                    if let Some((id, _)) = tail.events.into_iter().next() {
                        let _ = port.advance_read_mark(&channel, &session_key, id).await;
                    }
                }
                Err(e) => {
                    tracing::debug!("channel digest: peek_tail({channel}) failed: {e}");
                }
            }
            continue;
        };

        // Everything at-or-after the mark, oldest→newest. The store's
        // checkpoint is an *inclusive* count-offset ("drop the first N
        // events", see `ChannelStore::read_events`), while the mark is
        // the last line id the session observed — so the mark's own
        // line is filtered out to get "strictly after".
        let events = match port.peek_with_ids(&channel, &Checkpoint(mark.clone())).await {
            Ok(evs) => evs
                .into_iter()
                .filter(|(id, _)| strictly_after(id, &mark))
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::debug!("channel digest: peek_with_ids({channel}) failed: {e}");
                continue;
            }
        };
        let Some((max_id, _)) = events.last() else {
            continue; // nothing new — leave the mark where it is
        };
        let max_id = max_id.clone();

        let worthy: Vec<&(peko_channel::port::TaskId, ChannelEvent)> = events
            .iter()
            .filter(|e| digest_worthy(&e.1, &own_path, principal_id, is_dm))
            .collect();

        // Advance BEFORE rendering: the mark is "observed up to", so
        // filtered events are consumed too (see module doc). Skipped
        // only if the advance itself fails.
        if let Err(e) = port
            .advance_read_mark(&channel, &session_key, max_id)
            .await
        {
            tracing::debug!("channel digest: advance_read_mark({channel}) failed: {e}");
        }

        if worthy.is_empty() {
            continue;
        }
        blocks.push(render_channel_block(channel.as_str(), &worthy));
    }

    if blocks.is_empty() {
        return None;
    }
    let body = format!(
        "channel activity since you last read:\n{}",
        blocks.join("\n")
    );
    Some(cap_digest(body))
}

/// Is this event worth surfacing in the digest?
///
/// Pure so the exclusion matrix is unit-testable. v1 exclusions:
/// - the session's own outputs (`via == own_path`, stamped in the
///   Stage A `via` attribution work);
/// - DM roots by anyone but the principal itself — user inbound and
///   other principals' roots are already delivered as ingress/wake;
/// - group roots authored by a `user:` (the `GroupWakeResponder`
///   already drove a turn for them);
/// - non-`Posted` events (member join/leave) — roster churn is not
///   chat activity (v1 exclusion; revisit if join/leave needs surfacing).
///
/// Threaded replies (`parent.is_some()`) always surface unless they
/// are the session's own output.
fn digest_worthy(ev: &ChannelEvent, own_path: &str, principal_id: &str, is_dm: bool) -> bool {
    let ChannelEvent::Posted {
        author,
        parent,
        via,
        ..
    } = ev
    else {
        return false;
    };
    if via.as_deref() == Some(own_path) {
        return false;
    }
    match parent {
        Some(_) => true, // threaded reply
        None if is_dm => author == principal_id,
        None => !author.starts_with("user:"),
    }
}

/// Is `id` strictly after the read `mark`?
///
/// Line-number ids are numeric strings on the store-backed path; a
/// non-numeric id (in-memory fakes) falls back to string inequality so
/// the mark's own event is still dropped exactly once.
fn strictly_after(id: &str, mark: &str) -> bool {
    match (id.parse::<u64>(), mark.parse::<u64>()) {
        (Ok(i), Ok(m)) => i > m,
        _ => id != mark,
    }
}

/// Render one channel's block: a header line, up to
/// [`DIGEST_MAX_PREVIEWS_PER_CHANNEL`] preview lines, and an overflow
/// line when more digest-worthy posts exist than previews.
fn render_channel_block(channel: &str, worthy: &[&(peko_channel::port::TaskId, ChannelEvent)]) -> String {
    let mut out = format!("- #{channel}: {} new message(s)\n", worthy.len());
    for (_, ev) in worthy.iter().take(DIGEST_MAX_PREVIEWS_PER_CHANNEL) {
        if let ChannelEvent::Posted {
            author, text, via, ..
        } = ev
        {
            // `[via <path>]` when the post carries an agent
            // attribution, else the raw author (`[user:bob]`).
            let prefix = match via.as_deref() {
                Some(path) => format!("via {path}"),
                None => author.clone(),
            };
            out.push_str(&format!("  - [{prefix}] \"{}\"\n", truncate_preview(text)));
        }
    }
    let hidden = worthy
        .len()
        .saturating_sub(DIGEST_MAX_PREVIEWS_PER_CHANNEL);
    if hidden > 0 {
        out.push_str(&format!("  - … +{hidden} more\n"));
    }
    out
}

/// Truncate a preview body to [`DIGEST_PREVIEW_MAX_CHARS`] characters
/// (char-safe — never slices a multi-byte boundary).
fn truncate_preview(text: &str) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(DIGEST_PREVIEW_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// Cap the whole digest at [`DIGEST_MAX_BYTES`], dropping whole lines
/// from the end and appending a pointer (mirrors the skills-catalog
/// cap at `extensions/skill/prompt.rs`).
fn cap_digest(body: String) -> String {
    if body.len() <= DIGEST_MAX_BYTES {
        return body;
    }
    let pointer = "… digest truncated — ChannelRead the channels above to catch up";
    let mut lines: Vec<&str> = body.lines().collect();
    while lines.len() > 1 && lines.join("\n").len() + pointer.len() + 1 > DIGEST_MAX_BYTES {
        lines.pop();
    }
    format!("{}\n{pointer}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_channel::{ChannelConfig, ChannelStore, CreateOpts, PostMsg};
    use peko_protocol::channel::ChannelId;
    use peko_session::metadata::SessionMetadata;
    use peko_subject::{PrincipalId, Subject};

    fn trunk_id() -> peko_session::id::SessionId {
        peko_session::id::SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
    }

    fn child_id() -> peko_session::id::SessionId {
        peko_session::id::SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap()
    }

    fn posted(author: &str, parent: Option<&str>, via: Option<&str>) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: ChannelId::generate(),
            author: author.to_string(),
            parent: parent.map(str::to_string),
            text: "body".to_string(),
            at: "2026-09-12T00:00:00Z".to_string(),
            via: via.map(str::to_string),
        }
    }

    #[test]
    fn filter_matrix() {
        let own = "/user-a";
        let me = "prin_self";
        // Own output never echoes back (both DM and group).
        assert!(!digest_worthy(&posted(me, None, Some(own)), own, me, true));
        assert!(!digest_worthy(&posted(me, None, Some(own)), own, me, false));
        // DM roots: inbound user + other principals excluded; own root included.
        assert!(!digest_worthy(&posted("user:alice", None, None), own, me, true));
        assert!(!digest_worthy(&posted("prin_other", None, None), own, me, true));
        assert!(digest_worthy(&posted(me, None, None), own, me, true));
        // DM same-principal post by ANOTHER agent (via is not our path) included.
        assert!(digest_worthy(
            &posted(me, None, Some("/user-b")),
            own,
            me,
            true
        ));
        // Group roots: user: excluded; other-principal root included.
        assert!(!digest_worthy(
            &posted("user:alice", None, None),
            own,
            me,
            false
        ));
        assert!(digest_worthy(
            &posted("prin_other", None, None),
            own,
            me,
            false
        ));
        // Threaded replies included regardless of author/kind...
        assert!(digest_worthy(
            &posted("user:alice", Some("3"), None),
            own,
            me,
            true
        ));
        assert!(digest_worthy(
            &posted("user:alice", Some("3"), None),
            own,
            me,
            false
        ));
        // ...unless they are our own output.
        assert!(!digest_worthy(
            &posted(me, Some("3"), Some(own)),
            own,
            me,
            false
        ));
        // Non-Posted events are excluded in v1.
        let joined = ChannelEvent::MemberJoined {
            channel: ChannelId::generate(),
            member: "prin_other".to_string(),
            at: "2026-09-12T00:00:00Z".to_string(),
        };
        assert!(!digest_worthy(&joined, own, me, false));
    }

    #[test]
    fn preview_truncates_char_safe() {
        let long = "é".repeat(200);
        let out = truncate_preview(&long);
        assert_eq!(out.chars().count(), DIGEST_PREVIEW_MAX_CHARS + 1); // + ellipsis
        assert!(out.ends_with('…'));
        assert_eq!(truncate_preview("short"), "short");
    }

    #[test]
    fn strictly_after_drops_mark_line() {
        assert!(strictly_after("3", "2"));
        assert!(!strictly_after("2", "2"));
        assert!(!strictly_after("1", "2"));
        // Numeric compare, not lexicographic: "10" > "9".
        assert!(strictly_after("10", "9"));
        // Non-numeric fallback: drop only the exact mark.
        assert!(!strictly_after("node_b", "node_b"));
        assert!(strictly_after("node_c", "node_b"));
    }

    /// Digest end-to-end against a real `ChannelStore`: first
    /// observation marks the tip and emits nothing; a later
    /// same-principal post (via another agent) renders and advances.
    #[tokio::test]
    async fn digest_first_observation_then_render_and_advance() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().join("runtime"),
            shared_dir: None,
        }));
        let port: Arc<dyn ChannelPort> = store.clone();
        let me = "prin_self";
        let me_subject = Subject::from(&PrincipalId(me.to_string()));

        // A DM channel bound to our session `/user-a`.
        let channel = port
            .create(
                &PrincipalId(me.to_string()),
                CreateOpts::runtime("dm-user-a").with_passive_binding("/user-a"),
            )
            .await
            .unwrap();
        // Pre-existing traffic (inbound peer + our own reply).
        port.post_attributed(&channel, &me_subject, "user:alice", PostMsg::root("hello there"))
            .await
            .unwrap();
        port.post_attributed(&channel, &me_subject, me, PostMsg::root("hi alice"))
            .await
            .unwrap();

        // Synthetic session tree: trunk + the bound child `/user-a`.
        let mut child = SessionMetadata::new(child_id(), "root", "child.jsonl");
        child.parent_session_id = Some(trunk_id());
        child.standing = true;
        child.slug = Some("user-a".to_string());
        child.peer_type = Some("user".to_string());
        child.peer_id = Some("alice".to_string());
        let trunk = SessionMetadata::new(trunk_id(), "root", "trunk.jsonl");
        let metas = vec![trunk, child];

        // First observation: nothing rendered, tip adopted.
        let first = render_channel_digest(&port, &metas, child_id(), me).await;
        assert!(first.is_none(), "first observation must not dump history");
        let mark = port.read_mark(&channel, &child_id().as_str()).await.unwrap();
        assert_eq!(mark.as_deref(), Some("2"), "mark adopted at the tip");

        // A new post by another agent of our principal (not our own via).
        port.post_attributed(
            &channel,
            &me_subject,
            me,
            PostMsg::root("status update from the other agent").with_via("/user-b"),
        )
        .await
        .unwrap();

        let second = render_channel_digest(&port, &metas, child_id(), me).await;
        let text = second.expect("digest must render the new post");
        assert!(
            text.starts_with("channel activity since you last read:"),
            "{text}"
        );
        assert!(text.contains("1 new message(s)"), "{text}");
        assert!(text.contains("[via /user-b]"), "{text}");
        assert!(text.contains("status update from the other agent"), "{text}");
        assert_eq!(
            port.read_mark(&channel, &child_id().as_str())
                .await
                .unwrap()
                .as_deref(),
            Some("3"),
            "mark advanced past the rendered post"
        );

        // Third run: nothing new ⇒ no render (change-detection stays quiet).
        assert!(
            render_channel_digest(&port, &metas, child_id(), me)
                .await
                .is_none()
        );
    }

    /// Group-bound discovery: a group channel has no `passive_binding`;
    /// the session's `peer_id == <channel wire id>` stamp is the only
    /// binding evidence. Root `user:` posts are wake-delivered and
    /// excluded; other principals' roots and threaded replies surface.
    #[tokio::test]
    async fn digest_group_bound_session_filters_user_roots() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().join("runtime"),
            shared_dir: None,
        }));
        let port: Arc<dyn ChannelPort> = store.clone();
        let me = "prin_self";
        let me_subject = Subject::from(&PrincipalId(me.to_string()));

        // A plain group channel: no passive binding.
        let group = port
            .create(&PrincipalId(me.to_string()), CreateOpts::runtime("team-room"))
            .await
            .unwrap();

        // Session bound to the group via the peer stamp.
        let mut child = SessionMetadata::new(child_id(), "root", "child.jsonl");
        child.parent_session_id = Some(trunk_id());
        child.standing = true;
        child.slug = Some("team-room".to_string());
        child.peer_type = Some("user".to_string());
        child.peer_id = Some(group.as_str().to_string());
        let trunk = SessionMetadata::new(trunk_id(), "root", "trunk.jsonl");
        let metas = vec![trunk, child];

        // First observation adopts the tip (2 events: user root + reply).
        port.post_attributed(&group, &me_subject, "user:bob", PostMsg::root("hi all"))
            .await
            .unwrap();
        port.post_attributed(
            &group,
            &me_subject,
            "user:bob",
            PostMsg::reply("1".to_string(), "anyone here?"),
        )
        .await
        .unwrap();
        assert!(
            render_channel_digest(&port, &metas, child_id(), me)
                .await
                .is_none(),
            "first observation must not dump history"
        );

        // A user root (wake-delivered) must NOT digest...
        port.post_attributed(&group, &me_subject, "user:bob", PostMsg::root("ping"))
            .await
            .unwrap();
        assert!(
            render_channel_digest(&port, &metas, child_id(), me)
                .await
                .is_none(),
            "group user roots are wake-delivered — no digest"
        );

        // ...but another principal's root does.
        port.post_attributed(&group, &me_subject, "prin_other", PostMsg::root("deploy done"))
            .await
            .unwrap();
        let text = render_channel_digest(&port, &metas, child_id(), me)
            .await
            .expect("other-principal group root must digest");
        assert!(text.contains("[prin_other]"), "{text}");
        assert!(text.contains("deploy done"), "{text}");
        assert!(!text.contains("ping"), "user root must be filtered: {text}");
    }
}
