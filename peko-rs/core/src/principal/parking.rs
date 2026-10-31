//! Parked runs (ADR-089 D2–D4).
//!
//! A run hit by a disruption — the peko's own spending limit, the
//! provider's quota, the provider being unreachable — parks instead of
//! failing. The session keeps what the run already saved; this store
//! keeps why it stopped and when it tries again, in
//! `<local>/parked.json`, so a restart picks it up. The daemon's
//! continuation scheduler (`daemon::continuations`) runs each due
//! session on from its saved history.
//!
//! Provider waits are per **lane** — the model a turn runs — not per
//! session: while a lane is down its sessions wait together, a failed
//! attempt grows the lane's wait once, and a success lets the others
//! continue. The peko's own limit is principal-wide: it parks every
//! session until the meter resets (or the owner lifts it).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::common::disruption::Disrupted;
use crate::common::disruption::{next_attempt, PEKO_LIMIT, PROVIDER_QUOTA};

/// When a lane comes back, its waiting sessions continue this far apart.
const STAGGER: Duration = Duration::from_secs(3);
/// A message to a waiting session brings its next attempt forward, but
/// no more often than this.
pub const BRING_FORWARD_GAP: Duration = Duration::from_mins(1);

/// One parked session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParkedRun {
    pub session_id: String,
    /// The conversation (DM or group channel id) the session runs, if any
    /// — where the continuation's reply is posted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// The model the run uses (catalog id).
    pub lane: String,
    /// `peko_limit`, `provider_quota`, or `provider_unreachable`.
    pub kind: String,
    pub since: DateTime<Utc>,
    /// Failed attempts so far (the first disruption is 1).
    pub attempts: u32,
    pub next_attempt_at: DateTime<Utc>,
    /// The last attempt a message brought forward ([`BRING_FORWARD_GAP`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brought_forward_at: Option<DateTime<Utc>>,
    /// When the last attempt failed and (re)parked the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_at: Option<DateTime<Utc>>,
    /// The last error, for status surfaces.
    pub message: String,
}

impl ParkedRun {
    fn waits_on(&self, lane: &str) -> bool {
        self.kind == PEKO_LIMIT || self.lane == lane
    }
}

/// A principal's parked runs, by session id.
#[derive(Debug)]
pub struct Parking {
    path: Option<PathBuf>,
    runs: Mutex<BTreeMap<String, ParkedRun>>,
}

