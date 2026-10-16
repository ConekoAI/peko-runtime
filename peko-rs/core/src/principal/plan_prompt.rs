//! The principal's open plans in every owner run's context.
//!
//! Renders the `plans` tail section: each open plan with unresolved
//! steps — in progress, ready next, blocked or failed — most recently
//! active first. It rides the ADR-052 change-detected tail, so a step
//! marked mid-run re-injects the updated plan, and it reaches every
//! owner run: the trunk, the owner's conversations, and their spawns.
//! (Plans used to ride a one-shot "Prior context" message built only
//! on trunk turns, so the owner's own conversations never saw them.)
//!
//! Only owner runs see plans (ADR-078): they are the principal's
//! internal work, and any other run reaches only its own conversation.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use peko_plan::{PlanNode, PlanNodeStatus, PlanPort, PlanRecord, PlanStorage};
use peko_subject::PrincipalId;

/// Most plans rendered; the rest are counted with a pointer to the
/// `Plan` tool.
pub const PLANS_SECTION_CAP: usize = 5;

/// Renders the `plans` section for owner runs.
#[derive(Debug, Default)]
pub struct WorkspacePlansPromptHandler;

#[async_trait]
impl crate::tools::prompt_sections::PromptSectionProvider for WorkspacePlansPromptHandler {
    fn section(&self) -> &'static str {
        "plans"
    }

    async fn render(
        &self,
        input: &crate::tools::prompt_sections::PromptSectionInput,
    ) -> Option<String> {
        if !input
            .acting_for
            .as_ref()
            .is_some_and(peko_tools_core::ActingFor::is_owner)
            || input.principal_id.is_empty()
        {
            return None;
        }
        let storage = PlanStorage::new(plans_dir_for_workspace(&input.workspace)?);
        render_open_plans(&storage, &PrincipalId(input.principal_id.clone())).await
    }
}

/// The principal's plan directory, derived from its workspace name like
/// `child_turns::sessions_dir_for_workspace`. `None` when the
/// workspace has no name or no plan was ever written.
fn plans_dir_for_workspace(workspace: &Path) -> Option<PathBuf> {
    let name = workspace.file_name()?.to_string_lossy().to_string();
    let dir = crate::common::paths::PathResolver::new()
        .principal_layout(&name)
        .local
        .plans_dir;
    dir.exists().then_some(dir)
}

/// Render the open plans of `principal_id`: at most
/// [`PLANS_SECTION_CAP`], most recently updated first (ties by plan
/// id), then a count of the rest. `None` when there are none or the
/// store can't be read (a storage hiccup must not block a turn).
async fn render_open_plans(port: &dyn PlanPort, principal_id: &PrincipalId) -> Option<String> {
    let mut plans = match port.load_resumable(principal_id).await {
        Ok(plans) => plans,
        Err(e) => {
            tracing::warn!(principal_id = %principal_id, error = %e, "plans section: load_resumable failed");
            return None;
        }
    };
    if plans.is_empty() {
        return None;
    }
    let now = Utc::now();
    plans.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.plan_id.cmp(&b.plan_id))
    });
    let hidden = plans.len().saturating_sub(PLANS_SECTION_CAP);
    let mut blocks: Vec<String> = plans
        .iter()
        .take(PLANS_SECTION_CAP)
        .map(|record| render_plan_block(record, now))
        .collect();
    if hidden > 0 {
        blocks.push(format!(
            "… {hidden} more open plan(s) — `Plan` with action `list` shows them all"
        ));
    }
    Some(blocks.join("\n"))
}

/// One plan: a header with title and id, then its due check-ins, its
/// in-progress steps, ready steps (dependencies done), and blocked or
/// failed steps with their reasons. A step with a future `check_at`
/// shows when it comes back (ADR-084). Sections without steps are
/// omitted.
fn render_plan_block(record: &PlanRecord, now: DateTime<Utc>) -> String {
    let mut out = format!("Plan: {} ({})\n", record.title, record.plan_id);
    let line = |n: &PlanNode| {
        format!(
            "    - [{}] {}{}\n",
            n.node_id.as_str(),
            n.step,
            check_label(n, now)
        )
    };

    let due = record.due_check_ins(now);
    if !due.is_empty() {
        out.push_str("  Check-ins due (act, then reschedule with checkAt or clear it):\n");
        for n in &due {
            out.push_str(&line(n));
        }
    }

    // A due step is listed once, under the check-ins.
    let not_due = |n: &&PlanNode| !due.iter().any(|d| d.node_id == n.node_id);
    let in_progress: Vec<_> = record
        .current_focus_nodes()
        .into_iter()
        .filter(not_due)
        .collect();
    if !in_progress.is_empty() {
        out.push_str("  In progress:\n");
        for n in &in_progress {
            out.push_str(&line(n));
        }
    }

    let ready: Vec<_> = record.ready_nodes().into_iter().filter(not_due).collect();
    if !ready.is_empty() {
        out.push_str("  Ready next:\n");
        for n in &ready {
            out.push_str(&line(n));
        }
    }

    let stuck: Vec<_> = record
        .nodes
        .iter()
        .filter(not_due)
        .filter_map(|n| match &n.status {
            PlanNodeStatus::Blocked { reason, .. } => Some((n, "blocked", reason)),
            PlanNodeStatus::Failed { reason, .. } => Some((n, "failed", reason)),
            _ => None,
        })
        .collect();
    if !stuck.is_empty() {
        out.push_str("  Needs attention:\n");
        for (n, label, reason) in stuck {
            out.push_str(&format!(
                "    - [{}] ({label}): {} ({reason}){}\n",
                n.node_id.as_str(),
                n.step,
                check_label(n, now)
            ));
        }
    }
    out
}

