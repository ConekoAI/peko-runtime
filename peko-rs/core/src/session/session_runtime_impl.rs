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
//! - **Live slots**: `root:*` ids without `#` refuse delete/archive;
//!   they are managed via chapter rotation (`new` / `resume`) only.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::session::ownership::{
    caller_context, chapter_family, descendants_of, err_chapters_principal_only,
    err_compact_archived, err_delete_ancestor, err_descendants_exist, err_live_base_managed,
    err_not_live_base, err_out_of_tree, err_resume_archived, err_resume_cross_family,
    err_resume_self, err_run_active, err_self_mutation, in_subtree, is_live_base_id, CallerContext,
};
use crate::tools::builtin::session::{
    BranchOutcome, ChapterChangeOutcome, CompactRequestOutcome, DeleteOutcome, HistoryMessage,
    SessionInfo, SessionRuntime, SessionSearchHit, SessionStatusResult, ToolCallInfo,
    ToolResultInfo, UsageStats,
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

    /// Resolve the manager's sessions dir for `chapters.json` writes,
    /// with an actionable error when the manager was built without one
    /// (stateless/test paths).
    async fn sessions_dir_for_chapters(&self) -> anyhow::Result<std::path::PathBuf> {
        self.session_manager
            .read()
            .await
            .sessions_dir()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Sessions directory not set — chapters unavailable"))
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
    fn guard_tree(
        &self,
        caller: &CallerContext,
        target: &str,
        metas: &[SessionMetadata],
    ) -> anyhow::Result<()> {
        if !caller.is_base && !in_subtree(caller, target, metas) {
            return Err(self.refuse(err_out_of_tree(target, &caller.current_session_id)));
        }
        Ok(())
    }

    /// Refuse chapter actions for spawned callers and for callers not
    /// sitting in a live `root:*` slot.
    fn guard_chapter_caller(&self, caller: &CallerContext) -> anyhow::Result<()> {
        if !caller.is_base {
            return Err(self.refuse(err_chapters_principal_only()));
        }
        if !is_live_base_id(&caller.current_session_id) {
            return Err(self.refuse(err_not_live_base(&caller.current_session_id)));
        }
        Ok(())
    }
}

#[async_trait]
impl SessionRuntime for SessionManagerRuntime {
    async fn list_sessions(
        &self,
        kinds: Option<&[String]>,
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
                let kind_match = kinds.map_or(true, |k| k.contains(&m.trigger));
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
                tree_match
                    && archived_match
                    && kind_match
                    && peer_match
                    && agent_match
                    && active_match
            })
            .take(limit)
            .map(|m| SessionInfo {
                session_key: m.session_id.clone(),
                session_id: m.session_id.clone(),
                kind: m.trigger.clone(),
                agent_id: Some(m.agent_name.clone()),
                label: m.title.clone(),
                created_at: chrono::DateTime::from_timestamp_millis(m.created_at as i64)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                last_activity: chrono::DateTime::from_timestamp_millis(m.updated_at as i64)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default(),
                message_count: m.message_count,
                is_active: true,
                peer_type: m.peer_type.clone(),
                peer_id: m.peer_id.clone(),
                archived: m.archived,
                run_active: false, // filled below when a registry is bound
            })
            .collect();

        // Run-permit snapshot so the agent can see why a delete would
        // refuse. `peek_run_held` is a snapshot (fine for display).
        if let Some(registry) = &self.inbox_registry {
            for info in &mut sessions {
                info.run_active = registry.peek_run_held(&info.session_id).await;
            }
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
        {
            let mut manager = self.session_manager.write().await;
            let (caller, metas) = self.caller_and_metas(&mut manager).await?;
            self.guard_tree(&caller, session_key, &metas)?;
        }

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
        self.guard_tree(&caller, session_key, &metas)?;

        let new_session_id = manager.branch_session_by_id(session_key, label).await?;
        Ok(BranchOutcome {
            new_session_id,
            parent_session_id: session_key.to_string(),
        })
    }

    async fn rename_session(&self, session_key: &str, title: String) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        self.guard_not_self(&caller, session_key)?;
        self.guard_tree(&caller, session_key, &metas)?;

        manager.set_session_title(session_key, Some(title)).await
    }

    async fn set_archived(&self, session_key: &str, archived: bool) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        self.guard_not_self(&caller, session_key)?;
        self.guard_tree(&caller, session_key, &metas)?;
        if archived && is_live_base_id(session_key) {
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

        if !metas.iter().any(|m| m.session_id == session_key) {
            return Err(anyhow::anyhow!("Session not found: {session_key}"));
        }
        self.guard_not_self(&caller, session_key)?;
        if caller.ancestors.iter().any(|a| a == session_key) {
            return Err(self.refuse(err_delete_ancestor(session_key)));
        }
        self.guard_tree(&caller, session_key, &metas)?;
        if is_live_base_id(session_key) {
            return Err(self.refuse(err_live_base_managed(session_key)));
        }

        // Subtree, children first (post-order).
        let mut descendants = descendants_of(session_key, &metas);
        descendants.sort();
        if !descendants.is_empty() && !recursive {
            return Err(self.refuse(err_descendants_exist(session_key, &descendants)));
        }
        let mut post_order = Vec::new();
        let mut stack = vec![session_key.to_string()];
        while let Some(id) = stack.pop() {
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
                for id in std::iter::once(&session_key.to_string()).chain(descendants.iter()) {
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

        let mut deleted = Vec::new();
        for id in &post_order {
            if manager.delete_session_by_id(id).await? {
                deleted.push(id.clone());
            }
        }
        Ok(DeleteOutcome { deleted })
    }

    async fn request_compaction(&self, session_key: &str) -> anyhow::Result<CompactRequestOutcome> {
        let mut manager = self.session_manager.write().await;
        let (caller, metas) = self.caller_and_metas(&mut manager).await?;
        // Compacting the CURRENT session is allowed and encouraged
        // (fires at the next iteration); only the ownership guard and
        // the archived refusal apply.
        self.guard_tree(&caller, session_key, &metas)?;
        let meta = metas
            .iter()
            .find(|m| m.session_id == session_key)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_key}"))?;
        if meta.archived {
            return Err(self.refuse(err_compact_archived(session_key)));
        }

        manager.set_compact_requested(session_key, true).await?;
        Ok(CompactRequestOutcome {
            session_id: session_key.to_string(),
            message: "Compaction scheduled — fires at the next iteration for the \
                      current session, at its next run for others"
                .to_string(),
        })
    }

    async fn new_chapter(&self, title: Option<String>) -> anyhow::Result<ChapterChangeOutcome> {
        let live_id = self.current_session_key();
        if live_id.is_empty() {
            return Err(anyhow::anyhow!(
                "No current session to start a new chapter for"
            ));
        }
        let sessions_dir = self.sessions_dir_for_chapters().await?;
        {
            let mut manager = self.session_manager.write().await;
            let (caller, _metas) = self.caller_and_metas(&mut manager).await?;
            self.guard_chapter_caller(&caller)?;
        }
        peko_session::chapters::request(
            &sessions_dir,
            &live_id,
            peko_session::chapters::ChapterRequest::New { title },
        )?;
        Ok(ChapterChangeOutcome {
            live_session_id: live_id,
            message: "New chapter queued — takes effect on the next incoming message".to_string(),
        })
    }

    async fn resume_chapter(
        &self,
        target_session_id: &str,
    ) -> anyhow::Result<ChapterChangeOutcome> {
        let live_id = self.current_session_key();
        if live_id.is_empty() {
            return Err(anyhow::anyhow!("No current session to resume into"));
        }
        let sessions_dir = self.sessions_dir_for_chapters().await?;
        {
            let mut manager = self.session_manager.write().await;
            let (caller, metas) = self.caller_and_metas(&mut manager).await?;
            self.guard_chapter_caller(&caller)?;
            if target_session_id == live_id {
                return Err(self.refuse(err_resume_self(target_session_id)));
            }
            if chapter_family(target_session_id) != live_id {
                return Err(self.refuse(err_resume_cross_family(target_session_id, &live_id)));
            }
            let target_meta = metas
                .iter()
                .find(|m| m.session_id == target_session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {target_session_id}"))?;
            if target_meta.archived {
                return Err(self.refuse(err_resume_archived(target_session_id)));
            }
        }

        // Refuse while a run is in flight for the target; hold the
        // permit across the queue write so it can't start in between.
        // `None` registry degrades to metadata-only.
        let _permit = match &self.inbox_registry {
            Some(registry) => Some(
                registry
                    .try_acquire_run(target_session_id)
                    .await
                    .ok_or_else(|| self.refuse(err_run_active(target_session_id)))?,
            ),
            None => {
                tracing::debug!(
                    "no inbox registry bound; queueing resume of {target_session_id} without run-permit check"
                );
                None
            }
        };
        peko_session::chapters::request(
            &sessions_dir,
            &live_id,
            peko_session::chapters::ChapterRequest::Resume {
                target: target_session_id.to_string(),
            },
        )?;
        Ok(ChapterChangeOutcome {
            live_session_id: live_id,
            message: "Resume queued — takes effect on the next incoming message".to_string(),
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
        temp: TempDir,
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
                temp,
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
            .list_sessions(None, None, None, 50, None, false)
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
            .rename_session("spawn2", "renamed".to_string())
            .await
            .unwrap();
        h.runtime.set_archived("spawn2", true).await.unwrap();
        let listed = h
            .runtime
            .list_sessions(None, None, None, 50, None, false)
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
            .rename_session("root:user:alice", "x".to_string())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("currently running in"), "{err}");

        // A second live base id refuses delete/archive (rotation only).
        h.create("root:cron:alice", None).await;
        let err = h
            .runtime
            .delete_session("root:cron:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("chapter rotation"), "{err}");
        let err = h
            .runtime
            .set_archived("root:cron:alice", true)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("chapter rotation"), "{err}");
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
            .list_sessions(None, None, None, 50, None, false)
            .await
            .unwrap();
        let child = all.iter().find(|s| s.session_id == "child1").unwrap();
        assert!(child.run_active);
        let other = all.iter().find(|s| s.session_id == "spawn2").unwrap();
        assert!(!other.run_active);
        drop(guard);
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
            .rename_session("spawn2", "x".to_string())
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
            .rename_session("child1", "kid".to_string())
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
            .list_sessions(None, None, None, 50, None, false)
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

    #[tokio::test]
    async fn subtree_caller_chapters_refused() {
        let h = tree_harness("spawn1").await;

        let err = h.runtime.new_chapter(None).await.unwrap_err();
        assert!(err.to_string().contains("principal-level"), "{err}");
        let err = h.runtime.resume_chapter("child1").await.unwrap_err();
        assert!(err.to_string().contains("principal-level"), "{err}");
    }

    // ─── Chapters (principal-level caller) ──────────────────────────

    #[tokio::test]
    async fn new_chapter_writes_pending_request() {
        let h = tree_harness("root:user:alice").await;

        let outcome = h
            .runtime
            .new_chapter(Some("morning".to_string()))
            .await
            .unwrap();
        assert_eq!(outcome.live_session_id, "root:user:alice");
        assert!(outcome.message.contains("next incoming message"));

        let pending = peko_session::chapters::take(h.temp.path(), "root:user:alice").unwrap();
        assert_eq!(
            pending,
            Some(peko_session::chapters::ChapterRequest::New {
                title: Some("morning".to_string())
            })
        );
    }

    #[tokio::test]
    async fn resume_chapter_guards_and_happy_path() {
        let h = tree_harness("root:user:alice").await;
        h.create("root:user:alice#c1", Some("root:user:alice"))
            .await;

        // Self.
        let err = h
            .runtime
            .resume_chapter("root:user:alice")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already your current session"),
            "{err}"
        );

        // Cross-family (checked before existence, so no fixture needed).
        let err = h
            .runtime
            .resume_chapter("root:cron:alice#c9")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("different conversation family"),
            "{err}"
        );

        // Unknown target.
        let err = h
            .runtime
            .resume_chapter("root:user:alice#nope")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");

        // Archived target.
        h.runtime
            .set_archived("root:user:alice#c1", true)
            .await
            .unwrap();
        let err = h
            .runtime
            .resume_chapter("root:user:alice#c1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unarchive"), "{err}");
        h.runtime
            .set_archived("root:user:alice#c1", false)
            .await
            .unwrap();

        // Run-active target.
        let guard = h
            .registry
            .try_acquire_run("root:user:alice#c1")
            .await
            .unwrap();
        let err = h
            .runtime
            .resume_chapter("root:user:alice#c1")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("active run"), "{err}");
        drop(guard);

        // Happy path: the pending request lands in chapters.json.
        let outcome = h
            .runtime
            .resume_chapter("root:user:alice#c1")
            .await
            .unwrap();
        assert_eq!(outcome.live_session_id, "root:user:alice");
        let pending = peko_session::chapters::take(h.temp.path(), "root:user:alice").unwrap();
        assert_eq!(
            pending,
            Some(peko_session::chapters::ChapterRequest::Resume {
                target: "root:user:alice#c1".to_string()
            })
        );
    }
}
