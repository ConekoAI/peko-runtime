//! Unread channel summary for the `{{session_context}}` tail section.
//!
//! Only humans and cron start runs (ADR-072): a post from another
//! principal never wakes this one, and a group post wakes it only when a
//! user root post mentions `@<name>`. Everything else waits on its
//! channel until an agent reads it. [`ChannelDigestSessionContextHandler`]
//! tells the running session what is waiting: per channel, "N new
//! messages" with previews, tracking per-(session, channel) read
//! positions in the channel's `read_marks.json` (see
//! [`peko_channel::read_marks`]). `ChannelRead` advances the same marks.
//!
//! Two scopes:
//! - **bound** — the session's own conversation channel (its DM, or its
//!   group). Everything except the posts that started its turns.
//! - **elsewhere** — every other channel of the principal, for sessions
//!   that humans or cron drive (the trunk and peer standing children;
//!   spawned subagents see only their bound channels). Posts from other
//!   principals, and group posts no other session was woken for.
//!
//! It rides the existing ADR-052 machinery: the `session_context`
//! slot's change-detection (`RuntimeContextState::take_changed`)
//! re-injects only when the rendered text changes, so each batch of
//! posts is shown once and unchanged iterations pay nothing.
//!
//! ## Seen is not handled: the waiting list
//!
//! Another principal's post wakes no one, so the digest is the only
//! place a session learns of it — and a post the model glanced past on
//! one keepalive turn must not be gone for good. Such posts go to the
//! session's waiting list ([`peko_channel::attention`]) instead of the
//! one-shot preview and head the digest ("waiting on you") on every run
//! until the session reads them with `ChannelRead` or the principal
//! posts on the channel after them. Users' posts stay one-shot: the
//! ones that want a reply start turns themselves.
//!
//! ## Deliberate render-time side effect
//!
//! The handler advances the read mark as it renders (to the max line
//! id it observed, *even when every event was filtered*) and updates
//! the waiting list. The mark means "observed up to", not "shown to the
//! model" — a filtered event must not resurface on the next iteration
//! just because the digest chose not to print it. This is the one place
//! the prompt path writes to disk, which is why it is called out here
//! and at the call site.

use std::path::Path;
use std::sync::Arc;

use peko_channel::{AttentionItem, ChannelPort, Checkpoint};
use peko_protocol::channel::ChannelEvent;

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

/// Render the principal's unread channel activity into the
/// `{{session_context}}` section.
///
/// Registered once on the daemon-global core; per-principal state is
/// resolved per invoke from the hook context's workspace + principal
/// id, like the sibling `SessionContextBuild` handlers in
/// `principal::child_turns`. Renders nothing when nothing is unread or
/// the workspace doesn't resolve.
#[derive(Debug)]
pub(crate) struct ChannelDigestSessionContextHandler;

#[async_trait::async_trait]
impl crate::tools::prompt_sections::PromptSectionProvider for ChannelDigestSessionContextHandler {
    fn section(&self) -> &'static str {
        "session_context"
    }

    fn priority(&self) -> i32 {
        CHANNEL_DIGEST_HOOK_PRIORITY
    }

    async fn render(
        &self,
        input: &crate::tools::prompt_sections::PromptSectionInput,
    ) -> Option<String> {
        let session_id = peko_session::id::SessionId::parse(&input.session_id)?;
        let workspace = input.workspace.to_string_lossy().to_string();
        let principal_id = input.principal_id.clone();
        if workspace.is_empty() || principal_id.is_empty() {
            return None;
        }
        let sessions_dir = crate::principal::child_turns::sessions_dir_for_workspace(&workspace)?;
        let metas = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir)
            .list_all_sessions(false)
            .await
            .unwrap_or_default();
        let port = input.channel_port.clone()?;
        let actor = port
            .actor_info(&peko_subject::Subject::from(&peko_subject::PrincipalId(
                principal_id.clone(),
            )))
            .await;
        let hub_address = actor.hub_address();
        let me = Me {
            hub: hub_address.as_deref(),
            address: &actor.address,
            id: &principal_id,
            name: &principal_name(&input.workspace),
        };

        // ADR-078 D4: only an owner run sees the principal's other
        // conversations; any other run sees its own channel.
        let owner = input
            .acting_for
            .as_ref()
            .is_some_and(peko_tools_core::ActingFor::is_owner);
        render_channel_digest(&port, &metas, session_id, &me, owner).await
    }
}

