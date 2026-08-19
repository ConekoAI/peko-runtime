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
/// it lives only in the session JSONL (engine context), not on the
/// peer-facing DM channel. The peer view (full note in the child
/// JSONL + the DM channel row `peko log` reads) is unchanged.
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
        let principal = crate::daemon::cron_engine::resolve_principal(
            &self.principal_manager,
            &peko_subject::PrincipalId(principal_id.to_string()),
        )
        .await
        .ok_or_else(|| anyhow::anyhow!("Principal '{principal_id}' not loaded"))?;
        let sessions_dir = principal.memory.sessions_dir().clone();
        let mut session_manager = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        // Phase 7: the peer's conversational session is its standing
        // child of the trunk. FIND-ONLY (see the trait docs): a note
        // never provisions a peer child.
        let metas = session_manager.list_all_sessions(false).await?;
        let child = crate::principal::peer_children::find_peer_child(&metas, target);
        let appended = match &child {
            Some(child_id) => match session_manager.open_session(child_id).await {
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

        // Sprint 3 Phase 12b: the DM channel post replaces the retired
        // chat-log projection. Peer notes (📨 from `send_peer`, ⏰ from
        // cron Send/Notify) are principal-authored root posts on the
        // peer's DM channel — `peko log` reads them there, and the
        // bound `PassiveBindingResponder` self-skips them (author ==
        // the principal's own id). The channel is the durable record,
        // the child JSONL is working memory — both get the note.
        //
        // Find-only posture, best-effort: no channel port (standalone/
        // test contexts) or no provisioned channel → skip with a debug
        // log; a failed post warns but does not fail the call — the
        // JSONL write already succeeded.
        if appended {
            if let (Some(port), Some(child_id)) =
                (self.principal_manager.channel_port(), child.as_ref())
            {
                let binding = metas
                    .iter()
                    .find(|m| &m.session_id == child_id)
                    .and_then(|m| m.slug.clone())
                    .map(|slug| format!("/{slug}"));
                let channel = match binding {
                    Some(binding) => crate::principal::peer_dm::find_peer_dm_channel(
                        &port,
                        &principal.id,
                        &binding,
                    )
                    .await
                    .unwrap_or_else(|e| {
                        tracing::debug!(
                            principal_id,
                            peer = %target,
                            "peer DM channel lookup failed; skipping channel post: {e}"
                        );
                        None
                    }),
                    None => None,
                };
                match channel {
                    Some(channel) => {
                        if let Err(e) = port
                            .post(
                                &channel,
                                &principal.id,
                                peko_channel::PostMsg::root(note),
                            )
                            .await
                        {
                            let source_dbg = format!("{source:?}").to_lowercase();
                            tracing::warn!(
                                principal_id,
                                peer = %target,
                                source = %source_dbg,
                                error = %e,
                                "peer DM channel post (note) failed; note was persisted to session JSONL but not to the channel"
                            );
                        }
                    }
                    None => {
                        tracing::debug!(
                            principal_id,
                            peer = %target,
                            "no DM channel for the peer child; note lives in the session JSONL only"
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
        // turn. This is NOT posted to the peer's DM channel (the
        // consumer-facing conversation record `peko log` reads); it
        // lives only in the trunk session JSONL (engine context).
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

    // --------------------------------------------------------------
    // Phase 12b: deliver_note posts to the peer's DM channel (not the
    // retired chat log) alongside the child-JSONL row and the trunk
    // `[notify]` self-view.
    // --------------------------------------------------------------

    /// A delivered note lands in all three homes — the peer child's
    /// session JSONL (working memory), the peer's DM channel (the
    /// durable record `peko log` reads), and the trunk `[notify]`
    /// self-view — and writes NOTHING to the chat log.
    #[tokio::test(flavor = "multi_thread")]
    // This test mutates process-global state (PEKO_HOME,
    // init_global_core). All such tests share the plain `serial`
    // group — 2026-08-19: the separate `global_core_lock` group ran
    // in parallel with it and raced (PEKO_HOME swapped mid-test).
    #[serial_test::serial]
    async fn deliver_note_lands_on_dm_channel_child_jsonl_and_trunk_notify() {
        use crate::principal::router::{ChannelContext, ChannelKind};
        use peko_channel::{ChannelEvent, ChannelPort, Checkpoint};

        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("PEKO_HOME", tmp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            tmp.path().join("config"),
            tmp.path().join("data"),
            tmp.path().join("cache"),
        );
        let tool_runtime = crate::engine::tool_runtime::ToolRuntime::with_workspace(
            path_resolver.clone(),
            tmp.path(),
        )
        .await
        .expect("tool runtime should initialize");
        crate::extensions::framework::core::init_global_core(
            tool_runtime.extension_core().clone(),
        );

        let catalog_path = tmp.path().join("models.toml");
        let (resolver, adapter) =
            peko_providers::LlmResolver::mock(peko_providers::MockAdapter::new(), catalog_path)
                .await;
        adapter.queue_text("conversational reply");
        adapter.queue_text("trunk reply");

        let store = Arc::new(peko_channel::ChannelStore::new(peko_channel::ChannelConfig {
            runtime_dir: tmp.path().join("runtime"),
            shared_dir: None,
        }));
        let channel_port: Arc<dyn ChannelPort> = store.clone();
        let chat_logs_dir = tmp.path().join("chat_logs");
        let manager = Arc::new(
            crate::principal::PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
                Arc::new(crate::principal::DefaultPrincipalRouterFactory),
                crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
            )
            .with_resolver(resolver)
            .with_channel_port(channel_port.clone())
            .with_chat_log_store(Arc::new(peko_chat_log::ChatLogStore::new(
                chat_logs_dir.clone(),
            ))),
        );

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let agents_dir = workspace.join("boss").join("agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        tokio::fs::write(
            agents_dir.join("primary.md"),
            "---\ndescription: \"Boss\"\n---\n\nYou are boss, a test assistant.\n",
        )
        .await
        .unwrap();
        let owner = Subject::User("local".to_string());
        let principal = manager
            .create(crate::principal::PrincipalConfig {
                name: "boss".to_string(),
                did: None,
                owner: owner.clone(),
                identity: Default::default(),
                intent: Default::default(),
                governance: Default::default(),
                memory: Default::default(),
                routing: Default::default(),
                capabilities: Default::default(),
                exposure: crate::principal::config::Exposure::Public,
                status: None,
                permissions: Vec::new(),
                preferred_model_id: Some("mock".to_string()),
                transport_preference: Default::default(),
                quota: None,
                children: Default::default(),
            })
            .await
            .unwrap();

        // A conversational turn provisions the owner's standing child
        // + DM channel; a trunk turn creates `root:self` so the
        // `[notify]` self-view has somewhere to land.
        manager
            .receive_streaming(
                principal.id.clone(),
                owner.clone(),
                "hi".to_string(),
                ChannelContext {
                    kind: ChannelKind::Cli,
                    streaming: false,
                },
                Box::new(|_| {}),
                None,
            )
            .await
            .expect("conversational turn");
        manager
            .receive_trunk(principal.id.clone(), "self upkeep".to_string(), None)
            .await
            .expect("trunk turn");

        let messenger = PrincipalPeerMessenger::new(manager.clone());
        let delivered = messenger
            .deliver_note(
                &principal.id.0,
                &owner,
                "📨 [root] the report is ready",
                MessageSource::Agent,
                Some("agent some-session"),
            )
            .await
            .expect("deliver_note");
        assert!(delivered, "the owner has a conversational session");

        // 1. Child JSONL holds the note.
        let sessions_dir = principal.memory.sessions_dir().clone();
        let mut sm = peko_session::manager::SessionManager::new()
            .with_sessions_dir_internal(sessions_dir.clone());
        let metas = sm.list_all_sessions(false).await.unwrap();
        let child_id = crate::principal::peer_children::find_peer_child(&metas, &owner)
            .expect("owner child exists");
        let child_jsonl = std::fs::read_to_string(sessions_dir.join(format!("{child_id}.jsonl")))
            .expect("child JSONL");
        assert!(
            child_jsonl.contains("📨 [root] the report is ready"),
            "note must land in the child JSONL: {child_jsonl}"
        );

        // 2. The DM channel holds the note as a principal-authored
        //    root post (self-skipped by the responder).
        let channel = crate::principal::peer_dm::find_peer_dm_channel(
            &channel_port,
            &principal.id,
            "/local-user",
        )
        .await
        .expect("dm lookup")
        .expect("owner DM channel exists");
        let events = channel_port
            .peek(&channel, &Checkpoint::default())
            .await
            .expect("peek");
        assert!(
            events.iter().any(|ev| matches!(
                ev,
                ChannelEvent::Posted { author, parent: None, text, .. }
                    if *author == principal.id.0 && text == "📨 [root] the report is ready"
            )),
            "note must land on the DM channel as a principal-authored root post: {events:?}"
        );

        // 3. The trunk carries the `[notify]` self-view line.
        let trunk_jsonl =
            std::fs::read_to_string(sessions_dir.join("root:self.jsonl")).expect("trunk JSONL");
        assert!(
            trunk_jsonl.contains("[notify] 📨 agent some-session sent to user:local"),
            "trunk must carry the notify self-view: {trunk_jsonl}"
        );

        // 4. The chat log is untouched (Phase 12b removed the
        //    chat-log projection).
        let chat_log_entries = std::fs::read_dir(&chat_logs_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(
            chat_log_entries, 0,
            "deliver_note must not write chat-log shards anymore"
        );
    }
}
