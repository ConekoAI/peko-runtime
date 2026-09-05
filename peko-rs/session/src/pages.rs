//! ADR-051: logical compaction pages — an addressable archive read model
//! over the session's existing append-only event log.
//!
//! A **page** is the segment of the stitched event stream between two
//! consecutive compaction boundary events (`SessionEvent::System` with
//! `event == "compaction"` — the same detection
//! [`crate::message_conversion::latest_compaction_boundary`] uses):
//!
//! - page 1: genesis … first boundary
//! - page k: boundary k−1 … boundary k
//! - page N+1 ("live page"): the newest boundary … now
//!
//! Pages are computed by a pure scan over `SessionStorage::load_events`
//! output (which already stitches rotated `<id>.N.jsonl` files
//! transparently). Nothing here touches storage layout — no new files,
//! no re-keying, no truncation (ADR-051 D1, non-goal "physical
//! archiving").
//!
//! Three pure read primitives plus thin [`crate::unified::Session`]
//! wrappers:
//!
//! - [`list_pages`] — the page catalog (number, terminating boundary id
//!   + compaction number, event span, token estimate, title excerpt).
//! - [`read_page`] — render one page's messages as transcript text with
//!   Read-style `offset`/`limit` line windowing and a hard per-call
//!   token cap ([`READ_PAGE_MAX_TOKENS`]) so reviewing history cannot
//!   silently re-inflate the context compaction just shed (D5).
//! - [`search_pages`] — case-insensitive substring search across all
//!   pages including the live one, returning capped, page-tagged
//!   snippets. Plain substring only: `regex` is not a dependency of
//!   this crate (ADR-051 D3, non-goal "semantic/vector search").

use crate::events::SessionEvent;
use crate::message::SessionMessage;
use peko_message::{ContentBlock, MessageRole};
use serde::Serialize;

/// Hard per-call token cap for [`read_page`] (ADR-051 D5). Pages are
/// potentially huge; retrieval is paginated and capped so a careless
/// review pass cannot re-inflate the context window. Estimated with
/// the same chars/4 heuristic the compactor uses.
pub const READ_PAGE_MAX_TOKENS: usize = 8_000;

/// Approximate characters per token — same heuristic as
/// `compaction::compaction_top`'s private `CHARS_PER_TOKEN` (and the
/// engine driver's copy). Kept local per the established
/// duplication-is-cheaper-than-a-shared-crate pattern.
const CHARS_PER_TOKEN: usize = 4;

/// One logical page of a session's event stream (ADR-051 D1/D2).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionPage {
    /// 1-based page number. Page 1 is genesis…first boundary; the live
    /// page (after the newest boundary) is last.
    pub page_number: usize,
    /// Id of the compaction boundary event that terminates this page
    /// (`compact_<uuid>`). `None` for the live page.
    pub boundary_event_id: Option<String>,
    /// The terminating boundary's per-session `compaction_number`.
    /// `None` for the live page and for legacy boundaries written
    /// before `compaction_number` existed.
    pub compaction_number: Option<usize>,
    /// Inclusive start index into the stitched event list.
    pub start_index: usize,
    /// Exclusive end index into the stitched event list (includes the
    /// terminating boundary event).
    pub end_index: usize,
    /// Chars/4 estimate of the page's message content (same estimator
    /// as `Compactor::estimate_tokens`).
    pub token_estimate: usize,
    /// First User-message text excerpt (~80 chars, whitespace
    /// collapsed). Empty when the page has no user message.
    pub title: String,
}

/// One match from [`search_pages`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchHit {
    /// Page containing the match (live page included).
    pub page_number: usize,
    /// Index of the matching event in the stitched event list.
    pub event_index: usize,
    /// ~200 chars of message text centered on the match, `…`-marked
    /// when truncated (char-boundary safe).
    pub snippet: String,
}

/// Whether `event` is a compaction boundary — the same predicate
/// [`crate::message_conversion::latest_compaction_boundary`] scans with.
#[must_use]
pub fn is_compaction_boundary(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::System(sys) if sys.event == "compaction")
}

