//! Peer messenger — agent-originated notes into a peer's session
//!
//! The delivery half of the `send_peer` unification (2026-08-08): any
//! agent in a run tree (root, subagent, cron-spawned) can surface a
//! message to a human peer without owning the peer's conversational
//! session. The messenger appends a labeled, source-tagged note to
//! `root:{peer}`; it never triggers a turn — the note waits in the
//! JSONL for the peer's next message (mid-turn interleaves are
//! shape-repaired at the next load by `peko_message::repair`).
//!
//! The port trait + global registry mirror the `peko_cron::tools`
//! `CronRuntime` pattern: built-in tools stay free of
//! `SessionManager`/`PrincipalManager` imports, and tests substitute a
//! stub.

use anyhow::Result;
use peko_auth::Subject;
use peko_session::events::MessageSource;
use std::str::FromStr;
use std::sync::Arc;

use crate::principal::manager::PrincipalManager;

/// Maximum parent-linkage hops when resolving the originating peer of
/// a deeply nested subagent session (cycle guard; spawn depth is
/// capped well below this elsewhere).
const MAX_PEER_WALK_DEPTH: usize = 8;

/// Extract the peer from a session id/key WITHOUT touching the session
/// store. Handles:
/// - `root:{subject}` and `root:cron:{subject}` (root-agent turns; the
///   cron variant's originator is the same human owner),
/// - v2 keys `agent:{agent}:peer:{type}:{id}` (optional overlay
///   suffix),
/// - subagent keys (`…:subagent:{uuid}`, possibly nested) — stripped
///   down to the base key.
///
/// Spawn pseudo-peers (`principal:spawn_<uuid>` — the child base
/// session's placeholder peer) are deliberately NOT returned: they
/// carry no originator, so callers must continue walking the parent
/// linkage instead.
#[must_use]
pub fn peer_from_session_key(session_id: &str) -> Option<Subject> {
    // Strip any `:subagent:{uuid}` trail first (possibly nested) — a
    // subagent key's peer is always its base key's peer, and stripping
    // first keeps `root:user:local:subagent:uuid` from misparsing as a
    // user named "local:subagent:uuid".
    let mut current = session_id;
    while let Some((base, _)) = current.rsplit_once(":subagent:") {
        current = base;
    }
    for prefix in ["root:cron:", "root:"] {
        if let Some(rest) = current.strip_prefix(prefix) {
            return non_spawn_subject(rest);
        }
    }
    if let Some(parsed) = peko_session::key::parse_session_key_v2(current) {
        let subject_str = format!("{}:{}", parsed.peer_type, parsed.peer_id);
        return non_spawn_subject(&subject_str);
    }
    None
}

/// Parse a Subject string, rejecting spawn pseudo-peers.
fn non_spawn_subject(s: &str) -> Option<Subject> {
    match Subject::from_str(s) {
        Ok(Subject::Principal(id)) if id.0.starts_with("spawn_") => None,
        Ok(other) => Some(other),
        Err(_) => None,
    }
}

/// Port: deliver a note into a peer's conversational root session, and
/// resolve the originating peer of a run session.
///
/// `deliver_note` takes the note fully formatted — call sites own
/// their label conventions (`⏰ [cron job …]` for the cron engine,
/// `📨 [<agent>]` for `send_peer`). It returns `Ok(false)` when the
/// peer has no conversational session yet (silent skip: the note has
/// nothing to attach to; the outcome still lives in the caller's own
/// session). No session is ever created.
///
/// When `caller_label` is supplied (e.g. `"agent <session_id>"` for a
/// `send_peer` call, `"cron job <name>"` for a cron fire), the
/// messenger ALSO writes a structured `[notify] …` line to the
/// principal's canonical `root:{owner}` session JSONL so the principal
/// sees what was sent on its behalf in its next turn. The
/// notification is meta-info: it does not appear in the chat-log
/// (consumer-facing `peko log`), only in the session JSONL (engine
/// context). The peer view (full note + chat-log row) is unchanged.
#[async_trait::async_trait]
pub trait PeerMessenger: Send + Sync {
    async fn deliver_note(
        &self,
        principal_id: &str,
        target: &Subject,
        note: &str,
        source: MessageSource,
        caller_label: Option<&str>,
    ) -> Result<bool>;

