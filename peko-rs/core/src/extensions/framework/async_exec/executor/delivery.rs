//! Result delivery mechanisms for async task completion

use super::event_bus::AsyncTaskCompletionEvent;
use super::queue::SharedAsyncResultQueueManager;
use super::registry::AsyncTaskEntry;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Trait for formatting tool results into announcement messages.
///
/// Each tool can register its own formatter; the default formatter
/// produces a generic JSON block.
pub trait ResultFormatter: Send + Sync {
    fn format(&self, tool_name: &str, result: &Value) -> String;
}

/// Default formatter for tools without a registered custom formatter.
pub struct DefaultResultFormatter;

impl ResultFormatter for DefaultResultFormatter {
    fn format(&self, tool_name: &str, result: &Value) -> String {
        format!(
            "## {} Result\n\n```json\n{}\n```",
            tool_name,
            serde_json::to_string_pretty(result).unwrap_or_default()
        )
    }
}

/// Registry of result formatters keyed by tool name.
pub struct FormatterRegistry {
    formatters: HashMap<String, Box<dyn ResultFormatter>>,
    default: Box<dyn ResultFormatter>,
}

impl Default for FormatterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatterRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            formatters: HashMap::new(),
            default: Box::new(DefaultResultFormatter),
        }
    }

    pub fn register(&mut self, tool_name: impl Into<String>, formatter: Box<dyn ResultFormatter>) {
        self.formatters.insert(tool_name.into(), formatter);
    }

    pub fn format(&self, tool_name: &str, result: &Value) -> String {
        self.formatters
            .get(tool_name)
            .map(|f| f.format(tool_name, result))
            .unwrap_or_else(|| self.default.format(tool_name, result))
    }
}

/// Build a completion event from a task entry.
/// Shared by all delivery mechanisms to ensure consistent formatting.
pub fn build_completion_event(task: &AsyncTaskEntry) -> AsyncTaskCompletionEvent {
    let result_message = task
        .formatted_result
        .clone()
        .or_else(|| task.result.as_ref().map(|r| task.format_result(r)))
        .unwrap_or_else(|| format!("Task {} completed with no result", task.task_id));

    AsyncTaskCompletionEvent {
        task_id: task.task_id.clone(),
        tool_name: task.tool_name.clone(),
        result_message,
        parent_session_key: task.parent_session_key.clone(),
        label: task.config.label.clone(),
    }
}

/// Trait for result delivery mechanisms
#[async_trait::async_trait]
pub trait ResultDelivery: Send + Sync {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()>;
    fn clone_box(&self) -> Box<dyn ResultDelivery>;
}

impl Clone for Box<dyn ResultDelivery> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Queue-based delivery mechanism
#[derive(Debug, Clone)]
pub struct QueueDelivery {
    queue_manager: SharedAsyncResultQueueManager,
}

impl QueueDelivery {
    #[must_use]
    pub fn new(queue_manager: SharedAsyncResultQueueManager) -> Self {
        Self { queue_manager }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for QueueDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        let event = build_completion_event(task);

        let mut manager = self.queue_manager.write().await;
        manager.enqueue(event);

        tracing::debug!(
            "Queued result for task {} in session {}",
            task.task_id,
            task.parent_session_key
        );

        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        Box::new(self.clone())
    }
}

/// Direct channel delivery mechanism
#[derive(Debug)]
pub struct ChannelDelivery {
    sender: tokio::sync::mpsc::Sender<AsyncTaskCompletionEvent>,
}

impl ChannelDelivery {
    #[must_use]
    pub fn new(sender: tokio::sync::mpsc::Sender<AsyncTaskCompletionEvent>) -> Self {
        Self { sender }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for ChannelDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        let event = build_completion_event(task);

        self.sender
            .send(event)
            .await
            .map_err(|_| anyhow::anyhow!("Failed to send result via channel - receiver dropped"))?;

        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        panic!("ChannelDelivery cannot be cloned - use QueueDelivery for multi-consumer scenarios");
    }
}

/// Callback-based delivery mechanism
pub struct CallbackDelivery {
    callback: Arc<
        dyn Fn(&AsyncTaskEntry) -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync,
    >,
}

impl std::fmt::Debug for CallbackDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackDelivery")
            .field("callback", &"<async fn>")
            .finish()
    }
}

impl CallbackDelivery {
    pub fn new<F, Fut>(callback: F) -> Self
    where
        F: Fn(&AsyncTaskEntry) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<()>> + Send + 'static,
    {
        let callback = Arc::new(move |entry: &AsyncTaskEntry| {
            let fut = callback(entry);
            Box::pin(fut) as futures::future::BoxFuture<'static, Result<()>>
        });

        Self { callback }
    }
}

impl Clone for CallbackDelivery {
    fn clone(&self) -> Self {
        Self {
            callback: Arc::clone(&self.callback),
        }
    }
}

#[async_trait::async_trait]
impl ResultDelivery for CallbackDelivery {
    async fn deliver(&self, task: &AsyncTaskEntry) -> Result<()> {
        (self.callback)(task).await
    }

    fn clone_box(&self) -> Box<dyn ResultDelivery> {
        Box::new(self.clone())
    }
}
