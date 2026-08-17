//! Peer messenger — agent-originated notes into a peer's session
//!
//! The delivery half of the `send_peer` unification (2026-08-08): any
//! agent in a run tree (trunk, peer child, subagent, cron-spawned) can
//! surface a message to a human peer without owning the peer's
//! conversational session.
//!
//! Phase 7 (sprint 2, 2026-08-17): the peer's conversational session
//! is its STANDING CHILD of the trunk (provisioned by
//! [`crate::principal::peer_children::ensure_peer_child`] on ingress),
//! not the retired per-peer root session `root:{peer}`. The messenger
//! appends a labeled, source-tagged note to the peer's child; it never
//! triggers a turn — the note waits in the JSONL for the peer's next
//! message (mid-turn interleaves are shape-repaired at the next load
//! by `peko_message::repair`).
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
/// - v2 keys `agent:{agent}:peer:{type}:{id}` (optional overlay
///   suffix),
/// - subagent keys (`…:subagent:{uuid}`, possibly nested) — stripped
///   down to the base key.
///
/// Phase 7: the per-peer root-key forms (`root:{subject}`,
/// `root:cron:{subject}`) are RETIRED and no longer parse — those ids
/// are never created anymore. Peer-child sessions carry opaque ids;
/// their peer is resolved from the stamped `peer_type`/`peer_id`
/// metadata by the store-backed walk in
/// [`PrincipalPeerMessenger::originating_peer`].
///
/// The principal trunk session `root:self` (Phase 3, 2026-08-15) is
/// explicitly NOT a peer key: it has no external peer, so it resolves
/// to `None` — never to a literal peer named "self". Spawn pseudo-peers
/// (`principal:spawn_<uuid>` — the child base session's placeholder
/// peer) are likewise NOT returned: they carry no originator, so
/// callers must continue walking the parent linkage instead.
#[must_use]
pub fn peer_from_session_key(session_id: &str) -> Option<Subject> {
    // Strip any `:subagent:{uuid}` trail first (possibly nested) — a
    // subagent key's peer is always its base key's peer, and stripping
    // first keeps `agent:root:peer:user:alice:subagent:uuid` from
    // misparsing as a user named "alice:subagent:uuid".
    let mut current = session_id;
    while let Some((base, _)) = current.rsplit_once(":subagent:") {
        current = base;
    }
    // The trunk has no external peer. Handled ahead of the v2 parse
    // so `root:self` can never misparse as a peer named "self" even if
    // Subject parsing changes (today `"self"` fails `Subject::from_str`
    // anyway and would coincidentally return None — do not rely on it).
    if current == crate::principal::routers::root::trunk_session_id() {
        return None;
    }
    if let Some(parsed) = peko_session::key::parse_session_key_v2(current) {
        let subject_str = format!("{}:{}", parsed.peer_type, parsed.peer_id);
        return non_spawn_subject(&subject_str);
    }
    None
}

/// Parse a Subject string, rejecting spawn pseudo-peers and declared-
/// standing-child placeholders (neither carries an originator — the
/// caller must keep walking the parent linkage).
fn non_spawn_subject(s: &str) -> Option<Subject> {
    match Subject::from_str(s) {
        Ok(Subject::Principal(id))
            if id.0.starts_with("spawn_") || id.0.starts_with("standing_") =>
        {
            None
        }
        Ok(other) => Some(other),
        Err(_) => None,
    }
}

