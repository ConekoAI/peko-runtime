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
    BranchOutcome, CompactRequestOutcome, DeleteOutcome, HistoryMessage, SessionInfo,
    SessionRuntime, SessionSearchHit, SessionStatusResult, SharedSessionRuntime,
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
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));

        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let filtered: Vec<SessionInfo> = sessions
            .values()
            .filter(|s| {
                let archived_match = include_archived || !s.archived;
                let agent_match = agent_id.map_or(true, |a| s.agent_id.as_deref() == Some(a));
                let active_match = cutoff_ms.map_or(true, |_| {
                    chrono::DateTime::parse_from_rfc3339(&s.last_activity)
                        .map(|dt| dt.timestamp_millis() as u64 >= cutoff_ms.unwrap_or(0))
                        .unwrap_or(true)
                });
                archived_match
                    && Self::peer_matches(s, peer_filter.as_ref())
                    && agent_match
                    && active_match
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
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        let histories = self.histories.lock().expect("histories mutex poisoned");
        let history = histories
            .get(&session_key.to_string())
            .cloned()
            .unwrap_or_default();
        Ok(history.into_iter().take(limit).collect())
    }

    async fn get_status(&self, session_key: &str) -> anyhow::Result<SessionStatusResult> {
        self.statuses
            .lock()
            .expect("statuses mutex poisoned")
            .get(&session_key.to_string())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))
    }

    fn current_session_key(&self) -> String {
        self.current_session.clone()
    }

    async fn search_sessions(
        &self,
        query: &str,
        peer: Option<&peko_subject::Subject>,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionSearchHit>> {
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
        let needle = query.to_lowercase();

        let sessions = self.sessions.lock().expect("sessions mutex poisoned");
        let histories = self.histories.lock().expect("histories mutex poisoned");

        let mut hits = Vec::new();
        'outer: for (key, info) in sessions.iter() {
            if info.archived || !Self::peer_matches(info, peer_filter.as_ref()) {
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

    async fn copy_session(
        &self,
        session_key: &str,
        _target_parent: String,
        _target_slug: String,
        label: Option<String>,
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
        info.session_key = new_key.clone();
        info.session_id = new_key.clone();
        info.label = label.or(parent.label);
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
            new_session_id: new_key,
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
            info.label = Some(title.clone());
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
                status.label = Some(title);
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
        for id in &subtree {
            sessions.remove(id);
            histories.remove(id);
            statuses.remove(id);
        }

        Ok(DeleteOutcome { deleted: subtree })
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::session::UsageStats;

    fn info(key: &str) -> SessionInfo {
        SessionInfo {
            session_key: key.to_string(),
            session_id: key.to_string(),
            agent_id: Some("agent".to_string()),
            label: None,
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
            agent_name: "agent".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            last_activity: "2024-01-01T01:00:00Z".to_string(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: 1,
            usage: UsageStats {
                prompt_tokens: 0,
                completion_tokens: 0,
                last_total_tokens: 0,
                model_context_limit: None,
            },
            peer_type: None,
            peer_id: None,
            label: None,
            parent_session: parent.map(String::from),
        }
    }

    #[tokio::test]
    async fn branch_copies_session_and_records_parentage() {
        let cache = SessionCache::new("main");
        let mut parent = info("p1");
        parent.label = Some("parent".to_string());
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
            .list_sessions(None, None, 10, None, true)
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.session_key == outcome.new_session_id)
            .unwrap();
        // Label inherited when not supplied.
        assert_eq!(branch_info.label, Some("parent".to_string()));

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
            .list_sessions(None, None, 10, None, true)
            .await
            .unwrap()
            .is_empty());

        assert!(cache.delete_session("p", true).await.is_err());
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
        let hits = cache.search_sessions("NEEDLE", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert!(hits[0].snippet.contains("Needle"));
    }
}
