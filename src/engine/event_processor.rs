//! Event processor for converting `AgenticEvents` to channel actions
//!
//! This module provides a reusable, interface-agnostic way to process
//! agentic events and convert them to presentation-agnostic actions.
//! Channels can then execute these actions in a platform-specific way.

use crate::engine::AgenticEvent;

/// Actions that can be executed by any channel
///
/// These actions are platform-agnostic. Channels translate these
/// to platform-specific operations (print to console, send HTTP
/// response, post to Discord, etc.)
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelAction {
    /// Print text content (no newline)
    Print(String),
    /// Print text with newline
    Println(String),
    /// Print status/indicator message
    Status(String),
    /// Flush output buffer
    Flush,
    /// Start of a new assistant turn
    StartTurn(String), // agent name
    /// End of assistant turn
    EndTurn,
}

/// Processes `AgenticEvents` and produces `ChannelActions`
///
/// This struct maintains state across events to produce the correct
/// sequence of actions. It handles:
/// - Tracking whether we're in an interstitial (pre-tool) phase
/// - Managing turn boundaries
/// - Formatting status messages
#[derive(Debug, Clone)]
pub struct EventProcessor {
    state: ProcessorState,
    config: ProcessorConfig,
}

#[derive(Debug, Clone, Default)]
struct ProcessorState {
    /// Current sequence number for tracking
    sequence: usize,
    /// Whether we've started the current turn
    has_started_turn: bool,
    /// Whether we're currently in an interstitial phase (before tool calls)
    is_interstitial: bool,
    /// Last content was interstitial (needs newline before final)
    last_was_interstitial: bool,
}

/// Configuration for event processing
#[derive(Debug, Clone)]
pub struct ProcessorConfig {
    /// Agent name to display
    pub agent_name: String,
    /// Show thinking/reasoning content
    pub show_thinking: bool,
    /// Show tool execution status
    pub show_tools: bool,
    /// Replace newlines with spaces in thinking content
    pub thinking_single_line: bool,
}

impl Default for ProcessorConfig {
    fn default() -> Self {
        Self {
            agent_name: "Assistant".to_string(),
            show_thinking: true,
            show_tools: true,
            thinking_single_line: true,
        }
    }
}

impl EventProcessor {
    /// Create a new event processor with default config
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(ProcessorConfig::default())
    }

    /// Create a new event processor with custom config
    #[must_use]
    pub fn with_config(config: ProcessorConfig) -> Self {
        Self {
            state: ProcessorState::default(),
            config,
        }
    }

    /// Create a processor for a specific agent
    #[must_use]
    pub fn for_agent(agent_name: impl Into<String>) -> Self {
        let mut config = ProcessorConfig::default();
        config.agent_name = agent_name.into();
        Self::with_config(config)
    }

    /// Process a single event and return actions to execute
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let mut processor = EventProcessor::for_agent("testagent");
    ///
    /// // Interstitial text (before tool calls)
    /// let actions = processor.process(&AgenticEvent::AssistantText {
    ///     run_id: "run_1".to_string(),
    ///     text: "Let me check that...".to_string(),
    ///     sequence: 1,
    ///     is_interstitial: true,
    /// });
    /// // actions = [StartTurn("testagent"), Print("Let me check that...")]
    ///
    /// // After tool execution, final answer
    /// let actions = processor.process(&AgenticEvent::AssistantText {
    ///     run_id: "run_1".to_string(),
    ///     text: "The answer is 42.".to_string(),
    ///     sequence: 2,
    ///     is_interstitial: false,
    /// });
    /// // actions = [Println("The answer is 42."), EndTurn]
    /// ```
    pub fn process(&mut self, event: &AgenticEvent) -> Vec<ChannelAction> {
        let mut actions = Vec::new();

        match event {
            AgenticEvent::AssistantText {
                text,
                is_interstitial,
                sequence,
                ..
            } => {
                self.state.sequence = *sequence;
                self.process_assistant_text(text, *is_interstitial, &mut actions);
            }
            AgenticEvent::Thinking { text, .. } => {
                if self.config.show_thinking && !text.is_empty() {
                    self.process_thinking(text, &mut actions);
                }
            }
            AgenticEvent::ToolStart { name: _, .. } => {
                if self.config.show_tools {
                    // End current line if we're in interstitial mode
                    if self.state.has_started_turn && self.state.is_interstitial {
                        actions.push(ChannelAction::EndTurn);
                        self.state.has_started_turn = false;
                    }
                    self.state.is_interstitial = false;
                    self.state.last_was_interstitial = true;
                }
            }
            AgenticEvent::ToolEnd { .. } => {
                // Tool finished, next assistant text starts fresh
            }
            AgenticEvent::Lifecycle { phase, .. } => {
                use crate::engine::LifecyclePhase;
                match phase {
                    LifecyclePhase::End => {
                        if self.state.has_started_turn {
                            actions.push(ChannelAction::EndTurn);
                            self.state.has_started_turn = false;
                        }
                    }
                    LifecyclePhase::Error => {
                        if self.state.has_started_turn {
                            actions.push(ChannelAction::EndTurn);
                            self.state.has_started_turn = false;
                        }
                    }
                    _ => {}
                }
            }
            AgenticEvent::AssistantDelta {
                text,
                is_interstitial,
                sequence,
                ..
            } => {
                self.state.sequence = *sequence;
                // AssistantDelta is for streaming mode - always use Print (no newline)
                self.process_assistant_delta(text, *is_interstitial, &mut actions);
            }
            AgenticEvent::Usage { .. } => {
                // Usage stats - channels can choose to display or ignore
            }
            _ => {
                // Other events don't produce immediate actions
            }
        }

        actions
    }

    /// Process streaming delta event (`AssistantDelta`)
    ///
    /// Unlike `AssistantText`, deltas are incremental and should be printed
    /// without newlines to enable real-time streaming display.
    fn process_assistant_delta(
        &mut self,
        text: &str,
        is_interstitial: bool,
        actions: &mut Vec<ChannelAction>,
    ) {
        if text.is_empty() {
            return;
        }

        // Start turn if needed
        if !self.state.has_started_turn {
            actions.push(ChannelAction::StartTurn(self.config.agent_name.clone()));
            self.state.has_started_turn = true;
        }

        // Deltas are always printed inline (no newline) for streaming
        actions.push(ChannelAction::Print(text.to_string()));
        actions.push(ChannelAction::Flush);

        // Track interstitial state for when we transition to final
        self.state.is_interstitial = is_interstitial;
        self.state.last_was_interstitial = is_interstitial;
    }

    /// Process new-style `AssistantText` event
    fn process_assistant_text(
        &mut self,
        text: &str,
        is_interstitial: bool,
        actions: &mut Vec<ChannelAction>,
    ) {
        if text.is_empty() {
            return;
        }

        // Start turn if needed
        if !self.state.has_started_turn {
            actions.push(ChannelAction::StartTurn(self.config.agent_name.clone()));
            self.state.has_started_turn = true;
        }

        // If transitioning from interstitial to final, add newline
        if self.state.last_was_interstitial && !is_interstitial {
            actions.push(ChannelAction::Println(String::new()));
        }

        if is_interstitial {
            // Interstitial text: print inline (tool calls coming)
            actions.push(ChannelAction::Print(text.to_string()));
            actions.push(ChannelAction::Flush);
            self.state.is_interstitial = true;
            self.state.last_was_interstitial = true;
        } else {
            // Final text: print with newline
            actions.push(ChannelAction::Println(text.to_string()));
            actions.push(ChannelAction::EndTurn);
            self.state.has_started_turn = false;
            self.state.is_interstitial = false;
            self.state.last_was_interstitial = false;
        }
    }

    /// Process thinking content
    fn process_thinking(&mut self, text: &str, actions: &mut Vec<ChannelAction>) {
        if text.is_empty() {
            return;
        }

        // Start turn if needed
        if !self.state.has_started_turn {
            actions.push(ChannelAction::StartTurn(self.config.agent_name.clone()));
            self.state.has_started_turn = true;
        }

        // Format thinking content
        let formatted = if self.config.thinking_single_line {
            text.replace('\n', " ")
        } else {
            text.to_string()
        };

        actions.push(ChannelAction::Print(formatted));
        actions.push(ChannelAction::Flush);
        self.state.last_was_interstitial = false; // Thinking is separate from interstitial
    }

    /// Reset processor state (e.g., for a new conversation)
    pub fn reset(&mut self) {
        self.state = ProcessorState::default();
    }

    /// Get current sequence number
    #[must_use]
    pub fn sequence(&self) -> usize {
        self.state.sequence
    }

    /// Check if we're currently in an interstitial phase
    #[must_use]
    pub fn is_interstitial(&self) -> bool {
        self.state.is_interstitial
    }
}

