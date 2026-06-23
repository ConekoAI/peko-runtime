//! Typed errors for subagent spawning
//!
//! Provides a structured error enum for spawn failures so that callers can
//! classify errors without fragile string matching.

/// Errors that can occur when spawning a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// The spawn depth limit was exceeded.
    DepthLimitExceeded { current: u32, max: u32 },
    /// The concurrent subagent run limit was exceeded.
    ConcurrentLimitExceeded { current: usize, max: usize },
    /// The subagent execution timed out.
    Timeout { seconds: u64 },
    /// The subagent execution failed with an error message.
    ExecutionFailed(String),
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpawnError::DepthLimitExceeded { current, max } => {
                write!(f, "Maximum spawn depth exceeded: {current} (max: {max})")
            }
            SpawnError::ConcurrentLimitExceeded { current, max } => {
                write!(
                    f,
                    "Maximum concurrent subagent runs exceeded: {current} (max: {max})"
                )
            }
            SpawnError::Timeout { seconds } => {
                write!(f, "Subagent execution timed out after {seconds} seconds")
            }
            SpawnError::ExecutionFailed(msg) => {
                write!(f, "Subagent execution failed: {msg}")
            }
        }
    }
}

impl std::error::Error for SpawnError {}
