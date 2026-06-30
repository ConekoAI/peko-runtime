//! Stateless Agent Service - Cold-start execution for stateless architecture
//!
//! This module provides the `StatelessAgentService` which replaces the `AgentPool`
//! in the stateless cold-start architecture. Instead of maintaining a pool of
//! running agents, it cold-starts agents on-demand for each request.
//!
//! ## Architecture
//!
//! - Load configuration from `ConfigRegistry` (fast, in-memory)
//! - Spawn new Agent instance per request
//! - Execute and return result
//! - Agent is dropped after execution (stateless)
//! - Session state is persisted separately

use crate::agents::Agent;
use crate::auth::Subject;
use crate::common::paths::PathResolver;
use crate::common::services::{ConfigAuthority, ConfigAuthorityImpl};
use crate::common::types::message::LlmMessage;
use crate::engine::AgenticEvent;
use crate::providers::TokenUsage;
use crate::session::manager::SessionManager;
use crate::session::types::ChannelType;
// Note: Session storage uses jsonl module directly
use crate::common::types::message::ContentBlock;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::timeout;
use tracing::{debug, info, instrument, warn};

// The MessageRequest/MessageResult types and AgentMessageService trait
// now live in common::types::a2a. The local names here are re-export
// aliases for internal ergonomics.
pub use crate::common::types::a2a::{
    A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult, ToolCallInfo,
};

/// Execution request for stateless agent
#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    /// Agent name (registered in `ConfigRegistry`)
    pub agent_name: String,
    /// Session ID for persistence
    pub session_id: String,
    /// User message to process
    pub message: String,
    /// Optional execution context
    pub context: Option<ExecutionContext>,
    /// Optional timeout override (defaults to service default)
    pub timeout_secs: Option<u64>,
    /// Resolved caller identity for session isolation.
    ///
    /// Empty by default — production callers **must** set this explicitly
    /// via [`ExecutionRequest::with_user`] before handing the request to
    /// the agentic loop. The legacy literal `"default"` was removed
    /// (issue #17) so that no production path can ever attribute a
    /// request to a placeholder user. Tests that don't care about
    /// per-user attribution can leave this empty.
    pub user: String,
    /// Caller agent name for A2A messaging (optional)
    pub caller_agent: Option<String>,
    /// Resolved caller principal for session peer attribution.
    ///
    /// When set, this takes precedence over [`ExecutionRequest::user`]
    /// when constructing the session peer (issue #24). `a2a_send` sets
    /// this to `Subject::Principal(caller_agent_name)` so the receiving
    /// agent's session is keyed under `agent:{caller}` (not
    /// `user:{caller}`), and the audit log / `PermissionGrant`
    /// attribution is type-correct.
    pub caller_principal: Option<Subject>,
}

impl ExecutionRequest {
    /// Create a simple execution request.
    ///
    /// `user` defaults to the empty string. Production code paths set
    /// this explicitly via [`ExecutionRequest::with_user`] so the
    /// agentic loop and audit log see a real, resolved caller. Tests
    /// can leave it empty.
    pub fn new(
        agent_name: impl Into<String>,
        session_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            session_id: session_id.into(),
            message: message.into(),
            context: None,
            timeout_secs: None,
            user: String::new(),
            caller_agent: None,
            caller_principal: None,
        }
    }

    /// Set execution context
    #[must_use]
    pub fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Set timeout
    #[must_use]
    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = Some(secs);
        self
    }

    /// Set caller agent name for A2A messaging
    #[must_use]
    pub fn with_caller_agent(mut self, caller: impl Into<String>) -> Self {
        self.caller_agent = Some(caller.into());
        self
    }

    /// Set caller agent from Option, filtering out empty strings
    #[must_use]
    pub fn with_caller_agent_opt(mut self, caller: Option<String>) -> Self {
        self.caller_agent = caller.filter(|s| !s.is_empty());
        self
    }

    /// Set the resolved caller principal (issue #24).
    ///
    /// Use this for A2A messaging paths where the caller is an agent,
    /// not a user. The principal is used to construct the session peer
    /// on the receiving agent so the session is keyed under
    /// `agent:{caller}` (not `user:{caller}`).
    #[must_use]
    pub fn with_caller_principal(mut self, principal: Subject) -> Self {
        self.caller_principal = Some(principal);
        self
    }

    /// Set the resolved caller principal from an Option, rejecting
    /// principals that cannot be a session peer (Team / Public).
    #[must_use]
    pub fn with_caller_principal_opt(mut self, principal: Option<Subject>) -> Self {
        self.caller_principal = principal.filter(|p| p.is_session_peer());
        self
    }
}

/// Execution context for request
#[derive(Debug, Clone, Default)]
pub struct ExecutionContext {
    /// Parent message ID for threading
    pub parent_message_id: Option<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Tool call record for response
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    /// Tool name
    pub name: String,
    /// Tool parameters
    pub parameters: serde_json::Value,
    /// Tool result (if available)
    pub result: Option<String>,
}

