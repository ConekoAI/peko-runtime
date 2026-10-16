//! Quiet ticks (ADR-083): whether a due `when: attention` cron fire has
//! anything for the principal to do.
//!
//! The check calls no model and changes nothing. Something waits when:
//! - the session the job fires in would see channel posts in its digest
//!   (other principals' posts on its waiting list, or unread activity),
//! - an owner run's open plans have steps ready or in progress, or
//! - another of the principal's jobs failed since this job's last turn.
//!
//! Anything else (pending proposals and approvals, for instance) waits on
//! the owner, not the principal, and would keep every tick busy.
//!
//! A `when: quiet` fire (ADR-080 D9) is held instead until nobody has
//! talked to the principal for a while ([`quiet_hold`]).

use std::sync::Arc;

use peko_cron::{CronJob, QUIET_STATUS};
use peko_plan::PlanNodeStatus;

use crate::principal::Principal;

/// What waits on the principal for this fire, one short line per kind.
/// Empty means the fire may be quiet.
pub(crate) async fn what_waits(
    principal: &Principal,
    channels: Option<Arc<dyn peko_channel::ChannelPort>>,
    job: &CronJob,
    session_id: &str,
    owner: bool,
    jobs: &[CronJob],
) -> Vec<String> {
    let mut waiting = Vec::new();
    if let Some(port) = &channels {
        let posts =
            crate::principal::channel_digest::pending_posts(port, principal, session_id, owner)
                .await;
        if posts > 0 {
            waiting.push(format!("{posts} channel post(s) to look at"));
        }
    }
    if owner {
        let steps = open_plan_steps(principal).await;
        if steps > 0 {
            waiting.push(format!("{steps} plan step(s) ready or in progress"));
        }
    }
    let failed = failed_since(job, jobs);
    if !failed.is_empty() {
        waiting.push(format!("failed job(s): {}", failed.join(", ")));
    }
    waiting
}

/// Steps ready to start or in progress across the principal's open plans.
async fn open_plan_steps(principal: &Principal) -> usize {
    let plans = match principal.plan_port.load_resumable(&principal.id).await {
        Ok(plans) => plans,
        Err(e) => {
            tracing::debug!(principal = %principal.id, "quiet tick: plans unreadable: {e}");
            return 0;
        }
    };
    plans
        .iter()
        .map(|plan| {
            let in_progress = plan
                .nodes
                .iter()
                .filter(|n| matches!(n.status, PlanNodeStatus::InProgress))
                .count();
            in_progress + plan.ready_nodes().len()
        })
        .sum()
}

/// The names of the principal's other jobs whose last run failed after
/// `job`'s last turn (any time, when it has had none).
fn failed_since(job: &CronJob, jobs: &[CronJob]) -> Vec<String> {
    jobs.iter()
        .filter(|other| other.id != job.id && other.principal_id == job.principal_id)
        .filter(|other| {
            other
                .last_status
                .as_deref()
                .is_some_and(|s| !matches!(s, "success" | "running") && s != QUIET_STATUS)
        })
        .filter(|other| match (other.last_run, job.last_run) {
            (Some(failed_at), Some(last_turn)) => failed_at > last_turn,
            (Some(_), None) => true,
            (None, _) => false,
        })
        .map(|other| other.name.clone())
        .collect()
}

/// Whether a fire with nothing waiting may stay quiet: the job's last
/// turn is younger than [`peko_cron::ATTENTION_MAX_QUIET_MS`]. A job that
/// never ran a turn runs one.
#[must_use]
pub(crate) fn may_stay_quiet(job: &CronJob, now: chrono::DateTime<chrono::Utc>) -> bool {
    job.last_run
        .is_some_and(|last| (now - last).num_milliseconds() < peko_cron::ATTENTION_MAX_QUIET_MS)
}

