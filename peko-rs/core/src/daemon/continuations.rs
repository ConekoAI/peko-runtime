//! Continuations of parked runs (ADR-089 D3, D4).
//!
//! A run hit by a disruption parks (`principal::parking`). This task runs
//! each parked session on when its next attempt comes: from its saved
//! history, with no new user message, and posts the reply to the
//! session's conversation like any reply. Whether it went through, parked
//! again, or failed is settled by the turn itself
//! (`PeerChildTurns::settle`).
//!
//! While a lane (a model) is down, one of its sessions tries at a time —
//! the probe. When the probe goes through, the lane counts as back for a
//! while and the others continue without waiting on each other.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, info, warn};

use crate::agents::subagent_executor::{AgenticEventSink, PeerTurnSurface};
use crate::principal::manager::PrincipalManager;
use crate::principal::parking::ParkedRun;
use crate::principal::run_failure::PEKO_LIMIT;
use crate::principal::Principal;

/// Look again at least this often, so a limit the owner lifted or a
/// park written by another path is noticed.
const MAX_SLEEP: Duration = Duration::from_secs(30);
const MIN_SLEEP: Duration = Duration::from_secs(1);
/// After a probe goes through, the lane counts as back this long: its
/// other sessions continue in parallel instead of one probe at a time.
const RECOVERED_FOR: Duration = Duration::from_mins(10);

/// A lane, per principal: the peko's own limit is one lane for all its
/// models.
type LaneKey = (String, String);

#[derive(Default)]
struct State {
    /// Sessions whose continuation is running.
    running: HashSet<String>,
    /// Lanes whose probe is running.
    probing: HashSet<LaneKey>,
    /// Lanes whose probe went through, and when.
    recovered: HashMap<LaneKey, std::time::Instant>,
}

/// Start the continuation task. Called once from `Daemon::run`.
pub(crate) fn start(
    principal_manager: Arc<PrincipalManager>,
    inbox_registry: Arc<peko_session::InboxRegistry>,
    channel_port: Arc<dyn peko_channel::ChannelPort>,
) -> tokio::task::JoinHandle<()> {
    let state = Arc::new(Mutex::new(State::default()));
    tokio::spawn(async move {
        loop {
            let sleep = tick(&principal_manager, &inbox_registry, &channel_port, &state).await;
            tokio::time::sleep(sleep).await;
        }
    })
}

/// Start what is due; return how long to sleep.
async fn tick(
    pm: &Arc<PrincipalManager>,
    inbox: &Arc<peko_session::InboxRegistry>,
    channel_port: &Arc<dyn peko_channel::ChannelPort>,
    state: &Arc<Mutex<State>>,
) -> Duration {
    let now = Utc::now();
    let mut next = now + chrono::Duration::from_std(MAX_SLEEP).unwrap_or_default();
    for principal in pm.list_all().await {
        // The limit reset, or the owner raised it: its parks continue now.
        principal.quota_meter.advance_if_needed(now);
        if principal.quota_meter.check().is_none() {
            principal.parking.limit_lifted(now);
        }
        for run in pick(&principal, state, now) {
            spawn_continuation(pm, inbox, channel_port, state, &principal, run);
        }
        if let Some(due) = principal.parking.next_due() {
            next = next.min(due);
        }
    }
    (next - Utc::now())
        .to_std()
        .unwrap_or(MIN_SLEEP)
        .clamp(MIN_SLEEP, MAX_SLEEP)
}

fn lane_key(principal: &Principal, run: &ParkedRun) -> LaneKey {
    let lane = if run.kind == PEKO_LIMIT {
        PEKO_LIMIT.to_string()
    } else {
        run.lane.clone()
    };
    (principal.id.0.clone(), lane)
}