    /// The human (or principal) peer that started the run owning
    /// `session_id`, walking subagent/spawn parentage as needed.
    /// `None` when the chain bottoms out without a peer (the tool
    /// turns this into a clear error).
    async fn originating_peer(
        &self,
        principal_id: &str,
        session_id: &str,
    ) -> Result<Option<Subject>>;
}

/// Production [`PeerMessenger`] over the loaded principal set.
pub struct PrincipalPeerMessenger {
    principal_manager: Arc<PrincipalManager>,
}

impl PrincipalPeerMessenger {
    #[must_use]
    pub fn new(principal_manager: Arc<PrincipalManager>) -> Self {
        Self { principal_manager }
    }

    async fn sessions_dir_for(&self, principal_id: &str) -> Result<std::path::PathBuf> {
        let principal = crate::daemon::cron_engine::resolve_principal(
            &self.principal_manager,
            &peko_subject::PrincipalId(principal_id.to_string()),
        )
        .await
        .ok_or_else(|| anyhow::anyhow!("Principal '{principal_id}' not loaded"))?;
        Ok(principal.memory.sessions_dir().clone())
    }
}

#[async_trait::async_trait]
impl PeerMessenger for PrincipalPeerMessenger {
    async fn deliver_note(
        &self,
        principal_id: &str,
        target: &Subject,
        note: &str,
        source: MessageSource,
        caller_label: Option<&str>,
    ) -> Result<bool> {
        let sessions_dir = self.sessions_dir_for(principal_id).await?;
        let conv_session_id = crate::principal::routers::root::root_session_id(target);
        let mut session_manager = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let appended = match session_manager.open_session(&conv_session_id).await {
            Ok(Some(handle)) => {
                handle.add_user_with_source(note, source).await?;
                true
            }
            // No conversational session yet (the peer has never
            // chatted): nothing to attach the note to.
            Ok(None) => false,
            Err(e) => return Err(e),
        };

        // Chat-log projection: peer notes (📨 from `send_peer`,
        // ⏰ from cron Send/Notify) are principal-authored lines in
        // the owner's conversation thread. They must surface in
        // `peko log` alongside the model's reply. Sender is the
        // principal's DID; the chat-log peer is the human on the
        // other side of the conversation (`target`).
        //
        // Best-effort: a failed append logs a warning but does not
        // fail the call — the JSONL write already succeeded.
        if appended {
            if let Some(store) = self.principal_manager.chat_log_store() {
                if let Some(principal) = crate::daemon::cron_engine::resolve_principal(
                    &self.principal_manager,
                    &peko_subject::PrincipalId(principal_id.to_string()),
                )
                .await
                {
                    let did = principal.did().await;
                    let entry = peko_chat_log::ChatLogMessage::new(
                        peko_subject::Subject::Principal(did.clone()),
                        note.to_string(),
                        None,
                    );
                    let key = peko_chat_log::ChatThreadKey::new(did, target.clone());
                    if let Err(e) = store.append_message(&key, &entry).await {
                        let source_dbg = format!("{source:?}").to_lowercase();
                        tracing::warn!(
                            principal_id,
                            peer = %target,
                            source = %source_dbg,
                            error = %e,
                            "chat-log append (peer note) failed; note was persisted to session JSONL but not to chat-log"
                        );
                    }
                }
            }
        }

        // Principal-view notification: the principal's own session
        // JSONL gets a structured `[notify] 📨 <caller_label> sent to
        // <target>: <note>` line so the principal sees what was sent
        // on its behalf in its next turn. This is NOT in the chat-log
        // (consumer-facing `peko log`); only the session JSONL
        // (engine context). Skipped when no caller_label is supplied
        // (legacy callers) or when the principal's `root:{owner}`
        // session does not exist yet.
        //
        // Best-effort: a failed append logs a warning but does not
        // fail the call.
        if appended {
            if let Some(label) = caller_label {
                let owner = target.clone();
                let root_session_id = crate::principal::routers::root::root_session_id(&owner);
                let mut session_manager = peko_session::manager::SessionManager::new()
                    .with_sessions_dir_internal(sessions_dir.clone());
                if let Ok(Some(handle)) = session_manager.open_session(&root_session_id).await
                {
                    let notification = format!(
                        "[notify] 📨 {label} sent to {target}: {note}"
                    );
                    if let Err(e) = handle
                        .add_user_with_source(&notification, MessageSource::Agent)
                        .await
                    {
                        let source_dbg = format!("{source:?}").to_lowercase();
                        tracing::warn!(
                            principal_id,
                            peer = %target,
                            source = %source_dbg,
                            error = %e,
                            "principal-view notification append failed; note was delivered to peer but not to principal's session"
                        );
                    }
                }
            }
        }

        Ok(appended)
    }