/// Execution result
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    /// Final response from agent
    pub response: String,
    /// Tool calls made during execution
    pub tool_calls: Vec<ToolCallRecord>,
    /// Token usage
    pub usage: TokenUsage,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
    /// Number of iterations
    pub iterations: usize,
    /// Whether execution succeeded
    pub success: bool,
    /// Error message (if failed)
    pub error: Option<String>,
}

/// Execution metrics for monitoring
#[derive(Debug, Clone, Default)]
pub struct ExecutionMetrics {
    /// Total executions
    pub total_executions: u64,
    /// Successful executions
    pub successful_executions: u64,
    /// Failed executions
    pub failed_executions: u64,
    /// Total execution time (ms)
    pub total_duration_ms: u64,
    /// Average cold-start time (ms)
    pub avg_cold_start_ms: u64,
}

/// Stateless agent service - cold-start execution
pub struct StatelessAgentService {
    /// Agent configuration service
    config_service: Arc<ConfigAuthorityImpl>,
    /// Default execution timeout
    default_timeout: Duration,
    /// Execution metrics
    metrics: RwLock<ExecutionMetrics>,
    /// Path resolver for agent data paths
    path_resolver: PathResolver,
    /// v3 LLM resolver. Required in production (every `peko send`
    /// goes through `LlmResolver::build`); may be `None` only in
    /// offline unit tests that don't exercise the LLM path.
    resolver: Option<Arc<crate::providers::LlmResolver>>,
}

// Cycle 5 (refactor/clippy-cleanup-rust196): implement the
// `AgentMessageService` trait defined in `common::types::a2a` so the
// tool can be constructed via the trait-object factory without
// depending on the concrete type. Delegates to the existing
// `execute_message` method; no behavior change. The trait lives in
// `common::types` and the impl lives in `agents` so the dependency
// arrow goes `agents → common` (one-way), breaking the cycle that
// would otherwise form via the concrete-type dependency.
#[async_trait::async_trait]
impl crate::common::types::a2a::AgentMessageService for StatelessAgentService {
    async fn execute_message(
        &self,
        req: crate::common::types::a2a::A2aMessageRequest,
    ) -> Result<crate::common::types::a2a::A2aMessageResponse> {
        // The local `MessageRequest`/`MessageResult` aliases are the
        // same types as `A2aMessageRequest`/`A2aMessageResponse`, so
        // we can forward `req` directly without an `.into()`.
        StatelessAgentService::execute_message(self, req).await
    }
}

impl StatelessAgentService {
    /// Create a new stateless agent service
    pub async fn new(
        config_service: Arc<ConfigAuthorityImpl>,
        path_resolver: PathResolver,
    ) -> Result<Self> {
        Self::new_with_resolver(config_service, path_resolver, None).await
    }

    /// Create a new stateless agent service with a v3 `LlmResolver`.
    ///
    /// Production path (`peko daemon`); every cold-started agent
    /// routes through `LlmResolver::build` so the agent never reads
    /// the inline-`[provider]` field. The unit-test constructor
    /// (`new(...)`) keeps an `Option::None` resolver for the few
    /// tests that don't exercise the LLM call path.
    pub async fn new_with_resolver(
        config_service: Arc<ConfigAuthorityImpl>,
        path_resolver: PathResolver,
        resolver: Option<Arc<crate::providers::LlmResolver>>,
    ) -> Result<Self> {
        let service = Self {
            config_service,
            default_timeout: Duration::from_mins(5), // 5 minutes default
            metrics: RwLock::new(ExecutionMetrics::default()),
            path_resolver,
            resolver,
        };

        info!(
            "StatelessAgentService initialized (resolver: {})",
            if service.resolver.is_some() {
                "wired"
            } else {
                "none"
            }
        );

        Ok(service)
    }

    /// The v3 `LlmResolver` if one was wired at construction time.
    #[must_use]
    pub fn resolver(&self) -> Option<&Arc<crate::providers::LlmResolver>> {
        self.resolver.as_ref()
    }

    /// Set default timeout
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Cold-start an `Agent` for an inbound request.
    ///
    /// In v3 this routes through `Agent::new_with_resolver` so the
    /// agent's `preferred_provider_id` / `preferred_model_id` resolve
    /// via the daemon's `LlmResolver` (catalog + keychain). If the
    /// service was constructed without a resolver (offline unit
    /// tests), it falls back to the deprecated `Agent::new` path
    /// that reads the inline `[provider]` block. The fallback is
    /// deleted in commit 2.
    async fn build_agent(
        &self,
        agent_name: &str,
        config: crate::agents::agent_config::AgentConfig,
    ) -> Result<Agent> {
        if let Some(resolver) = self.resolver.as_ref() {
            Agent::new_with_resolver(config, resolver.clone())
                .await
                .with_context(|| format!("Failed to create agent: {agent_name}"))
        } else {
            Agent::new(config)
                .await
                .with_context(|| format!("Failed to create agent: {agent_name}"))
        }
    }