/// The rendering principal: its id (post authorship) and its name (group
/// `@<name>` mentions).
struct Me<'a> {
    address: &'a str,
    hub: Option<&'a str>,
    id: &'a str,
    name: &'a str,
}

/// The principal's `name` from `<workspace>/principal.toml` (empty when
/// unreadable — nothing then counts as a mention).
fn principal_name(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("principal.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| t.get("name")?.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// How the rendering session relates to a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The session's own conversation channel.
    Bound,
    /// Another channel of the principal.
    Elsewhere,
}

/// What kind of channel it is: a DM (carries a `passive_binding`) or a
/// group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dm,
    Group,
}

/// Core digest logic, split out from [`ChannelDigestSessionContextHandler::render`]
/// so it is testable without the workspace/env-dependent session-dir
/// resolution: classifies the principal's channels, applies
/// first-observation marking, filters to digest-worthy posts, advances
/// the read marks, and renders.
async fn render_channel_digest(
    port: &Arc<dyn ChannelPort>,
    metas: &[peko_session::metadata::SessionMetadata],
    session_id: peko_session::id::SessionId,
    me: &Me<'_>,
    owner: bool,
) -> Option<String> {
    let own_meta = metas.iter().find(|m| m.session_id == session_id)?;
    let own_path = peko_session::path::compute_path(metas, session_id);
    let session_key = session_id.as_str();
    // Humans and cron drive the trunk and the peer standing children;
    // a spawned subagent works a narrow task and sees only its bound
    // channels.
    let conversation = crate::principal::conversation::binding(own_meta);
    let sees_elsewhere = owner && (own_meta.parent_session_id.is_none() || conversation.is_some());

    // Group sessions bind directly to a channel; the shared classifier also
    // recognizes legacy group stamps and excludes internal placeholder peers.
    let group_bound_channel = match &conversation {
        Some(crate::principal::conversation::Conversation::Channel(id)) => Some(id.as_str()),
        _ => None,
    };

    let principal = peko_subject::PrincipalId(me.id.to_string());
    let channels = match port.list_for_principal(&principal).await {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("channel digest: list_for_principal failed: {e}");
            return None;
        }
    };

    let mut waiting: Vec<String> = Vec::new();
    let mut bound: Vec<String> = Vec::new();
    let mut elsewhere: Vec<String> = Vec::new();
    for channel in channels {
        // DM channels carry a `passive_binding` equal to the bound
        // session's address (the legacy bare `/slug` path or raw id);
        // group channels bind via the session's peer stamp instead.
        // `own_path` carries the display-side `sess:` prefix, so both
        // sides are normalized through `strip_scheme_prefix` before
        // comparing — stored bindings never carry the prefix.
        let (kind, scope) = match port.binding_for(&channel, &principal).await.unwrap_or(None) {
            Some(b) => {
                let b_path = peko_session::path::strip_scheme_prefix(&b);
                let own = peko_session::path::strip_scheme_prefix(&own_path);
                let mine = b_path == own || b == session_key;
                (Kind::Dm, if mine { Scope::Bound } else { Scope::Elsewhere })
            }
            None if group_bound_channel == Some(channel.as_str()) => (Kind::Group, Scope::Bound),
            None => (Kind::Group, Scope::Elsewhere),
        };
        if scope == Scope::Elsewhere && !sees_elsewhere {
            continue;
        }

        let events = unread_events(port, &channel, &session_key, own_meta.created_at)
            .await
            .unwrap_or_default();
        let stored = port
            .attention(&channel, &session_key)
            .await
            .unwrap_or_default();
        if events.is_empty() && stored.is_empty() {
            continue;
        }

        let roster = port.participants(&channel).await.unwrap_or_default();
        let short_name = if peko_channel::short_name_unambiguous(&roster, me.name, me.address) {
            me.name
        } else {
            ""
        };
        let channel_me = Me {
            hub: me.hub,
            id: me.id,
            address: me.address,
            name: short_name,
        };
        let (fresh, waiting_now) =
            sort_new_posts(&events, stored.clone(), &own_path, &channel_me, scope, kind);

        // Advance BEFORE rendering: the mark is "observed up to", so
        // filtered events are consumed too (see module doc). Posts that
        // need an answer survive in the waiting list, not the mark.
        if let Some((max_id, _)) = events.last() {
            if let Err(e) = port
                .advance_read_mark(&channel, &session_key, max_id.clone())
                .await
            {
                tracing::debug!("channel digest: advance_read_mark({channel}) failed: {e}");
            }
        }
        if waiting_now != stored {
            if let Err(e) = port
                .set_attention(&channel, &session_key, waiting_now.clone())
                .await
            {
                tracing::debug!("channel digest: set_attention({channel}) failed: {e}");
            }
        }

        if !waiting_now.is_empty() {
            waiting.push(render_waiting_block(channel.as_str(), &waiting_now));
        }
        if fresh.is_empty() {
            continue;
        }
        let block = render_channel_block(channel.as_str(), &fresh);
        match scope {
            Scope::Bound => bound.push(block),
            Scope::Elsewhere => elsewhere.push(block),
        }
    }

    let mut sections = Vec::new();
    if !waiting.is_empty() {
        sections.push(format!(
            "waiting on you — other principals' posts you have not answered. They stay \
             listed until you ChannelRead the channel or post there:\n{}",
            waiting.join("\n")
        ));
    }
    if !bound.is_empty() {
        sections.push(format!(
            "channel activity since you last read:\n{}",
            bound.join("\n")
        ));
    }
    if !elsewhere.is_empty() {
        sections.push(format!(
            "unread on your other channels (posting there does not wake anyone; \
             ChannelRead for the thread, ChannelSend to answer):\n{}",
            elsewhere.join("\n")
        ));
    }
    if sections.is_empty() {
        return None;
    }
    Some(cap_digest(sections.join("\n")))
}

