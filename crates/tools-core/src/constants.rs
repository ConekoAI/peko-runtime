//! Cross-cutting tool/hook timing constants.
//!
//! Phase 9b.2 lifted `HOOK_TIMEOUT` out of root's
//! `agents::prompt::renderer` so the engine crate can import the same
//! timeout value without taking a root-only dep on `agents::*`. The
//! constant is a tool-execution concern (how long a hook handler may
//! block before its future is dropped), so `peko-tools-core` is the
//! natural home — it already owns `AbortSignal`, cancellation
//! bridging, and the rest of the tool-execution timing surface.

use std::time::Duration;

/// Maximum time a hook handler may block before its future is dropped.
///
/// Soft-fails open on timeout: the dispatcher logs and continues with
/// the original payload rather than tearing down the loop. This matches
/// the F31x `loop_per_hook_timeout_fails_open` policy documented in
/// `src/engine/agentic_loop.rs`.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(2);