//! `SessionManagerRuntime` — root-side adapter for the `SessionRuntime` port.
//!
//! Phase 10d lifts the unified `SessionTool` into
//! `peko_tools_builtin::session`. The tool surface there speaks to a
//! [`crate::tools::builtin::session::SessionRuntime`] port trait so the
//! built-in crate can stay free of root-only deps
//! (`crate::SessionManager`, `crate::jsonl::*`, the
//! LlmMessage event-conversion helpers, etc.). This file is the
//! production adapter: it routes through the peko_tools_builtin port
//! trait so the tool side has no `crate::*` dependency.
//!
//! Phase 4 adds the guard layer (plan D3/D4/D5):
//! - **Ownership** ([`crate::session::ownership`]): a caller in a base
//!   session (the principal's root agent) manages the whole store; a
//!   caller in a spawned session manages only its own subtree — reads
//!   (`list`/`search` filtered, `history`/`status` refused) and
//!   mutations alike.
//! - **Self guards**: the session the caller is running in (and its
//!   ancestors) refuse structural mutation.
//! - **Run permits** (D3): `delete` and `archive` acquire the
//!   `InboxRegistry` run permit for the target (and, for delete, every
//!   descendant) and hold it across the operation, so no run can be
//!   in flight — or start — mid-operation. When no registry is bound
//!   (stateless CLI), these guards degrade to metadata-only with a
//!   debug note.
//! - **Live slots**: the principal's trunk session (`root:self`) is
//!   continuous; it refuses delete/archive/move — the engine manages
//!   its lifecycle (paging + compaction). Phase 7 narrowed the guard
//!   from the whole `root:*` family to the trunk alone: the per-peer
//!   root sessions (`root:{peer}`, `root:cron:{peer}`) are retired.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::session::ownership::{
    caller_context, descendants_of, err_compact_archived, err_dangling, err_delete_ancestor,
    err_descendants_exist, err_live_base_managed, err_move_ancestor, err_move_cycle,
    err_out_of_tree, err_run_active, err_self_mutation, in_subtree, CallerContext,
};
use crate::tools::builtin::session::{
    BranchOutcome, CompactRequestOutcome, DeleteOutcome, HistoryMessage, SessionInfo,
    SessionRuntime, SessionSearchHit, SessionStatusResult, ToolCallInfo, ToolResultInfo,
    UsageStats,
};
use peko_message::LlmMessage;
use peko_subject::Subject;

use peko_message::ContentBlock;
use peko_session::jsonl::SessionStorage;
use peko_session::message_conversion::event_to_llm_message;
use peko_session::{InboxRegistry, SessionManager, SessionMetadata};

/// Adapter that exposes the real `SessionManager` through the
/// `SessionRuntime` port trait, enforcing the D3/D4/D5 ownership,
/// self, and run-permit guards for agent-owned session management.
pub struct SessionManagerRuntime {
    session_manager: Arc<tokio::sync::RwLock<SessionManager>>,
    current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
    /// Name of the agent this runtime serves (diagnostics only; the
    /// ownership classification itself derives from the caller's
    /// current session).
    caller_agent_name: String,
    /// Run-permit source for D3. `None` (stateless CLI / tests without
    /// a daemon) degrades delete/archive/run-active guards to
    /// metadata-only with a debug note.
    inbox_registry: Option<Arc<InboxRegistry>>,
}

impl SessionManagerRuntime {
    /// Build a new runtime wrapping the supplied manager.
    #[must_use]
    pub fn new(
        session_manager: Arc<tokio::sync::RwLock<SessionManager>>,
        current_session_id: Arc<tokio::sync::RwLock<Option<String>>>,
        caller_agent_name: String,
        inbox_registry: Option<Arc<InboxRegistry>>,
    ) -> Self {
        Self {
            session_manager,
            current_session_id,
            caller_agent_name,
            inbox_registry,
        }
    }

    /// Load the caller classification + the full metadata snapshot the
    /// guards walk over.
    async fn caller_and_metas(
        &self,
        manager: &mut SessionManager,
    ) -> anyhow::Result<(CallerContext, Vec<SessionMetadata>)> {
        let metas = manager.list_all_sessions(false).await?;
        let caller = caller_context(&self.current_session_key(), &metas);
        Ok((caller, metas))
    }

    /// Log-then-return a guard refusal (the agent name makes refusals
    /// attributable in debug logs).
    fn refuse(&self, err: anyhow::Error) -> anyhow::Error {
        tracing::debug!(agent = %self.caller_agent_name, "session guard refusal: {err:#}");
        err
    }

    /// Refuse structural mutation of the session the caller is
    /// currently running in.
    fn guard_not_self(&self, caller: &CallerContext, target: &str) -> anyhow::Result<()> {
        if target == caller.current_session_id {
            return Err(self.refuse(err_self_mutation(target)));
        }
        Ok(())
    }

    /// Refuse a subtree (spawned) caller acting outside its subtree.
    /// Dangling callers (own metadata missing) are refused at this
    /// site — a caller whose subtree cannot be verified gets no
    /// privilege, even though `is_base` is also false for them.
    fn guard_tree(
        &self,
        caller: &CallerContext,
        target: &str,
        metas: &[SessionMetadata],
    ) -> anyhow::Result<()> {
        if caller.dangling {
            return Err(self.refuse(err_dangling(&caller.current_session_id)));
        }
        if !caller.is_base && !caller.privileged && !in_subtree(caller, target, metas) {
            return Err(self.refuse(err_out_of_tree(target, &caller.current_session_id)));
        }
        Ok(())
    }

    /// Resolve a tool-supplied session reference to a session id.
    ///
    /// Two accepted forms (see [`peko_session::path::resolve_reference`]):
    ///
    /// - `/a/b/c` — absolute slug path (anchored at the caller's
    ///   tree root, never a global root).
    /// - The caller's own session id — engine-internal self-reference
    ///   shape; the engine passes its own `current_session_id`
    ///   verbatim (UUIDs in production) so callers don't have to
    ///   resolve a slug path before invoking the runtime.
    ///
    /// Raw session ids, caller-relative slugs, and the legacy
    /// `root:<dim>:<name>` shapes are REFUSED with a structured
    /// message so the model learns to use the `path` field from
    /// `session list` instead.
    ///
    /// The ownership/scoping guards run AFTER this resolution, on
    /// the resolved id.
    fn resolve_ref(
        &self,
        caller: &CallerContext,
        metas: &[SessionMetadata],
        reference: &str,
    ) -> anyhow::Result<String> {
        let caller_id = peko_session::SessionId::from(caller.current_session_id.as_str());
        peko_session::path::resolve_reference(metas, caller_id, reference)
            .map(|id| id.to_string())
            .map_err(|e| self.refuse(e))
    }
}