/// The events on `channel` this session has not observed yet,
/// oldest→newest, or `None` when the channel can't be read or this is
/// the session's first look at a channel that predates it.
///
/// First observation adopts the current tip as the mark and reports
/// nothing — never dump history into the prompt — unless the channel is
/// newer than the session (`session_created_ms`): then everything on it
/// is unread, so the first message from a new peer is not swallowed.
async fn unread_events(
    port: &Arc<dyn ChannelPort>,
    channel: &peko_channel::ChannelId,
    session_key: &str,
    session_created_ms: u64,
) -> Option<Vec<(peko_channel::port::TaskId, ChannelEvent)>> {
    let mark = match port.read_mark(channel, session_key).await {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("channel digest: read_mark({channel}) failed: {e}");
            return None;
        }
    };
    let Some(mark) = mark else {
        let all = match port.peek_with_ids(channel, &Checkpoint::default()).await {
            Ok(all) => all,
            Err(e) => {
                tracing::debug!("channel digest: peek_with_ids({channel}) failed: {e}");
                return None;
            }
        };
        let opened_ms = all
            .first()
            .and_then(|(_, ev)| chrono::DateTime::parse_from_rfc3339(ev.at()).ok())
            .map(|t| t.timestamp_millis());
        let newer = opened_ms.is_some_and(|ms| ms > session_created_ms as i64);
        if newer {
            return Some(all);
        }
        if let Some((id, _)) = all.last() {
            let _ = port
                .advance_read_mark(channel, session_key, id.clone())
                .await;
        }
        return None;
    };

    // Everything at-or-after the mark, oldest→newest. The store's
    // checkpoint is an *inclusive* count-offset ("drop the first N
    // events", see `ChannelStore::read_events`), while the mark is
    // the last line id the session observed — so the mark's own
    // line is filtered out to get "strictly after".
    match port.peek_with_ids(channel, &Checkpoint(mark.clone())).await {
        Ok(evs) => Some(
            evs.into_iter()
                .filter(|(id, _)| strictly_after(id, &mark))
                .collect(),
        ),
        Err(e) => {
            tracing::debug!("channel digest: peek_with_ids({channel}) failed: {e}");
            None
        }
    }
}