    /// Load agent config fresh, bypassing any stale cache
    async fn load_config_fresh(
        &self,
        agent_name: &str,
    ) -> Result<crate::common::services::AgentConfigEntry> {
        // Invalidate cache to ensure we read the latest config from disk
        // This is critical for mid-session tool enable/disable changes made by CLI
        self.config_service.invalidate_cache(agent_name).await;

        let entry = self
            .config_service
            .get(agent_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", agent_name))?;

        Ok(entry)
    }

    /// Execute a message and return a blocking response
    ///
    /// This is the high-level interface that replaces `MessageService::send_message()`
    /// It handles session resolution and executes the agent, returning the complete result.
    ///
    /// # Arguments
    /// * `request` - The message request containing agent name, message, and session options
    ///
    /// # Returns
    /// A `MessageResult` containing the response, session info, and execution metadata
    pub async fn execute_message(&self, request: MessageRequest) -> Result<MessageResult> {
        let start = Instant::now();

        // Resolve session using SessionManager (single authority)
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            &request.user,
        );
        if let Some(principal) = request.caller_principal.as_ref() {
            session_manager = session_manager.with_peer_principal(principal.clone());
        }

        let resolved = session_manager
            .resolve_session(
                &request.agent_name,
                ChannelType::Http,
                "default",
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = resolved.session_id.clone();
        let is_new_session = resolved.is_new;

        // Build execution request
        let exec_request = ExecutionRequest {
            agent_name: request.agent_name.clone(),
            session_id: session_id.clone(),
            message: request.message,
            context: None,
            timeout_secs: request.timeout_secs,
            user: request.user.clone(),
            caller_agent: request.caller_agent,
            caller_principal: request.caller_principal,
        };

        // Execute via stateless service
        let exec_result = self.execute(exec_request).await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match exec_result {
            Ok(result) => {
                // Convert tool calls
                let tool_calls: Vec<ToolCallInfo> = result
                    .tool_calls
                    .into_iter()
                    .map(|tc| ToolCallInfo {
                        id: format!("tool_{}", uuid::Uuid::new_v4().simple()),
                        name: tc.name,
                        parameters: tc.parameters,
                        result: tc.result,
                    })
                    .collect();

                Ok(MessageResult {
                    content: result.response,
                    session_id,
                    is_new_session,
                    usage: result.usage,
                    tool_calls,
                    duration_ms,
                    iterations: result.iterations,
                    success: result.success,
                    error: result.error,
                })
            }
            Err(e) => Err(e),
        }
    }

    /// Execute a message and return an `EventStream` for streaming
    ///
    /// This is the high-level streaming interface that replaces `MessageService::send_message_unified()`
    /// It handles session resolution and executes the agent, returning a stream of events.
    ///
    /// # Arguments
    /// * `request` - The message request containing agent name, message, and session options
    ///
    /// # Returns
    /// An `EventStream` containing the receiver for events and completion signal
    pub async fn execute_message_streaming(
        &self,
        request: MessageRequest,
    ) -> Result<crate::engine::EventStream> {
        let start = Instant::now();

        // Resolve session using SessionManager (single authority)
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            &request.user,
        );
        if let Some(principal) = request.caller_principal.as_ref() {
            session_manager = session_manager.with_peer_principal(principal.clone());
        }

        let resolved = session_manager
            .resolve_session(
                &request.agent_name,
                ChannelType::Http,
                "default",
                request.session_id.clone(),
                request.new_session,
            )
            .await?;

        let session_id = resolved.session_id.clone();
        let is_new_session = resolved.is_new;

        // Build execution request
        let exec_request = ExecutionRequest {
            agent_name: request.agent_name.clone(),
            session_id: session_id.clone(),
            message: request.message,
            context: None,
            timeout_secs: request.timeout_secs,
            user: request.user.clone(),
            caller_agent: request.caller_agent,
            caller_principal: request.caller_principal,
        };

        // Get base session for execution
        let base_session = resolved.handle.base().clone();

        // Execute streaming - directly returns EventStream
        let event_stream = self
            .execute_streaming_with_session(exec_request, base_session)
            .await?;

        let duration_ms = start.elapsed().as_millis() as u64;
        info!(
            "Streaming setup complete for '{}' in {}ms",
            request.agent_name, duration_ms
        );

        Ok(crate::engine::EventStream {
            receiver: event_stream.receiver,
            completion: event_stream.completion,
            session_id,
            is_new_session,
        })
    }

    /// Execute agent with cold start
    #[instrument(skip(self, request), fields(agent = %request.agent_name, session = %request.session_id))]
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let start = Instant::now();
        let timeout_duration = request
            .timeout_secs
            .map_or(self.default_timeout, Duration::from_secs);