/// Split the stitched event list into logical pages at compaction
/// boundaries (ADR-051 D1).
///
/// The live page (segment after the newest boundary) is included only
/// when it holds at least one event. An empty event list yields no
/// pages; a boundary-less non-empty list yields exactly one live page.
#[must_use]
pub fn list_pages(events: &[SessionEvent]) -> Vec<SessionPage> {
    let mut pages = Vec::new();
    let mut start = 0usize;
    for (idx, event) in events.iter().enumerate() {
        if !is_compaction_boundary(event) {
            continue;
        }
        let SessionEvent::System(sys) = event else {
            continue;
        };
        let compaction_number = sys
            .detail
            .get("compaction_number")
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize);
        pages.push(build_page(
            pages.len() + 1,
            start,
            idx + 1,
            events,
            Some((sys.envelope.id.clone(), compaction_number)),
        ));
        start = idx + 1;
    }
    // Live page — only when something follows the newest boundary (or
    // there is no boundary at all and the list is non-empty).
    if start < events.len() {
        pages.push(build_page(
            pages.len() + 1,
            start,
            events.len(),
            events,
            None,
        ));
    }
    pages
}

/// Render one page's `MessageV2` events as transcript text
/// (role-prefixed lines: `user: …` / `assistant: …` / `system: …` /
/// `tool: [<name> result: …]`, mirroring
/// [`crate::message_conversion::entries_to_context_text`]).
///
/// `offset`/`limit` are in rendered lines (Read-style; multi-line
/// messages are flattened so a window may cut mid-message). `limit ==
/// 0` means "no line limit" — the [`READ_PAGE_MAX_TOKENS`] cap still
/// applies. When the window ends before the page does (by limit or by
/// the token cap), a trailing marker names the `offset` to continue
/// from. A single line larger than the remaining budget is truncated
/// at the cap and the marker points past it.
#[must_use]
pub fn read_page(
    events: &[SessionEvent],
    page_number: usize,
    offset: usize,
    limit: usize,
) -> String {
    let pages = list_pages(events);
    let Some(page) = pages.iter().find(|p| p.page_number == page_number) else {
        return format!(
            "Page {page_number} does not exist — this session has {} page(s). \
             Call list_pages for the catalog.",
            pages.len()
        );
    };

    let rendered: Vec<String> = events[page.start_index..page.end_index]
        .iter()
        .filter_map(render_event)
        .collect();
    // Flatten multi-line messages so offset/limit are plain line windows.
    let lines: Vec<&str> = rendered.iter().flat_map(|block| block.lines()).collect();
    let total = lines.len();
    if offset >= total {
        return format!(
            "Offset {offset} is past the end of page {page_number} ({total} rendered line(s))."
        );
    }

    let line_limit = if limit == 0 { usize::MAX } else { limit };
    let budget_chars = READ_PAGE_MAX_TOKENS * CHARS_PER_TOKEN;
    let mut out = String::new();
    let mut next = offset;
    while next < total && next - offset < line_limit {
        let line = lines[next];
        let needed = line.len() + usize::from(!out.is_empty());
        if out.len() + needed > budget_chars {
            // The next line would breach the token cap. Truncate it to
            // the remaining budget (char-boundary safe) so the caller
            // sees the head, then stop; the marker points at the
            // FOLLOWING line (the cut tail of this one is only
            // recoverable from the raw JSONL — single lines larger
            // than the whole cap are pathological).
            let remaining = budget_chars - out.len() - usize::from(!out.is_empty());
            if remaining > 0 {
                if !out.is_empty() {
                    out.push('\n');
                }
                let mut end = remaining;
                while end > 0 && !line.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&line[..end]);
            }
            next += 1;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        next += 1;
    }
    if next < total {
        out.push_str(&format!(
            "\n[... {} more line(s) on page {}; continue with read_page offset={}]",
            total - next,
            page_number,
            next
        ));
    }
    out
}

/// Case-insensitive substring search over message text across all pages
/// of the session, including the live page (ADR-051 D3). One hit per
/// matching message, oldest first, capped at `max_results`.
#[must_use]
pub fn search_pages(events: &[SessionEvent], pattern: &str, max_results: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    if pattern.is_empty() || max_results == 0 {
        return hits;
    }
    let needle = pattern.to_lowercase();

    for page in list_pages(events) {
        for (i, event) in events[page.start_index..page.end_index].iter().enumerate() {
            let Some(msg) = event.as_message() else {
                continue;
            };
            let text = msg.text_content();
            // Byte offsets into the lowercased copy can drift from the
            // original for non-ASCII text; `snippet_around` snaps back
            // to char boundaries (same approach as
            // `jsonl::SessionStorage::search_transcripts`).
            let Some(start) = text.to_lowercase().find(&needle) else {
                continue;
            };
            hits.push(SearchHit {
                page_number: page.page_number,
                event_index: page.start_index + i,
                snippet: snippet_around(&text, start, needle.len()),
            });
            if hits.len() >= max_results {
                return hits;
            }
        }
    }
    hits
}