/// Is this event worth surfacing in the digest?
///
/// Pure so the exclusion matrix is unit-testable. Never: non-`Posted`
/// events (roster churn is not chat activity) and the session's own
/// outputs (`via == own_path`). Otherwise, by scope and kind:
/// - **bound DM** — all but user root posts (those start this
///   session's turns);
/// - **group** — all but user root posts mentioning `@<name>` (those
///   woke the group's session) and, elsewhere, the principal's own
///   posts;
/// - **elsewhere DM** — other principals' posts only (the users there
///   talk to the session bound to that DM).
fn digest_worthy(ev: &ChannelEvent, own_path: &str, me: &Me<'_>, scope: Scope, kind: Kind) -> bool {
    let ChannelEvent::Posted {
        author,
        parent,
        via,
        text,
        ..
    } = ev
    else {
        return false;
    };
    if via.as_deref() == Some(own_path) {
        return false;
    }
    let by_user = author.starts_with("user:") || author.starts_with("visitor:");
    let by_me = author == me.id || author == me.address;
    match (scope, kind) {
        (Scope::Bound, Kind::Dm) => parent.is_some() || !by_user,
        (_, Kind::Group) => {
            let woke = by_user
                && parent.is_none()
                && (peko_channel::mentions(text, me.name)
                    || peko_channel::mentions(text, me.address)
                    || me.hub.is_some_and(|h| peko_channel::mentions(text, h)));
            !woke && !(scope == Scope::Elsewhere && by_me)
        }
        (Scope::Elsewhere, Kind::Dm) => !by_user && !by_me,
    }
}

/// Sort a channel's new events (oldest→newest) against the session's
/// stored waiting list. Returns the digest-worthy posts to show once
/// and the updated waiting list.
///
/// A worthy post by another principal joins the waiting list instead of
/// the one-shot preview: it never woke anyone (ADR-072 D1), so it must
/// not vanish once seen. A post by this principal answers everything
/// waiting before it.
fn sort_new_posts<'a>(
    events: &'a [(peko_channel::port::TaskId, ChannelEvent)],
    mut waiting: Vec<AttentionItem>,
    own_path: &str,
    me: &Me<'_>,
    scope: Scope,
    kind: Kind,
) -> (
    Vec<&'a (peko_channel::port::TaskId, ChannelEvent)>,
    Vec<AttentionItem>,
) {
    let mut fresh = Vec::new();
    for event in events {
        let (id, ev) = event;
        let ChannelEvent::Posted {
            author, text, at, ..
        } = ev
        else {
            continue;
        };
        if author == me.id || author == me.address {
            waiting.retain(|w| !strictly_after(id, &w.id));
        }
        if !digest_worthy(ev, own_path, me, scope, kind) {
            continue;
        }
        if needs_answer(author, me) {
            if !waiting.iter().any(|w| w.id == *id) {
                waiting.push(AttentionItem {
                    id: id.clone(),
                    author: author.clone(),
                    at: at.clone(),
                    preview: truncate_preview(text),
                });
            }
        } else {
            fresh.push(event);
        }
    }
    let excess = waiting
        .len()
        .saturating_sub(peko_channel::attention::MAX_WAITING_PER_CHANNEL);
    waiting.drain(..excess);
    (fresh, waiting)
}

/// Does a worthy post by `author` wait for an answer? Only other
/// principals' posts do: users' posts that want a reply start turns
/// themselves (a DM root post, a group `@<name>` mention).
fn needs_answer(author: &str, me: &Me<'_>) -> bool {
    let by_user = author.starts_with("user:") || author.starts_with("visitor:");
    !by_user && author != me.id && author != me.address
}

