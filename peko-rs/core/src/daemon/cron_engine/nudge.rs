//! ADR-086: a post by another principal brings the recipient's attention
//! keepalive forward.
//!
//! A principal's post never starts a turn (ADR-072 D1); it waits on the
//! recipient's list (D4a) until the recipient's `keepalive` fires. With a
//! 10-minute cadence that was a 10-minute latency between principals. A
//! nudge moves the keepalive's next fire to `last turn + NUDGE_MIN_GAP`
//! (or now), so the fire is still the cron job's and the attention check
//! still decides whether it runs a turn.
//!
//! Damping keeps the bound ADR-072 relies on: turns stay at least
//! [`NUDGE_MIN_GAP`] apart, so posts that arrive inside the gap (sibling
//! principals' replies in a group) share one turn; and at most
//! [`NUDGE_BUDGET_PER_HOUR`] nudges per principal per hour bring a fire
//! forward. Past that the normal cadence takes over.

use std::collections::{HashMap, VecDeque};

use chrono::{DateTime, Duration, Utc};
use peko_cron::{CronJob, FireWhen};
use peko_subject::PrincipalId;
use tracing::{debug, warn};

use super::{resolve_principal, CronEngine};
use crate::principal::genesis::KEEPALIVE_JOB_ID;

/// Shortest time between the keepalive's last turn and a nudged fire.
pub const NUDGE_MIN_GAP: Duration = Duration::minutes(2);

/// Most nudges per principal per rolling hour that bring a fire forward.
pub const NUDGE_BUDGET_PER_HOUR: usize = 6;

/// What a nudge did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NudgeOutcome {
    /// The keepalive now fires at this time.
    Moved(DateTime<Utc>),
    /// A keepalive turn is in flight; its completion brings the next fire
    /// forward.
    Pending,
    /// The keepalive already fires by then.
    AlreadySooner,
    /// The principal has no enabled `when: attention` keepalive.
    NotAttentionGated,
    /// This hour's nudges are spent; the normal cadence applies.
    OverBudget,
}

/// Per-principal nudge state, shared by every clone of the engine. Kept
/// in memory: a restart only resets the hourly budget.
#[derive(Debug, Default)]
pub(super) struct NudgeBook {
    entries: HashMap<PrincipalId, NudgeEntry>,
}

#[derive(Debug, Default)]
struct NudgeEntry {
    spent: VecDeque<DateTime<Utc>>,
    pending: bool,
}

impl NudgeBook {
    /// Spend one of `key`'s nudges at `now`, if this hour has one left.
    fn try_spend(&mut self, key: &PrincipalId, now: DateTime<Utc>) -> bool {
        let entry = self.entries.entry(key.clone()).or_default();
        while entry
            .spent
            .front()
            .is_some_and(|at| now - *at >= Duration::hours(1))
        {
            entry.spent.pop_front();
        }
        if entry.spent.len() >= NUDGE_BUDGET_PER_HOUR {
            return false;
        }
        entry.spent.push_back(now);
        true
    }
}

/// When a nudged keepalive may fire: [`NUDGE_MIN_GAP`] after its last
/// turn, and not before `now`.
#[must_use]
pub fn nudge_target(last_turn: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
    last_turn.map_or(now, |last| (last + NUDGE_MIN_GAP).max(now))
}

/// Whether `job` is a keepalive the attention check gates (ADR-083).
fn attention_gated(job: &CronJob) -> bool {
    job.enabled && crate::principal::genesis::fire_when(job) == FireWhen::Attention
}

impl CronEngine {
    /// The key nudges are booked under: the principal's own id, whichever
    /// spelling (`prin_…` or DID) the caller holds.
    async fn nudge_key(&self, principal: &PrincipalId) -> PrincipalId {
        match &self.principal_manager {
            Some(pm) => resolve_principal(pm, principal)
                .await
                .map_or_else(|| principal.clone(), |p| p.id.clone()),
            None => principal.clone(),
        }
    }

