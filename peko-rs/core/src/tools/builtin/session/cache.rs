//! In-memory `SessionRuntime` implementation for tests and placeholder
//! use (CLI/test harnesses that don't have a real `SessionManager`).
//!
//! Replaces the legacy `SessionCache` from root's
//! `src/tools/builtin/session.rs`. Mirrors the same shape: keyed by
//! session_key, returns pre-loaded `SessionInfo` / `HistoryMessage` /
//! `SessionStatusResult` records.
//!
//! The lifecycle actions (branch / rename / archive / delete / compact /
//! move) are modeled with plain in-memory semantics — no
//! ownership guards (those are a production-adapter concern).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::{
    BranchOutcome, DeleteOutcome, HistoryMessage, SessionInfo, SessionRuntime, SessionSearchHit,
    SessionStatusResult, SharedSessionRuntime,
};

/// In-memory session cache for testing and placeholder use.
///
/// Backed by three `HashMap<String, T>` slots. The current session is
/// held by-value (no clone-on-read for that hot path). The session_key
/// field on `SessionInfo`/`HistoryMessage`/`SessionStatusResult` is
/// always the lookup key.
#[derive(Debug)]
pub struct SessionCache {
    current_session: String,
    sessions: Mutex<HashMap<String, SessionInfo>>,
    histories: Mutex<HashMap<String, Vec<HistoryMessage>>>,
    statuses: Mutex<HashMap<String, SessionStatusResult>>,
    /// Raw stored events, seeded per session by `add_events` for the
    /// ADR-051 page primitives (which are pure scans over the event
    /// list). Sessions without seeded events read as empty.
    events: Mutex<HashMap<String, Vec<peko_session::SessionEvent>>>,
    /// Monotonic counter for deterministic branch ids in tests.
    branch_counter: Mutex<usize>,
}

impl SessionCache {
    /// Create a new in-memory session cache.
    #[must_use]
    pub fn new(current_session: impl Into<String>) -> Self {
        Self {
            current_session: current_session.into(),
            sessions: Mutex::new(HashMap::new()),
            histories: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
            events: Mutex::new(HashMap::new()),
            branch_counter: Mutex::new(0),
        }
    }

    /// Add a session with its history and status.
    pub fn add_session(
        &self,
        key: String,
        info: SessionInfo,
        history: Vec<HistoryMessage>,
        status: SessionStatusResult,
    ) {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(key.clone(), info);
        self.histories
            .lock()
            .expect("histories mutex poisoned")
            .insert(key.clone(), history);
        self.statuses
            .lock()
            .expect("statuses mutex poisoned")
            .insert(key, status);
    }

    /// Seed the raw stored events for a session (ADR-051 page tests).
    pub fn add_events(&self, key: &str, events: Vec<peko_session::SessionEvent>) {
        self.events
            .lock()
            .expect("events mutex poisoned")
            .insert(key.to_string(), events);
    }

    /// Wrap into a `SharedSessionRuntime` for tool construction.
    #[must_use]
    pub fn as_shared(self: Arc<Self>) -> SharedSessionRuntime {
        self as Arc<dyn SessionRuntime>
    }

    /// Peer filter helper shared by `list_sessions` / `search_sessions`.
    fn peer_matches(info: &SessionInfo, peer_filter: Option<&(String, String)>) -> bool {
        peer_filter.map_or(true, |(want_kind, want_id)| {
            let (have_kind, have_id) = match (info.peer_type.as_deref(), info.peer_id.as_deref()) {
                (Some(k), Some(i)) => (k, i),
                _ => return false,
            };
            have_kind == want_kind.as_str() && have_id == want_id.as_str()
        })
    }

    /// Normalize a session path for subtree matching: strip the `sess:`
    /// scheme prefix (the cache stores bare or prefixed paths depending
    /// on the seeder) and guarantee a leading `/`.
    fn normalize_path(path: &str) -> String {
        let stripped = peko_session::path::strip_scheme_prefix(path);
        if stripped.starts_with('/') {
            stripped.to_string()
        } else {
            format!("/{stripped}")
        }
    }