#[async_trait]
impl SessionRuntime for SessionManagerRuntime {
    async fn list_sessions(
        &self,
        peer: Option<&Subject>,
        agent_id: Option<&str>,
        limit: usize,
        active_minutes: Option<i64>,
        include_archived: bool,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let mut manager = self.session_manager.write().await;
        let metadatas = manager.list_all_sessions(false).await?;

        let now = chrono::Utc::now().timestamp_millis() as u64;
        let cutoff_ms = active_minutes.map(|m| now.saturating_sub(m as u64 * 60 * 1000));

        // Build the peer filter's expected (kind, id) pair so we can
        // match against the persisted `peer_type`/`peer_id` strings on
        // `SessionMetadata`. Sessions without recorded peer info are
        // skipped when a peer filter is supplied (so branches don't
        // bleed across principals).
        let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));

        // D5: subtree callers see only their own subtree; principal-
        // level callers keep the full view.
        let caller = caller_context(&self.current_session_key(), &metadatas);

        let mut sessions: Vec<SessionInfo> = metadatas
            .iter()
            .filter(|m| {
                let tree_match = caller.is_base
                    || caller.privileged
                    || in_subtree(&caller, &m.session_id.as_str(), &metadatas);
                let archived_match = include_archived || !m.archived;
                let agent_match = agent_id.map_or(true, |a| m.agent_name == a);
                let active_match = cutoff_ms.map_or(true, |cutoff| m.updated_at as u64 >= cutoff);
                let peer_match = peer_filter.as_ref().map_or(true, |(want_kind, want_id)| {
                    // No peer recorded on the metadata — skip when the
                    // caller asked for a specific peer.
                    let (have_kind, have_id) = match (m.peer_type.as_deref(), m.peer_id.as_deref())
                    {
                        (Some(k), Some(i)) => (k, i),
                        _ => return false,
                    };
                    have_kind == want_kind.as_str() && have_id == want_id.as_str()
                });
                tree_match && archived_match && peer_match && agent_match && active_match
            })
            .take(limit)
            .map(|m| {
                let id_str = m.session_id.to_string();
                let path = peko_session::path::compute_path(&metadatas, m.session_id);
                SessionInfo {
                    session_key: id_str.clone(),
                    session_id: id_str,
                    agent_id: Some(m.agent_name.clone()),
                    label: m.title.clone(),
                    created_at: chrono::DateTime::from_timestamp_millis(m.created_at as i64)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                    last_activity: chrono::DateTime::from_timestamp_millis(m.updated_at as i64)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default(),
                    message_count: m.message_count,
                    peer_type: m.peer_type.clone(),
                    peer_id: m.peer_id.clone(),
                    archived: m.archived,
                    run_active: false, // filled below when a registry is bound
                    slug: m.slug.clone(),
                    // Computed display path (slug segments; slugless
                    // ancestors skipped, slugless target falls back to its
                    // raw id). Ids stay the canonical key.
                    path,
                }
            })
            .collect();

        // Run-permit snapshot so the agent can see why a delete would
        // refuse. `peek_run_held` is a snapshot (fine for display).
        // Subagent runs never hold `InboxRegistry` permits — they
        // register in the per-agent `AsyncTaskRegistry` instead — so a
        // live subagent session shows up via the unified-registry check
        // (the same guard the delete path uses at `delete_session`).
        use crate::extensions::framework::async_exec::executor::registry::has_active_subagent_run_across_all_registries;
        for info in &mut sessions {
            let run_held = match &self.inbox_registry {
                Some(registry) => registry.peek_run_held(&info.session_id).await,
                None => false,
            };
            info.run_active =
                run_held || has_active_subagent_run_across_all_registries(&info.session_id).await;
        }

        Ok(sessions)
    }

    async fn get_history(
        &self,
        session_key: &str,
        limit: usize,
        include_tools: bool,
    ) -> anyhow::Result<Vec<HistoryMessage>> {
        // D5: subtree callers may not read out-of-tree sessions.
        let session_key = {
            let mut manager = self.session_manager.write().await;
            let (caller, metas) = self.caller_and_metas(&mut manager).await?;
            let session_key = self.resolve_ref(&caller, &metas, session_key)?;
            self.guard_tree(&caller, &session_key, &metas)?;
            session_key
        };
        let session_key = session_key.as_str();

        // Try to open the session to get a handle, then load history
        let llm_messages: Vec<LlmMessage> = {
            let mut manager = self.session_manager.write().await;
            if let Ok(Some(handle)) = manager.open_session(session_key).await {
                handle.load_history().await?
            } else {
                // Fallback: try loading directly from storage
                let sessions_dir = manager.sessions_dir().cloned();
                drop(manager); // drop lock before async storage ops

                if let Some(dir) = sessions_dir {
                    let storage = SessionStorage::new(dir);
                    let events = storage.load_events(session_key).await?;
                    events.iter().filter_map(event_to_llm_message).collect()
                } else {
                    vec![]
                }
            }
        };

        let messages: Vec<HistoryMessage> = llm_messages
            .iter()
            .filter_map(|m| llm_message_to_history(m, include_tools))
            .take(limit)
            .collect();

        Ok(messages)
    }

    async fn get_status(&self, session_id: &str) -> anyhow::Result<SessionStatusResult> {
        if session_id.is_empty() {
            return Err(anyhow::anyhow!("No current session available"));
        }

        let mut manager = self.session_manager.write().await;
        // D5: subtree callers may not read out-of-tree sessions.
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_id = &self.resolve_ref(&caller, &metas, session_id)?;
        self.guard_tree(&caller, session_id, &metas)?;

        let metadata = manager.get_session_metadata(session_id).await?;

        Ok(SessionStatusResult {
            session_id: metadata.session_id.to_string(),
            agent_name: metadata.agent_name,
            created_at: chrono::DateTime::from_timestamp_millis(metadata.created_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            last_activity: chrono::DateTime::from_timestamp_millis(metadata.updated_at as i64)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default(),
            timestamp_utc: String::new(),
            timestamp: String::new(),
            message_count: metadata.message_count,
            usage: UsageStats {
                prompt_tokens: metadata.total_input_tokens as u64,
                completion_tokens: metadata.total_output_tokens as u64,
                last_total_tokens: metadata.last_total_tokens as u64,
                model_context_limit: metadata.model_context_limit,
            },
            peer_type: metadata.peer_type,
            peer_id: metadata.peer_id,
            label: metadata.title,
            parent_session: metadata.parent_session_id.map(|id| id.to_string()),
        })
    }

    fn current_session_key(&self) -> String {
        self.current_session_id
            .try_read()
            .ok()
            .and_then(|id| id.clone())
            .unwrap_or_default()
    }

    async fn search_sessions(
        &self,
        query: &str,
        peer: Option<&Subject>,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionSearchHit>> {
        let (ids, sessions_dir) = {
            let mut manager = self.session_manager.write().await;
            let (caller, metas) = self.caller_and_metas(&mut manager).await?;
            let peer_filter = peer.map(|p| (p.kind().to_string(), p.subject_id().to_string()));
            let ids: Vec<String> = metas
                .iter()
                .filter(|m| {
                    // D5: subtree callers search only their subtree.
                    let tree_match = caller.is_base
                        || caller.privileged
                        || in_subtree(&caller, &m.session_id.as_str(), &metas);
                    let visible_match = !m.archived;
                    let peer_match = peer_filter.as_ref().map_or(true, |(want_kind, want_id)| {
                        let (have_kind, have_id) =
                            match (m.peer_type.as_deref(), m.peer_id.as_deref()) {
                                (Some(k), Some(i)) => (k, i),
                                _ => return false,
                            };
                        have_kind == want_kind.as_str() && have_id == want_id.as_str()
                    });
                    tree_match && visible_match && peer_match
                })
                .map(|m| m.session_id.to_string())
                .collect();
            let sessions_dir = manager
                .sessions_dir()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Sessions directory not set"))?;
            (ids, sessions_dir)
        };

        let storage = SessionStorage::new(sessions_dir);
        let hits = storage.search_transcripts(&ids, query, limit).await?;
        Ok(hits
            .into_iter()
            .map(|h| SessionSearchHit {
                session_id: h.session_id,
                role: h.role,
                timestamp: h.timestamp.to_rfc3339(),
                snippet: h.snippet,
            })
            .collect())
    }

    async fn copy_session(
        &self,
        session_key: &str,
        target_parent: String,
        target_slug: String,
        label: Option<String>,
    ) -> anyhow::Result<BranchOutcome> {
        let mut manager = self.session_manager.write().await;
        // Ownership guard on the source. Branching the caller's CURRENT
        // session is allowed (Phase 1 made the copy lock-safe).
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = self.resolve_ref(&caller, &metas, session_key)?;
        self.guard_tree(&caller, &session_key, &metas)?;

        // Resolve the destination parent. The tool layer guarantees
        // `target_parent` is a slug path; here we turn it into a session
        // id (or refuse if it doesn't exist / is out of the caller's
        // subtree).
        let target_parent_id = self.resolve_ref(&caller, &metas, &target_parent)?;
        self.guard_tree(&caller, &target_parent_id, &metas)?;

        // Slug uniqueness pre-check against the destination's children
        // (actionable error before we touch state; set_session_slug
        // below re-checks authoritatively).
        let source_id = peko_session::SessionId::from(session_key.as_str());
        let target_parent_typed = peko_session::SessionId::from(target_parent_id.as_str());
        if let Some(conflict) = peko_session::path::slug_conflict(
            &metas,
            Some(target_parent_typed),
            &target_slug,
            source_id,
        ) {
            return Err(self.refuse(peko_session::path::err_slug_conflict(
                &target_slug,
                conflict,
                Some(target_parent_typed),
            )));
        }

        // branch_session_by_id creates a child of source with the
        // source's JSONL, peer, agent, and trigger="branch". We then
        // reparent under target_parent (if it differs from source) and
        // stamp the explicit slug. manager.move_session skips the
        // runtime-level user-facing guards (self / trunk / cycle) —
        // those guards operate on user-initiated mutations; for a
        // copy's internal reparenting, the parent-validity check above
        // is the right gate.
        let new_session_id = manager.branch_session_by_id(&session_key, label).await?;

        if target_parent_id != session_key {
            manager
                .move_session(&new_session_id.as_str(), Some(target_parent_typed))
                .await?;
        }

        manager
            .set_session_slug(&new_session_id.as_str(), Some(target_slug))
            .await?;

        Ok(BranchOutcome {
            new_session_id: new_session_id.to_string(),
            parent_session_id: session_key,
        })
    }

    async fn rename_session(
        &self,
        session_key: &str,
        title: Option<String>,
        slug: Option<String>,
    ) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = self.resolve_ref(&caller, &metas, session_key)?;
        self.guard_not_self(&caller, &session_key)?;
        self.guard_tree(&caller, &session_key, &metas)?;

        // Slug format + per-parent uniqueness are enforced inside
        // `set_session_slug` (the controller scans the siblings); its
        // structured error names the conflicting session id.
        if let Some(slug) = slug {
            manager.set_session_slug(&session_key, Some(slug)).await?;
        }
        if let Some(title) = title {
            manager.set_session_title(&session_key, Some(title)).await?;
        }
        Ok(())
    }

    async fn set_archived(&self, session_key: &str, archived: bool) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = &self.resolve_ref(&caller, &metas, session_key)?;
        self.guard_not_self(&caller, session_key)?;
        self.guard_tree(&caller, session_key, &metas)?;
        // The live trunk session (`root:self`) is continuous and
        // engine-managed: archiving it is refused outright. Phase 7
        // narrowed this from the whole `root:*` family — the per-peer
        // root sessions are retired; the trunk is the only one left.
        if archived && session_key.as_str() == crate::principal::routers::root::trunk_session_id() {
            return Err(self.refuse(err_live_base_managed(session_key)));
        }

        if archived {
            // Archiving blocks future runs of the session; refuse while
            // one is in flight. The permit is held across the flag write
            // so no run can start in between. `None` registry (stateless
            // CLI) degrades to metadata-only.
            match &self.inbox_registry {
                Some(registry) => {
                    let _permit = registry
                        .try_acquire_run(session_key)
                        .await
                        .ok_or_else(|| self.refuse(err_run_active(session_key)))?;
                    manager.set_archived(session_key, true).await?;
                }
                None => {
                    tracing::debug!(
                        "no inbox registry bound; archiving {session_key} without run-permit check"
                    );
                    manager.set_archived(session_key, true).await?;
                }
            }
        } else {
            // Unarchive needs ownership only.
            manager.set_archived(session_key, false).await?;
        }
        Ok(())
    }

    async fn delete_session(
        &self,
        session_key: &str,
        recursive: bool,
    ) -> anyhow::Result<DeleteOutcome> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = &self.resolve_ref(&caller, &metas, session_key)?;

        if !metas.iter().any(|m| m.session_id.as_str() == *session_key) {
            return Err(anyhow::anyhow!("Session not found: {session_key}"));
        }
        self.guard_not_self(&caller, session_key)?;
        if caller.ancestors.iter().any(|a| a == session_key) {
            return Err(self.refuse(err_delete_ancestor(session_key)));
        }
        self.guard_tree(&caller, session_key, &metas)?;
        // The live trunk session (`root:self`) is continuous and
        // engine-managed: deleting it is refused outright. Phase 7
        // narrowed this from the whole `root:*` family — the per-peer
        // root sessions are retired; the trunk is the only one left.
        if session_key.as_str() == crate::principal::routers::root::trunk_session_id() {
            return Err(self.refuse(err_live_base_managed(session_key)));
        }

        // Subtree, children first (post-order).
        let mut descendants = descendants_of(session_key, &metas);
        descendants.sort();
        if !descendants.is_empty() && !recursive {
            return Err(self.refuse(err_descendants_exist(session_key, &descendants)));
        }
        let mut post_order = Vec::new();
        let mut stack = vec![session_key.clone()];
        // Cycle guard: a corrupt metadata chain (parent↔child loop)
        // would otherwise push the same id onto `stack` forever. We
        // already filter on push so the walk terminates.
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            post_order.push(id.clone());
            for m in &metas {
                if m.parent_session_id.as_ref().map(|p| p.as_str()).as_deref() == Some(id.as_str()) {
                    stack.push(m.session_id.to_string());
                }
            }
        }
        post_order.reverse();

        // D3 run-permit protocol: acquire target + every descendant and
        // HOLD the permits across the deletes so no run can be in
        // flight — or start — mid-operation. `None` registry (stateless
        // CLI) degrades to metadata-only with a debug note.
        let mut _permits = Vec::new();
        match &self.inbox_registry {
            Some(registry) => {
                for id in std::iter::once(&session_key.clone()).chain(descendants.iter()) {
                    let permit = registry
                        .try_acquire_run(id)
                        .await
                        .ok_or_else(|| self.refuse(err_run_active(id)))?;
                    _permits.push(permit);
                }
            }
            None => {
                tracing::debug!(
                    "no inbox registry bound; deleting {session_key} metadata-only (no run-permit checks)"
                );
            }
        }

        // Subagent runs are NOT tracked by `InboxRegistry` (those are
        // root-only); they live in the global `AsyncTaskRegistry`
        // index. A recursive delete on a parent whose child is mid-
        // iteration in `Agent::run_subagent` must be refused for the
        // same reason as the InboxRegistry check above — otherwise the
        // in-flight task's completion announcement lands on a
        // tombstone session id. See PR review 2026-08-10.
        use crate::extensions::framework::async_exec::executor::registry::has_active_subagent_run_across_all_registries;
        for id in std::iter::once(&session_key.clone()).chain(descendants.iter()) {
            if has_active_subagent_run_across_all_registries(id).await {
                return Err(self.refuse(err_run_active(id)));
            }
        }

        let mut deleted = Vec::new();
        for id in &post_order {
            if manager.delete_session_by_id(id).await? {
                deleted.push(id.clone());
            }
        }
        Ok(DeleteOutcome { deleted })
    }

    async fn move_session(
        &self,
        session_key: &str,
        new_parent: String,
        new_slug: Option<String>,
    ) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = &self.resolve_ref(&caller, &metas, session_key)?;
        let new_parent = &self.resolve_ref(&caller, &metas, &new_parent)?;

        if !metas.iter().any(|m| m.session_id.as_str() == *session_key) {
            return Err(anyhow::anyhow!("Session not found: {session_key}"));
        }
        if !metas.iter().any(|m| m.session_id.as_str() == *new_parent) {
            return Err(anyhow::anyhow!("Session not found: {new_parent}"));
        }
        self.guard_not_self(&caller, session_key)?;
        if caller.ancestors.iter().any(|a| a == session_key) {
            return Err(self.refuse(err_move_ancestor(session_key)));
        }
        // Tree guard on BOTH endpoints: a subtree caller may only move
        // sessions within its own subtree, and the destination must be
        // in that subtree too.
        self.guard_tree(&caller, session_key, &metas)?;
        self.guard_tree(&caller, new_parent, &metas)?;
        // The live trunk session (`root:self`) is continuous and
        // engine-managed: moving it is refused outright (moving UNDER
        // it is fine). Phase 7 narrowed this from the whole `root:*`
        // family — the per-peer root sessions are retired; the trunk
        // is the only one left.
        if session_key.as_str() == crate::principal::routers::root::trunk_session_id() {
            return Err(self.refuse(err_live_base_managed(session_key)));
        }

        // Cycle guard: ancestry walkers are cycle-safe but silently
        // truncate, so a cycle must never be CREATED. Refuse when the
        // destination is the target itself or sits in its subtree.
        let mut descendants = descendants_of(session_key, &metas);
        descendants.sort();
        if new_parent == session_key || descendants.iter().any(|d| d == new_parent) {
            return Err(self.refuse(err_move_cycle(session_key, new_parent)));
        }

        // Slug uniqueness re-check against the DESTINATION's siblings.
        // Sprint 7 Commit H (2026-08-22): when `new_slug` is supplied,
        // check THAT slug (it'll be applied after the reparent); when
        // omitted, check the source's current slug (it'll be carried
        // into the destination unchanged). The `exclude` arg is the
        // source itself so a same-parent + same-slug move (no-op)
        // isn't refused by the check.
        let effective_slug: Option<&str> = match new_slug.as_deref() {
            Some(s) => Some(s),
            None => metas
                .iter()
                .find(|m| m.session_id.as_str() == *session_key)
                .and_then(|m| m.slug.as_deref()),
        };
        if let Some(slug) = effective_slug {
            let new_parent_id = peko_session::SessionId::from(new_parent);
            let session_id_for_check = peko_session::SessionId::from(session_key);
            if let Some(conflict) = peko_session::path::slug_conflict(
                &metas,
                Some(new_parent_id),
                slug,
                session_id_for_check,
            ) {
                return Err(self.refuse(peko_session::path::err_slug_conflict(
                    slug,
                    conflict,
                    Some(new_parent_id),
                )));
            }
        }

        // Live-run guard: REFUSE while the target or any descendant has
        // a run in flight. Delete holds the InboxRegistry permits across
        // its multi-step removal; a reparent is a single metadata write,
        // so a refuse-and-retry snapshot check is enough. `None`
        // registry (stateless CLI) degrades to metadata-only.
        match &self.inbox_registry {
            Some(registry) => {
                for id in std::iter::once(&session_key.clone()).chain(descendants.iter()) {
                    if registry.peek_run_held(id).await {
                        return Err(self.refuse(err_run_active(id)));
                    }
                }
            }
            None => {
                tracing::debug!(
                    "no inbox registry bound; moving {session_key} without run-permit check"
                );
            }
        }
        // Subagent runs never hold InboxRegistry permits (they register
        // in the per-agent AsyncTaskRegistry) — same check as delete.
        use crate::extensions::framework::async_exec::executor::registry::has_active_subagent_run_across_all_registries;
        for id in std::iter::once(&session_key.clone()).chain(descendants.iter()) {
            if has_active_subagent_run_across_all_registries(id).await {
                return Err(self.refuse(err_run_active(id)));
            }
        }

        manager
            .move_session(
                session_key,
                Some(peko_session::SessionId::from(new_parent)),
            )
            .await?;

        // Apply the slug change (if any). The uniqueness check above
        // used the post-reparent parent (`new_parent`), so applying
        // the slug after reparent keeps the metadata consistent with
        // the check. set_session_slug's per-parent-uniqueness check is
        // authoritative.
        if let Some(slug) = new_slug {
            manager.set_session_slug(session_key, Some(slug)).await?;
        }

        Ok(())
    }

    async fn request_compaction(&self, session_key: &str) -> anyhow::Result<CompactRequestOutcome> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = &self.resolve_ref(&caller, &metas, session_key)?;
        // Compacting the CURRENT session is allowed and encouraged
        // (fires at the next iteration); only the ownership guard and
        // the archived refusal apply.
        self.guard_tree(&caller, session_key, &metas)?;
        let meta = metas
            .iter()
            .find(|m| m.session_id.as_str() == *session_key)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
        if meta.archived {
            return Err(self.refuse(err_compact_archived(session_key)));
        }

        manager.set_compact_requested(session_key, true).await?;
        Ok(CompactRequestOutcome {
            session_id: session_key.clone(),
            message: "Compaction scheduled — fires at the next iteration for the \
                      current session, at its next run for others"
                .to_string(),
        })
    }
}

