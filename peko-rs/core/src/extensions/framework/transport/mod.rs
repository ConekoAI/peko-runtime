//! Cross-boundary transport surface.
//!
//! ## Module layout
//!
//! - [`async_router`]: the concrete router (5-minute timeout funnel +
//!   background detach). The `ToolDispatcher`
//!   (`crate::tools::dispatcher`) routes tool executions through it.
//! - [`async_transport`]: the in-process transport (`LocalAsyncTransport`)
//!   + the `BoxedExecutionFn` helper type and the `create_local_transport*`
//!   factories.
//!
//! 2026-09-27 consolidation (ADR-063 (dead IPC path)): the
//! `DaemonTransport` IPC projection is deleted with the rest of the dead
//! IPC async-spawn path — the CLI never executes tools (ADR-021), so no
//! component hands background work to the daemon over IPC.
//!
//! ADR-066 P3: the `AsyncExecutionRouter` trait port + `ToolExecConfig` /
//! `PreprocessorFn` / `ExecFn` bridge types were deleted — the
//! `ToolDispatcher` drives the concrete router directly.

pub mod async_router;
pub mod async_transport;
