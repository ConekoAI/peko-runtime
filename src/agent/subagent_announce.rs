//! Subagent Result Announcement
//!
//! Handles announcing subagent results back to parent sessions.
//! When a subagent completes, its result is added as a message to the parent's base session.

use crate::agent::subagent_registry::{SubagentRun, SubagentStatus};
use crate::session::manager::SessionHandle;
use anyhow::{Context, Result};

/// Format a subagent result as an announcement message
#[must_use]
pub fn format_announcement(run: &SubagentRun) -> String {
    let label_part = run
        .label
        .as_ref()
        .map(|l| format!(" [{l}]"))
        .unwrap_or_default();

    let header = format!("## Subagent Result{label_part}\n\n");

    let status_emoji = match run.status {
        SubagentStatus::Completed => "✅",
        SubagentStatus::Failed => "❌",
        SubagentStatus::Cancelled => "🚫",
        SubagentStatus::TimedOut => "⏱️",
        SubagentStatus::Running => "🔄",
    };

    let status_line = format!("**Status:** {} {}\n\n", status_emoji, run.status);

    let content = match &run.result {
        Some(result) => match run.status {
            SubagentStatus::Completed => {
                if let Some(output) = &result.output {
                    format!("**Output:**\n\n{output}\n")
                } else {
                    "**Output:** (no content)\n".to_string()
                }
            }
            SubagentStatus::Failed => {
                if let Some(error) = &result.error {
                    format!("**Error:** {error}\n")
                } else {
                    "**Error:** Unknown error occurred\n".to_string()
                }
            }
            SubagentStatus::TimedOut => "**Error:** Subagent timed out\n".to_string(),
            SubagentStatus::Cancelled => "**Info:** Subagent was cancelled\n".to_string(),
            SubagentStatus::Running => "**Info:** Subagent is still running\n".to_string(),
        },
        None => "**Info:** No result available\n".to_string(),
    };

    let metadata = format!(
        "\n---\n*Run ID: `{}` | Child Session: `{}`*",
        run.run_id, run.child_session_key
    );

    format!("{header}{status_line}{content}{metadata}")
}

/// Announce a subagent result to its parent session
///
/// This adds the result as an assistant message to the parent's base session.
pub async fn announce_to_parent(parent_handle: &SessionHandle, run: &SubagentRun) -> Result<()> {
    if !run.announce_completion {
        tracing::debug!(
            "Skipping announcement for run {} (announce_completion=false)",
            run.run_id
        );
        return Ok(());
    }

    let announcement = format_announcement(run);

    // Add the announcement as an assistant message to the parent session
    parent_handle
        .add_assistant(&announcement, None, None)
        .await
        .with_context(|| {
            format!(
                "Failed to add announcement to parent session for run {}",
                run.run_id
            )
        })?;

    tracing::info!(
        "Announced subagent result to parent session: run={} parent={}",
        run.run_id,
        run.parent_session_key
    );

    Ok(())
}

/// Build a system prompt for a subagent
///
/// This provides context to the subagent about its task and relationship to the parent.
#[must_use]
pub fn build_subagent_system_prompt(
    parent_session_key: &str,
    child_session_key: &str,
    task: &str,
    label: Option<&str>,
    depth: u32,
    max_depth: u32,
) -> String {
    let label_part = label
        .map(|l| format!(" with label '{l}'"))
        .unwrap_or_default();

    format!(
        r"[Subagent Context]
You are running as a subagent (depth {depth}/{max_depth}).

**Your Task:** {task}

**Key Information:**
- You are executing in a subagent session: {child_session_key}
- Your parent session is: {parent_session_key}
- Your results will be automatically announced back to the parent when you complete{label_part}

**Important Instructions:**
1. Focus solely on the task provided above
2. Do NOT spawn additional subagents unless absolutely necessary (you are at depth {depth} of {max_depth} max)
3. Complete your task efficiently and provide clear output
4. Do NOT busy-poll for status - the system will handle result announcement automatically
5. ALWAYS respond with text output after completing your task - empty responses cannot be captured
6. Return your results as normal assistant text responses - they will be captured and announced

**Result Announcement:**
When you complete your work, the result will be automatically sent back to your requester. You do not need to do anything special for this to happen.
"
    )
}

