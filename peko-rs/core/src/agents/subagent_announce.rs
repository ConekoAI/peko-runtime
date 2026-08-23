//! Subagent system prompt + task-message builders.
//!
//! B4 cleanup: `format_announcement` was removed — its only consumer
//! was the deleted announcement-sender chain (B4 #17). Live
//! consumers remain:
//! - [`build_subagent_system_prompt`]
//! - [`build_subagent_task_message`]

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
