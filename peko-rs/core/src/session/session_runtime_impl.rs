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
//! - **Live slots**: `root:*` ids are the principal's continuous
//!   sessions; they refuse delete/archive/move — the engine manages
//!   their lifecycle (paging + compaction).

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
        if !caller.is_base && !in_subtree(caller, target, metas) {
            return Err(self.refuse(err_out_of_tree(target, &caller.current_session_id)));
        }
        Ok(())
    }

    /// Resolve a tool-supplied session reference to a session id.
    ///
    /// A value starting with `/` is a session path (slug segments
    /// anchored at the root of the caller's tree — see
    /// [`peko_session::path`]); anything else is a raw session id and
    /// passes through unchanged. The ownership/scoping guards run
    /// AFTER this resolution, on the resolved id.
    fn resolve_ref(
        &self,
        caller: &CallerContext,
        metas: &[SessionMetadata],
        reference: &str,
    ) -> anyhow::Result<String> {
        if reference.starts_with('/') {
            peko_session::path::resolve_path(metas, &caller.current_session_id, reference)
                .map_err(|e| self.refuse(e))
        } else {
            Ok(reference.to_string())
        }
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
                let tree_match = caller.is_base || in_subtree(&caller, &m.session_id, &metadatas);
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
            .map(|m| SessionInfo {
                session_key: m.session_id.clone(),
                session_id: m.session_id.clone(),
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
                path: peko_session::path::compute_path(&metadatas, &m.session_id),
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
            session_id: metadata.session_id,
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
            parent_session: metadata.parent_session_id,
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
                    let tree_match = caller.is_base || in_subtree(&caller, &m.session_id, &metas);
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
                .map(|m| m.session_id.clone())
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

    async fn branch_session(
        &self,
        session_key: &str,
        label: Option<String>,
    ) -> anyhow::Result<BranchOutcome> {
        let mut manager = self.session_manager.write().await;
        // Ownership guard on the source. Branching the caller's CURRENT
        // session is allowed (Phase 1 made the copy lock-safe).
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = self.resolve_ref(&caller, &metas, session_key)?;
        self.guard_tree(&caller, &session_key, &metas)?;

        // When the source has a slug, the branch derives
        // `<source-slug>-branch` (uniquified among the source's
        // children on conflict) so the copy stays path-addressable.
        // The branch is a child of its source, so the pre-branch
        // metadata slice is the right sibling set.
        let derived_slug = peko_session::path::derive_branch_slug(&metas, &session_key);

        let new_session_id = manager.branch_session_by_id(&session_key, label).await?;
        if let Some(slug) = derived_slug {
            manager
                .set_session_slug(&new_session_id, Some(slug))
                .await?;
        }
        Ok(BranchOutcome {
            new_session_id,
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
        // Live `root:*` sessions are continuous and engine-managed:
        // archiving one is refused outright.
        if archived && session_key.starts_with("root:") {
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

        if !metas.iter().any(|m| &m.session_id == session_key) {
            return Err(anyhow::anyhow!("Session not found: {session_key}"));
        }
        self.guard_not_self(&caller, session_key)?;
        if caller.ancestors.iter().any(|a| a == session_key) {
            return Err(self.refuse(err_delete_ancestor(session_key)));
        }
        self.guard_tree(&caller, session_key, &metas)?;
        // Live `root:*` sessions are continuous and engine-managed:
        // deleting one is refused outright.
        if session_key.starts_with("root:") {
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
                if m.parent_session_id.as_deref() == Some(id.as_str()) {
                    stack.push(m.session_id.clone());
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

    async fn move_session(&self, session_key: &str, new_parent: String) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        let session_key = &self.resolve_ref(&caller, &metas, session_key)?;
        let new_parent = &self.resolve_ref(&caller, &metas, &new_parent)?;

        if !metas.iter().any(|m| &m.session_id == session_key) {
            return Err(anyhow::anyhow!("Session not found: {session_key}"));
        }
        if !metas.iter().any(|m| &m.session_id == new_parent) {
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
        // Live `root:*` sessions are continuous and engine-managed:
        // moving one is refused outright (moving UNDER one is fine).
        if session_key.starts_with("root:") {
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

        // Slug uniqueness re-check against the DESTINATION's siblings:
        // a slug that was unique under the old parent may collide under
        // the new one. Refusal names the conflicting session id.
        let target_slug = metas
            .iter()
            .find(|m| &m.session_id == session_key)
            .and_then(|m| m.slug.as_deref());
        if let Some(slug) = target_slug {
            if let Some(conflict) =
                peko_session::path::slug_conflict(&metas, Some(new_parent), slug, session_key)
            {
                return Err(self.refuse(peko_session::path::err_slug_conflict(
                    slug,
                    &conflict,
                    Some(new_parent),
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
            .move_session(session_key, Some(new_parent.clone()))
            .await
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
            .find(|m| &m.session_id == session_key)
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
            *self.current.write().await = Some(id.to_string());
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
            let mut manager = self.manager.write().await;
            let handle = manager
                .open_session(id)
                .await
                .unwrap()
                .expect("session openable");
            handle.add_user(text).await.unwrap();
        }
    }

    /// Tree used by most tests:
    /// `root:user:alice` (live) ── `spawn1` ── `child1`
    ///                     └──── `spawn2`
    async fn tree_harness(current: &str) -> Harness {
        let h = Harness::new().await;
        h.create("root:user:alice", None).await;
        h.create("spawn1", Some("root:user:alice")).await;
        h.create("child1", Some("spawn1")).await;
        h.create("spawn2", Some("root:user:alice")).await;
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
        let history = h.runtime.get_history("spawn2", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);
        let status = h.runtime.get_status("spawn2").await.unwrap();
        assert_eq!(status.session_id, "spawn2");

        // Search spans the store.
        h.add_user_message("child1", "needle in child").await;
        let hits = h.runtime.search_sessions("needle", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "child1");

        // Rename / archive / unarchive / compact / branch on others.
        h.runtime
            .rename_session("spawn2", Some("renamed".to_string()), None)
            .await
            .unwrap();
        h.runtime.set_archived("spawn2", true).await.unwrap();
        let listed = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        assert_eq!(listed.len(), 3, "archived hidden by default");
        let err = h.runtime.request_compaction("spawn2").await.unwrap_err();
        assert!(err.to_string().contains("unarchive"), "{err}");
        h.runtime.set_archived("spawn2", false).await.unwrap();
        h.runtime.request_compaction("spawn2").await.unwrap();
        let outcome = h.runtime.branch_session("spawn2", None).await.unwrap();
        assert_eq!(outcome.parent_session_id, "spawn2");

        // Branching the CURRENT session is allowed (lock-safe copy).
        h.runtime
            .branch_session("root:user:alice", None)
            .await
            .unwrap();

        // Compacting the CURRENT session is allowed (fires next iteration).
        h.runtime
            .request_compaction("root:user:alice")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn principal_level_self_and_live_base_guards() {
        let h = tree_harness("root:user:alice").await;

        // Self: delete / archive / rename the current session → refused.
        let err = h
            .runtime
            .delete_session("root:user:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");
        let err = h
            .runtime
            .set_archived("root:user:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");
        let err = h
            .runtime
            .rename_session("root:user:alice", Some("x".to_string()), None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // A second live base id refuses delete/archive (engine-managed).
        h.create("root:cron:alice", None).await;
        let err = h
            .runtime
            .delete_session("root:cron:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
        let err = h
            .runtime
            .set_archived("root:cron:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
    }

    #[tokio::test]
    async fn delete_descendants_recursive_and_post_order() {
        let h = tree_harness("root:user:alice").await;

        // spawn1 has child1: non-recursive refuses and names the child.
        let err = h.runtime.delete_session("spawn1", false).await.unwrap_err();
        assert!(err.to_string().contains("child1"), "{err}");
        assert!(err.to_string().contains("recursive:true"), "{err}");

        // Recursive deletes children first.
        let outcome = h.runtime.delete_session("spawn1", true).await.unwrap();
        assert_eq!(
            outcome.deleted,
            vec!["child1".to_string(), "spawn1".to_string()]
        );
        assert!(h.runtime.get_status("spawn1").await.is_err());
        assert!(h.runtime.get_status("child1").await.is_err());
    }

    #[tokio::test]
    async fn delete_run_permit_held_refuses_then_succeeds() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run("spawn2").await.unwrap();
        let err = h.runtime.delete_session("spawn2", false).await.unwrap_err();
        assert!(err.to_string().contains("spawn2"), "{err}");
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        let outcome = h.runtime.delete_session("spawn2", false).await.unwrap();
        assert_eq!(outcome.deleted, vec!["spawn2".to_string()]);
    }

    #[tokio::test]
    async fn archive_run_permit_held_refuses() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run("spawn2").await.unwrap();
        let err = h.runtime.set_archived("spawn2", true).await.unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime.set_archived("spawn2", true).await.unwrap();
    }

    #[tokio::test]
    async fn list_marks_run_active_with_held_permit() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run("child1").await.unwrap();
        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let child = all.iter().find(|s| s.session_id == "child1").unwrap();
        assert!(child.run_active);
        let other = all.iter().find(|s| s.session_id == "spawn2").unwrap();
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
        h.create("subrun_probe", Some("root:user:alice")).await;

        let registry = get_or_create_registry_for_agent("test-agent-list-run-active");
        let task_id = "task_subrun_probe".to_string();
        registry
            .write()
            .await
            .register(AsyncTaskEntry::with_metadata(
                task_id.clone(),
                "Agent".to_string(),
                json!({}),
                "root:user:alice".to_string(),
                AsyncToolConfig::default(),
                TaskMetadata::Subagent(SubagentMetadata {
                    child_session_key: "subrun_probe".to_string(),
                    child_session_id: Some("subrun_probe".to_string()),
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
        let probe = all.iter().find(|s| s.session_id == "subrun_probe").unwrap();
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
        let probe = all.iter().find(|s| s.session_id == "subrun_probe").unwrap();
        assert!(!probe.run_active);
    }

    // ─── Subtree (spawned) caller ───────────────────────────────────

    #[tokio::test]
    async fn subtree_caller_mutation_guards() {
        let h = tree_harness("spawn1").await;

        // Out-of-tree targets refuse every mutation.
        let err = h.runtime.delete_session("spawn2", true).await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.set_archived("spawn2", true).await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h
            .runtime
            .rename_session("spawn2", Some("x".to_string()), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.request_compaction("spawn2").await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.branch_session("spawn2", None).await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Ancestor delete refuses (ancestor guard, not the tree message).
        let err = h
            .runtime
            .delete_session("root:user:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("ancestor"), "{err}");

        // Self delete refuses.
        let err = h.runtime.delete_session("spawn1", true).await.unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // In-tree mutations work.
        h.runtime.set_archived("child1", true).await.unwrap();
        h.runtime.set_archived("child1", false).await.unwrap();
        h.runtime.request_compaction("child1").await.unwrap();
        let branch = h.runtime.branch_session("child1", None).await.unwrap();
        h.runtime
            .rename_session("child1", Some("kid".to_string()), None)
            .await
            .unwrap();
        // child1 now has the branch as a descendant: recursive delete
        // removes both, branch first.
        let outcome = h.runtime.delete_session("child1", true).await.unwrap();
        assert_eq!(
            outcome.deleted,
            vec![branch.new_session_id, "child1".to_string()]
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
        assert!(ids.contains(&"spawn1"));
        assert!(ids.contains(&"child1"));
        assert!(!ids.contains(&"spawn2"));
        assert!(!ids.contains(&"root:user:alice"));

        // search: hits only from in-tree sessions.
        let hits = h.runtime.search_sessions("needle", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "child1");

        // history/status: out-of-tree explicit keys refuse; in-tree works.
        let err = h
            .runtime
            .get_history("spawn2", 10, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let err = h.runtime.get_status("spawn2").await.unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );
        let history = h.runtime.get_history("child1", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);
        h.runtime.get_status("child1").await.unwrap();
    }

    // ─── Move (reparent) ────────────────────────────────────────────

    #[tokio::test]
    async fn move_reparents_and_appends_audit_event() {
        let h = tree_harness("root:user:alice").await;

        // Principal-level caller: move child1 (with its subtree) from
        // spawn1 to spawn2.
        h.runtime
            .move_session("child1", "spawn2".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("child1").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some("spawn2"));

        // Audit trail: a System "reparent" event landed in child1's
        // JSONL recording old → new parent.
        let storage = SessionStorage::new(h._temp.path().to_path_buf());
        let events = storage.load_events("child1").await.unwrap();
        let reparent = events
            .iter()
            .find_map(|e| match e {
                peko_session::SessionEvent::System(sys) if sys.event == "reparent" => Some(sys),
                _ => None,
            })
            .expect("reparent System event must be appended");
        assert_eq!(reparent.detail["old_parent"], "spawn1");
        assert_eq!(reparent.detail["new_parent"], "spawn2");

        // Moving UNDER a live root:* session is allowed.
        h.runtime
            .move_session("child1", "root:user:alice".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("child1").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some("root:user:alice"));

        // Unknown endpoints error.
        assert!(h
            .runtime
            .move_session("ghost", "spawn2".to_string())
            .await
            .is_err());
        assert!(h
            .runtime
            .move_session("child1", "ghost".to_string())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn move_self_and_ancestor_refused() {
        // Caller spawn1: moving its own session refuses (not-self).
        let h = tree_harness("spawn1").await;
        let err = h
            .runtime
            .move_session("spawn1", "spawn2".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // Caller child1: moving an ancestor refuses.
        let h = tree_harness("child1").await;
        let err = h
            .runtime
            .move_session("spawn1", "spawn2".to_string())
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
            .move_session("spawn2", "child1".to_string())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Destination outside the caller's subtree.
        let err = h
            .runtime
            .move_session("child1", "spawn2".to_string())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("outside your session subtree"),
            "{err}"
        );

        // Fully in-tree move works.
        h.create("grandchild1", Some("child1")).await;
        h.runtime
            .move_session("grandchild1", "spawn1".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("grandchild1").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some("spawn1"));
    }

    #[tokio::test]
    async fn move_cycle_refused() {
        let h = tree_harness("root:user:alice").await;

        // Moving spawn1 under its own descendant child1 would create a
        // cycle (spawn1 → child1 → spawn1) — ancestry walkers truncate
        // silently on cycles, so this must be refused at move time.
        let err = h
            .runtime
            .move_session("spawn1", "child1".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");

        // Moving a session under itself is the degenerate cycle.
        let err = h
            .runtime
            .move_session("child1", "child1".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle"), "{err}");
    }

    #[tokio::test]
    async fn move_root_source_refused() {
        let h = tree_harness("root:user:alice").await;
        h.create("root:cron:alice", None).await;

        let err = h
            .runtime
            .move_session("root:cron:alice", "spawn1".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
    }

    #[tokio::test]
    async fn move_run_permit_held_refuses_then_succeeds() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run("child1").await.unwrap();
        let err = h
            .runtime
            .move_session("child1", "spawn2".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime
            .move_session("child1", "spawn2".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("child1").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some("spawn2"));
    }

    /// A run on a DESCENDANT of the move target also refuses — the
    /// subtree moves with the target, so no part of it may be live.
    #[tokio::test]
    async fn move_descendant_run_active_refuses() {
        let h = tree_harness("root:user:alice").await;

        let guard = h.registry.try_acquire_run("child1").await.unwrap();
        let err = h
            .runtime
            .move_session("spawn1", "spawn2".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        h.runtime
            .move_session("spawn1", "spawn2".to_string())
            .await
            .unwrap();
    }

    // ─── Tool-surface wiring (SessionTool over the production runtime) ─

    /// The restored `archive` action on the `session` tool routes
    /// through the production guard layer: a live `root:*` session is
    /// continuous and engine-managed, so archiving it is refused.
    #[tokio::test]
    async fn tool_archive_refuses_root_session() {
        use crate::tools::builtin::session::{SessionTool, SharedSessionRuntime};
        use peko_tools_core::traits::Tool;

        let h = tree_harness("root:user:alice").await;
        h.create("root:cron:alice", None).await;
        let runtime: SharedSessionRuntime = Arc::new(SessionManagerRuntime::new(
            Arc::clone(&h.manager),
            Arc::clone(&h.current),
            "test-agent".to_string(),
            Some(Arc::clone(&h.registry)),
        ));
        let tool = SessionTool::new(runtime);

        let err = tool
            .execute(json!({"action": "archive", "session_key": "root:cron:alice"}))
            .await
            .expect_err("archiving a root session must be refused");
        assert!(err.to_string().contains("managed by the engine"), "{err}");

        // A non-root session archives fine through the same surface.
        tool.execute(json!({"action": "archive", "session_key": "spawn2"}))
            .await
            .unwrap();
    }

    /// Phase 3 (2026-08-15): the principal trunk session `root:self`
    /// keeps the `root:` prefix precisely so the root-family guards
    /// apply to it unchanged — delete, archive, and move are all
    /// refused as "managed by the engine", exactly like `root:{peer}`
    /// and `root:cron:{peer}`.
    #[tokio::test]
    async fn trunk_session_inherits_root_family_guards() {
        let h = tree_harness("root:user:alice").await;
        h.create("root:self", None).await;

        let err = h
            .runtime
            .delete_session("root:self", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");

        let err = h.runtime.set_archived("root:self", true).await.unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");

        let err = h
            .runtime
            .move_session("root:self", "spawn1".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("managed by the engine"), "{err}");
    }

    // ─── Slugs + path addressing (Phase 1b) ─────────────────────────

    impl Harness {
        /// Set a session's slug directly through the manager (the raw
        /// write path; uniqueness is enforced by the controller).
        async fn set_slug(&self, id: &str, slug: &str) {
            self.manager
                .read()
                .await
                .set_session_slug(id, Some(slug.to_string()))
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
        assert_eq!(status.session_id, "spawn1");
        let status = h.runtime.get_status("/a/b").await.unwrap();
        assert_eq!(status.session_id, "child1");
        h.add_user_message("child1", "hello via path").await;
        let history = h.runtime.get_history("/a/b", 10, false).await.unwrap();
        assert_eq!(history.len(), 1);

        // "/" is the caller's topmost ancestor (the tree root).
        let status = h.runtime.get_status("/").await.unwrap();
        assert_eq!(status.session_id, "root:user:alice");

        // rename via path, slug-only.
        h.runtime
            .rename_session("/a/b", None, Some("b2".to_string()))
            .await
            .unwrap();
        let status = h.runtime.get_status("/a/b2").await.unwrap();
        assert_eq!(status.session_id, "child1");

        // move accepts /paths for BOTH params.
        h.runtime
            .move_session("/a/b2", "/c".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("/c/b2").await.unwrap();
        assert_eq!(status.parent_session.as_deref(), Some("spawn2"));

        // compact + archive + delete via path.
        h.runtime.request_compaction("/c/b2").await.unwrap();
        h.runtime.set_archived("/c/b2", true).await.unwrap();
        h.runtime.set_archived("/c/b2", false).await.unwrap();
        let outcome = h.runtime.delete_session("/c/b2", false).await.unwrap();
        assert_eq!(outcome.deleted, vec!["child1".to_string()]);
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
        assert_eq!(status.session_id, "child1");
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
            .rename_session("spawn2", None, Some("a".to_string()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("spawn1"), "{err}");
        assert!(err.to_string().contains("unique per parent"), "{err}");

        // Same slug under a DIFFERENT parent is fine.
        h.runtime
            .rename_session("child1", None, Some("c".to_string()))
            .await
            .unwrap();
        let status = h.runtime.get_status("/a/c").await.unwrap();
        assert_eq!(status.session_id, "child1");

        // Invalid slug format: structured error.
        let err = h
            .runtime
            .rename_session("spawn2", None, Some("has/slash".to_string()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid slug"), "{err}");
    }

    #[tokio::test]
    async fn move_refuses_slug_conflict_at_destination() {
        let h = slug_tree_harness("root:user:alice").await;
        // Give spawn2 its own child with slug "b" — child1's slug.
        h.create("child2", Some("spawn2")).await;
        h.set_slug("child2", "b").await;

        let err = h
            .runtime
            .move_session("child1", "spawn2".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("child2"), "{err}");
        assert!(err.to_string().contains("unique per parent"), "{err}");

        // Moving under a parent with no conflicting child slug works.
        h.runtime
            .move_session("child1", "root:user:alice".to_string())
            .await
            .unwrap();
        let status = h.runtime.get_status("/b").await.unwrap();
        assert_eq!(status.session_id, "child1");
    }

    #[tokio::test]
    async fn branch_derives_uniquified_slug() {
        let h = slug_tree_harness("root:user:alice").await;

        let outcome = h.runtime.branch_session("/a", None).await.unwrap();
        let meta = h
            .manager
            .write()
            .await
            .get_session_metadata(&outcome.new_session_id)
            .await
            .unwrap();
        assert_eq!(meta.slug.as_deref(), Some("a-branch"));

        // Second branch of the same source uniquifies.
        let outcome2 = h.runtime.branch_session("/a", None).await.unwrap();
        let meta2 = h
            .manager
            .write()
            .await
            .get_session_metadata(&outcome2.new_session_id)
            .await
            .unwrap();
        assert_eq!(meta2.slug.as_deref(), Some("a-branch-2"));

        // Branching a slugless source leaves the branch slugless.
        h.create("plain", Some("root:user:alice")).await;
        let outcome3 = h.runtime.branch_session("plain", None).await.unwrap();
        let meta3 = h
            .manager
            .write()
            .await
            .get_session_metadata(&outcome3.new_session_id)
            .await
            .unwrap();
        assert_eq!(meta3.slug, None);
    }

    #[tokio::test]
    async fn list_shows_slug_and_computed_path() {
        let h = slug_tree_harness("root:user:alice").await;

        let all = h
            .runtime
            .list_sessions(None, None, 50, None, false)
            .await
            .unwrap();
        let by_id = |id: &str| all.iter().find(|s| s.session_id == id).unwrap();
        assert_eq!(by_id("spawn1").slug.as_deref(), Some("a"));
        assert_eq!(by_id("spawn1").path, "/a");
        assert_eq!(by_id("child1").path, "/a/b");
        assert_eq!(by_id("spawn2").path, "/c");
        // Slugless sessions fall back to their raw id as last segment.
        assert_eq!(by_id("root:user:alice").slug, None);
        assert_eq!(by_id("root:user:alice").path, "/root:user:alice");
    }
}