/// Until when a due `when: quiet` fire is held (ADR-080 D9); `None`
/// runs it now. It runs once nobody has talked to the principal for
/// [`peko_cron::QUIET_STRETCH_MS`] since `last_activity`, or
/// [`peko_cron::QUIET_MAX_WAIT_MS`] after it fell due. The due time is
/// the schedule's next slot after the job's last turn (its creation,
/// when it has had none), so holding the fire, which moves `next_run`,
/// does not move the deadline.
#[must_use]
pub(crate) fn quiet_hold(
    job: &CronJob,
    last_activity: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let quiet_at = last_activity + chrono::Duration::milliseconds(peko_cron::QUIET_STRETCH_MS);
    if now >= quiet_at {
        return None;
    }
    let after_last_turn =
        peko_cron::calculate_next_run(&job.schedule, job.last_run.unwrap_or(job.created_at))
            .unwrap_or(job.next_run);
    let deadline = after_last_turn.min(job.next_run)
        + chrono::Duration::milliseconds(peko_cron::QUIET_MAX_WAIT_MS);
    (now < deadline).then(|| quiet_at.min(deadline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use peko_cron::{CronJobAction, ScheduleKind};
    use peko_subject::PrincipalId;

    fn job(id: &str, last_run: Option<chrono::DateTime<Utc>>, status: Option<&str>) -> CronJob {
        CronJob {
            id: id.into(),
            name: id.into(),
            principal_id: PrincipalId("p".into()),
            schedule: ScheduleKind::Every { every_ms: 600_000 },
            action: CronJobAction::Send {
                message: "tick".into(),
                target: None,
                model: None,
                when: None,
            },
            delete_after_run: false,
            enabled: true,
            created_at: Utc::now(),
            next_run: Utc::now(),
            last_run,
            last_status: status.map(str::to_string),
            run_count: 0,
            consecutive_failures: 0,
            max_retries: None,
            origin_session: None,
            created_by: None,
        }
    }

    #[test]
    fn only_failures_after_the_last_turn_wait() {
        let now = Utc::now();
        let tick = job(
            "keepalive",
            Some(now - Duration::minutes(10)),
            Some("success"),
        );
        let jobs = vec![
            tick.clone(),
            job("old-fail", Some(now - Duration::hours(1)), Some("failed")),
            job("new-fail", Some(now - Duration::minutes(1)), Some("failed")),
            job("fine", Some(now), Some("success")),
            job("hushed", Some(now), Some(QUIET_STATUS)),
        ];
        assert_eq!(failed_since(&tick, &jobs), vec!["new-fail"]);
        let fresh = job("keepalive", None, None);
        assert_eq!(failed_since(&fresh, &jobs), vec!["old-fail", "new-fail"]);
    }

    #[test]
    fn quiet_lasts_at_most_the_cap() {
        let now = Utc::now();
        assert!(!may_stay_quiet(&job("k", None, None), now));
        assert!(may_stay_quiet(
            &job("k", Some(now - Duration::minutes(50)), None),
            now
        ));
        assert!(!may_stay_quiet(
            &job("k", Some(now - Duration::minutes(61)), None),
            now
        ));
    }

    #[test]
    fn a_quiet_fire_waits_for_a_quiet_stretch_up_to_the_cap() {
        let now = Utc::now();
        let day = Duration::hours(24);
        let mut nightly = job("memory", Some(now - day), Some("success"));
        nightly.schedule = ScheduleKind::Every {
            every_ms: day.num_milliseconds() as u64,
        };
        nightly.next_run = now;
        // Quiet for five hours: it runs.
        assert_eq!(quiet_hold(&nightly, now - Duration::hours(5), now), None);
        // Talked to an hour ago: held until four hours after that.
        assert_eq!(
            quiet_hold(&nightly, now - Duration::hours(1), now),
            Some(now + Duration::hours(3))
        );
        // Holding moved `next_run`; the deadline still counts from the
        // due time, so a principal never quiet runs twelve hours late.
        nightly.next_run = now + Duration::hours(11);
        let later = now + Duration::hours(11);
        assert_eq!(
            quiet_hold(&nightly, later - Duration::minutes(5), later),
            Some(now + Duration::hours(12))
        );
        let deadline = now + Duration::hours(12);
        assert_eq!(
            quiet_hold(&nightly, deadline - Duration::minutes(5), deadline),
            None
        );
    }
}
