//! Agent State Machine - Tracks and manages agent execution state

use anyhow::{anyhow, Result};
use std::fmt;
use tracing::{debug, info};

/// Possible states for an agent
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    /// Idle, waiting for input
    Idle,
    /// Running a task
    Running,
    /// Paused (can resume)
    Paused,
    /// Stopping (graceful shutdown)
    Stopping,
    /// Error state
    Error,
    /// Completed a task
    Completed,
}

impl fmt::Display for AgentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentState::Idle => write!(f, "idle"),
            AgentState::Running => write!(f, "running"),
            AgentState::Paused => write!(f, "paused"),
            AgentState::Stopping => write!(f, "stopping"),
            AgentState::Error => write!(f, "error"),
            AgentState::Completed => write!(f, "completed"),
        }
    }
}

/// State transition definition
#[derive(Debug, Clone)]
pub struct StateTransition {
    /// From state
    pub from: AgentState,
    /// To state
    pub to: AgentState,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Optional reason
    pub reason: Option<String>,
}

/// State machine for managing agent lifecycle
pub struct StateMachine {
    /// Current state
    current: AgentState,
    /// State transition history
    history: Vec<StateTransition>,
    /// Maximum history size
    max_history: usize,
}

impl StateMachine {
    /// Create a new state machine with initial state
    pub fn new(initial: AgentState) -> Self {
        let mut sm = Self {
            current: initial,
            history: vec![],
            max_history: 100,
        };

        // Record initial state
        sm.history.push(StateTransition {
            from: initial,
            to: initial,
            timestamp: chrono::Utc::now(),
            reason: Some("Initial state".to_string()),
        });

        sm
    }

    /// Get current state
    pub fn current(&self) -> AgentState {
        self.current
    }

    /// Check if in a specific state
    pub fn is(&self, state: AgentState) -> bool {
        self.current == state
    }

    /// Check if can transition to a state
    pub fn can_transition(&self, to: AgentState) -> bool {
        match (self.current, to) {
            // Idle can go to running, or stay idle
            (AgentState::Idle, AgentState::Running) => true,
            (AgentState::Idle, AgentState::Idle) => true,

            // Running can go to paused, completed, error, or stopping
            (AgentState::Running, AgentState::Paused) => true,
            (AgentState::Running, AgentState::Completed) => true,
            (AgentState::Running, AgentState::Error) => true,
            (AgentState::Running, AgentState::Stopping) => true,

            // Paused can resume to running or stop
            (AgentState::Paused, AgentState::Running) => true,
            (AgentState::Paused, AgentState::Stopping) => true,

            // Stopping can only go to idle
            (AgentState::Stopping, AgentState::Idle) => true,
            (AgentState::Stopping, AgentState::Error) => true,

            // Error can go to idle (recovery)
            (AgentState::Error, AgentState::Idle) => true,

            // Completed can go to idle
            (AgentState::Completed, AgentState::Idle) => true,

            // Same state is always allowed (no-op)
            (from, to) if from == to => true,

            // Everything else is invalid
            _ => false,
        }
    }

    /// Attempt a state transition
    pub fn transition(&mut self, to: AgentState) -> Result<()> {
        if !self.can_transition(to) {
            return Err(anyhow!(
                "Invalid state transition: {} -> {}",
                self.current,
                to
            ));
        }

        let from = self.current;
        self.current = to;

        self.history.push(StateTransition {
            from,
            to,
            timestamp: chrono::Utc::now(),
            reason: None,
        });

        // Trim history if needed
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        debug!("State transition: {} -> {}", from, to);
        Ok(())
    }

    /// Transition with reason
    pub fn transition_with_reason(
        &mut self,
        to: AgentState,
        reason: impl Into<String>,
    ) -> Result<()> {
        if !self.can_transition(to) {
            return Err(anyhow!(
                "Invalid state transition: {} -> {}",
                self.current,
                to
            ));
        }

        let from = self.current;
        self.current = to;
        let reason_str = reason.into();

        self.history.push(StateTransition {
            from,
            to,
            timestamp: chrono::Utc::now(),
            reason: Some(reason_str.clone()),
        });

        // Trim history if needed
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        info!("State transition: {} -> {} ({})", from, to, reason_str);
        Ok(())
    }

    /// Get transition history
    pub fn history(&self) -> &[StateTransition] {
        &self.history
    }

    /// Time in current state
    pub fn time_in_current_state(&self) -> Option<chrono::Duration> {
        self.history.last().map(|t| {
            chrono::Utc::now().signed_duration_since(t.timestamp)
        })
    }

    /// Check if agent is active (running or paused)
    pub fn is_active(&self) -> bool {
        matches!(self.current, AgentState::Running | AgentState::Paused)
    }

    /// Check if agent can be stopped
    pub fn can_stop(&self) -> bool {
        matches!(
            self.current,
            AgentState::Running | AgentState::Paused | AgentState::Error
        )
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new(AgentState::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_machine_transitions() {
        let mut sm = StateMachine::new(AgentState::Idle);

        // Idle -> Running is valid
        assert!(sm.transition(AgentState::Running).is_ok());
        assert_eq!(sm.current(), AgentState::Running);

        // Running -> Idle is invalid (must go through completed/error/stopping)
        assert!(sm.transition(AgentState::Idle).is_err());

        // Running -> Completed is valid
        assert!(sm.transition(AgentState::Completed).is_ok());
        assert_eq!(sm.current(), AgentState::Completed);

        // Completed -> Idle is valid
        assert!(sm.transition(AgentState::Idle).is_ok());
        assert_eq!(sm.current(), AgentState::Idle);
    }

    #[test]
    fn test_can_transition() {
        let sm = StateMachine::new(AgentState::Idle);

        assert!(sm.can_transition(AgentState::Running));
        assert!(!sm.can_transition(AgentState::Completed)); // Can't skip running
    }

    #[test]
    fn test_is_active() {
        let mut sm = StateMachine::new(AgentState::Idle);
        assert!(!sm.is_active());

        sm.transition(AgentState::Running).unwrap();
        assert!(sm.is_active());

        sm.transition(AgentState::Paused).unwrap();
        assert!(sm.is_active());

        sm.transition(AgentState::Completed).unwrap();
        assert!(!sm.is_active());
    }
}