/// Build the task message for a subagent
///
/// This is the actual user message that contains the task.
#[must_use]
pub fn build_subagent_task_message(task: &str, depth: u32, max_depth: u32) -> String {
    format!(
        r"[Subagent Task]

{task}

---
Remember: You are running as a subagent (depth {depth}/{max_depth}). Results auto-announce to your requester; do not busy-poll for status."
    )
}

/// Handle cleanup of a subagent session based on policy
pub async fn handle_cleanup(
    run: &SubagentRun,
    cleanup_fn: impl FnOnce() -> Result<()>,
) -> Result<()> {
    match run.cleanup {
        crate::session::types::SpawnCleanupPolicy::Delete => {
            tracing::info!(
                "Cleaning up subagent session: {} (run: {})",
                run.child_session_key,
                run.run_id
            );
            cleanup_fn().with_context(|| {
                format!(
                    "Failed to cleanup subagent session: {}",
                    run.child_session_key
                )
            })?;
        }
        crate::session::types::SpawnCleanupPolicy::Keep => {
            tracing::debug!(
                "Keeping subagent session: {} (run: {}, cleanup=keep)",
                run.child_session_key,
                run.run_id
            );
        }
    }
    Ok(())
}

/// Announce a subagent completion event
///
/// This is called when a subagent finishes (successfully or not).
/// It handles both the announcement to the parent and session cleanup.
pub async fn on_subagent_complete(
    parent_handle: &SessionHandle,
    run: &SubagentRun,
    cleanup_fn: impl FnOnce() -> Result<()>,
) -> Result<()> {
    // First, announce the result to the parent
    announce_to_parent(parent_handle, run).await?;

    // Then, handle cleanup if needed
    handle_cleanup(run, cleanup_fn).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::subagent_registry::{SubagentResult, SubagentRun, SubagentStatus};
    use chrono::Utc;

    #[test]
    fn test_format_announcement_completed() {
        let run = SubagentRun {
            run_id: "run_123".to_string(),
            child_session_key: "child_key".to_string(),
            parent_session_key: "parent_key".to_string(),
            task: "Test task".to_string(),
            status: SubagentStatus::Completed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            cleanup: crate::session::types::SpawnCleanupPolicy::Keep,
            label: Some("my_label".to_string()),
            result: Some(SubagentResult {
                status: SubagentStatus::Completed,
                output: Some("Success output".to_string()),
                error: None,
                token_usage: Some((10, 20, 30)),
                completed_at: Utc::now(),
            }),
            depth: 1,
            announce_completion: true,
        };

        let announcement = format_announcement(&run);
        assert!(announcement.contains("Subagent Result [my_label]"));
        assert!(announcement.contains("✅"));
        assert!(announcement.contains("completed"));
        assert!(announcement.contains("Success output"));
        assert!(announcement.contains("run_123"));
    }

    #[test]
    fn test_format_announcement_failed() {
        let run = SubagentRun {
            run_id: "run_456".to_string(),
            child_session_key: "child_key".to_string(),
            parent_session_key: "parent_key".to_string(),
            task: "Test task".to_string(),
            status: SubagentStatus::Failed,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            cleanup: crate::session::types::SpawnCleanupPolicy::Keep,
            label: None,
            result: Some(SubagentResult {
                status: SubagentStatus::Failed,
                output: None,
                error: Some("Something went wrong".to_string()),
                token_usage: None,
                completed_at: Utc::now(),
            }),
            depth: 1,
            announce_completion: true,
        };

        let announcement = format_announcement(&run);
        assert!(announcement.contains("❌"));
        assert!(announcement.contains("failed"));
        assert!(announcement.contains("Something went wrong"));
    }

    #[test]
    fn test_build_subagent_system_prompt() {
        let prompt = build_subagent_system_prompt(
            "parent:session:key",
            "child:session:key",
            "Summarize this conversation",
            Some("summarizer"),
            1,
            3,
        );

        assert!(prompt.contains("depth 1/3"));
        assert!(prompt.contains("Summarize this conversation"));
        assert!(prompt.contains("summarizer"));
        assert!(prompt.contains("child:session:key"));
        assert!(prompt.contains("parent:session:key"));
    }

    #[test]
    fn test_build_subagent_task_message() {
        let message = build_subagent_task_message("Analyze data", 2, 3);
        assert!(message.contains("Analyze data"));
        assert!(message.contains("depth 2/3"));
        assert!(message.contains("Results auto-announce"));
    }
}