/// Convert an `LlmMessage` to a `HistoryMessage` for tool output.
///
/// Mirrors the legacy root-side `llm_message_to_history` so the
/// runtime adapter and the production adapter produce identical
/// shapes; a JSON-roundtrip pin test in `peko_tools_builtin::session`
/// catches any drift between the two.
fn llm_message_to_history(msg: &LlmMessage, include_tools: bool) -> Option<HistoryMessage> {
    let role = format!("{:?}", msg.role).to_lowercase();

    // Extract text content
    let content = msg
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let (tool_calls, tool_results) = if include_tools {
        let calls: Vec<ToolCallInfo> = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => Some(ToolCallInfo {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect();

        let results: Vec<ToolResultInfo> = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    name,
                    content: result_content,
                    is_error,
                } => {
                    let result_text = result_content
                        .iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Some(ToolResultInfo {
                        tool_call_id: tool_call_id.clone(),
                        success: !is_error,
                        result: Some(json!({ "name": name, "content": result_text })),
                        error: if *is_error {
                            Some("Tool execution failed".to_string())
                        } else {
                            None
                        },
                    })
                }
                _ => None,
            })
            .collect();

        (
            if calls.is_empty() { None } else { Some(calls) },
            if results.is_empty() {
                None
            } else {
                Some(results)
            },
        )
    } else {
        (None, None)
    };

    Some(HistoryMessage {
        role,
        content,
        tool_calls,
        tool_results,
        timestamp: msg.timestamp.to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_session::SessionCreateOptions;
    use tempfile::TempDir;

    /// Sprint 6: convert a test-fixture literal (e.g. "spawn1") to
    /// the v5-derived UUID form the runtime stores in
    /// `SessionMetadata.session_id` and returns over the wire.
    fn sid(literal: &str) -> String {
        peko_session::SessionId::from(literal).to_string()
    }

    /// Tempdir-backed harness: real `SessionManager`, real
    /// `InboxRegistry` (standalone factory), caller's current session
    /// settable per test.
    struct Harness {
        runtime: SessionManagerRuntime,
        manager: Arc<tokio::sync::RwLock<SessionManager>>,
        current: Arc<tokio::sync::RwLock<Option<String>>>,
        registry: Arc<InboxRegistry>,
        /// Held only to keep the tempdir alive for the harness's
        /// lifetime (the manager reads/writes under it).
        _temp: TempDir,
    }

    impl Harness {
        async fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let manager = SessionManager::new()
                .with_sessions_dir_internal(temp.path())
                .with_agent_name("test-agent");
            let manager = Arc::new(tokio::sync::RwLock::new(manager));
            let current = Arc::new(tokio::sync::RwLock::new(None));
            let registry =
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry();
            let runtime = SessionManagerRuntime::new(
                Arc::clone(&manager),
                Arc::clone(&current),
                "test-agent".to_string(),
                Some(Arc::clone(&registry)),
            );
            Self {
                runtime,
                manager,
                current,
                registry,
                _temp: temp,
            }
        }

        async fn set_current(&self, id: &str) {
            // Sprint 6: convert the literal to its v5-derived UUID
            // form so it matches the canonical id stored in
            // `SessionMetadata.session_id`.
            *self.current.write().await =
                Some(peko_session::SessionId::from(id).to_string());
        }

        async fn create(&self, id: &str, parent: Option<&str>) {
            let peer = Subject::User("alice".to_string());
            let mut options = SessionCreateOptions::new().with_session_id(id);
            if let Some(parent) = parent {
                options = options.with_parent(parent);
            }
            self.manager
                .write()
                .await
                .create_session("test-agent", &peer, options)
                .await
                .unwrap();
        }

        async fn add_user_message(&self, id: &str, text: &str) {
            // Sprint 6: convert the literal to its v5-derived UUID
            // form so it matches the canonical id stored in
            // `SessionMetadata.session_id`.
            let id = peko_session::SessionId::from(id).to_string();
            let mut manager = self.manager.write().await;
            let handle = manager
                .open_session(&id)
                .await
                .unwrap()
                .expect("session openable");
            handle.add_user(text).await.unwrap();
        }
    }

    /// Tree used by most tests:
    /// `root:user:alice` (live) ── `spawn1` ("a") ── `child1` ("b")
    ///                     └──── `spawn2` ("c")
    ///
    /// Phase 7 note: the base id's `root:` prefix carries NO special
    /// semantics anymore — the engine-managed family guard matches
    /// exactly `root:self` (the trunk), so this fixture's base is
    /// simply a plain parentless (base-caller) session id. Tests that
    /// exercise the family guard use `root:self` explicitly.
    ///
    /// Sprint 5 note: the runtime now refuses raw session ids at the
    /// tool boundary (see [`resolve_ref`] → [`resolve_reference`]),
    /// so every child carries a slug from the start. Tests that want
    /// to exercise raw-id paths should construct their own tree with
    /// `Harness::create` directly.
    async fn tree_harness(current: &str) -> Harness {
        let h = Harness::new().await;
        h.create("root:user:alice", None).await;
        h.create("spawn1", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("spawn1", "a").await;
        h.create("child1", Some(sid("spawn1").as_str())).await;
        h.set_slug("child1", "b").await;
        h.create("spawn2", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("spawn2", "c").await;
        h.set_current(current).await;
        h
    }

    // ─── Principal-level caller ─────────────────────────────────────

    #[tokio::test]
    async fn principal_level_full_view_and_mutations() {
        let h = tree_harness("root:user:alice").await;

        // Full list view.
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        assert_eq!(all.len(), 4);
        assert!(all.iter().all(|s| !s.run_active));

        // Reads on any session.
        h.add_user_message("spawn2", "hello from spawn2").await;
        let history = h.runtime.get_history("/c", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);
        let status = h.runtime.get_status("/c").await.unwrap();
        assert_eq!(status.session_id, sid("spawn2"));

        // Search spans the store.
        h.add_user_message("child1", "needle in child").await;
        let hits = h.runtime.search_sessions("needle", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, sid("child1"));

        // Rename / archive / unarchive / compact / branch on others.
        h.runtime
            .rename_session("/c", Some("renamed".to_string()), None)
            .await
            .unwrap();
        h.runtime.set_archived("/c", true).await.unwrap();
        let listed = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        assert_eq!(listed.len(), 3, "archived hidden by default");
        let err = h.runtime.request_compaction("/c").await.unwrap_err();
        assert!(err.to_string().contains("unarchive"), "{err}");
        h.runtime.set_archived("/c", false).await.unwrap();
        h.runtime.request_compaction("/c").await.unwrap();
        let outcome = h
            .runtime
            .copy_session("/c", "/".into(), "c-copy".into(), None)
            .await
            .unwrap();
        assert_eq!(outcome.parent_session_id, sid("spawn2"));

        // Branching the CURRENT session is allowed (lock-safe copy).
        h.runtime
            .copy_session("/", "/".into(), "root-copy".into(), None)
            .await
            .unwrap();

        // Compacting the CURRENT session is allowed (fires next iteration).
        h.runtime.request_compaction("/").await.unwrap();
    }

    #[tokio::test]
    async fn principal_level_self_and_live_base_guards() {
        // Sprint 5: the resolver refuses raw ids, so engine-internal
        // tests for sibling-root behavior use slugs. The point of
        // each guard (self, engine-managed trunk, retired-shape
        // unprotected) is preserved — the id shape is opaque to the
        // runtime; only the guard logic cares.
        let h = tree_harness("root:user:alice").await;

        // Self: delete / archive / rename the current session → refused.
        let err = h.runtime.delete_session("/", true).await.unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");
        let err = h.runtime.set_archived("/", true).await.unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");
        let err = h
            .runtime
            .rename_session("/", Some("x".to_string()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // The trunk (`root:self`) — the only engine-managed id left
        // after Phase 7 — refuses delete/archive. We address it as a
        // child of root:user:alice via a slug so the resolver can
        // find it (sibling-root addressing is out of scope for v1;
        // see sprint 5 plan).
        h.create("root:self", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("root:self", "self").await;
        let err = h
            .runtime
            .delete_session("/self", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
        let err = h.runtime.set_archived("/self", true).await.unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");

        // A retired-shape id (`root:cron:*`) has no family protection
        // anymore: it deletes like any plain session.
        h.create("root:cron:alice", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("root:cron:alice", "cron-alice").await;
        let outcome = h
            .runtime
            .delete_session("/cron-alice", true)
            .await
            .unwrap();
        assert_eq!(outcome.deleted, vec![sid("root:cron:alice")]);
    }

    #[tokio::test]
    async fn delete_descendants_recursive_and_post_order() {
        let h = tree_harness("root:user:alice").await;

        // spawn1 has child1: non-recursive refuses and names the child.
        let err = h.runtime.delete_session("/a", false).await.unwrap_err();
        assert!(err.to_string().contains(&sid("child1")), "{err}");
        assert!(err.to_string().contains("recursive:true"), "{err}");

        // Recursive deletes children first.
        let outcome = h.runtime.delete_session("/a", true).await.unwrap();
        assert_eq!(
            outcome.deleted,
            vec![sid("child1"), sid("spawn1")]
        );
        assert!(h.runtime.get_status("/a").await.is_err());
        assert!(h.runtime.get_status("/a/b").await.is_err());
    }

    #[tokio::test]
    async fn delete_run_permit_held_refuses_then_succeeds() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run(&sid("spawn2")).await.unwrap();
        let err = h.runtime.delete_session("/c", false).await.unwrap_err();
        assert!(err.to_string().contains(&sid("spawn2")), "{err}");
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        let outcome = h.runtime.delete_session("/c", false).await.unwrap();
        assert_eq!(outcome.deleted, vec![sid("spawn2")]);
    }

    #[tokio::test]
    async fn archive_run_permit_held_refuses() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run(&sid("spawn2")).await.unwrap();
        let err = h.runtime.set_archived("/c", true).await.unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime.set_archived("/c", true).await.unwrap();
    }

    #[tokio::test]
    async fn list_marks_run_active_with_held_permit() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run(&sid("child1")).await.unwrap();
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let child = all.iter().find(|s| s.session_id == sid("child1")).unwrap();
        assert!(child.run_active);
        let other = all.iter().find(|s| s.session_id == sid("spawn2")).unwrap();
        assert!(!other.run_active);
        drop(guard);
    }

    /// Subagent runs never hold `InboxRegistry` permits — they register
    /// in the per-agent `AsyncTaskRegistry`. `session list` must still
    /// mark such a session `run_active` (same check the delete path
    /// uses). The probe session id is unique to this test so entries
    /// from other tests in the shared global registries can't collide.
    #[tokio::test]
    async fn list_marks_run_active_for_subagent_run() {
        use crate::extensions::framework::async_exec::executor::registry::{
            get_or_create_registry_for_agent, AsyncTaskEntry, SubagentMetadata, TaskMetadata,
        };
        use crate::extensions::framework::async_exec::executor::types::{
            AsyncTaskStatus, AsyncToolConfig,
        };

        let h = tree_harness("root:user:alice").await;
        h.create("subrun_probe", Some(sid("root:user:alice").as_str())).await;

        let registry = get_or_create_registry_for_agent("test-agent-list-run-active");
        let task_id = "task_subrun_probe".to_string();
        registry
            .write()
            .await
            .register(AsyncTaskEntry::with_metadata(
                task_id.clone(),
                "Agent".to_string(),
                json!({}),
                sid("root:user:alice"),
                AsyncToolConfig::default(),
                TaskMetadata::Subagent(SubagentMetadata {
                    // Sprint 6: child_session_id is the v5 UUID form
                    // that `SessionMetadata.session_id` carries, so the
                    // `has_active_subagent_run_for_child` lookup
                    // (matched on `info.session_id == child`) finds the
                    // run. The legacy overlay `child_session_key` is
                    // kept populated for compat in case any
                    // engine-internal caller still matches it.
                    child_session_key: sid("subrun_probe"),
                    child_session_id: Some(sid("subrun_probe")),
                    cleanup: peko_session::types::SpawnCleanupPolicy::Keep,
                    depth: 1,
                    announce_completion: false,
                    subagent_result: None,
                }),
            ));

        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let probe = all.iter().find(|s| s.session_id == sid("subrun_probe")).unwrap();
        assert!(probe.run_active, "live subagent run must mark run_active");

        // Terminal runs no longer count.
        registry
            .write()
            .await
            .update_status(&task_id, AsyncTaskStatus::Cancelled);
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let probe = all.iter().find(|s| s.session_id == sid("subrun_probe")).unwrap();
        assert!(!probe.run_active);
    }

    // ─── Subtree (spawned) caller ───────────────────────────────────

    #[tokio::test]
    async fn subtree_caller_mutation_guards() {
        let h = tree_harness("spawn1").await;

        // Out-of-tree targets refuse every mutation.
        let err = h.runtime.delete_session("/c", true).await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.set_archived("/c", true).await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h
            .runtime
            .rename_session("/c", Some("x".to_string()), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.request_compaction("/c").await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h
            .runtime
            .copy_session("/c", "/".into(), "c-copy".into(), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Ancestor delete refuses (ancestor guard, not the tree message).
        let err = h
            .runtime
            .delete_session("/", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");

        // Self delete refuses.
        let err = h.runtime.delete_session("/a", true).await.unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // In-tree mutations work.
        h.runtime.set_archived("/a/b", true).await.unwrap();
        h.runtime.set_archived("/a/b", false).await.unwrap();
        h.runtime.request_compaction("/a/b").await.unwrap();
        let branch = h
            .runtime
            .copy_session("/a/b", "/a/b".into(), "b-copy".into(), None)
            .await
            .unwrap();
        h.runtime
            .rename_session("/a/b", Some("kid".to_string()), None)
            .await
            .unwrap();
        // child1 now has the branch as a descendant: recursive delete
        // removes both, branch first.
        let outcome = h.runtime.delete_session("/a/b", true).await.unwrap();
        assert_eq!(
            outcome.deleted,
            vec![branch.new_session_id, sid("child1")]
        );
    }

    #[tokio::test]
    async fn subtree_caller_read_scoping() {
        let h = tree_harness("spawn1").await;
        h.add_user_message("child1", "needle in tree").await;
        h.add_user_message("spawn2", "needle out of tree").await;

        // list: only the subtree is visible.
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let ids: Vec<&str> = all.iter().map(|s| s.session_id.as_str()).collect();
        assert!(ids.contains(&sid("spawn1").as_str()));
        assert!(ids.contains(&sid("child1").as_str()));
        assert!(!ids.contains(&sid("spawn2").as_str()));
        assert!(!ids.contains(&sid("root:user:alice").as_str()));

        // search: hits only from in-tree sessions.
        let hits = h.runtime.search_sessions("needle", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, sid("child1"));

        // history/status: out-of-tree explicit keys refuse; in-tree works.
        let err = h
            .runtime
            .get_history("/c", 10, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.get_status("/c").await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let history = h.runtime.get_history("/a/b", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);
        h.runtime.get_status("/a/b").await.unwrap();
    }

    /// A `privileged` spawned caller (sprint 2 peer-child provisioning)
    /// gets whole-store reach through the same guards that confine a
    /// plain spawned caller — while the self / ancestor / trunk
    /// (`root:self`) guards (independent of `is_base`/`privileged`)
    /// still apply.
    #[tokio::test]
    async fn privileged_caller_whole_store_reach() {
        let h = tree_harness("spawn1").await;
        h.manager
            .write()
            .await
            .set_privileged(&sid("spawn1"), true)
            .await
            .unwrap();

        // Whole-store list view (a plain spawned caller sees only its
        // subtree — see subtree_caller_read_scoping).
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        assert_eq!(all.len(), 4);

        // Out-of-subtree mutations pass: rename, archive, move, delete.
        h.runtime
            .rename_session("/c", Some("renamed".to_string()), None)
            .await
            .unwrap();
        h.runtime.set_archived("/c", true).await.unwrap();
        h.runtime.set_archived("/c", false).await.unwrap();
        h.runtime
            .move_session("/c", "/a".to_string(), None)
            .await
            .unwrap();
        // After the move, spawn2 is under spawn1 (caller). Address it
        // via the new path /a/c.
        let outcome = h.runtime.delete_session("/a/c", true).await.unwrap();
        assert_eq!(outcome.deleted, vec![sid("spawn2")]);

        // Self-mutation is still refused.
        let err = h.runtime.delete_session("/a", true).await.unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // The caller's base ancestor is still refused as an ancestor
        // (delete); the TRUNK (`root:self`) is refused as
        // engine-managed (archive) — Phase 7 narrowed the family guard
        // to the trunk alone, so the archive assertion targets it.
        let err = h
            .runtime
            .delete_session("/", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");
        // Hang root:self under root:user:alice with a slug so the
        // resolver can address it from a non-self caller (sibling-root
        // addressing isn't supported in the resolver scope; see
        // sprint 5 plan). The engine-managed guard fires on the id
        // shape regardless of where it sits in the tree.
        h.create("root:self", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("root:self", "self").await;
        let err = h.runtime.set_archived("/self", true).await.unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
    }

    // ─── Move (reparent) ────────────────────────────────────────────

    #[tokio::test]
    async fn move_reparents_and_appends_audit_event() {
        let h = tree_harness("root:user:alice").await;

        // Principal-level caller: move child1 (with its subtree) from
        // spawn1 to spawn2.
        h.runtime
            .move_session("/a/b", "/c".to_string(), None)
            .await
            .unwrap();
        // After move, child1 sits under spawn2 → path /c/b.
        let status = h.runtime.get_status("/c/b").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some(sid("spawn2").as_str()));

        // Audit trail: a System "reparent" event landed in child1's
        // JSONL recording old → new parent.
        let storage = SessionStorage::new(h._temp.path().to_path_buf());
        let events = storage.load_events(&sid("child1")).await.unwrap();
        let reparent = events
            .iter()
            .find_map(|e| match e {
                peko_session::SessionEvent::System(sys) if sys.event == "reparent" => Some(sys),
                _ => None,
            })
            .expect("reparent System event must be appended");
        assert_eq!(reparent.detail["old_parent"], sid("spawn1"));
        assert_eq!(reparent.detail["new_parent"], sid("spawn2"));

        // Moving UNDER a live root:* session is allowed. child1's
        // current path is /c/b (just moved under spawn2).
        h.runtime
            .move_session("/c/b", "/".to_string(), None)
            .await
            .unwrap();
        // After this move, child1 sits directly under root:user:alice
        // with slug "b" → path /b.
        let status = h.runtime.get_status("/b").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some(sid("root:user:alice").as_str()));

        // Unknown endpoints error.
        assert!(h
            .runtime
            .move_session("/ghost", "/c".to_string(), None)
            .await
            .is_err());
        assert!(h
            .runtime
            .move_session("/b", "/ghost".to_string(), None)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn move_self_and_ancestor_refused() {
        // Caller spawn1: moving its own session refuses (not-self).
        let h = tree_harness("spawn1").await;
        let err = h
            .runtime
            .move_session("/a", "/c".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // Caller child1: moving an ancestor refuses.
        let h = tree_harness("child1").await;
        let err = h
            .runtime
            .move_session("/a", "/c".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");
    }

    #[tokio::test]
    async fn move_out_of_tree_refused_for_subtree_caller() {
        let h = tree_harness("spawn1").await;

        // Target outside the caller's subtree.
        let err = h
            .runtime
            .move_session("/c", "/a/b".to_string(), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Destination outside the caller's subtree.
        let err = h
            .runtime
            .move_session("/a/b", "/c".to_string(), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Fully in-tree move works.
        h.create("grandchild1", Some(sid("child1").as_str())).await;
        h.set_slug("grandchild1", "g").await;
        h.runtime
            .move_session("/a/b/g", "/a".to_string(), None)
            .await
            .unwrap();
        let status = h.runtime.get_status("/a/g").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some(sid("spawn1").as_str()));
    }

    #[tokio::test]
    async fn move_cycle_refused() {
        let h = tree_harness("root:user:alice").await;

        // Moving spawn1 under its own descendant child1 would create a
        // cycle (spawn1 → child1 → spawn1) — ancestry walkers truncate
        // silently on cycles, so this must be refused at move time.
        let err = h
            .runtime
            .move_session("/a", "/a/b".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");

        // Moving a session under itself is the degenerate cycle.
        let err = h
            .runtime
            .move_session("/a/b", "/a/b".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[tokio::test]
    async fn move_root_source_refused() {
        let h = tree_harness("root:user:alice").await;
        // Hang root:self under root:user:alice with a slug so the
        // resolver can address it (sibling-root addressing isn't
        // supported in the resolver scope). The engine-managed guard
        // fires on the id shape regardless of where it sits.
        h.create("root:self", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("root:self", "self").await;

        let err = h
            .runtime
            .move_session("/self", "/a".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");

        // A retired-shape id (`root:cron:*`) moves like any plain
        // session — Phase 7 narrowed the family guard to the trunk.
        h.create("root:cron:alice", Some(sid("root:user:alice").as_str())).await;
        h.set_slug("root:cron:alice", "cron-alice").await;
        h.runtime
            .move_session("/cron-alice", "/a".to_string(), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn move_run_permit_held_refuses_then_succeeds() {
        let h = tree_harness("root:user:alice").await;

        // Sprint 6: permit key is the v5 UUID form of `child1` (the
        // canonical `SessionMetadata.session_id`) so the run-active
        // check inside `move_session` sees it.
        let guard = h.registry.try_acquire_run(&sid("child1")).await.unwrap();
        let err = h
            .runtime
            .move_session("/a/b", "/c".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime
            .move_session("/a/b", "/c".to_string(), None)
            .await
            .unwrap();
        // After move, child1 sits under spawn2 → /c/b.
        let status = h.runtime.get_status("/c/b").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some(sid("spawn2").as_str()));
    }

    /// A run on a DESCENDANT of the move target also refuses — the
    /// subtree moves with the target, so no part of it may be live.
    #[tokio::test]
    async fn move_descendant_run_active_refuses() {
        let h = tree_harness("root:user:alice").await;

        // Sprint 6: the run permit key must be the v5 UUID form of
        // `child1` (the canonical `SessionMetadata.session_id`) to
        // match what `move_session`'s descendant `peek_run_held` call
        // looks up.
        let guard = h.registry.try_acquire_run(&sid("child1")).await.unwrap();
        let err = h
            .runtime
            .move_session("/a", "/c".to_string(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime
            .move_session("/a", "/c".to_string(), None)
            .await
            .unwrap();
    }

    // ─── Tool-surface wiring (SessionTool over the production runtime) ─

    // Sprint 7 Commit F (2026-08-21) retired `archive` / `unarchive`
    // — the prior `tool_archive_refuses_root_session` test routed the
    // `archive` action through the production guard layer to assert
    // the trunk is engine-managed. The action no longer exists, so
    // the test was deleted. Root-session engine-managed guards are
    // still covered at the runtime layer by `move_root_source_refused`
    // and `trunk_session_is_engine_managed` (which spans delete,
    // archive, move).

    /// Phase 3 (2026-08-15) / Phase 7 (2026-08-17): the principal trunk
    /// session `root:self` IS the root family now — delete, archive,
    /// and move are refused as "managed by the engine". Retired-shape
    /// ids (`root:{peer}`, `root:cron:{peer}`) carry no protection:
    /// they are plain sessions and mutate freely (asserted in
    /// `principal_level_self_and_live_base_guards` and
    /// `move_root_source_refused`).
    #[tokio::test]
    async fn trunk_session_is_engine_managed() {
        // Caller IS the trunk — `/` resolves to root:self from here,
        // and the guard stack refuses the mutation. The self-guard
        // fires first; the engine-managed guard would also fire on
        // a non-self caller but addressing a sibling root isn't
        // supported in the resolver scope (see sprint 5 plan).
        //
        // `move_session` is omitted because resolving the new_parent
        // requires the destination to be reachable from the caller,
        // and root:self has no slugged children to use as a target.
        let h = tree_harness("root:self").await;
        h.create("root:self", None).await;

        let err = h.runtime.delete_session("/", true).await.unwrap_err();
        assert!(
            err.to_string().contains("currently running in")
                || err.to_string().contains("managed by the engine"),
            "{err}"
        );

        let err = h.runtime.set_archived("/", true).await.unwrap_err();
        assert!(
            err.to_string().contains("currently running in")
                || err.to_string().contains("managed by the engine"),
            "{err}"
        );
    }

    // ─── Slugs + path addressing (Phase 1b) ─────────────────────────

    impl Harness {
        /// Set a session's slug directly through the manager (the raw
        /// write path; uniqueness is enforced by the controller).
        async fn set_slug(&self, id: &str, slug: &str) {
            // Sprint 6: convert the literal to its v5-derived UUID
            // form so it matches the canonical id stored in
            // `SessionMetadata.session_id`.
            let id = peko_session::SessionId::from(id).to_string();
            self.manager
                .read()
                .await
                .set_session_slug(&id, Some(slug.to_string()))
                .await
                .unwrap();
        }
    }

    /// Tree with slugs: root:user:alice ── spawn1 ("a") ── child1 ("b")
    ///                                 └──── spawn2 ("c")
    async fn slug_tree_harness(current: &str) -> Harness {
        let h = tree_harness(current).await;
        h.set_slug("spawn1", "a").await;
        h.set_slug("child1", "b").await;
        h.set_slug("spawn2", "c").await;
        h
    }

    #[tokio::test]
    async fn path_refs_resolve_across_actions() {
        let h = slug_tree_harness("root:user:alice").await;

        // status + history accept /paths (multi-segment included).
        let status = h.runtime.get_status("/a").await.unwrap();
        assert_eq!(status.session_id, sid("spawn1"));
        let status = h.runtime.get_status("/a/b").await.unwrap();
        assert_eq!(status.session_id, sid("child1"));
        h.add_user_message("child1", "hello via path").await;
        let history = h.runtime.get_history("/a/b", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);

        // "/" is the caller's topmost ancestor (the tree root).
        let status = h.runtime.get_status("/").await.unwrap();
        assert_eq!(status.session_id, sid("root:user:alice"));

        // rename via path, slug-only.
        h.runtime
            .rename_session("/a/b", None, Some("b2".to_string()))
            .await
            .unwrap();
        let status = h.runtime.get_status("/a/b2").await.unwrap();
        assert_eq!(status.session_id, sid("child1"));

        // move accepts /paths for BOTH params.
        h.runtime
            .move_session("/a/b2", "/c".to_string(), None)
            .await
            .unwrap();
        let status = h.runtime.get_status("/c/b2").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some(sid("spawn2").as_str()));

        // compact + archive + delete via path.
        h.runtime.request_compaction("/c/b2").await.unwrap();
        h.runtime.set_archived("/c/b2", true).await.unwrap();
        h.runtime.set_archived("/c/b2", false).await.unwrap();
        let outcome = h.runtime.delete_session("/c/b2", false).await.unwrap();
        assert_eq!(outcome.deleted, vec![sid("child1")]);
    }

    #[tokio::test]
    async fn path_resolution_applies_before_ownership_guards() {
        // A subtree caller resolving "/" gets its OWN tree root — and
        // the guards then refuse it (out-of-tree), proving resolution
        // feeds ids into the unchanged guard layer.
        let h = slug_tree_harness("spawn1").await;
        let err = h.runtime.get_status("/").await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // In-tree path resolves and reads fine for the subtree caller.
        let status = h.runtime.get_status("/a/b").await.unwrap();
        assert_eq!(status.session_id, sid("child1"));
    }

    #[tokio::test]
    async fn path_resolution_error_lists_available_slugs() {
        let h = slug_tree_harness("root:user:alice").await;
        let err = h.runtime.get_status("/nope").await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/nope"), "{msg}");
        assert!(msg.contains("available child slugs: [a, c]"), "{msg}");
    }

    #[tokio::test]
    async fn rename_slug_conflict_and_validation_errors() {
        let h = slug_tree_harness("root:user:alice").await;

        // Sibling conflict (spawn1 already owns "a" under the root).
        let err = h
            .runtime
            .rename_session("/c", None, Some("a".to_string()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains(&sid("spawn1")), "{err}");
        assert!(err.to_string().contains("unique per parent"), "{err}");

        // Same slug under a DIFFERENT parent is fine.
        h.runtime
            .rename_session("/a/b", None, Some("c".to_string()))
            .await
            .unwrap();
        let status = h.runtime.get_status("/a/c").await.unwrap();
        assert_eq!(status.session_id, sid("child1"));

        // Invalid slug format: structured error.
        let err = h
            .runtime
            .rename_session("/c", None, Some("has/slash".to_string()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid slug"), "{err}");
    }

    #[tokio::test]
    async fn move_refuses_slug_conflict_at_destination() {
        let h = slug_tree_harness("root:user:alice").await;
        // Give spawn2 its own child with slug "b" — child1's slug.
        h.create("child2", Some(sid("spawn2").as_str())).await;
        h.set_slug("child2", "b").await;

        let err = h
            .runtime
            .move_session("/a/b", "/c".to_string(), None)
            .await
            .unwrap_err();
        // Sprint 6: the conflicting session is identified by its v5
        // UUID form in the error message.
        assert!(err.to_string().contains(&sid("child2")), "{err}");
        assert!(err.to_string().contains("unique per parent"), "{err}");

        // Moving under a parent with no conflicting child slug works.
        h.runtime
            .move_session("/a/b", "/".to_string(), None)
            .await
            .unwrap();
        let status = h.runtime.get_status("/b").await.unwrap();
        assert_eq!(status.session_id, sid("child1"));
    }

    #[tokio::test]
    async fn copy_session_uses_caller_supplied_slug() {
        // Sprint 7 Commit H: `copy_session` now takes an explicit
        // target_slug (mirrors bash `cp src dst`). The old
        // auto-derived "a-branch" / "a-branch-2" behavior is gone —
        // the caller is responsible for picking a unique slug. This
        // test pins the new contract: a fresh explicit slug lands as
        // the new session's slug; a colliding slug is refused.
        let h = slug_tree_harness("root:user:alice").await;

        let outcome = h
            .runtime
            .copy_session("/a", "/".into(), "a-branch".into(), None)
            .await
            .unwrap();
        let meta = h
            .manager
            .write()
            .await
            .get_session_metadata(&outcome.new_session_id)
            .await
            .unwrap();
        assert_eq!(meta.slug.as_deref(), Some("a-branch"));

        // Second copy with the same explicit target_slug conflicts
        // with the first's slug under the same parent — refused.
        let err = h
            .runtime
            .copy_session("/a", "/".into(), "a-branch".into(), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("slug"), "{err}");
    }

    #[tokio::test]
    async fn list_shows_slug_and_computed_path() {
        let h = slug_tree_harness("root:user:alice").await;

        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let by_id = |id: &str| {
            let id = peko_session::SessionId::from(id).to_string();
            all.iter().find(|s| s.session_id == id).unwrap()
        };
        assert_eq!(by_id("spawn1").slug.as_deref(), Some("a"));
        assert_eq!(by_id("spawn1").path, "/a");
        assert_eq!(by_id("child1").path, "/a/b");
        assert_eq!(by_id("spawn2").path, "/c");
        // Slugless sessions fall back to their raw id (v5 UUID form
        // after Sprint 6) as last segment.
        assert_eq!(by_id("root:user:alice").slug, None);
        assert_eq!(by_id("root:user:alice").path, format!("/{}", sid("root:user:alice")));
    }

    // ─── Sprint 5 + 6: path-only LLM-facing surface ─────────────────
    //
    // Sprint 6 collapsed the three-form grammar (slug path, caller-
    // relative slug, raw id) into a strict two-form grammar (slug path
    // or self-reference) since engine-internal ids are opaque UUIDs.
    // The caller-relative slug tests from sprint 5 are gone — raw ids
    // and caller-relative slugs are uniformly refused with the
    // actionable message below.

    /// Raw session ids — anything that isn't a `/`-prefixed path or the
    /// caller's own id — are REFUSED at the LLM-facing surface so the
    /// model learns to use the `path` field from `session list` instead.
    #[tokio::test]
    async fn raw_id_refused_with_actionable_message() {
        let h = tree_harness("root:user:alice").await;
        h.set_slug("spawn1", "a").await;
        h.set_slug("child1", "b").await;
        h.set_slug("spawn2", "c").await;

        // `:`-bearing id: refused with actionable message.
        // (Skip the self-reference shortcut: caller IS
        // `root:user:alice`, so passing that id returns Ok. Use a
        // different raw id instead.)
        let err = h
            .runtime
            .get_status("spawn2:root:user:alice")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a slug path"), "{err}");
        // UUID-shaped id: now accepted by `resolve_reference` (engine-internal
        // entrypoints hand the runtime raw UUIDs). The session simply
        // doesn't exist — a different refusal.
        let err = h
            .runtime
            .get_status("550e8400-e29b-41d4-a716-446655440000")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");

        // The refusal is consistent across actions (history / rename /
        // delete all flow through `resolve_reference`).
        let err = h
            .runtime
            .get_history("spawn2", 10, false)
            .await
            .unwrap_err();
        // "spawn2" isn't a `/`-prefixed path or the caller's own id,
        // so it falls into the refusal arm.
        assert!(
            err.to_string().contains("not a slug path"),
            "{err}"
        );

        // Now use an actual raw id (UUID) — accepted by `resolve_reference`,
        // refused by the per-action existence guard instead. `get_history`
        // is lenient (returns `Ok(vec![])` for missing sessions), but
        // `rename_session` / `delete_session` error on the existence guard.
        let raw_id = "550e8400-e29b-41d4-a716-446655440000";
        let msgs = h.runtime.get_history(raw_id, 10, false).await.unwrap();
        assert!(msgs.is_empty(), "missing session: empty history");
        let err = h
            .runtime
            .rename_session(raw_id, Some("renamed".to_string()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        let err = h.runtime.delete_session(raw_id, false).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }
}