impl Parking {
    /// Load `path` (absent or unreadable: nothing parked).
    #[must_use]
    pub fn load(path: PathBuf) -> Self {
        let runs = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<ParkedRun>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|run| (run.session_id.clone(), run))
            .collect();
        Self {
            path: Some(path),
            runs: Mutex::new(runs),
        }
    }

    /// Not saved anywhere (tests, standalone principals).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            path: None,
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut BTreeMap<String, ParkedRun>) -> (T, bool)) -> T {
        let mut runs = self.runs.lock().expect("parking lock poisoned");
        let (out, changed) = f(&mut runs);
        if changed {
            self.save(&runs);
        }
        out
    }

    fn save(&self, runs: &BTreeMap<String, ParkedRun>) {
        let Some(path) = &self.path else { return };
        let list: Vec<&ParkedRun> = runs.values().collect();
        let result = serde_json::to_vec_pretty(&list)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, bytes)?;
                std::fs::rename(&tmp, path)
            });
        if let Err(e) = result {
            tracing::warn!(path = %path.display(), "parked runs not saved: {e}");
        }
    }

    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<ParkedRun> {
        self.with(|runs| (runs.get(session_id).cloned(), false))
    }

    #[must_use]
    pub fn all(&self) -> Vec<ParkedRun> {
        self.with(|runs| (runs.values().cloned().collect(), false))
    }

    /// The parked run of a conversation, if any.
    #[must_use]
    pub fn for_conversation(&self, channel: &str) -> Option<ParkedRun> {
        self.with(|runs| {
            (
                runs.values()
                    .find(|run| run.conversation.as_deref() == Some(channel))
                    .cloned(),
                false,
            )
        })
    }

    /// Park `session_id` (again). A failed attempt grows the wait once
    /// for the whole lane: every session waiting on it moves to the new
    /// time. Returns the run as parked.
    pub fn park(
        &self,
        session_id: &str,
        conversation: Option<String>,
        lane: &str,
        disrupted: Disrupted,
        now: DateTime<Utc>,
    ) -> ParkedRun {
        self.with(|runs| {
            let prior = runs.get(session_id).cloned();
            // The lane's attempts so far: a session joining a lane that is
            // already down shares its wait instead of starting over.
            let lane_attempts = runs
                .values()
                .filter(|run| run.kind == disrupted.kind && run.waits_on(lane))
                .map(|run| run.attempts)
                .max()
                .unwrap_or(0);
            let attempts = prior.as_ref().map_or(0, |p| p.attempts).max(lane_attempts) + 1;
            let next = next_attempt(&disrupted, attempts, now);
            let run = ParkedRun {
                session_id: session_id.to_string(),
                conversation: conversation
                    .or_else(|| prior.as_ref().and_then(|p| p.conversation.clone())),
                lane: lane.to_string(),
                kind: disrupted.kind.to_string(),
                since: prior.as_ref().map_or(now, |p| p.since),
                attempts,
                next_attempt_at: next,
                brought_forward_at: prior.and_then(|p| p.brought_forward_at),
                parked_at: Some(now),
                message: disrupted.message.clone(),
            };
            if disrupted.kind != PEKO_LIMIT {
                for other in runs.values_mut() {
                    if other.session_id != session_id
                        && other.kind == disrupted.kind
                        && other.lane == lane
                    {
                        other.attempts = attempts;
                        other.next_attempt_at = next;
                    }
                }
            }
            runs.insert(session_id.to_string(), run.clone());
            (run, true)
        })
    }

    /// `session_id` arrived while `waiting` holds its lane: it waits
    /// with it — same kind, attempts, and next attempt — without
    /// growing the lane's wait (no attempt was made).
    pub fn join(
        &self,
        session_id: &str,
        conversation: Option<String>,
        waiting: &ParkedRun,
    ) -> ParkedRun {
        self.with(|runs| {
            if let Some(existing) = runs.get(session_id) {
                return (existing.clone(), false);
            }
            let run = ParkedRun {
                session_id: session_id.to_string(),
                conversation,
                brought_forward_at: None,
                ..waiting.clone()
            };
            runs.insert(session_id.to_string(), run.clone());
            (run, true)
        })
    }

    /// The session's run went through: it is no longer parked.
    pub fn unpark(&self, session_id: &str) -> Option<ParkedRun> {
        self.with(|runs| {
            let removed = runs.remove(session_id);
            let changed = removed.is_some();
            (removed, changed)
        })
    }

    /// A run on `lane` went through: the sessions waiting on it continue
    /// now, a few seconds apart. Peko-limit parks wait for their reset.
    pub fn lane_recovered(&self, lane: &str, now: DateTime<Utc>) {
        self.with(|runs| {
            let mut changed = false;
            let mut i = 0u32;
            for run in runs.values_mut() {
                if run.lane == lane && run.kind != PEKO_LIMIT {
                    run.next_attempt_at =
                        now + chrono::Duration::from_std(STAGGER * i).unwrap_or_default();
                    i += 1;
                    changed = true;
                }
            }
            ((), changed)
        })
    }

    /// The peko's limit no longer holds (it reset, or the owner raised
    /// it): its peko-limit parks continue now.
    pub fn limit_lifted(&self, now: DateTime<Utc>) {
        self.with(|runs| {
            let mut changed = false;
            for run in runs.values_mut() {
                if run.kind == PEKO_LIMIT && run.next_attempt_at > now {
                    run.next_attempt_at = now;
                    changed = true;
                }
            }
            ((), changed)
        })
    }

    /// Whether a turn on `lane` should wait instead of calling the model:
    /// the peko's limit is parked, or another session waits on the lane
    /// and its next attempt is not due. Returns that parked run.
    #[must_use]
    pub fn waiting_on(&self, lane: &str, now: DateTime<Utc>) -> Option<ParkedRun> {
        self.with(|runs| {
            (
                runs.values()
                    .filter(|run| run.waits_on(lane) && run.next_attempt_at > now)
                    .min_by_key(|run| run.next_attempt_at)
                    .cloned(),
                false,
            )
        })
    }

    /// A message arrived for a waiting session: may it bring the next
    /// attempt forward now? At most once per [`BRING_FORWARD_GAP`], and
    /// never past the peko's own limit. Records the attempt when it may.
    pub fn bring_forward(&self, session_id: &str, now: DateTime<Utc>) -> bool {
        self.with(|runs| {
            let Some(run) = runs.get_mut(session_id) else {
                return (true, false);
            };
            if run.kind == PEKO_LIMIT {
                return (false, false);
            }
            // Counted from the last attempt, whether a message brought it
            // or it just failed: a message right after one waits.
            let gap = chrono::Duration::from_std(BRING_FORWARD_GAP).unwrap_or_default();
            let last = run.brought_forward_at.max(run.parked_at);
            if last.is_some_and(|at| now - at < gap) {
                return (false, false);
            }
            run.brought_forward_at = Some(now);
            (true, true)
        })
    }

    /// Parked runs whose next attempt is due, oldest first.
    #[must_use]
    pub fn due(&self, now: DateTime<Utc>) -> Vec<ParkedRun> {
        self.with(|runs| {
            let mut due: Vec<ParkedRun> = runs
                .values()
                .filter(|run| run.next_attempt_at <= now)
                .cloned()
                .collect();
            due.sort_by_key(|run| run.next_attempt_at);
            (due, false)
        })
    }

    /// The earliest next attempt, if anything is parked.
    #[must_use]
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.with(|runs| (runs.values().map(|run| run.next_attempt_at).min(), false))
    }
}

