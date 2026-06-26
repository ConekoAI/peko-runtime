use async_trait::async_trait;
use std::sync::Arc;

use crate::auth::Subject;
use crate::principal::{
    memory::{PrincipalMemory, SessionArtifact},
    router::{PrincipalRouter, RouteDecision, RouterContext, RouterError},
};

/// Deterministic `builtin:default` router.
///
/// - Resumes the most recent peer-specific session if one exists.
/// - Otherwise starts a fresh session with `principal.routing.default_agent`.
/// - Always rewrites the input message to include the caller identity.
pub struct BuiltinDefaultRouter {
    memory: Arc<dyn PrincipalMemory>,
}

impl BuiltinDefaultRouter {
    pub fn new(memory: Arc<dyn PrincipalMemory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl PrincipalRouter for BuiltinDefaultRouter {
    async fn route(
        &self,
        ctx: RouterContext,
    ) -> Result<RouteDecision, RouterError> {
        let default_agent = ctx.routing.default_agent.clone();

        // Find the most recent session for this peer, if any.
        let latest = self
            .memory
            .find_latest_session_for_peer(&ctx.peer)
            .await
            .map_err(|e| RouterError::AgentFailed(e.to_string()))?;

        let input_message = build_input_message(&ctx.peer, ctx.message);

        if let Some(SessionArtifact { session_id, .. }) = latest {
            return Ok(RouteDecision::Continue {
                target_agent: default_agent,
                input_message,
                resume_session_id: Some(session_id),
                context_injection: Vec::new(),
                synthesize: false,
                async_execution: false,
                timeout_seconds: None,
            });
        }

        Ok(RouteDecision::Spawn {
            target_agent: default_agent,
            input_message,
            context_injection: Vec::new(),
            synthesize: false,
            async_execution: false,
            timeout_seconds: None,
        })
    }
}

/// Build the input message seen by the target agent prompt.
/// Includes the caller identity so the agent prompt can personalize.
fn build_input_message(peer: &Subject, user_message: String) -> String {
    format!("Caller: {}\n\n{}", peer, user_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::{
        memory::{Artifact, MemoryError, PrincipalMemory, SessionArtifact},
        router::{ChannelContext, ChannelKind, RouterContext},
        PrincipalCapabilities, PrincipalGovernanceConfig, PrincipalIntentConfig,
        PrincipalRoutingConfig,
    };
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockMemory {
        sessions: Mutex<Vec<SessionArtifact>>,
    }

    impl MockMemory {
        fn empty() -> Self {
            Self {
                sessions: Mutex::new(Vec::new()),
            }
        }

        fn with_session(peer: Subject, session_id: &str) -> Self {
            let artifact = SessionArtifact {
                session_id: session_id.to_string(),
                peer,
                title: Some("test".to_string()),
                updated_at: chrono::Utc::now(),
                summary: Some("previous summary".to_string()),
            };
            Self {
                sessions: Mutex::new(vec![artifact]),
            }
        }
    }

    #[async_trait]
    impl PrincipalMemory for MockMemory {
        async fn record_session(&self, artifact: SessionArtifact) -> Result<(), MemoryError> {
            self.sessions.lock().unwrap().push(artifact);
            Ok(())
        }

        async fn find_latest_session_for_peer(
            &self,
            peer: &Subject,
        ) -> Result<Option<SessionArtifact>, MemoryError> {
            let sessions = self.sessions.lock().unwrap();
            let peer_key = peer.to_string();
            Ok(sessions
                .iter()
                .filter(|s| s.peer.to_string() == peer_key)
                .next_back()
                .cloned())
        }

        async fn list_sessions(&self) -> Result<Vec<SessionArtifact>, MemoryError> {
            Ok(self.sessions.lock().unwrap().clone())
        }

        async fn store(&self, _artifact: Artifact) -> Result<(), MemoryError> {
            Ok(())
        }

        async fn recall(&self, _query: &str, _k: usize) -> Result<Vec<Artifact>, MemoryError> {
            Ok(Vec::new())
        }

        async fn compact(
            &self,
        ) -> Result<crate::principal::memory::CompactSummary, MemoryError> {
            Ok(crate::principal::memory::CompactSummary {
                sessions_compacted: 0,
                memories_archived: 0,
            })
        }

        fn sessions_dir(&self) -> std::path::PathBuf {
            std::env::temp_dir()
        }

        fn router_session_path(&self) -> std::path::PathBuf {
            std::env::temp_dir().join("router.jsonl")
        }
    }

    fn test_context(peer: Subject, message: &str) -> RouterContext {
        RouterContext {
            principal_id: crate::principal::PrincipalId("prin_test".to_string()),
            principal_name: "test".to_string(),
            peer,
            message: message.to_string(),
            channel: ChannelContext {
                kind: ChannelKind::Cli,
                streaming: false,
            },
            routing: PrincipalRoutingConfig {
                default_agent: "primary".to_string(),
                strategy: crate::principal::RoutingStrategy::BuiltinDefault,
                context_window_messages: 20,
                recall_top_k: 5,
                max_router_iterations: 3,
            },
            recalled_context: Vec::new(),
            available_agents: Vec::new(),
            capabilities: PrincipalCapabilities::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
        }
    }

    #[tokio::test]
    async fn spawns_when_no_peer_session_exists() {
        let memory = Arc::new(MockMemory::empty());
        let router = BuiltinDefaultRouter::new(memory);
        let peer = Subject::User("alice".to_string());
        let ctx = test_context(peer.clone(), "hello");

        let decision = router.route(ctx).await.unwrap();
        match decision {
            RouteDecision::Spawn {
                target_agent,
                input_message,
                ..
            } => {
                assert_eq!(target_agent, "primary");
                assert!(input_message.contains("hello"));
                assert!(input_message.contains("user:alice"));
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continues_latest_peer_session() {
        let peer = Subject::User("alice".to_string());
        let memory = Arc::new(MockMemory::with_session(peer.clone(), "sess_123"));
        let router = BuiltinDefaultRouter::new(memory);
        let ctx = test_context(peer.clone(), "follow up");

        let decision = router.route(ctx).await.unwrap();
        match decision {
            RouteDecision::Continue {
                target_agent,
                resume_session_id,
                input_message,
                ..
            } => {
                assert_eq!(target_agent, "primary");
                assert_eq!(resume_session_id, Some("sess_123".to_string()));
                assert!(input_message.contains("follow up"));
            }
            other => panic!("expected Continue, got {other:?}"),
        }
    }
}
