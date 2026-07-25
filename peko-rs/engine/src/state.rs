//! Agent State Machine - Simple 2-state tracking

use std::sync::atomic::{AtomicU8, Ordering};
use tracing::debug;

/// Simple agent state: Idle or Busy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Waiting for work
    Idle,
    /// Processing a request
    Busy,
}

impl AgentState {
    /// Check if agent is busy
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(self, AgentState::Busy)
    }

    /// Check if agent is idle
    #[must_use]
    pub fn is_idle(self) -> bool {
        matches!(self, AgentState::Idle)
    }
}

/// Simple state machine using atomic for thread safety
pub struct StateMachine {
    /// Current state (0 = Idle, 1 = Busy)
    state: AtomicU8,
}

impl StateMachine {
    /// Create new state machine starting in Idle
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    /// Get current state
    pub fn current(&self) -> AgentState {
        match self.state.load(Ordering::Relaxed) {
            1 => AgentState::Busy,
            _ => AgentState::Idle,
        }
    }

    /// Check if in a specific state
    pub fn is(&self, state: AgentState) -> bool {
        self.current() == state
    }

    /// Transition to Idle
    pub fn set_idle(&self) {
        self.state.store(0, Ordering::Relaxed);
        debug!("Agent state: Idle");
    }

    /// Transition to Busy
    pub fn set_busy(&self) {
        self.state.store(1, Ordering::Relaxed);
        debug!("Agent state: Busy");
    }

    /// Try to acquire (Idle -> Busy)
    /// Returns true if successful
    pub fn try_acquire(&self) -> bool {
        self.state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_transitions() {
        let sm = StateMachine::new();

        assert!(sm.is(AgentState::Idle));
        assert!(!sm.is(AgentState::Busy));

        sm.set_busy();
        assert!(sm.is(AgentState::Busy));
        assert!(!sm.is(AgentState::Idle));

        sm.set_idle();
        assert!(sm.is(AgentState::Idle));
    }

    #[test]
    fn test_try_acquire() {
        let sm = StateMachine::new();

        // First acquire should succeed
        assert!(sm.try_acquire());
        assert!(sm.is(AgentState::Busy));

        // Second acquire should fail (already busy)
        assert!(!sm.try_acquire());

        // After releasing, can acquire again
        sm.set_idle();
        assert!(sm.try_acquire());
    }
}