        info!(
            "Starting cold execution for agent '{}' (timeout: {:?})",
            request.agent_name, timeout_duration
        );

        // Execute with timeout
        let result = timeout(timeout_duration, self.execute_inner(request)).await;

        let duration = start.elapsed();

        match result {
            Ok(Ok(result)) => {
                self.update_metrics(true, duration).await;
                Ok(result)
            }
            Ok(Err(e)) => {
                self.update_metrics(false, duration).await;
                Err(e)
            }
            Err(_) => {
                self.update_metrics(false, duration).await;
                Err(anyhow::anyhow!(
                    "Execution timeout after {timeout_duration:?}"
                ))
            }
        }
    }

    /// Inner execution (without timeout wrapper)
    async fn execute_inner(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
        let start = Instant::now();

        // 1. Load agent configuration fresh from disk
        // ADR-019: Invalidate cache to pick up mid-session tool config changes
        let config_entry = self.load_config_fresh(&request.agent_name).await?;

        // 2. Open the specific session by ID
        // This ensures we write to the correct session file
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            &request.user,
        );
        if let Some(principal) = request.caller_principal.as_ref() {
            session_manager = session_manager.with_peer_principal(principal.clone());
        }

        // Try to open existing session, create if not exists
        let session =
            if let Some(handle) = session_manager.open_session(&request.session_id).await? {
                debug!("Opened existing session '{}'", request.session_id);
                handle.base().clone()
            } else {
                debug!("Session '{}' not found, creating new", request.session_id);
                // Issue #24: prefer the resolved caller principal (e.g.
                // `Subject::Principal("helper")` for a2a_send) over the
                // legacy `Subject::User(user)` masquerade. The principal
                // sets the session key correctly and makes audit /
                // permission-grant attribution type-safe.
                let peer = request
                    .caller_principal
                    .clone()
                    .unwrap_or_else(|| Subject::User(request.user.clone()));
                let options = crate::session::SessionCreateOptions::new()
                    .with_trigger("api")
                    .with_session_id(&request.session_id);
                let handle = session_manager
                    .create_session(&request.agent_name, &peer, options)
                    .await?;

                handle.base().clone()
            };

        // 3. Load session history from the opened session
        let history = self.load_session_history(session.clone()).await?;

        // 4. Cold-start agent (spawn)
        let agent = self
            .build_agent(&request.agent_name, config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // 5. Build the prompt with optional caller annotation
        // ADR-023 Phase 2: caller_agent is structured on ExecutionRequest;
        // the annotation is prepended to the message so the target agent sees it.
        let prompt = match request.caller_agent.as_deref() {
            Some(caller) if !caller.is_empty() => {
                format!("[Message from agent: {caller}]\n\n{}", request.message)
            }
            _ => request.message,
        };

        // 6. Invoke ChannelInput hook — extensions may transform the prompt or session
        // ADR-025 Phase 4: Consume ChannelInput result to transform the prompt
        let prompt = match agent
            .extension_core()
            .invoke_hook(
                crate::extensions::framework::core::HookPoint::ChannelInput,
                crate::extensions::framework::types::HookInput::Unit,
            )
            .await
        {
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            ) => {
                debug!("ChannelInput hook transformed prompt");
                transformed
            }
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Json(json),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Json(json),
            ) => {
                // If the hook returns JSON with a "message" or "prompt" field, use it
                if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
                    debug!("ChannelInput hook transformed prompt from JSON");
                    msg.to_string()
                } else if let Some(p) = json.get("prompt").and_then(|v| v.as_str()) {
                    debug!("ChannelInput hook transformed prompt from JSON");
                    p.to_string()
                } else {
                    prompt
                }
            }
            other => {
                debug!(
                    "ChannelInput hook result: {:?}",
                    std::mem::discriminant(&other)
                );
                prompt
            }
        };

        // 7. Execute agent with session and history
        // Use the new execute_with_session method that properly handles session resumption
        let execute_result = agent
            .execute_with_session(&prompt, session.clone(), Some(history), |_event| {
                // Events are ignored for non-streaming execution
                // All data comes from the AgenticResult
            })
            .await;

        let (success, final_response, token_usage, iterations, tool_calls, error_msg) =
            match execute_result {
                Ok(result) => {
                    // Convert ContentBlock tool calls to ToolCallRecord
                    let tool_calls: Vec<ToolCallRecord> = result
                        .tool_calls
                        .into_iter()
                        .filter_map(|block| {
                            if let ContentBlock::ToolCall {
                                name, arguments, ..
                            } = block
                            {
                                Some(ToolCallRecord {
                                    name,
                                    parameters: arguments,
                                    result: None,
                                })
                            } else {
                                None
                            }
                        })
                        .collect();

                    (
                        result.success,
                        result.final_answer,
                        result.usage,
                        result.iterations,
                        tool_calls,
                        None,
                    )
                }
                Err(e) => {
                    warn!("Agent execution failed: {}", e);
                    (
                        false,
                        String::new(),
                        TokenUsage::default(),
                        0,
                        Vec::new(),
                        Some(e.to_string()),
                    )
                }
            };

        // 8. Invoke ChannelOutput hook — extensions may transform the result
        // ADR-025 Phase 4: Consume ChannelOutput result to transform the response
        let final_response = match agent
            .extension_core()
            .invoke_hook(
                crate::extensions::framework::core::HookPoint::ChannelOutput,
                crate::extensions::framework::types::HookInput::Unit,
            )
            .await
        {
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            ) => {
                debug!("ChannelOutput hook transformed response");
                transformed
            }
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Json(json),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Json(json),
            ) => {
                if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
                    debug!("ChannelOutput hook transformed response from JSON");
                    msg.to_string()
                } else if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                    debug!("ChannelOutput hook transformed response from JSON");
                    text.to_string()
                } else {
                    final_response
                }
            }
            other => {
                debug!(
                    "ChannelOutput hook result: {:?}",
                    std::mem::discriminant(&other)
                );
                final_response
            }
        };

        // Note: Session persistence is handled by the engine loop via Session.
        // The engine loop's session.add_user() and session.add_assistant() methods
        // already write to the session file during execution.
        // We do NOT write here to avoid duplication and format conflicts.

        let duration = start.elapsed();

        info!(
            "Execution complete for '{}' (success: {}, duration: {}ms, iterations: {}, tokens: {}/{})",
            request.agent_name,
            success,
            duration.as_millis(),
            iterations,
            token_usage.input,
            token_usage.output
        );

        // 7. Agent is dropped here (stateless)
        //
        // ADR-020: If DaemonHttpTransport is active, async tasks live in the daemon
        // and survive CLI exit. If LocalAsyncTransport is active, tasks are in-process
        // and will be dropped. The global ExtensionCore is initialized with the
        // appropriate transport in main.rs::init_extension_core().

        Ok(ExecutionResult {
            response: final_response,
            tool_calls,
            usage: token_usage,
            duration_ms: duration.as_millis() as u64,
            iterations,
            success,
            error: error_msg,
        })
    }

    /// Execute streaming - returns `EventStream` with completion signal
    ///
    /// This is the low-level streaming interface. For high-level usage,
    /// prefer `execute_message_streaming()` which handles session resolution.
    #[instrument(skip(self, request), fields(agent = %request.agent_name))]
    pub async fn execute_streaming(
        &self,
        request: ExecutionRequest,
    ) -> Result<crate::engine::EventStream> {
        // Load config, history, agent, session - these are all Send-safe
        let config_entry = self
            .config_service
            .get(&request.agent_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Agent not found: {}", request.agent_name))?;

        let _agent = self
            .build_agent(&request.agent_name, config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // Open the specific session by ID (same logic as execute_inner)
        let mut session_manager = SessionManager::for_cli(
            self.path_resolver.clone(),
            &request.agent_name,
            &request.user,
        );
        if let Some(principal) = request.caller_principal.as_ref() {
            session_manager = session_manager.with_peer_principal(principal.clone());
        }

        let session =
            if let Some(handle) = session_manager.open_session(&request.session_id).await? {
                debug!(
                    "Opened existing session '{}' for streaming",
                    request.session_id
                );
                handle.base().clone()
            } else {
                debug!(
                    "Session '{}' not found, creating new for streaming",
                    request.session_id
                );
                // Issue #24: prefer the resolved caller principal (e.g.
                // `Subject::Principal("helper")` for a2a_send) over the
                // legacy `Subject::User(user)` masquerade.
                let peer = request
                    .caller_principal
                    .clone()
                    .unwrap_or_else(|| Subject::User(request.user.clone()));
                let options = crate::session::SessionCreateOptions::new()
                    .with_trigger("api")
                    .with_session_id(&request.session_id);
                let handle = session_manager
                    .create_session(&request.agent_name, &peer, options)
                    .await?;

                handle.base().clone()
            };

        // Delegate to the internal method (which loads history itself)
        self.execute_streaming_with_session(request, session).await
    }

    /// Execute streaming with an already-resolved session
    ///
    /// This is the internal implementation used by both `execute_streaming`
    /// and `execute_message_streaming`. It uses `tokio::spawn` (not `spawn_blocking`)
    /// for single-runtime execution and provides completion signals.
    #[instrument(skip(self, request, session), fields(agent = %request.agent_name))]
    async fn execute_streaming_with_session(
        &self,
        request: ExecutionRequest,
        session: Arc<RwLock<crate::session::unified::Session>>,
    ) -> Result<crate::engine::EventStream> {
        let message = match request.caller_agent.as_deref() {
            Some(caller) if !caller.is_empty() => {
                format!("[Message from agent: {caller}]\n\n{}", request.message)
            }
            _ => request.message,
        };
        let prompt = self.build_prompt(&message, &[])?;

        // Load history for the agent
        let history = self.load_session_history(session.clone()).await?;

        // Load agent config fresh from disk
        // ADR-019: Invalidate cache to pick up mid-session tool config changes
        let config_entry = self.load_config_fresh(&request.agent_name).await?;

        let agent = self
            .build_agent(&request.agent_name, config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {}", request.agent_name))?;

        // Invoke ChannelInput hook — extensions may transform the prompt
        // ADR-025 Phase 4: Consume ChannelInput result for streaming path
        let prompt = match agent
            .extension_core()
            .invoke_hook(
                crate::extensions::framework::core::HookPoint::ChannelInput,
                crate::extensions::framework::types::HookInput::Unit,
            )
            .await
        {
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Text(transformed),
            ) => {
                debug!("ChannelInput hook transformed prompt (streaming)");
                transformed
            }
            crate::extensions::framework::types::HookResult::Continue(
                crate::extensions::framework::types::HookOutput::Json(json),
            )
            | crate::extensions::framework::types::HookResult::Replace(
                crate::extensions::framework::types::HookOutput::Json(json),
            ) => {
                if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
                    debug!("ChannelInput hook transformed prompt from JSON (streaming)");
                    msg.to_string()
                } else if let Some(p) = json.get("prompt").and_then(|v| v.as_str()) {
                    debug!("ChannelInput hook transformed prompt from JSON (streaming)");
                    p.to_string()
                } else {
                    prompt
                }
            }
            other => {
                debug!(
                    "ChannelInput hook result (streaming): {:?}",
                    std::mem::discriminant(&other)
                );
                prompt
            }
        };

        // Create channels
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<AgenticEvent>(1000);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel::<Result<()>>();

        // Spawn task on main runtime (NOT spawn_blocking)
        // This ensures session writes complete before completion signal
        let extension_core = agent.extension_core();
        let event_tx_for_error = event_tx.clone();
        tokio::spawn(async move {
            let on_event = move |event: AgenticEvent| {
                tracing::info!(
                    "EventStream: sending event: {:?}",
                    std::mem::discriminant(&event)
                );
                let _ = event_tx.try_send(event);
            };

            // Execute and collect result
            // Issue #17: thread the resolved caller identity into the
            // agentic loop so every tool call carries the caller through
            // to `HookInput::ToolCall`. The empty placeholder
            // (`MessageRequest::user` default) and the dispatcher's
            // `"anonymous"` fallback both map to `None` so downstream
            // per-user permission checks (issue #17 follow-up) don't
            // mis-attribute local / unverified invocations to a fake user.
            let caller_id = match request.user.as_str() {
                "" | "anonymous" => None,
                other => Some(other.to_string()),
            };
            let result = agent
                .execute_streaming_with_session(
                    &prompt,
                    session,
                    Some(history.clone()),
                    caller_id,
                    on_event,
                )
                .await;

            // Surface hard agent-loop failures (e.g. no provider configured)
            // as a stream error instead of silently returning success.
            if let Err(e) = result {
                let run_id = agent.name().to_string();
                let _ = event_tx_for_error.try_send(crate::engine::AgenticEvent::Lifecycle {
                    run_id,
                    phase: crate::engine::LifecyclePhase::Error,
                    error: Some(format!("Agent execution failed: {e:#}")),
                });
            }

            // Invoke ChannelOutput hook — extensions may process or log the result
            // Note: For streaming, the response has already been sent via events.
            // The hook can still perform side effects (logging, metrics, etc.)
            let channel_output_result = extension_core
                .invoke_hook(
                    crate::extensions::framework::core::HookPoint::ChannelOutput,
                    crate::extensions::framework::types::HookInput::Unit,
                )
                .await;
            debug!(
                "ChannelOutput hook result (streaming): {:?}",
                std::mem::discriminant(&channel_output_result)
            );

            // At this point, all session writes have completed because
            // add_assistant/add_tool_result are synchronous

            // Signal completion: background work is done and session is in a consistent state.
            // Stream execution status is already communicated via events.
            let _ = completion_tx.send(Ok(()));
            // event_tx is dropped here, signaling end of stream
        });

        Ok(crate::engine::EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id: request.session_id,
            is_new_session: false,
        })
    }

    /// Run a new agentic loop for an existing session whose
    /// [`crate::session::InboxRegistry`] has just received a steering
    /// message. The handler (`ipc::server::handle_session_steer`)
    /// pushes the steering item into the registry's inbox, acquires
    /// the per-session run permit, and calls this method.
    ///
    /// Semantics:
    /// - Resolves the agent from the session record (`session.agent_name`).
    /// - Loads history from disk via `load_session_history`.
    /// - Builds a fresh `AgenticLoop` with the same per-session inbox
    ///   the executor already pushes into (looked up from
    ///   `inbox_registry` by `session.id`).
    /// - Skips the user-message persistence step (the handler already
    ///   persisted the steering content via `session.add_user`).
    /// - Spawns the loop on the current Tokio runtime and returns an
    ///   `EventStream`. The `run_permit` is moved into the spawn
    ///   future so it is released exactly when the spawn future
    ///   returns — i.e. when the run is fully drained to the sink.
    /// - The empty-inbox guard is enforced inside the loop via the
    ///   post-drain `messages` check (no `User` role → no LLM call).
    pub async fn run_session_on_inbox(
        &self,
        session: Arc<RwLock<crate::session::Session>>,
        inbox_registry: Arc<crate::session::InboxRegistry>,
        run_permit: crate::session::RunPermitGuard,
        caller_id: Option<String>,
    ) -> Result<crate::engine::EventStream> {
        let session_id = session.read().await.id.clone();
        let agent_name = session.read().await.agent_name.clone();
        info!(
            "run_session_on_inbox: starting new loop for agent '{}' session '{}'",
            agent_name, session_id
        );

        // Load agent config + history up front (these are Send-safe).
        let config_entry = self.load_config_fresh(&agent_name).await?;
        let history = self.load_session_history(session.clone()).await?;
        let agent = self
            .build_agent(&agent_name, config_entry.config.clone())
            .await
            .with_context(|| format!("Failed to create agent: {agent_name}"))?;

        // Create channels.
        let (event_tx, event_rx) = tokio::sync::mpsc::channel::<crate::engine::AgenticEvent>(1000);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel::<Result<()>>();

        let event_tx_for_error = event_tx.clone();
        let session_for_spawn = session.clone();
        let session_id_for_log = session_id.clone();

        tokio::spawn(async move {
            // The run_permit lives for the duration of this spawn
            // future. When the future returns (after the loop ends
            // and the last event_tx.try_send call), the permit is
            // released and the registry reports the session as idle.
            let _permit = run_permit;

            let on_event = move |event: crate::engine::AgenticEvent| {
                let _ = event_tx.try_send(event);
            };

            let result = agent
                .run_streaming_with_session_skip_user_add(
                    on_event,
                    session_for_spawn,
                    Some(history),
                    caller_id,
                )
                .await;

            if let Err(e) = result {
                let _ = event_tx_for_error.try_send(crate::engine::AgenticEvent::Lifecycle {
                    run_id: agent.name().to_string(),
                    phase: crate::engine::LifecyclePhase::Error,
                    error: Some(format!("Steering run failed: {e:#}")),
                });
            }

            // Drop the agent and inbox_registry only after the loop
            // returns. We keep `session_id_for_log` for diagnostics
            // and let the daemon-side handlers do their work.
            let _ = (session_id_for_log, inbox_registry);

            let _ = completion_tx.send(Ok(()));
            // event_tx drops here, closing the stream.
        });

        Ok(crate::engine::EventStream {
            receiver: event_rx,
            completion: completion_rx,
            session_id,
            is_new_session: false,
        })
    }

    /// Get current metrics
    pub async fn metrics(&self) -> ExecutionMetrics {
        self.metrics.read().await.clone()
    }

    /// Load session history from storage
    ///
    /// Uses the session's native `load_history` to preserve full `ContentBlock` fidelity
    /// (including `ToolCall` and `ToolResult` blocks), instead of the lossy normalized format.
    async fn load_session_history(
        &self,
        session: Arc<RwLock<crate::session::Session>>,
    ) -> Result<Vec<LlmMessage>> {
        let messages = session.read().await.load_history().await?;
        Ok(messages)
    }

    /// Build prompt with conversation history
    fn build_prompt(&self, message: &str, history: &[LlmMessage]) -> Result<String> {
        // For now, just return the message
        // In a more complex implementation, this would format the full conversation
        // including system prompts, history, and the new message
        if history.is_empty() {
            Ok(message.to_string())
        } else {
            // Append the new message to history context
            // The agent's system prompt handles the conversation format
            Ok(message.to_string())
        }
    }

    /// Update execution metrics
    async fn update_metrics(&self, success: bool, duration: Duration) {
        let mut metrics = self.metrics.write().await;
        metrics.total_executions += 1;
        metrics.total_duration_ms += duration.as_millis() as u64;

        if success {
            metrics.successful_executions += 1;
        } else {
            metrics.failed_executions += 1;
        }

        // Calculate running average of cold-start time
        // Note: This is a simplified calculation
        if metrics.total_executions > 0 {
            metrics.avg_cold_start_ms = metrics.total_duration_ms / metrics.total_executions;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_execution_request_builder() {
        let request = ExecutionRequest::new("test-agent", "test-session", "Hello")
            .with_timeout(60)
            .with_context(ExecutionContext {
                parent_message_id: Some("parent-123".to_string()),
                metadata: [("key".to_string(), "value".to_string())]
                    .into_iter()
                    .collect(),
            });

        assert_eq!(request.agent_name, "test-agent");
        assert_eq!(request.session_id, "test-session");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.timeout_secs, Some(60));
        assert!(request.context.is_some());
    }

    #[tokio::test]
    async fn test_stateless_service_creation() {
        let temp_dir = TempDir::new().unwrap();

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));

        let service = StatelessAgentService::new(config_service, path_resolver)
            .await
            .unwrap();

        let metrics = service.metrics().await;
        assert_eq!(metrics.total_executions, 0);
    }

    // ====================================================================================
    // MessageRequest builder tests (ADR-016 Phase 1)
    // ====================================================================================

    #[test]
    fn test_message_request_builder() {
        let request = MessageRequest::new("my-agent", "Hello")
            .with_session("sess_123")
            .with_new_session(false)
            .with_timeout(60);

        assert_eq!(request.agent_name, "my-agent");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.session_id, Some("sess_123".to_string()));
        assert!(!request.new_session);
        assert_eq!(request.timeout_secs, Some(60));
    }

    #[test]
    fn test_message_request_builder_defaults() {
        let request = MessageRequest::new("my-agent", "Hello");

        assert_eq!(request.agent_name, "my-agent");
        assert_eq!(request.message, "Hello");
        assert_eq!(request.session_id, None);
        assert!(!request.new_session);
        assert_eq!(request.timeout_secs, None);
    }

    #[test]
    fn test_message_request_with_session_opt() {
        // Test with Some
        let request1 =
            MessageRequest::new("agent", "hi").with_session_opt(Some("session-id".to_string()));
        assert_eq!(request1.session_id, Some("session-id".to_string()));

        // Test with None
        let request2 = MessageRequest::new("agent", "hi").with_session_opt(None);
        assert_eq!(request2.session_id, None);
    }

    // ====================================================================================
    // Service method tests (ADR-016 Phase 1)
    // ====================================================================================

    #[test]
    fn test_message_request_caller_agent_opt_filters_empty() {
        let req1 = MessageRequest::new("agent", "hi")
            .with_caller_agent_opt(Some("researcher".to_string()));
        assert_eq!(req1.caller_agent, Some("researcher".to_string()));

        let req2 = MessageRequest::new("agent", "hi").with_caller_agent_opt(Some(String::new()));
        assert_eq!(req2.caller_agent, None);

        let req3 = MessageRequest::new("agent", "hi").with_caller_agent_opt(None);
        assert_eq!(req3.caller_agent, None);
    }

    #[test]
    fn test_execution_request_caller_agent_opt_filters_empty() {
        let req1 = ExecutionRequest::new("agent", "session", "hi")
            .with_caller_agent_opt(Some("researcher".to_string()));
        assert_eq!(req1.caller_agent, Some("researcher".to_string()));

        let req2 = ExecutionRequest::new("agent", "session", "hi")
            .with_caller_agent_opt(Some(String::new()));
        assert_eq!(req2.caller_agent, None);

        let req3 = ExecutionRequest::new("agent", "session", "hi").with_caller_agent_opt(None);
        assert_eq!(req3.caller_agent, None);
    }

    #[tokio::test]
    async fn test_with_default_timeout() {
        let temp_dir = TempDir::new().unwrap();

        let path_resolver = PathResolver::with_dirs(
            temp_dir.path().join("config"),
            temp_dir.path().join("data"),
            temp_dir.path().join("cache"),
        );

        let config_service = Arc::new(ConfigAuthorityImpl::new(path_resolver.clone()));
        let service = StatelessAgentService::new(config_service, path_resolver)
            .await
            .unwrap();

        // Verify default timeout is 300 seconds (5 minutes)
        // We can't directly check the private field, but we can verify
        // the method returns self correctly
        let service_with_timeout = service.with_default_timeout(std::time::Duration::from_mins(2));
        // Just verify it compiles and returns
        drop(service_with_timeout);
    }

    // ====================================================================================
    // ToolCallInfo tests (ADR-016 Phase 1)
    // ====================================================================================

    #[test]
    fn test_tool_call_info_creation() {
        let tool_call = ToolCallInfo {
            id: "tool_123".to_string(),
            name: "Read".to_string(),
            parameters: serde_json::json!({"path": "/tmp/test"}),
            result: Some("File contents".to_string()),
        };

        assert_eq!(tool_call.id, "tool_123");
        assert_eq!(tool_call.name, "Read");
        assert_eq!(tool_call.result, Some("File contents".to_string()));
    }

    #[test]
    fn test_tool_call_info_without_result() {
        let tool_call = ToolCallInfo {
            id: "tool_456".to_string(),
            name: "web_search".to_string(),
            parameters: serde_json::json!({"query": "rust"}),
            result: None,
        };

        assert_eq!(tool_call.result, None);
    }
}
