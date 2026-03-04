//! Simple session persistence with OpenClaw-compatible JSONL format
//!
//! Wraps the new JSONL session storage for mandatory persistence.

use crate::engine::ToolCall;
use crate::session::jsonl::SessionStorage;
use crate::types::ContentBlock;
use anyhow::Result;
use std::path::PathBuf;

/// Simple session that auto-persists in OpenClaw-compatible format
pub struct SimpleSession {
    /// Session ID
    pub id: String,
    /// Storage
    storage: SessionStorage,
    /// Last message ID (for parent chaining)
    last_message_id: Option<String>,
}

impl SimpleSession {
    /// Create a new session for an agent
    pub async fn create(agent_name: &str) -> Result<Self> {
        let session_id = format!("{}_{}", agent_name, chrono::Utc::now().timestamp_millis());
        
        // Use agent-specific session directory
        let storage_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".pekobot")
            .join("agents")
            .join(agent_name)
            .join("sessions");
        
        let storage = SessionStorage::new(storage_dir);
        
        // Create session entry
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        storage.create_session(&session_id, cwd).await?;
        
        Ok(Self {
            id: session_id,
            storage,
            last_message_id: None,
        })
    }
    
    /// Add system message
    pub async fn add_system(
        &mut self,
        content: impl Into<String>,
    ) -> Result<()> {
        let msg_id = self.storage.append_message(
            &self.id,
            self.last_message_id.clone(),
            "system",
            vec![ContentBlock::Text { text: content.into() }],
        ).await?;
        self.last_message_id = Some(msg_id);
        Ok(())
    }
    
    /// Add user message
    pub async fn add_user(
        &mut self,
        content: impl Into<String>,
    ) -> Result<()> {
        let msg_id = self.storage.append_message(
            &self.id,
            self.last_message_id.clone(),
            "user",
            vec![ContentBlock::Text { text: content.into() }],
        ).await?;
        self.last_message_id = Some(msg_id);
        Ok(())
    }
    
    /// Add assistant message
    pub async fn add_assistant(
        &mut self,
        content: impl Into<String>,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> Result<()> {
        let content_str = content.into();
        
        let content_blocks = if let Some(calls) = tool_calls {
            // Build content blocks with tool calls
            let mut blocks = vec![];
            
            // Add text if present
            if !content_str.is_empty() {
                blocks.push(ContentBlock::Text { text: content_str });
            }
            
            // Add tool calls
            for (idx, call) in calls.iter().enumerate() {
                blocks.push(ContentBlock::ToolCall {
                    id: format!("call_{}_{}", self.id, idx),
                    name: call.name.clone(),
                    arguments: call.parameters.clone(),
                });
            }
            
            blocks
        } else {
            vec![ContentBlock::Text { text: content_str }]
        };
        
        let msg_id = self.storage.append_message(
            &self.id,
            self.last_message_id.clone(),
            "assistant",
            content_blocks,
        ).await?;
        self.last_message_id = Some(msg_id);
        Ok(())
    }
    
    /// Add tool result
    pub async fn add_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
    ) -> Result<()> {
        self.storage.append_tool_result(
            &self.id,
            &tool_call_id.into(),
            &tool_name.into(),
            result.into(),
            false,
        ).await?;
        // Tool results don't update last_message_id (they're not messages)
        Ok(())
    }
    
    /// Add thinking block (streaming reasoning)
    pub async fn add_thinking(
        &mut self,
        thinking: impl Into<String>,
        signature: Option<String>,
    ) -> Result<()> {
        let msg_id = self.storage.append_message(
            &self.id,
            self.last_message_id.clone(),
            "assistant",
            vec![ContentBlock::Thinking { 
                text: thinking.into(),
                signature,
            }],
        ).await?;
        self.last_message_id = Some(msg_id);
        Ok(())
    }
    
    /// Get context as text (for LLM)
    pub async fn get_context_text(&self, _limit: usize) -> String {
        // Load session entries and format as conversation context
        let entries = match self.storage.load_session(&self.id).await {
            Ok(e) => e,
            Err(_) => return format!("Session: {}", self.id),
        };
        
        let mut context = String::new();
        
        for entry in entries {
            match entry {
                crate::session::jsonl::SessionEntry::Message { message, .. } => {
                    let role = &message.role;
                    let mut parts: Vec<String> = Vec::new();
                    
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => parts.push(text.clone()),
                            ContentBlock::Thinking { text, .. } => parts.push(text.clone()),
                            ContentBlock::ToolCall { name, arguments, .. } => {
                                let args_str = serde_json::to_string(arguments).unwrap_or_default();
                                parts.push(format!("[ToolCall: {}({})]", name, args_str));
                            }
                            ContentBlock::ToolResult { content, .. } => {
                                let result_text: String = content.iter()
                                    .filter_map(|c| match c {
                                        ContentBlock::Text { text } => Some(text.clone()),
                                        _ => None,
                                    })
                                    .collect();
                                parts.push(format!("[ToolResult: {}]", result_text));
                            }
                            _ => {}
                        }
                    }
                    
                    let content_text = parts.join("\n");
                    if !content_text.is_empty() {
                        context.push_str(&format!("{}: {}\n\n", role, content_text));
                    }
                }
                crate::session::jsonl::SessionEntry::ToolResult { tool_name, content, .. } => {
                    let result_text: String = content.iter()
                        .filter_map(|c| match c {
                            ContentBlock::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    context.push_str(&format!("tool: [{} result: {}]\n\n", tool_name, result_text));
                }
                _ => {}
            }
        }
        
        if context.is_empty() {
            format!("Session: {}", self.id)
        } else {
            context
        }
    }
    
    /// Record model change
    pub async fn record_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<()> {
        let entry_id = self.storage.append_model_change(
            &self.id,
            self.last_message_id.clone(),
            provider,
            model_id,
        ).await?;
        // Model changes don't update last_message_id
        let _ = entry_id;
        Ok(())
    }
}