impl Default for EventProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_assistant_text(text: &str, sequence: usize, is_interstitial: bool) -> AgenticEvent {
        AgenticEvent::AssistantText {
            run_id: "run_1".to_string(),
            text: text.to_string(),
            sequence,
            is_interstitial,
        }
    }

    #[test]
    fn test_interstitial_then_final() {
        let mut processor = EventProcessor::for_agent("testagent");

        // First: interstitial text (before tool calls)
        let actions = processor.process(&make_assistant_text("Let me search for that...", 1, true));
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], ChannelAction::StartTurn(_)));
        assert!(
            matches!(actions[1], ChannelAction::Print(ref t) if t == "Let me search for that...")
        );
        assert!(matches!(actions[2], ChannelAction::Flush));
        assert!(processor.is_interstitial());

        // Then: final answer
        // Transition from interstitial to final adds an empty newline first
        let actions = processor.process(&make_assistant_text("Found it!", 2, false));
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], ChannelAction::Println(ref t) if t.is_empty()));
        assert!(matches!(actions[1], ChannelAction::Println(ref t) if t == "Found it!"));
        assert!(matches!(actions[2], ChannelAction::EndTurn));
        assert!(!processor.is_interstitial());
    }

    #[test]
    fn test_final_answer_only() {
        let mut processor = EventProcessor::for_agent("testagent");

        let actions = processor.process(&make_assistant_text("Hello world", 1, false));
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], ChannelAction::StartTurn(_)));
        assert!(matches!(actions[1], ChannelAction::Println(ref t) if t == "Hello world"));
        assert!(matches!(actions[2], ChannelAction::EndTurn));
    }

    #[test]
    fn test_tool_start_ends_interstitial() {
        let mut processor = EventProcessor::for_agent("testagent");

        // Interstitial text
        processor.process(&make_assistant_text("Checking...", 1, true));
        assert!(processor.is_interstitial());

        // Tool start should end the turn
        let actions = processor.process(&AgenticEvent::ToolStart {
            run_id: "run_1".to_string(),
            tool_id: "tool_1".to_string(),
            name: "search".to_string(),
            params: serde_json::json!({}),
        });
        assert!(matches!(actions[0], ChannelAction::EndTurn));
    }

}
