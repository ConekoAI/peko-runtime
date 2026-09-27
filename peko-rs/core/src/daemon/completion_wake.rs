//! Completion-driven wake — the daemon side of the async delivery gap
//! (ADR-063 (delivery gap), 2026-09-27).
//!
//! A terminal async task pushes its outcome into the parent session's
//! inbox. While a run is in flight that suffices (the loop drains at
//! the next iteration), but an IDLE session used to get nothing: the
//! completion waited for the next unrelated user message — "background"
//! was effectively "fire and forget".
//!
//! The executor (framework layer, no principal/session machinery) fires
//! the process-global wake hook after each delivered completion; this
//! module is the handler the daemon installs. It mirrors the steering
//! successor path (`ipc::handlers::principal::run_steering_successor`):
//!
//! 1. `try_acquire_run` on the target session — `None` means a run is
//!    in flight and will drain the inbox itself, so there is nothing
//!    to do. Holding the permit for the whole chain serializes us
//!    against ingress turns and other wake handlers for this session.
//! 2. Resolve the owning principal from the task's stamped
//!    `principal_id`; build the shared `PeerChildTurns` bundle (the
//!    same recipe cron's origin-session driver uses).
//! 3. Drive a wake turn whose first iteration drains the inbox — the
//!    completion is injected by the loop's existing synthesis, the
//!    marker text only explains the turn in the transcript. Chain while
//!    items remain (a wake turn can itself complete more tasks),
//!    bounded so a self-perpetuating chain cannot spin forever.
//! 4. If the session is peer-bound, post the reply to the peer's DM
//!    channel — the same projection the cron Send driver performs.

use std::sync::Arc;

use tracing::{debug, warn};

use crate::agents::subagent_executor::{AgenticEventSink, PeerTurnSurface};
use crate::extensions::framework::async_exec::executor::wake::{
    install_completion_wake_handler, CompletionWakeNotice, WAKE_TURN_MARKER,
};
use crate::principal::manager::PrincipalManager;

/// Bound on chained wake turns driven per notice. Each turn drains the
/// inbox at its first iteration, so a chain longer than this means the
/// turns themselves keep producing completions — stop and let the next
/// push's wake notice re-enter here instead of spinning.
const MAX_WAKE_CHAIN: u8 = 4;

/// The daemon-side context the wake handler needs, as `Arc` clones of
/// the `AppState` components (the daemon's `AppState` is not itself
/// behind an `Arc`).
struct WakeContext {
    principal_manager: Arc<PrincipalManager>,
    inbox_registry: Arc<peko_session::InboxRegistry>,
    observability: Arc<peko_observability::Observability>,
    channel_port: Arc<dyn peko_channel::ChannelPort>,
}

/// Install the process-global completion wake handler. Called once from
/// `Daemon::run` after `AppState` exists. Re-installing replaces the
/// previous handler (in-process daemon restart, tests).
pub(crate) fn install_completion_wake(
    principal_manager: Arc<PrincipalManager>,
    inbox_registry: Arc<peko_session::InboxRegistry>,
    observability: Arc<peko_observability::Observability>,
    channel_port: Arc<dyn peko_channel::ChannelPort>,
) {
    let ctx = Arc::new(WakeContext {
        principal_manager,
        inbox_registry,
        observability,
        channel_port,
    });
    install_completion_wake_handler(Arc::new(move |notice| {
        // The hook must not block the executor's spawned task — the
        // handler does an LLM turn's worth of work.
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            handle_completion_wake(ctx, notice).await;
        });
    }));
}

async fn handle_completion_wake(ctx: Arc<WakeContext>, notice: CompletionWakeNotice) {
    let session_key = notice.session_key.trim().to_string();
    if session_key.is_empty() || session_key == "unknown" {
        debug!(
            task = %notice.task_id,
            "completion wake: unattributed session key; leaving the event queued"
        );
        return;
    }

    // Gate on the run permit: a session with a run in flight drains
    // the inbox at its next iteration — no wake needed. The permit is
    // also the mutual-exclusion token against concurrent wake handlers
    // and post-run successor chains for this session.
    let Some(_permit) = ctx.inbox_registry.try_acquire_run(&session_key).await else {
        debug!(
            session = %session_key,
            task = %notice.task_id,
            "completion wake: run in flight; loop drains the inbox"
        );
        return;
    };

    let Some(principal_id) = notice.principal_id.clone() else {
        debug!(
            session = %session_key,
            task = %notice.task_id,
            "completion wake: task has no principal stamp; leaving the event queued"
        );
        return;
    };
    let pm = &ctx.principal_manager;
    let Some(principal) = pm
        .get(peko_subject::PrincipalId(principal_id.clone()))
        .await
    else {
        warn!(
            session = %session_key,
            task = %notice.task_id,
            principal = %principal_id,
            "completion wake: principal not loaded; leaving the event queued"
        );
        return;
    };
    let Some(resolver) = pm.llm_resolver() else {
        warn!(
            session = %session_key,
            "completion wake: no LLM resolver bound; leaving the event queued"
        );
        return;
    };
    let turns = match crate::principal::child_turns::PeerChildTurns::build(
        &principal,
        &resolver,
        Arc::clone(&ctx.observability),
        Some(pm.shared_inbox_registry()),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            warn!(
                session = %session_key,
                "completion wake: cannot build turn driver ({e:#}); leaving the event queued"
            );
            return;
        }
    };

    let mut chained = 0u8;
    loop {
        let pending = match ctx.inbox_registry.peek_inbox(&session_key).await {
            Some(inbox) => !inbox.is_empty().await,
            None => false,
        };
        if !pending || chained >= MAX_WAKE_CHAIN {
            break;
        }
        chained += 1;
        let sink: AgenticEventSink = Arc::new(|_| {});
        match turns
            .drive_turn_streaming(&session_key, WAKE_TURN_MARKER, sink, None, None)
            .await
        {
            Ok(outcome) => {
                // Peer-bound sessions (e.g. `/local-user`) get the reply
                // posted to the DM channel — the same projection cron's
                // origin-session driver performs (`PeerChildTurns`' own
                // executor deliberately carries no peer surface).
                let surface = crate::principal::child_turns::PeerTurnSurfaceImpl::new(
                    Arc::clone(turns.session_manager()),
                    Arc::clone(&ctx.channel_port),
                    principal.id.clone(),
                );
                if let Some((_peer, channel)) = surface.peer_surface(&session_key).await {
                    surface.post_reply(&channel, &outcome.final_text).await;
                }
            }
            Err(e) => {
                // e.g. the session was deleted underneath us, or the
                // trunk refused a resume. The items stay queued for the
                // next ingress — log and stop chaining.
                warn!(
                    session = %session_key,
                    task = %notice.task_id,
                    "completion wake: wake turn failed ({e:#}); leaving the inbox queued"
                );
                break;
            }
        }
    }
}