/// Port: deliver a note into a peer's conversational session (its
/// standing child of the trunk), and resolve the originating peer of
/// a run session.
///
/// `deliver_note` takes the note fully formatted — call sites own
/// their label conventions (`⏰ [cron job …]` for the cron engine,
/// `📨 [<agent>]` for `send_peer`). It returns `Ok(false)` when the
/// peer has no conversational session yet (silent skip: the note has
/// nothing to attach to; the outcome still lives in the caller's own
/// session). **No session is ever created** (Phase 7 spawn-on-contact
/// decision: note delivery is find-only — a note to a peer that has
/// never chatted is dropped rather than provisioning a child with no
/// turn behind it. The owner's child is ensured on the owner's first
/// `peko send`, so owner-targeted notes — cron Send/Notify outcomes —
/// land as soon as the owner has chatted once).
///
/// When `caller_label` is supplied (e.g. `"agent <session_id>"` for a
/// `send_peer` call, `"cron job <name>"` for a cron fire), the
/// messenger ALSO writes a structured `[notify] …` line to the
/// principal's TRUNK session (`root:self` — Phase 7: the principal's
/// self-view lives in the trunk now) so the principal sees what was
/// sent on its behalf in its next turn. The notification is meta-info:
/// it does not appear in the chat-log (consumer-facing `peko log`),
/// only in the session JSONL (engine context). The peer view (full
/// note + chat-log row) is unchanged.
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
        let mut session_manager = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        // Phase 7: the peer's conversational session is its standing
        // child of the trunk. FIND-ONLY (see the trait docs): a note
        // never provisions a peer child.
        let metas = session_manager.list_all_sessions(false).await?;
        let appended = match crate::principal::peer_children::find_peer_child(&metas, target) {
            Some(child_id) => match session_manager.open_session(&child_id).await {
                Ok(Some(handle)) => {
                    handle.add_user_with_source(note, source).await?;
                    true
                }
                // Index entry without a JSONL (shouldn't happen) —
                // treat as missing.
                Ok(None) => false,
                Err(e) => return Err(e),
            },
            // No conversational session yet (the peer has never
            // chatted): nothing to attach the note to.
            None => false,
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

        // Principal-view notification: the principal's TRUNK session
        // JSONL (`root:self` — Phase 7: the principal's self-view
        // lives in the trunk now) gets a structured `[notify] 📨
        // <caller_label> sent to <target>: <note>` line so the
        // principal sees what was sent on its behalf in its next
        // turn. This is NOT in the chat-log (consumer-facing
        // `peko log`); only the session JSONL (engine context).
        // Skipped when no caller_label is supplied (legacy callers)
        // or when the trunk session does not exist yet (no self-turn
        // has run — the line is meta-info, not worth creating the
        // trunk for).
        //
        // Best-effort: a failed append logs a warning but does not
        // fail the call.
        if appended {
            if let Some(label) = caller_label {
                let trunk_id = crate::principal::routers::root::trunk_session_id();
                let mut session_manager = peko_session::manager::SessionManager::new()
                    .with_sessions_dir_internal(sessions_dir.clone());
                if let Ok(Some(handle)) = session_manager.open_session(&trunk_id).await {
                    let notification = format!("[notify] 📨 {label} sent to {target}: {note}");
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
                            "principal-view notification append failed; note was delivered to peer but not to principal's trunk session"
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
        // Store-free forms first (v2 keys, subagent-suffix stripping).
        if let Some(peer) = peer_from_session_key(session_id) {
            return Ok(Some(peer));
        }
        // Phase 7: peer-child sessions carry opaque ids — resolve via
        // the STAMPED `peer_type`/`peer_id` metadata (the stamp is
        // load-bearing: Phase 5 stamps the REAL peer on peer children),
        // walking the persisted `parent_session_id` linkage for spawn
        // overlays and subagent sessions whose placeholder peers
        // (`principal:spawn_*` / `principal:standing_*`) carry no
        // originator. The trunk (`root:self`) is peer-less: its owner
        // stamp is a proxy-subject artifact, never an originator.
        let sessions_dir = self.sessions_dir_for(principal_id).await?;
        let session_manager =
            peko_session::manager::SessionManager::new().with_sessions_dir_internal(sessions_dir);
        let trunk = crate::principal::routers::root::trunk_session_id();
        let mut current = session_id.to_string();
        for _ in 0..MAX_PEER_WALK_DEPTH {
            let Ok(meta) = session_manager.get_session_metadata(&current).await else {
                return Ok(None);
            };
            if current != trunk {
                if let (Some(kind), Some(id)) = (meta.peer_type.as_deref(), meta.peer_id.as_deref())
                {
                    if let Some(peer) = non_spawn_subject(&format!("{kind}:{id}")) {
                        return Ok(Some(peer));
                    }
                }
            }
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
    fn retired_root_forms_do_not_parse() {
        // Phase 7: `root:{peer}` / `root:cron:{peer}` ids are retired —
        // they are never created anymore and no longer parse as peer
        // keys. (Legacy on-disk ids from before the prelaunch break
        // simply resolve to None.)
        assert_eq!(peer_from_session_key("root:user:local"), None);
        assert_eq!(peer_from_session_key("root:cron:user:local"), None);
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
    fn spawn_and_standing_pseudo_peers_are_not_originators() {
        // The spawn placeholder peer must NOT be accepted — the
        // originator is only reachable via the parent-linkage walk
        // (needs the session store; covered by the `originating_peer`
        // integration path). Same for the `standing_*` placeholder
        // `ensure_declared_children` stamps on declared children.
        assert_eq!(
            peer_from_session_key("agent:x:peer:principal:spawn_abc123"),
            None
        );
        assert_eq!(
            non_spawn_subject("principal:standing_memory"),
            None,
            "declared-child placeholder carries no originator"
        );
    }

    #[test]
    fn trunk_session_has_no_peer() {
        // The principal trunk `root:self` (Phase 3) is a self session:
        // it must never misparse as a peer literally named "self".
        // Callers (`originating_peer`) fall through to the parent-linkage
        // walk, which bottoms out at None — a graceful "no originator".
        assert_eq!(peer_from_session_key("root:self"), None);
        assert_eq!(peer_from_session_key("root:self:subagent:uuid-1"), None);
    }

    #[test]
    fn garbage_resolves_to_none() {
        assert_eq!(peer_from_session_key("some:random:key"), None);
        assert_eq!(peer_from_session_key(""), None);
    }
}
