//! Provider trait

use async_trait::async_trait;
use tokio::sync::mpsc;

/// LLM Provider trait
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Complete a prompt (legacy/simple interface)
    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        self.chat(prompt, "default", 0.7).await
    }

    /// Chat with optional system prompt (zeroclaw-compatible interface)
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String>;

    /// Simple chat interface
    async fn chat(&self, message: &str, model: &str, temperature: f64) -> anyhow::Result<String> {
        self.chat_with_system(None, message, model, temperature)
            .await
    }

    /// Warm up the HTTP connection pool
    async fn warmup(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Stream completion with events
    ///
    /// Default implementation falls back to blocking `complete()`
    /// and emits a single Assistant event at the end.
    async fn complete_stream(
        &self,
        _prompt: &str,
        event_tx: mpsc::Sender<crate::engine::AgenticEvent>,
        run_id: String,
    ) -> anyhow::Result<()> {
        // Default: fall back to blocking completion
        use crate::engine::{AgenticEvent, LifecyclePhase};

        // Emit start event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Start,
                error: None,
            })
            .await;

        // Emit running event
        let _ = event_tx
            .send(AgenticEvent::Lifecycle {
                run_id: run_id.clone(),
                phase: LifecyclePhase::Running,
                error: None,
            })
            .await;

        // Do blocking completion
        match self.complete(_prompt).await {
            Ok(response) => {
                // Emit assistant event
                let _ = event_tx
                    .send(AgenticEvent::Assistant {
                        run_id: run_id.clone(),
                        text: response,
                        is_delta: false,
                        is_final: true,
                    })
                    .await;

                // Emit end event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id,
                        phase: LifecyclePhase::End,
                        error: None,
                    })
                    .await;

                Ok(())
            }
            Err(e) => {
                // Emit error event
                let _ = event_tx
                    .send(AgenticEvent::Lifecycle {
                        run_id,
                        phase: LifecyclePhase::Error,
                        error: Some(e.to_string()),
                    })
                    .await;

                Err(e)
            }
        }
    }
}
