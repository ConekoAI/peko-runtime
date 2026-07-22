//! Backwards-compat shim: implements `peko_engine::AsyncCompletionLike`
//! for the legacy root-owned `CompletionEvent` so
//! `src/engine/agentic_loop.rs` (which drains the root's
//! `SharedSessionInbox` from
//! `crate::extensions::framework::async_exec::executor::completion_queue::CompletionEvent`)
//! can pass its `Vec<CompletionEvent>` straight into
//! `peko_engine::async_completion::build_async_completion_message`
//! without an intermediate conversion.
//!
//! Phase 9b.N.1 surface rationale: the two `CompletionEvent` structs
//! are field-identical. Consolidating the root copy into
//! `peko_extension_host::CompletionEvent` (or vice versa) is the right
//! architectural move but requires migrating callers in the executor
//! and inbox implementations; that consolidation is deferred to the
//! Phase 8 bulk-move follow-up PR. This shim keeps the lift narrowly
//! scoped to the `agentic_loop.rs` call site.

use crate::extensions::framework::async_exec::executor::completion_queue::CompletionEvent;
use peko_engine::async_completion::AsyncCompletionLike;

impl AsyncCompletionLike for CompletionEvent {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn tool_name(&self) -> &str {
        &self.tool_name
    }
    fn result(&self) -> &serde_json::Value {
        &self.result
    }
    fn status(&self) -> &peko_extension_api::AsyncTaskStatus {
        // Root `executor::AsyncTaskStatus` is a re-export of
        // `peko_extension_api::AsyncTaskStatus` (see
        // `src/extensions/framework/async_exec/executor/types.rs:24`),
        // so this is a type-equivalent reference.
        &self.status
    }
    fn parent_session_key(&self) -> &str {
        &self.parent_session_key
    }
}
