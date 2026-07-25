//! Cross-boundary DTO: tool call + result snapshot.
//!
//! `ToolCallInfo` is the lightweight, serializable summary of a tool
//! call that flows between the extension framework, the engine, and
//! the principal-to-principal messaging path. It carries just enough
//! shape (`id`, `name`, `parameters`, and an optional textual `result`)
//! for callers to render a call record without needing the full
//! `ContentBlock::ToolCall` machinery or the `tools::builtin` impl
//! types.
//!
//! **Phase 9b.1 lift:** this type moved from
//! `peko_extension_host::principal_message::ToolCallInfo` (where it
//! lived after the Phase 8 commit 2 move) so that `peko-engine` can
//! hold `Vec<ToolCallInfo>` on `ChannelOutput` without taking a
//! host-crate dep just for a 4-field DTO. The extension-host crate
//! keeps a one-line `pub use peko_message::ToolCallInfo;` so every
//! existing principal_message call site keeps compiling unchanged.
//!
//! This is **distinct** from `tools::builtin::session::ToolCallInfo`,
//! which is a session-persistence-only shape with three fields
//! (`id`, `name`, `arguments`) and no result — leaving both intact
//! was less risky than collapsing them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Lightweight tool call DTO with optional result.
///
/// Used by `peko_extension_host::principal_message::PrincipalMessageResponse`,
/// by `peko_engine::stream_types::ChannelOutput`, and by any
/// extension/handler that wants to render a tool-call summary
/// without depending on `tools::builtin` concrete impls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    /// Tool call ID.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool parameters (already JSON-encoded).
    pub parameters: Value,
    /// Tool result, if execution has completed. `None` while the
    /// call is still in flight or when the call has no stringifiable
    /// result (e.g. structured-only tools).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

impl ToolCallInfo {
    /// Construct a `ToolCallInfo` with no result.
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>, parameters: Value) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            parameters,
            result: None,
        }
    }

    /// Attach a result string and consume self.
    #[must_use]
    pub fn with_result(mut self, result: impl Into<String>) -> Self {
        self.result = Some(result.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_omits_result_field() {
        let info = ToolCallInfo::new("tc1", "Read", serde_json::json!({"path": "/tmp/x"}));
        assert_eq!(info.id, "tc1");
        assert_eq!(info.name, "Read");
        assert!(info.result.is_none());

        // Result is `skip_serializing_if = "Option::is_none"`, so the
        // serialized payload must NOT carry a `"result"` key.
        let json = serde_json::to_string(&info).expect("serialize");
        assert!(
            !json.contains("result"),
            "result must be skipped when None: {json}"
        );
    }

    #[test]
    fn test_with_result_keeps_field() {
        let info = ToolCallInfo::new("tc1", "Read", serde_json::json!({"path": "/tmp/x"}))
            .with_result("ok");
        let json = serde_json::to_string(&info).expect("serialize");
        assert!(json.contains("\"result\":\"ok\""));
    }

    #[test]
    fn test_roundtrip_via_json() {
        let info = ToolCallInfo::new("tc2", "Bash", serde_json::json!({"cmd": "ls"}))
            .with_result("file.txt\n");
        let json = serde_json::to_string(&info).expect("serialize");
        let back: ToolCallInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.id, "tc2");
        assert_eq!(back.name, "Bash");
        assert_eq!(back.result.as_deref(), Some("file.txt\n"));
        assert_eq!(back.parameters, serde_json::json!({"cmd": "ls"}));
    }
}
