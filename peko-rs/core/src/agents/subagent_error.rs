//! Typed errors for subagent spawning
//!
//! Provides a structured error enum for spawn failures so that callers can
//! classify errors without fragile string matching.

/// Errors that can occur when spawning a subagent.
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnError {
    /// The spawn depth limit was exceeded.
    DepthLimitExceeded { current: u32, max: u32 },
    /// The concurrent subagent run limit was exceeded.
    ConcurrentLimitExceeded { current: usize, max: usize },
    /// The subagent execution timed out.
    Timeout { seconds: u64 },
    /// The subagent execution failed with an error message.
    ExecutionFailed(String),
    /// Phase 3 of `feature/multi-model-subagents` — the
    /// spawn-time pre-flight estimated cost for the call exceeds
    /// the principal's `cost_per_call_max`. `Eq` removed from the
    /// derive because `f64` (cost) doesn't implement `Eq`;
    /// callers compare fields individually.
    CostCeilingExceeded {
        /// Estimated cost in USD (positive).
        estimated: f64,
        /// Per-call ceiling in USD (positive).
        ceiling: f64,
        /// Model id of the chosen provider — for the error message.
        model_id: String,
    },
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
            SpawnError::CostCeilingExceeded {
                estimated,
                ceiling,
                model_id,
            } => {
                write!(
                    f,
                    "Per-spawn cost ceiling exceeded: ${:.4} estimated > ${:.4} ceiling for model '{}'",
                    estimated, ceiling, model_id
                )
            }
        }
    }
}

impl std::error::Error for SpawnError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_ceiling_display_shows_estimated_and_ceiling() {
        // The Display impl is the only signal the parent agent
        // sees when the pre-flight refuses a spawn. Round to
        // 4dp to match the format string.
        let err = SpawnError::CostCeilingExceeded {
            estimated: 0.5012,
            ceiling: 0.5,
            model_id: "claude-opus-4-8".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("$0.5012"));
        assert!(msg.contains("$0.5000"));
        assert!(msg.contains("claude-opus-4-8"));
    }

    #[test]
    fn cost_ceiling_partial_eq_ignores_f64_underflow() {
        // `Eq`/`PartialEq` removed from the derive because `f64`
        // doesn't implement `Eq`. Confirm the struct still
        // round-trips through clone + field access.
        let a = SpawnError::CostCeilingExceeded {
            estimated: 0.1,
            ceiling: 0.05,
            model_id: "haiku".into(),
        };
        let b = a.clone();
        match (a, b) {
            (
                SpawnError::CostCeilingExceeded {
                    estimated: e1,
                    ceiling: c1,
                    model_id: m1,
                },
                SpawnError::CostCeilingExceeded {
                    estimated: e2,
                    ceiling: c2,
                    model_id: m2,
                },
            ) => {
                assert!((e1 - e2).abs() < 1e-9);
                assert!((c1 - c2).abs() < 1e-9);
                assert_eq!(m1, m2);
            }
            _ => panic!("clone dropped variant"),
        }
    }
}
