//! Principal-owned workspace hooks, dispatched in registration order (ADR-066 D3).
//! Six points survive. Tool selectors are exact names or absent (all tools);
//! hooks cannot veto execution. Each handler has a two-second soft-fail budget.

use crate::extensions::workspace_io::{HookInput, HookOutput, HookResult, ToolRuntimeContext};
use async_trait::async_trait;
use futures::FutureExt;
use peko_subject::PrincipalId;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceHookPoint {
    PreToolUse { tool_name: Option<String> },
    PostToolUse { tool_name: Option<String> },
    Stop,
    AfterAgent,
    PromptSection { section: String },
    SessionContextBuild,
}

impl WorkspaceHookPoint {
    pub fn name(&self) -> String {
        match self {
            Self::PreToolUse { tool_name } => {
                format!("tool.pre.{}", tool_name.as_deref().unwrap_or("all"))
            }
            Self::PostToolUse { tool_name } => {
                format!("tool.post.{}", tool_name.as_deref().unwrap_or("all"))
            }
            Self::Stop => "agent.stop".into(),
            Self::AfterAgent => "agent.after".into(),
            Self::PromptSection { section } => format!("prompt.section.{section}"),
            Self::SessionContextBuild => "session.context.build".into(),
        }
    }

    fn matches(&self, point: &Self) -> bool {
        match (self, point) {
            (
                Self::PreToolUse {
                    tool_name: selector,
                },
                Self::PreToolUse { tool_name },
            )
            | (
                Self::PostToolUse {
                    tool_name: selector,
                },
                Self::PostToolUse { tool_name },
            ) => selector.is_none() || selector == tool_name,
            (Self::PromptSection { section }, Self::SessionContextBuild) => {
                section == "session_context"
            }
            _ => self == point,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceHookContext {
    pub point: WorkspaceHookPoint,
    pub input: HookInput,
    pub runtime: ToolRuntimeContext,
}

impl WorkspaceHookContext {
    pub fn new(point: WorkspaceHookPoint, input: HookInput) -> Self {
        let mut runtime = ToolRuntimeContext::new();
        match &input {
            HookInput::ToolCall {
                principal_id,
                principal_name,
                workspace,
                agent_id,
                session_id,
                ..
            } => {
                runtime.principal_id = principal_id.clone();
                runtime.principal_name = principal_name.clone();
                runtime.workspace = workspace.clone();
                runtime.agent_id = agent_id.clone();
                runtime.session_id = session_id.clone();
            }
            HookInput::Json(payload) => {
                runtime.principal_id = payload["principal_id"].as_str().map(str::to_owned);
                runtime.workspace = payload["workspace"].as_str().map(str::to_owned);
                runtime.agent_id = payload["agent_did"].as_str().map(str::to_owned);
                runtime.session_id = payload["session_id"].as_str().map(str::to_owned);
            }
            _ => {}
        }
        Self {
            point,
            input,
            runtime,
        }
    }
}

#[async_trait]
pub trait WorkspaceHookHandler: Send + Sync + std::fmt::Debug {
    async fn handle(&self, context: WorkspaceHookContext) -> HookResult;
}

#[derive(Clone)]
struct Binding {
    principal_id: PrincipalId,
    point: WorkspaceHookPoint,
    handler: Arc<dyn WorkspaceHookHandler>,
}

#[derive(Default)]
pub struct WorkspaceHookDispatcher {
    bindings: tokio::sync::RwLock<Vec<Binding>>,
}

impl WorkspaceHookDispatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register_hook(
        &self,
        point: WorkspaceHookPoint,
        handler: Arc<dyn WorkspaceHookHandler>,
        principal_id: &PrincipalId,
    ) -> anyhow::Result<()> {
        if let WorkspaceHookPoint::PreToolUse {
            tool_name: Some(name),
        }
        | WorkspaceHookPoint::PostToolUse {
            tool_name: Some(name),
        } = &point
        {
            anyhow::ensure!(
                !name.is_empty() && !name.contains(['*', '?']),
                "tool_name must be an exact name; omit it to observe all tools"
            );
        }
        self.bindings.write().await.push(Binding {
            principal_id: principal_id.clone(),
            point,
            handler,
        });
        Ok(())
    }

    pub async fn hook_count(&self) -> usize {
        self.bindings.read().await.len()
    }

    pub async fn prompt_sections(&self, principal_id: &PrincipalId) -> Vec<String> {
        self.bindings
            .read()
            .await
            .iter()
            .filter(|binding| {
                binding.principal_id == *principal_id
                    || binding.principal_id == *PrincipalId::system()
            })
            .filter_map(|binding| match &binding.point {
                WorkspaceHookPoint::PromptSection { section } => Some(section.clone()),
                _ => None,
            })
            .collect()
    }

    pub async fn invoke_hook(&self, point: WorkspaceHookPoint, input: HookInput) -> HookResult {
        self.invoke_hook_with_context(WorkspaceHookContext::new(point, input))
            .await
    }

    pub async fn invoke_hook_with_context(&self, context: WorkspaceHookContext) -> HookResult {
        // Drop the registration lock before running commands so a handler can
        // never hold up another principal's discovery or dispatch.
        let bindings = self.bindings.read().await.clone();
        let mut outputs = Vec::new();
        for binding in bindings {
            if binding.principal_id != *PrincipalId::system()
                && context.runtime.principal_id.as_deref() != Some(binding.principal_id.0.as_str())
            {
                continue;
            }
            if !binding.point.matches(&context.point) {
                continue;
            }
            let result = tokio::time::timeout(
                peko_tools_core::HOOK_TIMEOUT,
                std::panic::AssertUnwindSafe(binding.handler.handle(context.clone()))
                    .catch_unwind(),
            )
            .await;
            match result {
                Ok(Ok(HookResult::Continue(output) | HookResult::Replace(output))) => {
                    outputs.push(output)
                }
                Ok(Ok(HookResult::Error(error))) => {
                    tracing::warn!(%error, "Workspace hook failed; continuing")
                }
                Ok(Err(_)) => tracing::warn!("Workspace hook panicked; continuing"),
                Err(_) => tracing::warn!("Workspace hook timed out; continuing"),
                _ => {} // Handled cannot short-circuit subsequent observers.
            }
        }
        if outputs.is_empty() {
            HookResult::PassThrough
        } else {
            HookResult::Continue(HookOutput::Vec(outputs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy)]
    enum Outcome {
        Text,
        Handled,
        Error,
        Panic,
        Timeout,
    }
    #[derive(Debug)]
    struct Recorder {
        label: &'static str,
        outcome: Outcome,
        seen: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait]
    impl WorkspaceHookHandler for Recorder {
        async fn handle(&self, context: WorkspaceHookContext) -> HookResult {
            self.seen.lock().unwrap().push(format!(
                "{}:{}",
                self.label,
                context.runtime.principal_id.unwrap_or_default()
            ));
            match self.outcome {
                Outcome::Text => HookResult::Continue(HookOutput::Text(self.label.into())),
                Outcome::Handled => HookResult::Handled,
                Outcome::Error => HookResult::Error(anyhow::anyhow!("test hook error")),
                Outcome::Panic => {
                    assert!(!matches!(self.outcome, Outcome::Panic), "test hook panic");
                    HookResult::PassThrough
                }
                Outcome::Timeout => std::future::pending().await,
            }
        }
    }

    struct Echo;
    #[async_trait]
    impl peko_tools_core::Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> String {
            "Echo fixture".into()
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(params)
        }
    }

    #[tokio::test]
    async fn observers_keep_registration_order_and_soft_fail_without_vetoing_tool_execution() {
        use crate::tools::runtime::ToolingRuntime;
        use peko_engine::{ToolCallSpec, ToolFunnel};
        let runtime = ToolingRuntime::standalone();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for (label, outcome) in [
            ("handled", Outcome::Handled),
            ("error", Outcome::Error),
            ("panic", Outcome::Panic),
            ("timeout", Outcome::Timeout),
            ("last", Outcome::Text),
        ] {
            runtime
                .hooks()
                .register_hook(
                    WorkspaceHookPoint::PreToolUse { tool_name: None },
                    Arc::new(Recorder {
                        label,
                        outcome,
                        seen: seen.clone(),
                    }),
                    &p1,
                )
                .await
                .unwrap();
        }
        // Neither another owner's observer nor a nonmatching exact selector fires.
        for (owner, selector) in [(&p2, None), (&p1, Some("Read".into()))] {
            runtime
                .hooks()
                .register_hook(
                    WorkspaceHookPoint::PreToolUse {
                        tool_name: selector,
                    },
                    Arc::new(Recorder {
                        label: "wrong",
                        outcome: Outcome::Text,
                        seen: seen.clone(),
                    }),
                    owner,
                )
                .await
                .unwrap();
        }
        runtime
            .hooks()
            .register_hook(
                WorkspaceHookPoint::PostToolUse {
                    tool_name: Some("echo".into()),
                },
                Arc::new(Recorder {
                    label: "post",
                    outcome: Outcome::Text,
                    seen: seen.clone(),
                }),
                &p1,
            )
            .await
            .unwrap();
        runtime
            .catalog()
            .register_system(Arc::new(Echo), crate::tools::metadata::ToolSource::BuiltIn)
            .await;
        let mut call = ToolCallSpec::new("echo", serde_json::json!({"ok":true}));
        call.principal_id = Some(p1.to_string());
        let start = std::time::Instant::now();
        let result = runtime.execute(call).await.unwrap();
        assert!(result.2);
        assert_eq!(result.1, serde_json::json!({"ok":true}));
        assert!(start.elapsed() >= peko_tools_core::HOOK_TIMEOUT);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(
            *seen.lock().unwrap(),
            ["handled", "error", "panic", "timeout", "last", "post"]
                .map(|label| format!("{label}:{p1}"))
        );
    }

    #[tokio::test]
    async fn lifecycle_and_prompt_dispatch_preserve_scope_identity_and_all_handler_outputs() {
        use peko_engine::{EngineHooks, PromptSectionRequest, ToolFunnel};
        let runtime = crate::tools::runtime::ToolingRuntime::standalone();
        let p1 = PrincipalId::generate();
        let p2 = PrincipalId::generate();
        let seen = Arc::new(Mutex::new(Vec::new()));
        for owner in [&p1, &p2] {
            for point in [
                WorkspaceHookPoint::Stop,
                WorkspaceHookPoint::AfterAgent,
                WorkspaceHookPoint::PromptSection {
                    section: "custom".into(),
                },
            ] {
                for label in ["first", "second"] {
                    runtime
                        .hooks()
                        .register_hook(
                            point.clone(),
                            Arc::new(Recorder {
                                label,
                                outcome: Outcome::Text,
                                seen: seen.clone(),
                            }),
                            owner,
                        )
                        .await
                        .unwrap();
                }
            }
        }
        runtime
            .fire_stop_hook(serde_json::json!({"principal_id":p1,"agent_did":"agent"}))
            .await;
        runtime
            .fire_after_agent_hook(serde_json::json!({"principal_id":p1,"agent_did":"agent"}))
            .await;
        let sections = runtime
            .render_prompt_sections(&PromptSectionRequest {
                principal_id: p1.to_string(),
                workspace: "/tmp/workspace".into(),
                session_id: "session".into(),
            })
            .await;
        assert_eq!(sections.get("custom"), Some("first\nsecond"));
        assert_eq!(
            *seen.lock().unwrap(),
            ["first", "second", "first", "second", "first", "second"]
                .map(|label| format!("{label}:{p1}"))
        );
    }
}
