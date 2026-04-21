//! Agent execution orchestration
//!
//! This module provides `AgentExecutor`, which handles all execution methods
//! for an `Agent`. It takes `Arc<Agent>` at construction, eliminating the need
//! for `clone_for_loop()`.

use crate::engine::loop_v4::AgenticLoopV4;
use crate::engine::{AgenticEvent, AgenticResultV4, OrchestratorConfig};
use crate::extensions::core::ExtensionCore;
use crate::providers::Provider;
use crate::session::UnifiedSession;
use crate::types::agent::AgentState;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Execution orchestrator for an Agent.
///
/// Holds `Arc<Agent>` + provider + extension_core and owns all `execute*` methods.
/// This eliminates the need for `clone_for_loop()` by design.
pub struct AgentExecutor {
    agent: Arc<crate::agent::Agent>,
    provider: Arc<Provider>,
    extension_core: Arc<ExtensionCore>,
}

impl AgentExecutor {
    /// Create a new `AgentExecutor`.
    ///
    /// # Arguments
    /// * `agent` - The agent to execute on (shared via Arc)
    /// * `provider` - The LLM provider to use
    /// * `extension_core` - The extension core for tool registration
    pub fn new(
        agent: Arc<crate::agent::Agent>,
        provider: Arc<Provider>,
        extension_core: Arc<ExtensionCore>,
    ) -> Self {
        Self {
            agent,
            provider,
            extension_core,
        }
    }

    /// Execute a task with the LLM provider using the unified callback API.
    ///
    /// The `on_event` callback receives streaming events (thinking tokens,
    /// tool calls, lifecycle events, etc.). Use `|_| {}` if you don't need
    /// streaming updates.
    ///
    /// Returns the final result including the answer and tool calls made.
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResultV4> {
        if self.agent.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.agent.state()
            ));
        }

        self.agent.set_state(AgentState::Busy);

        let result = if let Err(e) = self.prepare_execution().await {
            self.agent.set_state(AgentState::Idle);
            return Err(e);
        };

        let supports_native = self.provider.supports_native_tools();
        info!(
            "Executing with {} tool calling",
            if supports_native {
                "native"
            } else {
                "text-based"
            }
        );

        let loop_ = AgenticLoopV4::new(
            Arc::clone(&self.agent),
            Arc::clone(&self.provider),
            Arc::clone(&self.extension_core),
        )
        .await;

        let result = match loop_.run(prompt, on_event).await {
            Ok(result) => Ok(result),
            Err(e) => {
                error!("Agentic loop V4 error: {}", e);
                Err(e)
            }
        };

        self.agent.set_state(AgentState::Idle);
        result
    }

    /// Execute with a specific session and history.
    ///
    /// This allows resuming an existing conversation with full context.
    /// The session is used for persistence, and history provides the conversation context.
    pub async fn execute_with_session(
        &self,
        prompt: &str,
        session: Arc<RwLock<UnifiedSession>>,
        history: Option<Vec<crate::providers::ChatMessage>>,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResultV4> {
        if self.agent.state() != AgentState::Idle {
            return Err(anyhow::anyhow!(
                "Agent is not idle (current state: {:?})",
                self.agent.state()
            ));
        }

        self.agent.set_state(AgentState::Busy);

        // Initialize tool config on ExtensionCore
        let tool_config = self.agent.config.tools.clone().unwrap_or_default();
        self.extension_core.set_tool_config(tool_config).await;

        let result = if let Err(e) = self.prepare_execution().await {
            self.agent.set_state(AgentState::Idle);
            return Err(e);
        };

        let supports_native = self.provider.supports_native_tools();
        info!(
            "Executing with session and {} tool calling",
            if supports_native {
                "native"
            } else {
                "text-based"
            }
        );

        let loop_ = AgenticLoopV4::new(
            Arc::clone(&self.agent),
            Arc::clone(&self.provider),
            Arc::clone(&self.extension_core),
        )
        .await;

        let result = match loop_
            .run_with_resume(prompt, on_event, session, history)
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                error!("Agentic loop V4 error: {}", e);
                Err(e)
            }
        };

        self.agent.set_state(AgentState::Idle);
        result
    }

    /// Execute with a channel-based event interface.
    ///
    /// Convenience wrapper around `execute()` that returns a receiver for
    /// async event streaming. Use this when you need to process events
    /// in a separate async context.
    pub async fn execute_streaming(
        &self,
        prompt: &str,
    ) -> Result<tokio::sync::mpsc::Receiver<AgenticEvent>> {
        // Use a large buffer to prevent event loss during bursts
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(10000);

        // Spawn the execution in a task
        let prompt = prompt.to_string();

        self.prepare_execution().await?;

        let agent_arc = Arc::clone(&self.agent);
        let provider_arc = Arc::clone(&self.provider);
        let event_tx_clone = event_tx.clone();
        let extension_core = Arc::clone(&self.extension_core);

        tokio::task::spawn_local(async move {
            let loop_ = AgenticLoopV4::new(agent_arc, provider_arc, extension_core).await;

            let _result = loop_
                .run(&prompt, move |event| {
                    // Try to send event - log if dropped (buffer full means consumer is slow)
                    if event_tx_clone.try_send(event).is_err() {
                        warn!("Agent event dropped (channel full)");
                    }
                })
                .await;
        });

        Ok(event_rx)
    }

    /// Execute with streaming support using the provided session.
    ///
    /// The session must be provided by the caller (typically via `SessionManager`).
    /// This ensures session lifecycle is managed centrally.
    ///
    /// This version takes a sender callback for event streaming, avoiding channel
    /// lifetime issues. The callback is invoked synchronously for each event.
    pub async fn execute_streaming_with_session<F>(
        &self,
        prompt: &str,
        session: Arc<RwLock<UnifiedSession>>,
        history: Option<Vec<crate::providers::ChatMessage>>,
        on_event: F,
    ) -> Result<AgenticResultV4>
    where
        F: Fn(AgenticEvent) + Send + Sync + 'static,
    {
        // Initialize tool config on ExtensionCore (mirrors AgentRunner behavior)
        let tool_config = self.agent.config.tools.clone().unwrap_or_default();
        self.extension_core.set_tool_config(tool_config).await;

        // Capture current session ID so session_status can look it up
        {
            let session_id = session.read().await.id.clone();
            let current_session_id = self.agent.current_session_id();
            let mut current = current_session_id.write().await;
            *current = Some(session_id);
        }

        self.prepare_execution().await?;

        let loop_ = AgenticLoopV4::new(
            Arc::clone(&self.agent),
            Arc::clone(&self.provider),
            Arc::clone(&self.extension_core),
        )
        .await;

        // Use streaming config with Live delivery mode for real-time output
        let streaming_config = OrchestratorConfig::live();

        loop_
            .run_streaming_with_resume(prompt, on_event, session, history, streaming_config)
            .await
    }

    /// Prepare agent for execution by initializing built-in tools and invoking `AgentInit` hooks.
    async fn prepare_execution(&self) -> anyhow::Result<()> {
        if let Err(e) = self.agent.init_builtins_async().await {
            return Err(anyhow::anyhow!("Failed to initialize tools: {e}"));
        }

        let init_result = self
            .extension_core
            .invoke_hook(
                crate::extensions::core::HookPoint::AgentInit,
                crate::extensions::types::HookInput::Unit,
            )
            .await;
        tracing::info!(
            "AgentInit hook result: {:?}",
            std::mem::discriminant(&init_result)
        );

        Ok(())
    }
}
