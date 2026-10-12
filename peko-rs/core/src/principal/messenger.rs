//! Peer messenger — agent-originated notes into a peer's session
//!
//! The delivery half of the ChannelSend `user:<id>` branch (sprint 4):
//! any
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
use std::sync::Arc;

use crate::principal::manager::PrincipalManager;

/// Maximum parent-linkage hops when resolving the originating peer of
/// a deeply nested subagent session (cycle guard; spawn depth is
/// capped well below this elsewhere).
const MAX_PEER_WALK_DEPTH: usize = 8;

/// Port: deliver a note into a peer's conversational session (its
/// standing child of the trunk), and resolve the originating peer of
/// a run session.
///
/// `deliver_note` takes the note fully formatted — call sites own
/// their label conventions (`⏰ [cron job …]` for the cron engine,
/// `📨 [<agent>]` for the ChannelSend `user:<id>` branch). It returns
/// `Ok(false)` when the
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
/// ChannelSend `user:<id>` call, `"cron job <name>"` for a cron fire),
/// the
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
        let normalized = match self.principal_manager.channel_port() {
            Some(port) => port.actor_info(target).await.subject(),
            None => target.clone(),
        };
        let target = &normalized;
        let port = self
            .principal_manager
            .channel_port()
            .ok_or_else(|| anyhow::anyhow!("Channel store unavailable"))?;
        let child =
            crate::principal::peer_children::find_dm_child(&metas, &principal.id, target, &port)
                .await;
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
        // chat-log projection. Peer notes (📨 from ChannelSend's
        // `user:<id>` branch, ⏰ from
        // cron Send/Notify) are principal-authored root posts on the
        // peer's DM channel — `peko log` reads them there (a DM post
        // never starts a run, ADR-072). The channel is the durable record,
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
                    .find(|m| m.session_id.to_string() == *child_id)
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
                                &Subject::from(&principal.id),
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
        // Walk parent links until a channel binding provides the conversation roster.
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
                match crate::principal::conversation::binding(&meta) {
                    Some(crate::principal::conversation::Conversation::Channel(channel)) => {
                        if let Some(port) = self.principal_manager.channel_port() {
                            if let Some(id) = peko_channel::ChannelId::parse(&channel) {
                                if port.passive_binding(&id).await.ok().flatten().is_some() {
                                    let members = port.participants(&id).await?;
                                    let me = port
                                        .actor_info(&Subject::Principal(principal_id.into()))
                                        .await;
                                    let peers = members
                                        .into_iter()
                                        .filter(|p| p.address != me.address)
                                        .collect::<Vec<_>>();
                                    if peers.len() == 1 {
                                        return Ok(Some(peers[0].subject()));
                                    }
                                }
                            }
                        }
                        return Ok(None);
                    }
                    None => {}
                }
            }
            let Some(parent) = meta.parent_session_id else {
                return Ok(None);
            };
            let parent_str = parent.to_string();
            current = parent_str;
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

    #[tokio::test(flavor = "multi_thread")]
    // This test mutates process-global state (PEKO_HOME,
    // runtime injection). All such tests share the plain `serial`
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

        let catalog_path = tmp.path().join("models.toml");
        let (resolver, adapter) =
            peko_providers::LlmResolver::mock(peko_providers::MockAdapter::new(), catalog_path)
                .await;
        adapter.queue_text("conversational reply");
        adapter.queue_text("trunk reply");

        let store = Arc::new(peko_channel::ChannelStore::new(
            peko_channel::ChannelConfig {
                runtime_dir: tmp.path().join("runtime"),
                shared_dir: None,
            },
        ));
        let channel_port: Arc<dyn ChannelPort> = store.clone();
        let manager = Arc::new(
            crate::principal::PrincipalManager::with_path_resolver(
                path_resolver,
                Arc::new(crate::principal::DefaultPrincipalMemoryFactory),
                Arc::new(crate::principal::DefaultPrincipalRouterFactory),
                crate::async_exec::executor::standalone_inbox_registry(),
            )
            .with_tooling(tool_runtime.tooling().clone())
            .with_resolver(resolver)
            .with_channel_port(channel_port.clone()),
        );

        let workspace = tmp.path().join("principals");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let roles_dir = workspace.join("boss").join("roles");
        tokio::fs::create_dir_all(&roles_dir).await.unwrap();
        tokio::fs::write(
            roles_dir.join("primary.md"),
            "---\ndescription: \"Boss\"\n---\n\nYou are boss, a test assistant.\n",
        )
        .await
        .unwrap();
        let owner = Subject::User("local".to_string());
        let principal = manager
            .create(crate::principal::PrincipalConfig {
                hub_id: None,
                actor_key: None,
                name: "boss".to_string(),
                id: None,
                did: None,
                owner: owner.clone(),
                identity: Default::default(),
                intent: Default::default(),
                governance: Default::default(),
                memory: Default::default(),
                routing: Default::default(),
                exposure: crate::principal::config::Exposure::Public,
                status: None,
                boot_state: None,
                permissions: Vec::new(),
                preferred_model_id: Some("mock".to_string()),
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
        let principal_address = format!("principal:local:{}", principal.id.0);
        assert!(
            events.iter().any(|ev| matches!(
                ev,
                ChannelEvent::Posted { author, parent: None, text, .. }
                    if *author == principal_address && text == "📨 [root] the report is ready"
            )),
            "note must land on the DM channel as a principal-authored root post: {events:?}"
        );

        // 3. The trunk carries the `[notify]` self-view line.
        //    Sprint 6: the trunk is a UUID; look it up by walking for
        //    `parent_session_id == None`.
        let trunk_id = metas
            .iter()
            .find(|m| m.parent_session_id.is_none())
            .map(|m| m.session_id.to_string())
            .expect("trunk exists");
        let trunk_jsonl = std::fs::read_to_string(sessions_dir.join(format!("{trunk_id}.jsonl")))
            .expect("trunk JSONL");
        assert!(
            trunk_jsonl.contains("[notify] 📨 agent some-session sent to user:local"),
            "trunk must carry the notify self-view: {trunk_jsonl}"
        );
        assert_eq!(
            messenger
                .originating_peer(&principal.id.0, &child_id)
                .await
                .unwrap(),
            Some(owner)
        );
        let group_id = crate::principal::peer_children::ensure_group_child(
            &trunk_id,
            "group:team",
            &Arc::new(tokio::sync::RwLock::new(sm)),
        )
        .await
        .unwrap();
        assert_eq!(
            messenger
                .originating_peer(&principal.id.0, &group_id)
                .await
                .unwrap(),
            None,
            "a group conversation has no single originating actor"
        );
    }
}
