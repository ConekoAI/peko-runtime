use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::RwLock;

use crate::agents::agent_config::{AgentConfig, PromptConfig, SystemFileConfig};
use crate::agents::Agent;
use crate::auth::Subject;
use crate::common::types::agent_legacy::ExtensionConfig;
use crate::common::types::message::LlmMessage;
use crate::providers::LlmResolver;
use crate::session::manager::SessionManager;
use crate::session::SessionCreateOptions;

use super::{agent_prompt::AgentPrompt, config::PrincipalCapabilities};

/// Build an `AgentConfig` from a thin Markdown prompt + Principal capabilities.
pub fn build_agent_config(
    prompt: &AgentPrompt,
    capabilities: &PrincipalCapabilities,
) -> AgentConfig {
    let enabled_extensions: Vec<String> = capabilities
        .tools
        .iter()
        .chain(capabilities.skills.iter())
        .chain(capabilities.mcps.iter())
        .cloned()
        .collect();

    let mut extensions = ExtensionConfig::default();
    extensions.enabled = enabled_extensions;

    AgentConfig {
        name: prompt.name.clone(),
        description: prompt.frontmatter.description.clone(),
        prompt: Some(PromptConfig {
            system: Some(SystemFileConfig {
                max_chars_per_file: 200_000,
                files: Some(vec![prompt.path.to_string_lossy().to_string()]),
            }),
        }),
        extensions: Some(extensions),
        // Inherit sensible defaults for the rest.
        ..AgentConfig::default()
    }
}

/// Run an agent prompt against an existing or new session under `sessions_dir`.
///
/// This is the Principal-layer adapter to the existing agent engine: it builds
/// a minimal `AgentConfig` from the prompt, cold-starts an `Agent`, resolves the
/// session, and runs the agentic loop.
pub async fn run_agent_prompt(
    prompt: &AgentPrompt,
    capabilities: &PrincipalCapabilities,
    peer: Subject,
    message: String,
    session_id: String,
    sessions_dir: PathBuf,
    resolver: Option<Arc<LlmResolver>>,
) -> Result<String> {
    let config = build_agent_config(prompt, capabilities);

    // Build a SessionManager scoped to the principal's sessions directory.
    let session_manager = SessionManager::new()
        .with_sessions_dir_internal(sessions_dir)
        .with_agent_name(&prompt.name)
        .with_peer_principal(peer.clone())
        .with_user(&peer.to_string());
    let session_manager = Arc::new(RwLock::new(session_manager));

    // Open or create the session. Do not hold the write lock across the
    // `open_session` await; otherwise a second write acquire on the same
    // task can deadlock with tokio's async RwLock.
    let maybe_handle = {
        let mut mgr = session_manager.write().await;
        mgr.open_session(&session_id).await?
    };
    let session = if let Some(handle) = maybe_handle {
        handle.base().clone()
    } else {
        let mut mgr = session_manager.write().await;
        let options = SessionCreateOptions::new().with_session_id(&session_id);
        let handle = mgr
            .create_session(&prompt.name, &peer, options)
            .await
            .context("failed to create session for principal")?;
        handle.base().clone()
    };

    // Load history.
    let history: Vec<LlmMessage> = session.read().await.load_history().await?;

    // Cold-start the agent.
    let agent = if let Some(r) = resolver {
        Agent::new_with_session_manager_and_resolver(config, session_manager, Some(r)).await?
    } else {
        Agent::new_with_session_manager(config, session_manager).await?
    };

    // Run the agentic loop.
    let result = agent
        .execute_with_session(
            &message,
            session,
            Some(history),
            |_event| {
                // Non-streaming: events are ignored.
            },
        )
        .await
        .context("agent execution failed")?;

    Ok(result.final_answer)
}

/// Generate a fresh session ID for a Principal-spawned session.
pub fn new_session_id() -> String {
    format!("sess_{}", uuid::Uuid::new_v4().simple())
}