/// The due runs to continue now: per lane, every due run when the lane
/// just came back, else one probe at a time.
fn pick(
    principal: &Principal,
    state: &Arc<Mutex<State>>,
    now: chrono::DateTime<Utc>,
) -> Vec<ParkedRun> {
    let mut state = state.lock().expect("continuations lock poisoned");
    state.recovered.retain(|_, at| at.elapsed() < RECOVERED_FOR);
    let mut picked = Vec::new();
    let mut probing_now: HashSet<LaneKey> = HashSet::new();
    for run in principal.parking.due(now) {
        if state.running.contains(&run.session_id) {
            continue;
        }
        let lane = lane_key(principal, &run);
        if state.recovered.contains_key(&lane) {
            picked.push(run);
            continue;
        }
        if state.probing.contains(&lane) || probing_now.contains(&lane) {
            continue;
        }
        probing_now.insert(lane);
        picked.push(run);
    }
    for run in &picked {
        state.running.insert(run.session_id.clone());
    }
    state.probing.extend(probing_now);
    picked
}

fn spawn_continuation(
    pm: &Arc<PrincipalManager>,
    inbox: &Arc<peko_session::InboxRegistry>,
    channel_port: &Arc<dyn peko_channel::ChannelPort>,
    state: &Arc<Mutex<State>>,
    principal: &Arc<Principal>,
    run: ParkedRun,
) {
    let (pm, inbox, channel_port, state, principal) = (
        Arc::clone(pm),
        Arc::clone(inbox),
        Arc::clone(channel_port),
        Arc::clone(state),
        Arc::clone(principal),
    );
    tokio::spawn(async move {
        let lane = lane_key(&principal, &run);
        let went_through = continue_run(&pm, &inbox, &channel_port, &principal, &run).await;
        let mut state = state.lock().expect("continuations lock poisoned");
        state.running.remove(&run.session_id);
        let was_probe = state.probing.remove(&lane);
        match went_through {
            Some(true) if was_probe => {
                state.recovered.insert(lane, std::time::Instant::now());
            }
            Some(false) => {
                state.recovered.remove(&lane);
            }
            _ => {}
        }
    });
}

/// Run one parked session on. `Some(true)` when it went through,
/// `Some(false)` when it didn't, `None` when it could not start (a run
/// is already going in the session — that run settles it).
async fn continue_run(
    pm: &Arc<PrincipalManager>,
    inbox: &Arc<peko_session::InboxRegistry>,
    channel_port: &Arc<dyn peko_channel::ChannelPort>,
    principal: &Arc<Principal>,
    run: &ParkedRun,
) -> Option<bool> {
    let session = run.session_id.as_str();
    let Some(_permit) = inbox.try_acquire_run(session).await else {
        debug!(
            session,
            "continuation: a run is going in the session; it settles the park"
        );
        return None;
    };
    let turns = match pm.peer_child_turns_for(principal).await {
        Ok(turns) => turns,
        Err(e) => {
            warn!(
                session,
                "continuation: no turn driver ({e:#}); trying again later"
            );
            return Some(false);
        }
    };
    info!(
        principal = %principal.id,
        session,
        kind = %run.kind,
        attempt = run.attempts + 1,
        "continuing a parked run"
    );
    // ADR-078: a continuation resumes the session's own work, under the
    // session's natural stamp — as a completion wake does.
    let acting_for = turns
        .root_stamp(None, session, peko_tools_core::Origin::Wake)
        .await;
    let sink: AgenticEventSink = Arc::new(|_| {});
    // An empty message continues from the saved history (ADR-089 D3).
    match turns
        .drive_turn_streaming(session, "", sink, None, None, acting_for)
        .await
    {
        Ok(outcome) => {
            let surface = crate::principal::child_turns::PeerTurnSurfaceImpl::new(
                Arc::clone(turns.session_manager()),
                Arc::clone(channel_port),
                principal.id.clone(),
            );
            if let Some(channel) = surface.conversation_channel(session).await {
                surface.post_reply(&channel, &outcome.final_text).await;
            }
            info!(principal = %principal.id, session, "parked run continued and went through");
            Some(true)
        }
        Err(e) => {
            debug!(session, "continuation did not go through: {e:#}");
            Some(false)
        }
    }
}