    /// Resolve a subtree scope reference (`sess:/a/b`) against the
    /// cached `SessionInfo`s.
    ///
    /// The cache has no real tree — sessions carry only their computed
    /// `path` — so membership is a path-prefix test on the normalized
    /// paths (the production adapter resolves the slug walk over real
    /// metadata and tests ancestor chains by id instead). Fails closed
    /// when no cached session's path matches the reference.
    fn resolve_subtree(&self, subtree: &str) -> anyhow::Result<String> {
        let want = Self::normalize_path(subtree);
        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        sessions
            .values()
            .map(|s| Self::normalize_path(&s.path))
            .find(|p| *p == want)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Session not found: {subtree} (no cached session has this path)"
                )
            })
    }

    /// Subtree membership test matching [`Self::resolve_subtree`] —
    /// the session's normalized path equals the root's or sits under it.
    fn in_subtree(info: &SessionInfo, root_path: &str) -> bool {
        let path = Self::normalize_path(&info.path);
        path == root_path || path.starts_with(&format!("{root_path}/"))
    }

    /// Seeded events for a session (empty when none were seeded —
    /// mirrors `get_history`'s empty-for-missing behavior).
    fn events_for(&self, session_key: &str) -> Vec<peko_session::SessionEvent> {
        self.events
            .lock()
            .expect("events mutex poisoned")
            .get(session_key)
            .cloned()
            .unwrap_or_default()
    }

    /// ~160-char snippet centered on the match, `…`-marked when
    /// truncated (simplified mirror of the storage-side helper).
    fn snippet_around(text: &str, match_start: usize, match_len: usize) -> String {
        const RADIUS: usize = 80;
        let floor = |mut i: usize| {
            while i > 0 && !text.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        let ceil = |mut i: usize| {
            while i < text.len() && !text.is_char_boundary(i) {
                i += 1;
            }
            i
        };
        let start = floor(match_start.saturating_sub(RADIUS));
        let end = ceil((match_start + match_len + RADIUS).min(text.len()));
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
}

#[async_trait]
impl SessionRuntime for SessionCache {
    async fn list_sessions(
        &self,
        peer: Option<&peko_subject::Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
        include_archived: bool,
        subtree: Option<&str>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));
        let root_path = match subtree {
            Some(reference) => Some(self.resolve_subtree(reference)?),
            None => None,
        };

        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let filtered: Vec<SessionInfo> = sessions
            .values()
            .filter(|s| {
                let archived_match = include_archived || !s.archived;
                let agent_match = agent_id.map_or(true, |a| s.agent_name.as_deref() == Some(a));
                let active_match = cutoff_ms.map_or(true, |_| {
                    chrono::DateTime::parse_from_rfc3339(&s.last_activity)
                        .map(|dt| dt.timestamp_millis() as u64 >= cutoff_ms.unwrap_or(0))
                        .unwrap_or(true)
                });
                let subtree_match = root_path
                    .as_deref()
                    .map_or(true, |root| Self::in_subtree(s, root));
                archived_match
                    && Self::peer_matches(s, peer_filter.as_ref())
                    && agent_match
                    && active_match
                    && subtree_match
            })
            .take(limit)
            .cloned()
            .collect();
        Ok(filtered)
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        _include_tools: bool,
    ) -> anyhow::Result<(Vec<HistoryMessage>, String)> {
        let histories = self.histories.lock().expect("histories mutex poisoned");
        let history = histories
            .get(&session_key.to_string())
            .cloned()
            .unwrap_or_default();
        // Addressable path echo (test-double approximation of the
        // production adapter's resolved slug path).
        let path = {
            let sessions = self.sessions.lock().expect("sessions mutex poisoned");
            sessions.get(session_key).map_or_else(
                || format!("sess:/{}", Self::normalize_path(session_key)),
                |info| format!("sess:{}", Self::normalize_path(&info.path)),
            )
        };
        Ok((history.into_iter().take(limit).collect(), path))
    }

    async fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult> {
        let path = {
            let sessions = self.sessions.lock().expect("sessions mutex poisoned");
            sessions
                .get(session_key)
                .map(|info| format!("sess:{}", Self::normalize_path(&info.path)))
        };
        let mut status = self
            .statuses
            .lock()
            .expect("statuses mutex poisoned")
            .get(&session_key.to_string())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
        status.path = path.unwrap_or_default();
        Ok(status)
    }

    fn current_session_key(&self) -> String {
        self.current_session.clone()
    }

    async fn search_sessions(
        &self,
        query: &str,
        peer: Option<&peko_subject::Subject>,
        limit: usize,
        subtree: Option<&str>,
    ) -> anyhow::Result<Vec<SessionSearchHit>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let needle = query.to_lowercase();
        let root_path = match subtree {
            Some(reference) => Some(self.resolve_subtree(reference)?),
            None => None,
        };

        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let histories = self.histories.lock().expect("histories mutex poisoned");

        let mut hits = Vec::new();
        'outer: for (key, info) in sessions.iter() {
            if info.archived || !Self::peer_matches(info, peer_filter.as_ref()) {
                continue;
            }
            if !root_path
                .as_deref()
                .map_or(true, |root| Self::in_subtree(info, root))
            {
                continue;
            }
            let Some(history) = histories.get(key) else {
                continue;
            };
            for msg in history {
                let Some(start) = msg.content.to_lowercase().find(&needle) else {
                    continue;
                };
                hits.push(SessionSearchHit {
                    session_id: info.session_id.clone(),
                    path: format!("sess:{}", Self::normalize_path(&info.path)),
                    role: msg.role.clone(),
                    timestamp: msg.timestamp.clone(),
                    snippet: Self::snippet_around(&msg.content, start, needle.len()),
                });
                if hits.len() >= limit {
                    break 'outer;
                }
            }
        }
        Ok(hits)
    }

    async fn list_pages(
        &self,
        session_key: &str,
    ) -> anyhow::Result<Vec<peko_session::pages::SessionPage>> {
        Ok(peko_session::pages::list_pages(
            &self.events_for(session_key),
        ))
    }

    async fn read_page(
        &self,
        session_key: &str,
        page: usize,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<String> {
        Ok(peko_session::pages::read_page(
            &self.events_for(session_key),
            page,
            offset,
            limit,
        ))
    }

    async fn search_pages(
        &self,
        session_key: &str,
        query: &str,
        max_results: usize,
    ) -> anyhow::Result<Vec<peko_session::pages::SearchHit>> {
        Ok(peko_session::pages::search_pages(
            &self.events_for(session_key),
            query,
            max_results,
        ))
    }

    async fn copy_session(
        &self,
        session_key: &str,
        _target_parent: String,
        _target_slug: String,
        title: Option<String>,
    ) -> anyhow::Result<BranchOutcome> {
        // In-memory test impl: ignore target_parent/target_slug (the
        // production adapter wires those into branch-then-reparent).
        // The new key is still sourced from the source session so the
        // existing tests can keep asserting on it. The cache layer
        // does not enforce slug uniqueness; the production adapter
        // does.
        let mut sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let parent = sessions
            .get(session_key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;

        let n = {
            let mut counter = self.branch_counter.lock().expect("counter mutex poisoned");
            *counter += 1;
            *counter
        };
        let new_key = format!("{session_key}-branch-{n}");

        let mut info = parent.clone();
        info.session_id = new_key.clone();
        info.title = title.or(parent.title);
        sessions.insert(new_key.clone(), info);

        let history = self
            .histories
            .lock()
            .expect("histories mutex poisoned")
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        self.histories
            .lock()
            .expect("histories mutex poisoned")
            .insert(new_key.clone(), history);

        let status = self
            .statuses
            .lock()
            .expect("statuses mutex poisoned")
            .get(session_key)
            .cloned();
        if let Some(mut status) = status {
            status.session_id = new_key.clone();
            status.parent_session = Some(session_key.to_string());
            self.statuses
                .lock()
                .expect("statuses mutex poisoned")
                .insert(new_key.clone(), status);
        }

        Ok(BranchOutcome {
            new_session_id: new_key.clone(),
            new_path: format!("sess:/{}", new_key),
            parent_session_id: session_key.to_string(),
        })
    }

    async fn rename_session(
        &self,
        session_key: &str,
        title: Option<String>,
        slug: Option<String>,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let info = sessions
            .get_mut(session_key)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
        if let Some(ref title) = title {
            info.title = Some(title.clone());
        }
        if let Some(ref slug) = slug {
            info.slug = Some(slug.clone());
        }
        drop(sessions);

        if let Some(status) = self
            .statuses
            .lock()
            .expect("statuses mutex poisoned")
            .get_mut(session_key)
        {
            if let Some(title) = title {
                status.title = Some(title);
            }
        }
        Ok(())
    }

    async fn move_session(
        &self,
        session_key: &str,
        new_parent: String,
        new_slug: Option<String>,
    ) -> anyhow::Result<()> {
        // Plain in-memory reparent + slug application — no
        // ownership/cycle guards (those are a production-adapter
        // concern). The cache accepts any new_parent string (including
        // "/" for caller-root or an arbitrary slug for a notional
        // parent), letting tests exercise "rename in place" via
        // `target = "/<new_slug>"` without having to seed every parent
        // into the cache. The production adapter
        // (`SessionManagerRuntime`) does the real existence + subtree
        // + slug-uniqueness checks.
        {
            let mut statuses = self.statuses.lock().expect("statuses mutex poisoned");
            let status = statuses
                .get_mut(session_key)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
            status.parent_session = Some(new_parent);
        }
        if let Some(slug) = new_slug {
            let mut sessions = self.sessions.lock().expect("sessions mutex poisoned");
            let info = sessions
                .get_mut(session_key)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
            info.slug = Some(slug);
        }
        Ok(())
    }

    async fn delete_session(
        &self,
        session_key: &str,
        recursive: bool,
    ) -> anyhow::Result<DeleteOutcome> {
        // Collect the descendant subtree via `parent_session` chains on
        // the stored statuses, children first (post-order).
        let subtree: Vec<String> = {
            let statuses = self.statuses.lock().expect("statuses mutex poisoned");
            if !statuses.contains_key(session_key)
                && !self
                    .sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .contains_key(session_key)
            {
                return Err(anyhow::anyhow!("Session not found: {session_key}"));
            }

            let mut ordered = Vec::new();
            let mut stack = vec![session_key.to_string()];
            let mut post_order = Vec::new();
            while let Some(id) = stack.pop() {
                post_order.push(id.clone());
                for (key, status) in statuses.iter() {
                    if status.parent_session.as_deref() == Some(id.as_str()) {
                        stack.push(key.clone());
                    }
                }
            }
            // post_order is parents-first; reverse for children-first.
            ordered.append(&mut post_order);
            ordered.reverse();
            ordered
        };

        let descendants: Vec<String> = subtree
            .iter()
            .filter(|id| id.as_str() != session_key)
            .cloned()
            .collect();
        if !descendants.is_empty() && !recursive {
            return Err(anyhow::anyhow!(
                "Session {session_key} has descendants {}; pass recursive:true to delete the whole subtree",
                descendants.join(", ")
            ));
        }

        let mut sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let mut histories = self.histories.lock().expect("histories mutex poisoned");
        let mut statuses = self.statuses.lock().expect("statuses mutex poisoned");
        // Slug paths at deletion time (post-order; the cache's path is
        // the raw seeded one — normalize + prefix for the sess: form).
        let deleted_paths = subtree
            .iter()
            .map(|id| {
                sessions
                    .get(id)
                    .map(|info| format!("sess:{}", Self::normalize_path(&info.path)))
                    .unwrap_or_else(|| format!("sess:/{}", id))
            })
            .collect();
        for id in &subtree {
            sessions.remove(id);
            histories.remove(id);
            statuses.remove(id);
        }

        Ok(DeleteOutcome {
            deleted: subtree,
            deleted_paths,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::session::UsageStats;

    fn info(key: &str) -> SessionInfo {
        SessionInfo {
            session_id: key.to_string(),
            agent_name: Some("agent".to_string()),
            title: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            message_count: 1,
            peer_type: None,
            peer_id: None,
            archived: false,
            run_active: false,
            slug: None,
            path: format!("/{key}"),
        }
    }

    fn status(key: &str, parent: Option<&str>) -> SessionStatusResult {
        SessionStatusResult {
            session_id: key.to_string(),
            path: String::new(),
            agent_name: "agent".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: 1,
            usage: UsageStats {
                cumulative_input_tokens: 0,
                cumulative_output_tokens: 0,
                last_total_tokens: 0,
                current_prompt_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                reasoning_tokens: None,
                model_context_limit: None,
                cache_read_total: 0,
                cache_creation_total: 0,
                cache_hit_rate: None,
            },
            quota: None,
            current_run_iterations: 0,
            peer_type: None,
            peer_id: None,
            title: None,
            parent_session: parent.map(String::from),
        }
    }

    #[tokio::test]
    async fn branch_copies_session_and_records_parentage() {
        let cache = SessionCache::new("main");
        let mut parent = info("p1");
        parent.title = Some("parent".to_string());
        cache.add_session("p1".to_string(), parent, vec![], status("p1", None));

        let outcome = cache
            .copy_session("p1", "/".into(), "p1-copy".into(), None)
            .await
            .unwrap();
        assert_eq!(outcome.parent_session_id, "p1");

        let branch = cache.get_status(&outcome.new_session_id).await.unwrap();
        assert_eq!(branch.parent_session, Some("p1".to_string()));
        // After the kind-filter removal, the model derives
        // "branchedness" from `parent_session` (a sibling field on
        // SessionStatusResult) rather than from a `kind` enum. The
        // SessionInfo itself carries the inherited label.
        let branch_info = cache
            .list_sessions(None, None, 10, None, true, None)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.session_id == outcome.new_session_id)
            .unwrap();
        // Title inherited when not supplied.
        assert_eq!(branch_info.title, Some("parent".to_string()));

        assert!(cache
            .copy_session("missing", "/".into(), "x".into(), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn delete_refuses_descendants_unless_recursive() {
        let cache = SessionCache::new("main");
        cache.add_session("p".to_string(), info("p"), vec![], status("p", None));
        cache.add_session(
            "c1".to_string(),
            info("c1"),
            vec![],
            status("c1", Some("p")),
        );
        cache.add_session(
            "g1".to_string(),
            info("g1"),
            vec![],
            status("g1", Some("c1")),
        );

        let err = cache.delete_session("p", false).await.unwrap_err();
        assert!(err.to_string().contains("recursive:true"), "{err}");

        let outcome = cache.delete_session("p", true).await.unwrap();
        // Children first, target last.
        assert_eq!(
            outcome.deleted,
            vec!["g1".to_string(), "c1".to_string(), "p".to_string()]
        );
        assert!(cache
            .list_sessions(None, None, 10, None, true, None)
            .await
            .unwrap()
            .is_empty());

        assert!(cache.delete_session("p", true).await.is_err());
    }

    #[tokio::test]
    async fn list_and_find_scope_to_subtree_path() {
        let cache = SessionCache::new("main");
        // Tree: p ── c1 ── g1, plus unrelated o1. The cache matches
        // subtree scopes by computed-path prefix (see resolve_subtree).
        let mut p = info("p");
        p.path = "sess:/p".to_string();
        let mut c1 = info("c1");
        c1.path = "sess:/p/c1".to_string();
        let mut g1 = info("g1");
        g1.path = "sess:/p/c1/g1".to_string();
        let mut o1 = info("o1");
        o1.path = "sess:/o1".to_string();
        let needle_msg = |text: &str| {
            vec![HistoryMessage {
                role: "user".to_string(),
                content: text.to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:00Z".to_string(),
            }]
        };
        cache.add_session("p".to_string(), p, needle_msg("needle in p"), status("p", None));
        cache.add_session("c1".to_string(), c1, vec![], status("c1", Some("p")));
        cache.add_session(
            "g1".to_string(),
            g1,
            needle_msg("needle in g1"),
            status("g1", Some("c1")),
        );
        cache.add_session(
            "o1".to_string(),
            o1,
            needle_msg("needle in o1"),
            status("o1", None),
        );

        // Scoped list: the subtree root itself + descendants, never o1.
        let scoped = cache
            .list_sessions(None, None, 10, None, true, Some("sess:/p"))
            .await
            .unwrap();
        let mut keys: Vec<&str> = scoped.iter().map(|s| s.session_id.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["c1", "g1", "p"]);

        // Legacy bare form is the same address.
        let bare = cache
            .list_sessions(None, None, 10, None, true, Some("/p"))
            .await
            .unwrap();
        assert_eq!(bare.len(), 3);

        // Unknown subtree path fails closed.
        let err = cache
            .list_sessions(None, None, 10, None, true, Some("sess:/missing"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no cached session"), "{err}");

        // Scoped search: p's and g1's needles surface, never o1's.
        let hits = cache
            .search_sessions("needle", None, 10, Some("sess:/p"))
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.session_id != "o1"));
    }

    #[tokio::test]
    async fn search_skips_archived_and_matches_case_insensitively() {
        let cache = SessionCache::new("main");
        let history = vec![HistoryMessage {
            role: "user".to_string(),
            content: "the Needle is here".to_string(),
            tool_calls: None,
            tool_results: None,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        }];
        cache.add_session("s1".to_string(), info("s1"), history, status("s1", None));
        let mut s2_info = info("s2");
        s2_info.archived = true;
        cache.add_session(
            "s2".to_string(),
            s2_info,
            vec![HistoryMessage {
                role: "assistant".to_string(),
                content: "another needle here".to_string(),
                tool_calls: None,
                tool_results: None,
                timestamp: "2024-01-01T00:00:01Z".to_string(),
            }],
            status("s2", None),
        );
        // s2 is seeded archived, so it must drop out of search results.
        let hits = cache.search_sessions("NEEDLE", None, 10, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert!(hits[0].snippet.contains("Needle"));
    }
}