// --------------------------------------------------------------------
// Internal helpers
// --------------------------------------------------------------------

fn build_page(
    page_number: usize,
    start: usize,
    end: usize,
    events: &[SessionEvent],
    boundary: Option<(String, Option<usize>)>,
) -> SessionPage {
    let (boundary_event_id, compaction_number) = match boundary {
        Some((id, n)) => (Some(id), n),
        None => (None, None),
    };
    SessionPage {
        page_number,
        boundary_event_id,
        compaction_number,
        start_index: start,
        end_index: end,
        token_estimate: estimate_page_tokens(&events[start..end]),
        title: page_title(&events[start..end]),
    }
}

/// Chars/4 token estimate over the span's messages, reusing the
/// compactor's estimator (per-message overhead included).
fn estimate_page_tokens(events: &[SessionEvent]) -> usize {
    let messages: Vec<peko_message::LlmMessage> = events
        .iter()
        .filter_map(crate::message_conversion::event_to_llm_message)
        .collect();
    crate::compaction::Compactor::estimate_tokens(&messages)
}

/// First User-message text excerpt (~80 chars, whitespace collapsed).
fn page_title(events: &[SessionEvent]) -> String {
    const MAX_CHARS: usize = 80;
    for event in events {
        let Some(msg) = event.as_message() else {
            continue;
        };
        if msg.role() != MessageRole::User {
            continue;
        }
        let collapsed = msg
            .text_content()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if collapsed.is_empty() {
            continue;
        }
        if collapsed.chars().count() > MAX_CHARS {
            let truncated: String = collapsed.chars().take(MAX_CHARS).collect();
            return format!("{truncated}…");
        }
        return collapsed;
    }
    String::new()
}

/// Render one event as a role-prefixed transcript line. Non-message
/// events and empty messages are skipped (mirrors
/// `entries_to_context_text`).
fn render_event(event: &SessionEvent) -> Option<String> {
    let msg: SessionMessage = event.as_message()?;
    let text = msg.text_content();
    if text.is_empty() {
        return None;
    }
    let line = match msg.role() {
        MessageRole::User => format!("user: {text}"),
        MessageRole::Assistant => format!("assistant: {text}"),
        MessageRole::System => format!("system: {text}"),
        MessageRole::Tool => {
            let name = msg
                .message
                .content
                .iter()
                .find_map(|b| match b {
                    ContentBlock::ToolResult { name, .. } => Some(name.as_str()),
                    _ => None,
                })
                .unwrap_or("tool");
            format!("tool: [{name} result: {text}]")
        }
    };
    Some(line)
}

