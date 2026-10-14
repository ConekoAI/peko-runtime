//! What started a tool call (ADR-079 D4).
//!
//! Most calls come straight from an agent turn and need no parent: the
//! session and run place them. Calls that run detached from that turn
//! name the call that caused them, so the ledger can stitch a
//! background body, a workflow callback, or an approved replay back to
//! its origin.

use serde::{Deserialize, Serialize};

/// The cause of a call that did not come straight from an agent turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallParent {
    /// The body of a background task. `call_id` is the call that
    /// spawned (or detached into) the task, when known.
    Task {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
    /// A callback from a workflow subprocess, naming its `Workflow` call.
    Workflow { call_id: String },
    /// The replay of a call the owner approved (ADR-078 D7).
    Approval {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_with_a_kind_tag() {
        let parent = CallParent::Task {
            id: "Bash:1".into(),
            call_id: Some("call_a".into()),
        };
        assert_eq!(
            serde_json::to_value(&parent).unwrap(),
            serde_json::json!({ "kind": "task", "id": "Bash:1", "call_id": "call_a" })
        );
        let workflow: CallParent =
            serde_json::from_value(serde_json::json!({ "kind": "workflow", "call_id": "c" }))
                .unwrap();
        assert_eq!(
            workflow,
            CallParent::Workflow {
                call_id: "c".into()
            }
        );
    }
}