/// Render one channel's waiting list: a header line and up to
/// [`DIGEST_MAX_PREVIEWS_PER_CHANNEL`] previews, newest last.
fn render_waiting_block(channel: &str, waiting: &[AttentionItem]) -> String {
    let mut out = format!("- #{channel}: {} waiting\n", waiting.len());
    let hidden = waiting
        .len()
        .saturating_sub(DIGEST_MAX_PREVIEWS_PER_CHANNEL);
    if hidden > 0 {
        out.push_str(&format!("  - … {hidden} older\n"));
    }
    for item in &waiting[hidden..] {
        out.push_str(&format!(
            "  - [{}] \"{}\" ({})\n",
            item.author,
            peko_tools_core::neutralize_runtime_tags(&item.preview),
            item.at
        ));
    }
    out
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
fn render_channel_block(
    channel: &str,
    worthy: &[&(peko_channel::port::TaskId, ChannelEvent)],
) -> String {
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
    let hidden = worthy.len().saturating_sub(DIGEST_MAX_PREVIEWS_PER_CHANNEL);
    if hidden > 0 {
        out.push_str(&format!("  - … +{hidden} more\n"));
    }
    out
}

/// Truncate a preview body to [`DIGEST_PREVIEW_MAX_CHARS`] characters
/// (char-safe — never slices a multi-byte boundary). Other parties'
/// posts may not impersonate runtime blocks (ADR-078).
fn truncate_preview(text: &str) -> String {
    let text = peko_tools_core::neutralize_runtime_tags(text);
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

    const ME: Me<'static> = Me {
        hub: None,
        address: "principal:local:prin_self",
        id: "prin_self",
        name: "self",
    };

    fn trunk_id() -> peko_session::id::SessionId {
        peko_session::id::SessionId::parse("11111111-1111-4111-8111-111111111111").unwrap()
    }

    fn child_id() -> peko_session::id::SessionId {
        peko_session::id::SessionId::parse("22222222-2222-4222-8222-222222222222").unwrap()
    }

    fn spawned_id() -> peko_session::id::SessionId {
        peko_session::id::SessionId::parse("33333333-3333-4333-8333-333333333333").unwrap()
    }

    fn posted(author: &str, parent: Option<&str>, via: Option<&str>, text: &str) -> ChannelEvent {
        ChannelEvent::Posted {
            channel: ChannelId::generate(),
            author: author.to_string(),
            parent: parent.map(str::to_string),
            text: text.to_string(),
            at: "2026-09-12T00:00:00Z".to_string(),
            via: via.map(str::to_string),
            msg_id: None,
            reply_to: None,
        }
    }

    fn store() -> (tempfile::TempDir, Arc<dyn ChannelPort>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ChannelStore::new(ChannelConfig {
            runtime_dir: dir.path().join("runtime"),
            shared_dir: None,
        }));
        (dir, store)
    }

    fn me_subject() -> Subject {
        Subject::from(&PrincipalId(ME.id.to_string()))
    }

    /// A session tree: the trunk, a user standing child `/user-a`, and a
    /// spawned subagent under the trunk. Every session is stamped as
    /// created `created_ms` (an hour from now by default, so channels
    /// made in the test predate it and first observations adopt the tip).
    fn tree(created_ms: Option<u64>) -> Vec<SessionMetadata> {
        let mut trunk = SessionMetadata::new(trunk_id(), "root", "trunk.jsonl");
        let mut child = SessionMetadata::new(child_id(), "root", "child.jsonl");
        child.parent_session_id = Some(trunk_id());
        child.slug = Some("user-a".to_string());
        child.channel_id = Some("chan_child".into());

        child.trigger = "spawn".into();
        let mut spawned = SessionMetadata::new(spawned_id(), "root", "spawned.jsonl");
        spawned.parent_session_id = Some(trunk_id());
        spawned.slug = Some("helper".to_string());
        let created = created_ms.unwrap_or(trunk.created_at + 3_600_000);
        for m in [&mut trunk, &mut child, &mut spawned] {
            m.created_at = created;
        }
        vec![trunk, child, spawned]
    }

    #[test]
    fn filter_matrix() {
        use Kind::{Dm, Group};
        use Scope::{Bound, Elsewhere};
        let own = "/user-a";
        let me = ME.id;
        let worthy = |ev: &ChannelEvent, scope, kind| digest_worthy(ev, own, &ME, scope, kind);

        // Own output never echoes back.
        for (scope, kind) in [
            (Bound, Dm),
            (Bound, Group),
            (Elsewhere, Dm),
            (Elsewhere, Group),
        ] {
            assert!(!worthy(&posted(me, None, Some(own), "x"), scope, kind));
        }

        // Bound DM: user roots started the turn; everything else shows,
        // including other principals' posts (they never wake us).
        assert!(!worthy(&posted("user:alice", None, None, "x"), Bound, Dm));
        assert!(worthy(
            &posted("user:alice", Some("3"), None, "x"),
            Bound,
            Dm
        ));
        assert!(worthy(&posted("prin_other", None, None, "x"), Bound, Dm));
        assert!(worthy(&posted(me, None, Some("/user-b"), "x"), Bound, Dm));

        // Groups: a user root mentioning us woke the group session; an
        // unmentioned one did not, so it shows.
        assert!(!worthy(
            &posted("user:bob", None, None, "hey @self"),
            Bound,
            Group
        ));
        assert!(worthy(
            &posted("user:bob", None, None, "hey all"),
            Bound,
            Group
        ));
        assert!(worthy(
            &posted("user:bob", Some("3"), None, "@self"),
            Bound,
            Group
        ));
        assert!(worthy(&posted("prin_other", None, None, "x"), Bound, Group));
        assert!(worthy(
            &posted(me, None, Some("/group-x"), "x"),
            Bound,
            Group
        ));
        assert!(!worthy(
            &posted("user:bob", None, None, "@self"),
            Elsewhere,
            Group
        ));
        assert!(worthy(
            &posted("prin_other", None, None, "x"),
            Elsewhere,
            Group
        ));
        assert!(!worthy(
            &posted(me, None, Some("/group-x"), "x"),
            Elsewhere,
            Group
        ));

        // Other DMs: only other principals.
        assert!(worthy(
            &posted("prin_other", None, None, "x"),
            Elsewhere,
            Dm
        ));
        assert!(worthy(
            &posted("prin_other", Some("2"), None, "x"),
            Elsewhere,
            Dm
        ));
        assert!(!worthy(
            &posted("user:carol", None, None, "x"),
            Elsewhere,
            Dm
        ));
        assert!(!worthy(
            &posted(me, None, Some("/user-c"), "x"),
            Elsewhere,
            Dm
        ));

        // Non-Posted events never show.
        let joined = ChannelEvent::MemberJoined {
            channel: ChannelId::generate(),
            member: "prin_other".to_string(),
            at: "2026-09-12T00:00:00Z".to_string(),
        };
        assert!(!worthy(&joined, Bound, Group));
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

    #[test]
    fn principal_name_reads_principal_toml() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(principal_name(dir.path()), "");
        std::fs::write(dir.path().join("principal.toml"), "name = \"ada\"\n").unwrap();
        assert_eq!(principal_name(dir.path()), "ada");
    }

    /// Digest end-to-end against a real `ChannelStore`: first
    /// observation marks the tip and emits nothing; a later
    /// same-principal post (via another agent) renders and advances.
    #[tokio::test]
    async fn digest_first_observation_then_render_and_advance() {
        let (_dir, port) = store();
        // A DM channel bound to our session `/user-a`.
        let channel = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("dm-user-a").with_passive_binding("/user-a"),
            )
            .await
            .unwrap();
        // Pre-existing traffic (inbound peer + our own reply).
        port.post_attributed(
            &channel,
            &me_subject(),
            "user:alice",
            PostMsg::root("hello there"),
        )
        .await
        .unwrap();
        port.post_attributed(&channel, &me_subject(), ME.id, PostMsg::root("hi alice"))
            .await
            .unwrap();
        let metas = tree(None);

        // First observation: nothing rendered, tip adopted.
        let first = render_channel_digest(&port, &metas, child_id(), &ME, true).await;
        assert!(first.is_none(), "first observation must not dump history");
        let mark = port
            .read_mark(&channel, &child_id().as_str())
            .await
            .unwrap();
        assert_eq!(mark.as_deref(), Some("2"), "mark adopted at the tip");

        // A new post by another agent of our principal (not our own via).
        port.post_attributed(
            &channel,
            &me_subject(),
            ME.id,
            PostMsg::root("status update from the other agent").with_via("/user-b"),
        )
        .await
        .unwrap();

        let text = render_channel_digest(&port, &metas, child_id(), &ME, true)
            .await
            .expect("digest must render the new post");
        assert!(
            text.starts_with("channel activity since you last read:"),
            "{text}"
        );
        assert!(text.contains("1 new message(s)"), "{text}");
        assert!(text.contains("[via /user-b]"), "{text}");
        assert!(
            text.contains("status update from the other agent"),
            "{text}"
        );
        assert_eq!(
            port.read_mark(&channel, &child_id().as_str())
                .await
                .unwrap()
                .as_deref(),
            Some("3"),
            "mark advanced past the rendered post"
        );

        // Third run: nothing new ⇒ no render (change-detection stays quiet).
        assert!(render_channel_digest(&port, &metas, child_id(), &ME, true)
            .await
            .is_none());
    }

    /// Group-bound discovery: a group channel has no `passive_binding`;
    /// the session's `peer_id == <channel wire id>` stamp is the only
    /// binding evidence. A user root mentioning us woke the session and
    /// is skipped; an unmentioned one and other principals' posts show.
    #[tokio::test]
    async fn digest_group_bound_session_skips_only_the_waking_posts() {
        let (_dir, port) = store();
        let group = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("team-room"),
            )
            .await
            .unwrap();
        let mut metas = tree(None);
        metas[1].slug = Some("team-room".to_string());

        metas[1].channel_id = Some(group.as_str().to_string());

        port.post_attributed(&group, &me_subject(), "user:bob", PostMsg::root("hi all"))
            .await
            .unwrap();
        assert!(
            render_channel_digest(&port, &metas, child_id(), &ME, true)
                .await
                .is_none(),
            "first observation must not dump history"
        );

        port.post_attributed(
            &group,
            &me_subject(),
            "user:bob",
            PostMsg::root("ping @self"),
        )
        .await
        .unwrap();
        assert!(
            render_channel_digest(&port, &metas, child_id(), &ME, true)
                .await
                .is_none(),
            "a mention woke the session — no digest"
        );

        port.post_attributed(&group, &me_subject(), "user:bob", PostMsg::root("lunch?"))
            .await
            .unwrap();
        port.post_attributed(
            &group,
            &me_subject(),
            "prin_other",
            PostMsg::root("deploy done"),
        )
        .await
        .unwrap();
        let text = render_channel_digest(&port, &metas, child_id(), &ME, true)
            .await
            .expect("unmentioned and other-principal posts must digest");
        let (waiting, fresh) = text
            .split_once("channel activity since you last read:")
            .unwrap();
        assert!(
            waiting.contains("1 waiting") && waiting.contains("deploy done"),
            "{text}"
        );
        assert!(
            fresh.contains("1 new message(s)") && fresh.contains("lunch?"),
            "{text}"
        );
        assert!(!text.contains("ping"), "{text}");
    }

    /// A reply from another principal lands on the DM bound to the peer's
    /// standing child — not the session that asked. The trunk and the
    /// user's session learn about it; a spawned subagent does not.
    #[tokio::test]
    async fn other_principals_posts_show_on_every_conversation_session() {
        let (_dir, port) = store();
        let dm = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("dm-other").with_passive_binding("/principal-other"),
            )
            .await
            .unwrap();
        port.post_attributed(
            &dm,
            &me_subject(),
            ME.id,
            PostMsg::root("can you review?").with_via("/user-a"),
        )
        .await
        .unwrap();
        let mut metas = tree(None);
        metas[2].peer_type = Some("principal".into());
        metas[2].peer_id = Some("spawn_placeholder".into());
        for session in [trunk_id(), child_id(), spawned_id()] {
            assert!(render_channel_digest(&port, &metas, session, &ME, true)
                .await
                .is_none());
        }

        port.post_attributed(
            &dm,
            &me_subject(),
            "prin_other",
            PostMsg::reply("1".to_string(), "looks good"),
        )
        .await
        .unwrap();
        // ADR-078 D4: a run acting for anyone but the owner never sees
        // the principal's other conversations.
        assert!(
            render_channel_digest(&port, &metas, child_id(), &ME, false)
                .await
                .is_none_or(|text| !text.contains("looks good")),
            "a non-owner run must not see other channels"
        );
        for session in [trunk_id(), child_id()] {
            let text = render_channel_digest(&port, &metas, session, &ME, true)
                .await
                .expect("the reply must show");
            assert!(text.starts_with("waiting on you"), "{text}");
            assert!(text.contains("[prin_other] \"looks good\""), "{text}");
        }
        assert!(
            render_channel_digest(&port, &metas, spawned_id(), &ME, true)
                .await
                .is_none(),
            "spawned subagents see only their bound channels"
        );
    }

    /// A channel opened after the session existed is unread from its
    /// first post: a new peer's opening message is not swallowed by the
    /// first-observation tip adoption.
    #[tokio::test]
    async fn a_channel_newer_than_the_session_is_unread_from_the_start() {
        let (_dir, port) = store();
        let metas = tree(Some(0));
        let dm = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("dm-new").with_passive_binding("/principal-new"),
            )
            .await
            .unwrap();
        port.post_attributed(
            &dm,
            &me_subject(),
            "prin_new",
            PostMsg::root("hello, I'm new"),
        )
        .await
        .unwrap();
        let text = render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .expect("the opening message must show");
        assert!(text.contains("hello, I'm new"), "{text}");
    }

    /// The trunk glances past another principal's post on one keepalive
    /// turn: the post must head the digest again on the next run, and
    /// leaves only once the principal answers on that channel.
    #[tokio::test]
    async fn another_principals_post_waits_until_the_principal_posts_there() {
        let (_dir, port) = store();
        let dm = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("dm-other").with_passive_binding("/principal-other"),
            )
            .await
            .unwrap();
        let metas = tree(None);
        assert!(render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .is_none());

        port.post_attributed(&dm, &me_subject(), "prin_other", PostMsg::root("ship it?"))
            .await
            .unwrap();
        let first = render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .expect("the post must show");
        assert!(first.contains("ship it?"), "{first}");

        // Next run: nothing new arrived, but the post is still waiting.
        let again = render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .expect("an unanswered post must keep showing");
        assert_eq!(again, first);

        // A user's post on the same DM does not answer it.
        port.post_attributed(&dm, &me_subject(), "user:carol", PostMsg::root("hm"))
            .await
            .unwrap();
        assert!(render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .unwrap()
            .contains("ship it?"));

        // The principal answers (from any of its sessions): cleared.
        port.post_attributed(
            &dm,
            &me_subject(),
            ME.address,
            PostMsg::root("yes").with_via("/principal-other"),
        )
        .await
        .unwrap();
        assert!(render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .is_none());
        assert!(port
            .attention(&dm, &trunk_id().as_str())
            .await
            .unwrap()
            .is_empty());
    }

    /// Each session keeps its own waiting list on a shared channel.
    #[tokio::test]
    async fn waiting_lists_are_per_session() {
        let (_dir, port) = store();
        let dm = port
            .create(
                &PrincipalId(ME.id.to_string()),
                CreateOpts::runtime("dm-other").with_passive_binding("/principal-other"),
            )
            .await
            .unwrap();
        let metas = tree(None);
        for session in [trunk_id(), child_id()] {
            assert!(render_channel_digest(&port, &metas, session, &ME, true)
                .await
                .is_none());
        }
        port.post_attributed(&dm, &me_subject(), "prin_other", PostMsg::root("ping"))
            .await
            .unwrap();
        for session in [trunk_id(), child_id()] {
            assert!(render_channel_digest(&port, &metas, session, &ME, true)
                .await
                .is_some());
        }
        port.set_attention(&dm, &trunk_id().as_str(), Vec::new())
            .await
            .unwrap();
        assert!(render_channel_digest(&port, &metas, trunk_id(), &ME, true)
            .await
            .is_none());
        assert!(render_channel_digest(&port, &metas, child_id(), &ME, true)
            .await
            .unwrap()
            .contains("ping"));
    }

    #[test]
    fn waiting_block_shows_the_newest_previews() {
        let items: Vec<AttentionItem> = (0..7)
            .map(|i| AttentionItem {
                id: i.to_string(),
                author: "prin_other".into(),
                at: "t".into(),
                preview: format!("post {i}"),
            })
            .collect();
        let block = render_waiting_block("chan_x", &items);
        assert!(
            block.starts_with("- #chan_x: 7 waiting\n  - … 2 older\n"),
            "{block}"
        );
        assert!(!block.contains("post 1\""), "{block}");
        assert!(block.contains("post 6"), "{block}");
    }
}