/// ` — check at <time>` for a future check-in, ` — check-in due since
/// <time>` for a past one, else nothing.
fn check_label(node: &PlanNode, now: DateTime<Utc>) -> String {
    match node.check_at {
        Some(at) if at > now => format!(" — check at {}", at.format("%Y-%m-%d %H:%M UTC")),
        Some(at) => format!(" — check-in due since {}", at.format("%Y-%m-%d %H:%M UTC")),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::prompt_sections::{PromptSectionInput, PromptSectionProvider};
    use peko_plan::NodeId;

    fn node(step: &str, status: PlanNodeStatus) -> PlanNode {
        let now = chrono::Utc::now();
        PlanNode {
            node_id: NodeId::generate(),
            step: step.to_string(),
            status,
            depends_on: vec![],
            evidence: None,
            blocked_reason: None,
            created_at: now,
            updated_at: now,
            check_at: None,
        }
    }

    fn storage() -> (tempfile::TempDir, PlanStorage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = PlanStorage::new(dir.path().join("plans"));
        (dir, storage)
    }

    #[tokio::test]
    async fn renders_progress_ready_and_stuck_steps() {
        let (_dir, storage) = storage();
        let p = PrincipalId::generate();
        let now = chrono::Utc::now();
        let plan = storage
            .create(
                p.clone(),
                "Migrate auth".into(),
                vec![
                    node("Add smoke tests", PlanNodeStatus::InProgress),
                    node("Wire SQLX", PlanNodeStatus::Pending),
                    node(
                        "Get review",
                        PlanNodeStatus::Blocked {
                            reason: "needs reviewer".into(),
                            since: now,
                        },
                    ),
                ],
            )
            .await
            .unwrap();

        let text = render_open_plans(&storage, &p).await.unwrap();
        assert!(
            text.starts_with(&format!("Plan: Migrate auth ({})", plan.plan_id)),
            "{text}"
        );
        let (progress, rest) = text.split_once("Ready next:").unwrap();
        assert!(progress.contains("In progress:") && progress.contains("Add smoke tests"));
        let (ready, stuck) = rest.split_once("Needs attention:").unwrap();
        assert!(ready.contains("Wire SQLX"), "{text}");
        assert!(
            stuck.contains("(blocked): Get review (needs reviewer)"),
            "{text}"
        );
    }

    /// ADR-084: a due check-in leads the plan (listed once); a future
    /// one shows when it comes back.
    #[tokio::test]
    async fn renders_due_and_upcoming_check_ins() {
        let (_dir, storage) = storage();
        let p = PrincipalId::generate();
        let now = chrono::Utc::now();
        let mut due = node("Ask Bob about the contract", PlanNodeStatus::Pending);
        due.check_at = Some(now - chrono::Duration::hours(1));
        let mut later = node("Review Q3 numbers", PlanNodeStatus::Pending);
        later.check_at = Some(now + chrono::Duration::days(2));
        storage
            .create(p.clone(), "Close the deal".into(), vec![due, later])
            .await
            .unwrap();
        let text = render_open_plans(&storage, &p).await.unwrap();
        let (check_ins, rest) = text.split_once("Ready next:").unwrap();
        assert!(check_ins.contains("Check-ins due"), "{text}");
        assert!(check_ins.contains("Ask Bob about the contract — check-in due since"));
        assert!(!rest.contains("Ask Bob"), "listed once: {text}");
        assert!(rest.contains("Review Q3 numbers — check at "), "{text}");
    }

    #[tokio::test]
    async fn no_open_plans_renders_nothing() {
        let (_dir, storage) = storage();
        let p = PrincipalId::generate();
        assert!(render_open_plans(&storage, &p).await.is_none());

        let done = storage
            .create(
                p.clone(),
                "done".into(),
                vec![node("x", PlanNodeStatus::Pending)],
            )
            .await
            .unwrap();
        storage
            .close(&done.plan_id, &p, "finished".into())
            .await
            .unwrap();
        assert!(render_open_plans(&storage, &p).await.is_none());

        // Another principal's plans never show.
        storage
            .create(
                PrincipalId::generate(),
                "theirs".into(),
                vec![node("y", PlanNodeStatus::Pending)],
            )
            .await
            .unwrap();
        assert!(render_open_plans(&storage, &p).await.is_none());
    }

    /// The most recently updated plans render first; past the cap the
    /// rest are counted.
    #[tokio::test]
    async fn caps_at_five_most_recent() {
        let (_dir, storage) = storage();
        let p = PrincipalId::generate();
        for i in 0..7 {
            storage
                .create(
                    p.clone(),
                    format!("plan-{i}"),
                    vec![node(&format!("step-{i}"), PlanNodeStatus::Pending)],
                )
                .await
                .unwrap();
        }
        let text = render_open_plans(&storage, &p).await.unwrap();
        let titles: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("Plan: "))
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(titles, ["plan-6", "plan-5", "plan-4", "plan-3", "plan-2"]);
        assert!(text.ends_with("… 2 more open plan(s) — `Plan` with action `list` shows them all"));
    }

    /// Plans are the principal's internal work: only owner runs see
    /// them (ADR-078).
    #[tokio::test]
    async fn non_owner_runs_get_no_section() {
        let input = |acting_for| PromptSectionInput {
            principal_id: "p".into(),
            workspace: PathBuf::from("/nonexistent/ws"),
            session_id: String::new(),
            channel_port: None,
            acting_for,
        };
        let mut guest = peko_tools_core::ActingFor::runtime();
        guest.trust = peko_tools_core::Trust::Trusted;
        for stamp in [
            None,
            Some(peko_tools_core::ActingFor::unattributed()),
            Some(guest),
        ] {
            assert!(WorkspacePlansPromptHandler
                .render(&input(stamp))
                .await
                .is_none());
        }
    }
}