    /// Bring `principal`'s attention keepalive forward because another
    /// principal posted on `channel` (ADR-086).
    pub async fn nudge_attention(
        &self,
        principal: &PrincipalId,
        channel: &str,
    ) -> anyhow::Result<NudgeOutcome> {
        let scheduler = self.scheduler_for(principal).await?;
        let Some(job) = scheduler.get_job(KEEPALIVE_JOB_ID)? else {
            return Ok(NudgeOutcome::NotAttentionGated);
        };
        if !attention_gated(&job) {
            return Ok(NudgeOutcome::NotAttentionGated);
        }
        let key = self.nudge_key(principal).await;
        let now = Utc::now();
        let in_flight = scheduler
            .list_running_runs()?
            .iter()
            .any(|run| run.job_id == job.id);
        if in_flight {
            // Completion rewrites `next_run`, so a move now would be lost.
            let mut book = self.nudges.lock().await;
            if book.entries.get(&key).is_some_and(|e| e.pending) {
                return Ok(NudgeOutcome::Pending);
            }
            if !book.try_spend(&key, now) {
                return Ok(NudgeOutcome::OverBudget);
            }
            book.entries.entry(key).or_default().pending = true;
            return Ok(NudgeOutcome::Pending);
        }
        let target = nudge_target(job.last_run, now);
        if job.next_run <= target {
            return Ok(NudgeOutcome::AlreadySooner);
        }
        if !self.nudges.lock().await.try_spend(&key, now) {
            debug!(principal = %key, channel, "nudge budget spent; keepalive keeps its cadence");
            return Ok(NudgeOutcome::OverBudget);
        }
        if !scheduler.bring_forward(&job.id, target)? {
            return Ok(NudgeOutcome::AlreadySooner);
        }
        self.audit_nudge(&job, channel, target).await;
        Ok(NudgeOutcome::Moved(target))
    }

    /// After a keepalive turn completes, apply a nudge that arrived while
    /// it ran.
    pub(super) async fn apply_pending_nudge(&self, job: &CronJob) {
        if job.id != KEEPALIVE_JOB_ID {
            return;
        }
        let key = self.nudge_key(&job.principal_id).await;
        {
            let mut book = self.nudges.lock().await;
            match book.entries.get_mut(&key) {
                Some(entry) if entry.pending => entry.pending = false,
                _ => return,
            }
        }
        let result = async {
            let scheduler = self.scheduler_for(&job.principal_id).await?;
            let Some(updated) = scheduler.get_job(&job.id)? else {
                return anyhow::Ok(None);
            };
            if !attention_gated(&updated) {
                return Ok(None);
            }
            let target = nudge_target(updated.last_run, Utc::now());
            Ok(scheduler
                .bring_forward(&updated.id, target)?
                .then_some((updated, target)))
        }
        .await;
        match result {
            Ok(Some((updated, target))) => self.audit_nudge(&updated, "pending", target).await,
            Ok(None) => {}
            Err(e) => warn!(job = %job.id, "pending nudge not applied: {e:#}"),
        }
    }

    async fn audit_nudge(&self, job: &CronJob, channel: &str, next_run: DateTime<Utc>) {
        let _ = self
            .observability
            .audit_with_caller(
                Some(&peko_auth::caller::CallerContext::local().subject()),
                "cron.nudge",
                Some(&job.principal_id.0),
                serde_json::json!({
                    "job_id": job.id,
                    "channel": channel,
                    "last_turn": job.last_run,
                    "next_run_at": next_run,
                }),
            )
            .await;
        debug!(job = %job.id, channel, "peer post brought the keepalive forward to {next_run}");
    }
}

#[async_trait::async_trait]
impl crate::daemon::channel_binding::PeerPostNudge for CronEngine {
    async fn peer_posted(&self, principal: &PrincipalId, channel: &peko_channel::ChannelId) {
        match self.nudge_attention(principal, channel.as_str()).await {
            Ok(outcome) => debug!(principal = %principal, %channel, ?outcome, "peer post nudge"),
            Err(e) => warn!(principal = %principal, %channel, "peer post nudge failed: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_keeps_the_gap_after_the_last_turn() {
        let now = Utc::now();
        assert_eq!(nudge_target(None, now), now);
        let recent = now - Duration::seconds(30);
        assert_eq!(nudge_target(Some(recent), now), recent + NUDGE_MIN_GAP);
        let old = now - Duration::minutes(9);
        assert_eq!(nudge_target(Some(old), now), now);
    }

    #[test]
    fn budget_rolls_over_after_an_hour() {
        let mut book = NudgeBook::default();
        let key = PrincipalId("prin_a".into());
        let start = Utc::now();
        for i in 0..NUDGE_BUDGET_PER_HOUR {
            assert!(book.try_spend(&key, start + Duration::minutes(i as i64)));
        }
        assert!(!book.try_spend(&key, start + Duration::minutes(30)));
        // Another principal has its own budget.
        assert!(book.try_spend(&PrincipalId("prin_b".into()), start));
        // An hour after the first spend, one comes back.
        assert!(book.try_spend(&key, start + Duration::minutes(60)));
        assert!(!book.try_spend(&key, start + Duration::minutes(60)));
    }
}