    async fn originating_peer(
        &self,
        principal_id: &str,
        session_id: &str,
    ) -> Result<Option<Subject>> {
        // Store-free forms first (root keys, v2 keys, subagent keys).
        if let Some(peer) = peer_from_session_key(session_id) {
            return Ok(Some(peer));
        }
        // Spawn-overlay base session (`principal:spawn_<uuid>` peer
        // carries no originator): walk the persisted
        // `parent_session_id` linkage stamped at spawn time.
        let sessions_dir = self.sessions_dir_for(principal_id).await?;
        let session_manager = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir);
        let mut current = session_id.to_string();
        for _ in 0..MAX_PEER_WALK_DEPTH {
            let Ok(meta) = session_manager.get_session_metadata(&current).await else {
                return Ok(None);
            };
            let Some(parent) = meta.parent_session_id else {
                return Ok(None);
            };
            if let Some(peer) = peer_from_session_key(&parent) {
                return Ok(Some(peer));
            }
            current = parent;
        }
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Global registry (mirrors `peko_cron::tools::set_global_runtime`)
// ---------------------------------------------------------------------------

static GLOBAL_MESSENGER: std::sync::OnceLock<Arc<dyn PeerMessenger>> = std::sync::OnceLock::new();

/// Install the process-wide messenger. Called once by the daemon at
/// startup; later calls are silently ignored (same semantics as the
/// cron runtime port).
pub fn set_global_messenger(messenger: Arc<dyn PeerMessenger>) {
    let _ = GLOBAL_MESSENGER.set(messenger);
}

/// The installed messenger, if the daemon has started.
#[must_use]
pub fn global_messenger() -> Option<Arc<dyn PeerMessenger>> {
    GLOBAL_MESSENGER.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_forms_resolve_to_subject() {
        assert_eq!(
            peer_from_session_key("root:user:local"),
            Some(Subject::User("local".to_string()))
        );
        assert_eq!(
            peer_from_session_key("root:cron:user:local"),
            Some(Subject::User("local".to_string()))
        );
    }

    #[test]
    fn v2_key_resolves_peer() {
        assert_eq!(
            peer_from_session_key("agent:root:peer:user:alice"),
            Some(Subject::User("alice".to_string()))
        );
    }

    #[test]
    fn subagent_suffix_strips_to_parent_peer() {
        assert_eq!(
            peer_from_session_key("agent:root:peer:user:alice:subagent:uuid-1"),
            Some(Subject::User("alice".to_string()))
        );
    }

    #[test]
    fn spawn_pseudo_peer_is_not_an_originator() {
        // The child base session's placeholder peer must NOT be
        // accepted — the originator is only reachable via the parent
        // linkage walk (needs the session store; covered by the
        // `originating_peer` integration path).
        assert_eq!(peer_from_session_key("root:principal:spawn_abc123"), None);
    }

    #[test]
    fn garbage_resolves_to_none() {
        assert_eq!(peer_from_session_key("some:random:key"), None);
        assert_eq!(peer_from_session_key(""), None);
    }
}
