//! `TaskCreate` tool — create a planning todo.
//!
//! Phase 10d re-export shim. The canonical implementation lives in
//! [`peko_tools_builtin::tasks::TaskCreateTool`]; this file preserves
//! the legacy `crate::tools::builtin::task_create::TaskCreateTool`
//! path for out-of-tree callers and root-side wiring that still
//! constructs the tool with `Arc<TodoStorage>`.
//!
//! The exported type has been *genericised*: it now wraps a
//! [`peko_tools_builtin::tasks::SharedTodoRuntime`] (an
//! `Arc<dyn TodoRuntime>`) instead of `Arc<TodoStorage>`. To keep
//! the legacy `TaskCreateTool::new(storage)` signature working, the
//! root-side `Arc<TodoStorage>` is auto-wrapped via
//! [`crate::session::todo_runtime_impl::TodoStorageRuntime`]. See
//! `src/agents/agent.rs` for the single production call site that
//! has been updated to construct the runtime directly.

pub use peko_tools_builtin::tasks::TaskCreateTool;
