//! Cron-style "your scheduled job completed" steer messages.
//!
//! When a cron-scheduled async run finishes and the schedule was set up
//! with `wake_on_completion=true`, the executor delivers a
//! [`SteeringMessage`](super::executor::completion_queue::SteeringMessage)
//! into the principal's root inbox instead of a `CompletionEvent`. The
//! agent picks it up at its next iteration start and can call
//! `TaskOutput`/`AsyncOutput` for the full task result.
//!
//! The format is intentionally simple and human-readable so the agent
//! can act on it without parsing: the job's name (or task description),
//! the `task_id`, the tool that ran, and the outcome. Test-only — see
//! `format_cron_steer_message`.

use super::executor::types::AsyncTaskStatus;

/// Format the steer text for a cron-spawned async run.
///
/// `job_name` is the schedule entry's `name` (or a fallback
/// `"scheduled job"` if the caller did not provide one). `task_id`
/// identifies the underlying `AsyncTask` for `TaskOutput` lookups.
/// `tool_name` is the tool the cron engine asked the executor to run.
/// `outcome` is the terminal `AsyncTaskStatus` from the executor.
#[must_use]
pub fn format_cron_steer_message(
    job_name: &str,
    task_id: &str,
    tool_name: &str,
    outcome: &AsyncTaskStatus,
) -> String {
    let label = if job_name.is_empty() {
        "scheduled job"
    } else {
        job_name
    };
    let outcome_text = match outcome {
        AsyncTaskStatus::Completed { .. } => "completed successfully",
        AsyncTaskStatus::Failed { error } => return format!(
            "Your scheduled cron job \"{label}\" ({tool_name}, task {task_id}) failed: {error}.\n\
             You can check details with the TaskOutput tool."
        ),
        AsyncTaskStatus::TimedOut { error } => return format!(
            "Your scheduled cron job \"{label}\" ({tool_name}, task {task_id}) timed out: {error}.\n\
             You can check details with the TaskOutput tool."
        ),
        AsyncTaskStatus::Cancelled => return format!(
            "Your scheduled cron job \"{label}\" ({tool_name}, task {task_id}) was cancelled.\n\
             You can check details with the TaskOutput tool."
        ),
        AsyncTaskStatus::Pending | AsyncTaskStatus::Running => {
            // Should not happen — the executor only calls this on a
            // terminal status. Defensive fallback.
            "finished"
        }
    };
    format!(
        "Your scheduled cron job \"{label}\" ({tool_name}, task {task_id}) {outcome_text}.\n\
         You can check details with the TaskOutput tool."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_tools_core::ToolResult;

    #[test]
    fn steer_message_for_completed_includes_task_id_and_tool() {
        let status = AsyncTaskStatus::Completed {
            result: ToolResult::success(serde_json::json!({"ok": true})),
        };
        let msg = format_cron_steer_message("daily-summary", "shell:abc", "shell", &status);
        assert!(msg.contains("\"daily-summary\""));
        assert!(msg.contains("shell:abc"));
        assert!(msg.contains("shell"));
        assert!(msg.contains("completed successfully"));
        assert!(msg.contains("TaskOutput"));
    }

    #[test]
    fn steer_message_for_failed_includes_error() {
        let status = AsyncTaskStatus::Failed {
            error: "exit 1".to_string(),
        };
        let msg = format_cron_steer_message("", "shell:abc", "shell", &status);
        assert!(msg.contains("scheduled job"));
        assert!(msg.contains("failed"));
        assert!(msg.contains("exit 1"));
    }

    #[test]
    fn steer_message_for_timed_out_includes_timeout_error() {
        let status = AsyncTaskStatus::TimedOut {
            error: "after 7200s".to_string(),
        };
        let msg = format_cron_steer_message("long-job", "agent:xyz", "Agent", &status);
        assert!(msg.contains("\"long-job\""));
        assert!(msg.contains("timed out"));
        assert!(msg.contains("after 7200s"));
    }

    #[test]
    fn steer_message_for_cancelled_is_handled() {
        let status = AsyncTaskStatus::Cancelled;
        let msg = format_cron_steer_message("cancelled-job", "shell:c", "shell", &status);
        assert!(msg.contains("cancelled"));
    }
}