/// ~200-char snippet centered on the match at
/// `[match_start, match_start + match_len)` (byte offsets snapped to
/// char boundaries), marking truncated ends with `…`. Mirrors
/// `jsonl::SessionStorage::match_snippet` with a wider radius.
fn snippet_around(text: &str, match_start: usize, match_len: usize) -> String {
    const RADIUS: usize = 100;

    let floor_boundary = |mut i: usize| {
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let ceil_boundary = |mut i: usize| {
        while i < text.len() && !text.is_char_boundary(i) {
            i += 1;
        }
        i
    };

    let start = floor_boundary(match_start.saturating_sub(RADIUS));
    let end = ceil_boundary((match_start + match_len + RADIUS).min(text.len()));

    let mut snippet = String::new();
    if start > 0 {
        snippet.push('…');
    }
    snippet.push_str(&text[start..end]);
    if end < text.len() {
        snippet.push('…');
    }
    snippet
}

// ====================================================================================
// Tests
// ====================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventEnvelope, MessageSource, SystemEvent};
    use chrono::Utc;

    fn user(text: &str) -> SessionEvent {
        SessionEvent::MessageV2(SessionMessage::user(text, MessageSource::User))
    }

    fn assistant(text: &str) -> SessionEvent {
        SessionEvent::MessageV2(SessionMessage::assistant_text(text, "test", "test-model"))
    }

    fn tool(text: &str) -> SessionEvent {
        SessionEvent::MessageV2(SessionMessage::tool_result("tc1", "Read", text, false))
    }

    fn boundary(n: usize) -> SessionEvent {
        SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: format!("compact_{n}"),
                ts: Utc::now(),
            },
            event: "compaction".to_string(),
            detail: serde_json::json!({
                "summary": format!("summary {n}"),
                "messages_compacted": n,
                "compaction_number": n,
            }),
        })
    }

    // --------------------------------------------------------------
    // list_pages
    // --------------------------------------------------------------

    #[test]
    fn list_pages_empty_session_has_no_pages() {
        assert!(list_pages(&[]).is_empty());
    }

    #[test]
    fn list_pages_boundary_less_session_is_single_live_page() {
        let events = vec![user("hello"), assistant("hi")];
        let pages = list_pages(&events);
        assert_eq!(pages.len(), 1);
        let page = &pages[0];
        assert_eq!(page.page_number, 1);
        assert_eq!(page.boundary_event_id, None);
        assert_eq!(page.compaction_number, None);
        assert_eq!((page.start_index, page.end_index), (0, 2));
        assert!(page.token_estimate > 0);
        assert_eq!(page.title, "hello");
    }

    #[test]
    fn list_pages_multi_boundary_segmentation() {
        let events = vec![
            user("first ask"),         // 0
            assistant("first answer"), // 1
            boundary(1),               // 2 — terminates page 1
            user("second ask"),        // 3
            boundary(2),               // 4 — terminates page 2
            assistant("live answer"),  // 5
            user("live question"),     // 6
        ];
        let pages = list_pages(&events);
        assert_eq!(pages.len(), 3);

        assert_eq!(pages[0].page_number, 1);
        assert_eq!(pages[0].boundary_event_id.as_deref(), Some("compact_1"));
        assert_eq!(pages[0].compaction_number, Some(1));
        assert_eq!((pages[0].start_index, pages[0].end_index), (0, 3));
        assert_eq!(pages[0].title, "first ask");

        assert_eq!(pages[1].page_number, 2);
        assert_eq!(pages[1].boundary_event_id.as_deref(), Some("compact_2"));
        assert_eq!(pages[1].compaction_number, Some(2));
        assert_eq!((pages[1].start_index, pages[1].end_index), (3, 5));
        assert_eq!(pages[1].title, "second ask");

        // Live page: after the newest boundary, no terminating info.
        assert_eq!(pages[2].page_number, 3);
        assert_eq!(pages[2].boundary_event_id, None);
        assert_eq!(pages[2].compaction_number, None);
        assert_eq!((pages[2].start_index, pages[2].end_index), (5, 7));
        // First user message in the live segment.
        assert_eq!(pages[2].title, "live question");
    }

    #[test]
    fn list_pages_trailing_boundary_yields_no_live_page() {
        let events = vec![user("q"), boundary(1)];
        let pages = list_pages(&events);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].compaction_number, Some(1));
    }

    #[test]
    fn list_pages_legacy_boundary_without_number_still_terminates() {
        let legacy = SessionEvent::System(SystemEvent {
            envelope: EventEnvelope {
                id: "compact_legacy".to_string(),
                ts: Utc::now(),
            },
            event: "compaction".to_string(),
            detail: serde_json::json!({"summary": "old", "messages_compacted": 3}),
        });
        let events = vec![user("q"), legacy, user("after")];
        let pages = list_pages(&events);
        assert_eq!(pages.len(), 2);
        assert_eq!(
            pages[0].boundary_event_id.as_deref(),
            Some("compact_legacy")
        );
        assert_eq!(pages[0].compaction_number, None);
        assert_eq!(pages[1].compaction_number, None, "live page");
    }

    #[test]
    fn page_title_collapses_whitespace_and_truncates() {
        let long = format!("{}\n{}", "word ".repeat(30), "tail");
        let events = vec![user(&long)];
        let pages = list_pages(&events);
        let title = &pages[0].title;
        assert!(!title.contains('\n'));
        assert!(title.ends_with('…'));
        assert_eq!(title.chars().count(), 81);
    }

    // --------------------------------------------------------------
    // read_page
    // --------------------------------------------------------------

    #[test]
    fn read_page_renders_role_prefixed_lines() {
        let events = vec![
            user("hello"),
            assistant("hi there"),
            tool("file body"),
            boundary(1),
            user("after"),
        ];
        let text = read_page(&events, 1, 0, 0);
        assert_eq!(
            text,
            "user: hello\nassistant: hi there\ntool: [Read result: file body]"
        );
    }

    #[test]
    fn read_page_unknown_page_explains() {
        let events = vec![user("q")];
        let text = read_page(&events, 5, 0, 0);
        assert!(text.contains("Page 5 does not exist"), "{text}");
        assert!(text.contains("1 page(s)"), "{text}");
    }

    #[test]
    fn read_page_offset_limit_with_continuation_marker() {
        let events = vec![
            user("l0"),
            assistant("l1"),
            user("l2"),
            assistant("l3"),
            user("l4"),
        ];
        let text = read_page(&events, 1, 1, 2);
        assert!(text.starts_with("assistant: l1\nuser: l2"), "{text}");
        assert!(
            text.contains("2 more line(s) on page 1; continue with read_page offset=3"),
            "{text}"
        );

        let rest = read_page(&events, 1, 3, 2);
        assert_eq!(rest, "assistant: l3\nuser: l4", "no marker at the end");

        let past = read_page(&events, 1, 5, 2);
        assert!(past.contains("past the end"), "{past}");
    }

    #[test]
    fn read_page_enforces_token_cap_with_marker() {
        // 40KB of text ≈ 10k tokens > READ_PAGE_MAX_TOKENS (8k).
        let big = "x".repeat(READ_PAGE_MAX_TOKENS * CHARS_PER_TOKEN + 4000);
        let events = vec![user(&big), assistant("after")];
        let text = read_page(&events, 1, 0, 0);
        assert!(
            text.len() <= READ_PAGE_MAX_TOKENS * CHARS_PER_TOKEN + 200,
            "hard cap must hold (marker aside), got {} bytes",
            text.len()
        );
        assert!(text.contains("more line(s) on page 1"), "{text}");
        assert!(text.contains("continue with read_page offset=1"), "{text}");
        assert!(!text.contains("after"));
    }

    // --------------------------------------------------------------
    // search_pages
    // --------------------------------------------------------------

    #[test]
    fn search_pages_spans_boundaries_including_live_page() {
        let events = vec![
            user("deploy the frambulator"), // 0 — page 1
            boundary(1),                    // 1
            assistant("frambulator done"),  // 2 — page 2 (live)
        ];
        let hits = search_pages(&events, "FRAMBULATOR", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page_number, 1);
        assert_eq!(hits[0].event_index, 0);
        assert!(hits[0].snippet.contains("frambulator"));
        assert_eq!(hits[1].page_number, 2, "live page included");
        assert_eq!(hits[1].event_index, 2);
    }

    #[test]
    fn search_pages_hit_cap_and_empty_pattern() {
        let events = vec![user("needle"), assistant("needle"), user("needle")];
        let hits = search_pages(&events, "needle", 2);
        assert_eq!(hits.len(), 2);
        assert!(search_pages(&events, "", 10).is_empty());
        assert!(search_pages(&events, "needle", 0).is_empty());
        assert!(search_pages(&events, "absent", 10).is_empty());
    }

    #[test]
    fn search_pages_snippet_is_char_boundary_safe() {
        let padding = "é".repeat(150);
        let text = format!("{padding} needle at the end of a long line {padding}");
        let hits = search_pages(&[user(&text)], "needle", 5);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.starts_with('…'));
        assert!(hits[0].snippet.contains("needle"));
    }

    // --------------------------------------------------------------
    // Session wrappers
    // --------------------------------------------------------------

    #[tokio::test]
    async fn session_wrappers_delegate_to_stored_events() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let storage = crate::jsonl::SessionStorage::new(temp_dir.path().to_path_buf());
        let peer = peko_subject::Subject::User("default".to_string());
        let session_id = "test-pages-wrappers";

        storage.create_session(session_id, None).await.unwrap();
        let mut session = crate::unified::Session::open_by_id(
            "test-agent",
            session_id,
            temp_dir.path(),
            Some(&peer),
        )
        .await
        .unwrap();

        session.add_user("archived question").await.unwrap();
        session
            .add_assistant("archived answer", None, None)
            .await
            .unwrap();
        session
            .record_compaction("summary one", 2, 100, 10, 1, None)
            .await
            .unwrap();
        session.add_user("live question").await.unwrap();

        let pages = session.list_pages().await.unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].compaction_number, Some(1));
        assert_eq!(pages[0].title, "archived question");
        assert_eq!(pages[1].compaction_number, None);

        let text = session.read_page(1, 0, 0).await.unwrap();
        assert!(text.contains("user: archived question"), "{text}");
        assert!(!text.contains("live question"));

        let hits = session.search_pages("question", 10).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page_number, 1);
        assert_eq!(hits[1].page_number, 2);
    }
}
