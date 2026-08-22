//! Standing named children — session-level helpers shared by the
//! principal ensure-declared path (`crate::principal::children`) and
//! the Agent tool's attach-by-name path (`SubagentExecutor`).
//!
//! A standing child is a session created from a `[children]` entry in
//! `principal.toml`: `trigger == "spawn"`, `standing == true`, `slug ==
//! <name>`, parented at the principal's owner root session. The
//! declaration (`subagent_type`, `description`) is recorded as a
//! `System` event in the child's JSONL so a later attach can recover
//! the declared type without re-reading the principal config.
//!
//! This module is pure session-level I/O (append / read events) plus
//! refusal constructors in the `session::ownership` style — it lives
//! under `crate::session` (not `crate::principal`) because
//! `src/agents/` must not import from `src/principal/` (workspace
//! boundary rule) and the `SubagentExecutor` attach branch needs the
//! recovery helper.

use std::path::Path;

use anyhow::Result;
use peko_session::{EventEnvelope, SessionEvent, SessionStorage, SystemEvent};

/// System event name recorded in a standing child's JSONL at creation.
/// Carries the `[children]` declaration so a later resume/attach can
/// default to the declared `subagent_type`.
pub const STANDING_CHILD_DECLARED_EVENT: &str = "standing_child_declared";

/// Append the `[children]` declaration record to the child's JSONL.
/// Best-effort audit trail: the index/metadata carry the load-bearing
/// flags (`standing`, `slug`, `trigger`); this event exists so the
/// declared `subagent_type`/`description` are recoverable from the
/// session alone.
pub async fn record_declared_child(
    sessions_dir: &Path,
    session_id: &str,
    name: &str,
    subagent_type: &str,
    description: Option<&str>,
) -> Result<()> {
    let event = SessionEvent::System(SystemEvent {
        envelope: EventEnvelope {
            id: format!("evt_{}", uuid::Uuid::new_v4().simple()),
            ts: chrono::Utc::now(),
        },
        event: STANDING_CHILD_DECLARED_EVENT.to_string(),
        detail: serde_json::json!({
            "name": name,
            "subagent_type": subagent_type,
            "description": description,
        }),
    });
    SessionStorage::new(sessions_dir.to_path_buf())
        .append_event(session_id, &event)
        .await
}

/// Recover the declared `subagent_type` from the child's JSONL, if a
/// declaration event was recorded. `None` when the session carries no
/// declaration or the transcript can't be read (unreadable history
/// must not block an attach — the caller then requires the
/// `subagent_type` param as usual).
pub async fn declared_subagent_type(sessions_dir: &Path, session_id: &str) -> Option<String> {
    let events = SessionStorage::new(sessions_dir.to_path_buf())
        .load_events(session_id)
        .await
        .ok()?;
    events.iter().rev().find_map(|e| match e {
        SessionEvent::System(sys) if sys.event == STANDING_CHILD_DECLARED_EVENT => sys
            .detail
            .get("subagent_type")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    })
}

/// `Agent` `new` with `name` colliding with a session that is NOT a
/// standing child (rename semantics live in the session tool).
pub fn err_name_not_standing(name: &str, session_id: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot spawn with name '{name}': session '{session_id}' already uses that slug but is \
         not a standing child — only standing sessions (declared via the principal's \
         [children] config) attach by name; pick a different name, or manage the existing \
         session with the session tool"
    )
}

/// `Agent` `new` attach whose `subagent_type` disagrees with the
/// declaration recorded on the standing child.
pub fn err_declared_type_mismatch(
    name: &str,
    session_id: &str,
    declared: &str,
    requested: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot attach to standing child '{name}' (session '{session_id}'): it was declared \
         with subagent_type '{declared}' but this call requested '{requested}' — match the \
         declaration or pick a different name"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Record → recover round-trip; the LAST declaration wins when
    /// several are present.
    #[tokio::test]
    async fn declaration_round_trips_through_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let storage = SessionStorage::new(dir.path().to_path_buf());
        // A session file must exist before events can be appended.
        storage
            .create_session_with_header(
                "child1",
                None,
                peko_session::SessionTrigger::Spawn,
                Some("root:user:alice".to_string()),
            )
            .await
            .unwrap();

        // No declaration yet → not recoverable.
        assert!(declared_subagent_type(dir.path(), "child1").await.is_none());

        record_declared_child(dir.path(), "child1", "memory", "archivist", Some("curator"))
            .await
            .unwrap();
        assert_eq!(
            declared_subagent_type(dir.path(), "child1")
                .await
                .as_deref(),
            Some("archivist")
        );

        // Unknown session id → None (no panic, no error propagation).
        assert!(declared_subagent_type(dir.path(), "ghost").await.is_none());
    }

    #[test]
    fn refusals_are_actionable() {
        for err in [
            err_name_not_standing("memory", "sess_x"),
            err_declared_type_mismatch("memory", "sess_x", "archivist", "writer"),
        ] {
            let msg = err.to_string();
            assert!(msg.len() > 40, "refusal too terse: {msg}");
            assert!(msg.contains("memory"), "{msg}");
            assert!(msg.contains("sess_x"), "{msg}");
        }
    }
}