/// What a sender is told when their message waits with a parked run:
/// transport UX, never a conversation row (ADR-089 D5).
#[must_use]
pub fn waiting_notice(peko: &str, run: &ParkedRun) -> String {
    let why = match run.kind.as_str() {
        PEKO_LIMIT => "its spending limit is reached",
        PROVIDER_QUOTA => "its model's quota is used up",
        _ => "its model can't be reached",
    };
    format!(
        "{peko} is waiting: {why}. Your message is saved and will be answered when it \
         continues (next try {}).",
        run.next_attempt_at.format("%Y-%m-%d %H:%M UTC")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::disruption::PROVIDER_UNREACHABLE;

    fn unreachable() -> Disrupted {
        Disrupted {
            kind: PROVIDER_UNREACHABLE,
            message: "error sending request".into(),
            resets_at: None,
        }
    }

    fn secs(from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
        (to - from).num_seconds()
    }

    #[test]
    fn a_lane_waits_as_one() {
        let parking = Parking::in_memory();
        let now = Utc::now();
        let a = parking.park("a", Some("chan_a".into()), "mimo", unreachable(), now);
        assert_eq!(a.attempts, 1);
        // A second session on the lane joins its wait, it doesn't restart it.
        let b = parking.park("b", None, "mimo", unreachable(), now);
        assert_eq!(b.attempts, 2);
        assert_eq!(parking.get("a").unwrap().next_attempt_at, b.next_attempt_at);
        // A newcomer joins the wait without growing it.
        let c = parking.join("c", None, &parking.get("a").unwrap());
        assert_eq!(
            (c.attempts, c.next_attempt_at),
            (b.attempts, b.next_attempt_at)
        );
        // Another lane is not held up.
        assert!(parking.waiting_on("other", now).is_none());
        assert!(parking.waiting_on("mimo", now).is_some());
        assert_eq!(parking.for_conversation("chan_a").unwrap().session_id, "a");

        // The probe went through: the rest continue, a few seconds apart.
        parking.unpark("a");
        parking.lane_recovered("mimo", now);
        assert!(secs(now, parking.get("b").unwrap().next_attempt_at) <= 0);
        assert!(secs(now, parking.get("c").unwrap().next_attempt_at) <= 3);
    }

    #[test]
    fn the_peko_limit_waits_for_its_reset_and_holds_every_lane() {
        let parking = Parking::in_memory();
        let now = Utc::now();
        let resets = now + chrono::Duration::hours(5);
        let run = parking.park(
            "a",
            None,
            "mimo",
            Disrupted {
                kind: PEKO_LIMIT,
                message: "USD budget exceeded".into(),
                resets_at: Some(resets),
            },
            now,
        );
        assert_eq!(run.next_attempt_at, resets);
        assert!(parking.waiting_on("any-model", now).is_some());
        // A message doesn't bring the limit forward.
        assert!(!parking.bring_forward("a", now));
        // The owner lifted it: continue now.
        parking.limit_lifted(now);
        assert_eq!(parking.due(now).len(), 1);
    }

    #[test]
    fn a_message_brings_the_next_attempt_forward_once_a_minute() {
        let parking = Parking::in_memory();
        let now = Utc::now();
        parking.park("a", None, "mimo", unreachable(), now);
        // Just after an attempt failed, a message waits.
        assert!(!parking.bring_forward("a", now + chrono::Duration::seconds(10)));
        let now = now + chrono::Duration::seconds(61);
        assert!(parking.bring_forward("a", now));
        assert!(!parking.bring_forward("a", now + chrono::Duration::seconds(30)));
        assert!(parking.bring_forward("a", now + chrono::Duration::seconds(61)));
    }

    #[test]
    fn parked_runs_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parked.json");
        let now = Utc::now();
        Parking::load(path.clone()).park("a", Some("chan_a".into()), "mimo", unreachable(), now);
        let reloaded = Parking::load(path);
        let run = reloaded.get("a").unwrap();
        assert_eq!(run.conversation.as_deref(), Some("chan_a"));
        assert_eq!(run.kind, PROVIDER_UNREACHABLE);
        reloaded.unpark("a");
        assert!(reloaded.all().is_empty());
    }
}
