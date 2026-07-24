//! Subagent Result Announcement
//!
//! Handles announcing subagent results back to parent sessions.
//! When a subagent completes, its result is added as a message to the parent's base session.

use crate::agents::subagent_types::{SubagentRunView, SubagentStatus};

/// Format a subagent result as an announcement message
#[must_use]
pub fn format_announcement(run: &SubagentRunView) -> String {
    let label_part = run
        .label
        .as_ref()
        .map(|l| format!(" [{l}]"))
        .unwrap_or_default();

    let header = format!("## Subagent Result{label_part}\n\n");

    let status_emoji = match &run.status {
        SubagentStatus::Completed { .. } => "✅",
        SubagentStatus::Failed { .. } => "❌",
        SubagentStatus::Cancelled => "🚫",
        SubagentStatus::TimedOut { .. } => "⏱️",
        SubagentStatus::Running => "🔄",
        _ => "❓",
    };

    let status_line = format!("**Status:** {} {}\n\n", status_emoji, run.status.as_str());

    let content = match &run.result {
        Some(result) => match &run.status {
            SubagentStatus::Completed { .. } => {
                if let Some(output) = &result.output {
                    format!("**Output:**\n\n{output}\n")
                } else {
                    "**Output:** (no content)\n".to_string()
                }
            }
            SubagentStatus::Failed { .. } => {
                if let Some(error) = &result.error {
                    format!("**Error:** {error}\n")
                } else {
                    "**Error:** Unknown error occurred\n".to_string()
                }
            }
            SubagentStatus::TimedOut { .. } => "**Error:** Subagent timed out\n".to_string(),
            SubagentStatus::Cancelled => "**Info:** Subagent was cancelled\n".to_string(),
            SubagentStatus::Running => "**Info:** Subagent is still running\n".to_string(),
            _ => "**Info:** Unknown status\n".to_string(),
        },
        None => "**Info:** No result available\n".to_string(),
    };

    let metadata = format!(
        "\n---\n*Run ID: `{}` | Child Session: `{}`*",
        run.run_id, run.child_session_key
    );

    format!("{header}{status_line}{content}{metadata}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::subagent_types::{SubagentResult, SubagentRunView, SubagentStatus};
    use chrono::Utc;

    fn make_test_view(status: SubagentStatus, result: Option<SubagentResult>) -> SubagentRunView {
        SubagentRunView {
            run_id: "run_123".to_string(),
            child_session_key: "child_key".to_string(),
            parent_session_key: "parent_key".to_string(),
            task: "Test task".to_string(),
            status,
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            cleanup: peko_session::types::SpawnCleanupPolicy::Keep,
            label: Some("my_label".to_string()),
            result,
            depth: 1,
            announce_completion: true,
        }
    }

    #[test]
    fn test_format_announcement_completed() {
        let run = make_test_view(
            SubagentStatus::Completed {
                result: peko_tools_core::ToolResult::success(serde_json::json!({})),
            },
            Some(SubagentResult {
                status: SubagentStatus::Completed {
                    result: peko_tools_core::ToolResult::success(serde_json::json!({})),
                },
                output: Some("Success output".to_string()),
                error: None,
                token_usage: Some((10, 20, 30)),
                completed_at: Utc::now(),
            }),
        );

        let announcement = format_announcement(&run);
        assert!(announcement.contains("Subagent Result [my_label]"));
        assert!(announcement.contains("✅"));
        assert!(announcement.contains("completed"));
        assert!(announcement.contains("Success output"));
        assert!(announcement.contains("run_123"));
    }

    #[test]
    fn test_format_announcement_failed() {
        let run = make_test_view(
            SubagentStatus::Failed {
                error: "Something went wrong".to_string(),
            },
            Some(SubagentResult {
                status: SubagentStatus::Failed {
                    error: "Something went wrong".to_string(),
                },
                output: None,
                error: Some("Something went wrong".to_string()),
                token_usage: None,
                completed_at: Utc::now(),
            }),
        );

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
