//! F36 — test-only PostToolUse sync primitive. See audit section 3 row 5.
//!
//! Lets a test block the test thread until a specific `(agent_id,
//! tool_name)` pair has been observed by a `PostToolUse` hook. The
//! signal does NOT include the tool result (`HookInput::ToolCall`
//! carries params only today — see `hook_io.rs:167-201`). Result-content
//! filtering ("wait until tool output contains X") is deferred to a
//! follow-up that adds a `HookInput::ToolCallResult` variant.
//!
//! # Pattern
//!
//! ```rust,ignore
//! let sync = TestSyncHandler::install(core.clone(), "agent-A", "Read").await;
//! // ... drive the loop in a background task ...
//! let ctx = sync.wait(Duration::from_secs(5)).await?;
//! // ctx.input carries the params; agent_id, tool_name match.
//! ```
//!
//! One handler per `(agent_id, tool_name)` — see the plan. Match-once
//! semantics: the second fire for the same pair is a no-op (the
//! oneshot receiver was already taken by the first fire). Call `wait`
//! again after the first fire to get `Err(HandlerDropped)` so tests
//! can detect re-entry.
//!
//! Cleanup is explicit via `unregister()` (mirrors the F31x
//! register/drive/unregister pattern). The handler stays registered
//! until `unregister` is called OR the `Arc<ExtensionCore>` is
//! dropped — whichever comes first.

#![cfg(all(test, feature = "test-utils"))]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use thiserror::Error;
use tokio::sync::oneshot;

use crate::extensions::framework::core::context::HookContext;
use crate::extensions::framework::core::handler::ClosureHookHandler;
use crate::extensions::framework::core::hook_points::HookPoint;
use crate::extensions::framework::core::registry::ExtensionCore;
use crate::extensions::framework::types::{ExtensionId, HookId, HookInput, HookResult};

/// Reasons `TestSyncHandler::wait` can fail without the tool ever firing.
#[derive(Debug, Error)]
pub enum WaitError {
    #[error(
        "timed out after {waited_ms}ms waiting for PostToolUse tool={tool_name} agent={agent_id}"
    )]
    Timeout {
        tool_name: String,
        agent_id: String,
        waited_ms: u64,
    },

    #[error("TestSyncHandler was dropped before tool={tool_name} fired on agent={agent_id}")]
    HandlerDropped { tool_name: String, agent_id: String },
}

/// Test-side `PostToolUse` sync primitive.
///
/// Construct via [`TestSyncHandler::install`]; call [`wait`](Self::wait)
/// to block. Drop is panic-safe — the receiver is left in the closure
/// state so the oneshot sender remains valid until the next fire (the
/// next fire takes the sender but sends to nobody — `oneshot::Sender::send`
/// returns `Err` in that case, which the closure ignores).
#[derive(Debug)]
pub struct TestSyncHandler {
    core: Arc<ExtensionCore>,
    hook_id: HookId,
    agent_id: String,
    tool_name: String,
    rx: Mutex<Option<oneshot::Receiver<HookContext>>>,
}

impl TestSyncHandler {
    /// Install a single-shot `PostToolUse` handler scoped to
    /// `(agent_id, tool_name)` and return the `TestSyncHandler` that
    /// owns the receiver.
    ///
    /// `agent_id` is matched against `HookInput::ToolCall::agent_id`
    /// (the agent name, not the DID). Use a uuid-suffixed agent name
    /// in tests that share a process to avoid cross-test collisions.
    pub async fn install(
        core: Arc<ExtensionCore>,
        agent_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let tool_name = tool_name.into();
        let (tx, rx) = oneshot::channel::<HookContext>();

        // Wrap the sender in `Arc<Mutex<Option<_>>>` so the closure
        // can `.take()` it on first fire. We don't need a `HashMap`
        // because the handler is registered for exactly one
        // `(agent_id, tool_name)` — see plan §2 / "single-handler-per-
        // install" divergence from the audit's multiplex sketch.
        let tx_slot = Arc::new(Mutex::new(Some(tx)));
        let aid_for_filter = agent_id.clone();

        let handler = ClosureHookHandler::new(
            HookPoint::PostToolUse {
                tool_name: tool_name.clone(),
            },
            100,
            format!("f36-test-sync:{agent_id}:{tool_name}"),
            move |ctx: HookContext| {
                let tx_slot = Arc::clone(&tx_slot);
                let aid_for_filter = aid_for_filter.clone();
                async move {
                    // HookPoint is already pinned to `tool_name`; the
                    // agent discriminator lives on HookInput::ToolCall.
                    let aid_matches = match &ctx.input {
                        HookInput::ToolCall {
                            agent_id: Some(a), ..
                        } => a == &aid_for_filter,
                        _ => false,
                    };
                    if !aid_matches {
                        return HookResult::PassThrough;
                    }
                    // Pop the sender. We do not hold the guard across
                    // any await — `take()` is synchronous and the
                    // drop of the guard happens before `send`.
                    let mut guard = tx_slot.lock().expect("f36 test_sync tx mutex poisoned");
                    if let Some(sender) = guard.take() {
                        // `send` is non-blocking; the receiver is
                        // parked on the test side. If the test has
                        // dropped the receiver, send returns Err —
                        // we ignore it (drop is documented as safe).
                        let _ = sender.send(ctx);
                    }
                    HookResult::PassThrough
                }
            },
        );

        let hook_id = core
            .register_hook(
                HookPoint::PostToolUse {
                    tool_name: tool_name.clone(),
                },
                Arc::new(handler),
                &ExtensionId::new(format!("f36-test-sync:{agent_id}")),
            )
            .await
            .expect("register PostToolUse handler on fresh core")
            .id;

        Self {
            core,
            hook_id,
            agent_id,
            tool_name,
            rx: Mutex::new(Some(rx)),
        }
    }

    /// Block until the handler fires for this `(agent_id, tool_name)`,
    /// or `timeout` elapses. Match-once: a second `wait` call after
    /// the first one consumed the receiver returns
    /// `Err(HandlerDropped)`.
    pub async fn wait(&self, timeout: Duration) -> Result<HookContext, WaitError> {
        let rx = {
            let mut guard = self.rx.lock().expect("f36 test_sync rx mutex");
            guard.take()
        }
        .ok_or_else(|| WaitError::HandlerDropped {
            tool_name: self.tool_name.clone(),
            agent_id: self.agent_id.clone(),
        })?;

        let started = Instant::now();
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(ctx)) => Ok(ctx),
            Ok(Err(_canceled)) => Err(WaitError::HandlerDropped {
                tool_name: self.tool_name.clone(),
                agent_id: self.agent_id.clone(),
            }),
            Err(_elapsed) => Err(WaitError::Timeout {
                tool_name: self.tool_name.clone(),
                agent_id: self.agent_id.clone(),
                waited_ms: started.elapsed().as_millis() as u64,
            }),
        }
    }

    /// Explicit unregister. Optional — the handler dies with the
    /// `Arc<ExtensionCore>` otherwise. Mirrors the F31x pattern.
    pub async fn unregister(&self) {
        let _ = self.core.unregister_hook(&self.hook_id).await;
    }
}

/// Convenience: install + wait in one call.
///
/// Use this when you don't need to drop the handler across an
/// `.await` — i.e. you want to register, fire, await, done.
///
/// ```rust,ignore
/// let ctx = block_until_tool(core.clone(), "agent-A", "Read", Duration::from_secs(5)).await?;
/// ```
pub async fn block_until_tool(
    core: Arc<ExtensionCore>,
    agent_id: impl Into<String>,
    tool_name: impl Into<String>,
    timeout: Duration,
) -> Result<HookContext, WaitError> {
    let h = TestSyncHandler::install(core, agent_id, tool_name).await;
    h.wait(timeout).await
}

/// Build a `HookInput::ToolCall` for tests. Centralized so the unit
/// tests don't all repeat the same boilerplate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tool_call_input(
    tool_name: &str,
    agent_id: Option<&str>,
    params: serde_json::Value,
) -> HookInput {
    HookInput::ToolCall {
        tool_name: tool_name.to_string(),
        params,
        workspace: None,
        agent_id: agent_id.map(str::to_string),
        session_id: None,
        caller_id: None,
        principal_id: None,
        principal_name: None,
        capabilities: None,
        active_extensions: None,
        abort_signal: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: install + register + cleanup. Returns the handler
    /// and the receiver side as separate values so tests can drive
    /// them independently. Use [`TestSyncHandler::install`] + an
    /// explicit `wait` for the production-shaped path.
    fn fresh_core() -> Arc<ExtensionCore> {
        Arc::new(ExtensionCore::new())
    }

    #[tokio::test]
    async fn register_and_wait_roundtrip() {
        let core = fresh_core();
        let agent = format!("f36-agent-{}", uuid::Uuid::new_v4());
        let sync = TestSyncHandler::install(core.clone(), agent.clone(), "Read").await;

        // Drive the hook layer directly — no real loop needed.
        let input = tool_call_input("Read", Some(&agent), json!({"path": "/tmp/x"}));
        let point = HookPoint::PostToolUse {
            tool_name: "Read".to_string(),
        };
        let _ = core.invoke_hook(point, input).await;

        let ctx = sync
            .wait(Duration::from_secs(1))
            .await
            .expect("wait resolves");
        let HookInput::ToolCall {
            tool_name,
            agent_id,
            ..
        } = ctx.input
        else {
            panic!("expected ToolCall input, got {:?}", ctx.input);
        };
        assert_eq!(tool_name, "Read");
        assert_eq!(agent_id.as_deref(), Some(agent.as_str()));

        sync.unregister().await;
    }

    #[tokio::test]
    async fn timeout_when_tool_does_not_fire() {
        let core = fresh_core();
        let agent = format!("f36-agent-{}", uuid::Uuid::new_v4());
        let sync = TestSyncHandler::install(core, agent, "Echo").await;

        let started = Instant::now();
        let result = sync.wait(Duration::from_millis(200)).await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(WaitError::Timeout { .. })),
            "expected Timeout, got {result:?}"
        );
        // Lower bound: timeout was honored (≥ 200ms).
        // Upper bound: didn't hang the test runner (>5s would mean
        // the timeout was bypassed).
        assert!(
            elapsed >= Duration::from_millis(200),
            "wait returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "wait did not honor timeout: {elapsed:?}"
        );

        if let Err(WaitError::Timeout { waited_ms, .. }) = result {
            assert!(
                waited_ms >= 200,
                "reported waited_ms {waited_ms} below 200ms threshold"
            );
        }

        sync.unregister().await;
    }

    #[tokio::test]
    async fn multi_tool_not_isolation() {
        let core = fresh_core();
        let agent = format!("f36-agent-{}", uuid::Uuid::new_v4());
        // Install on `Read`, fire `Bash` — handler must NOT fire.
        let sync = TestSyncHandler::install(core.clone(), agent.clone(), "Read").await;

        let bash_input = tool_call_input("Bash", Some(&agent), json!({"cmd": "ls"}));
        let _ = core
            .invoke_hook(
                HookPoint::PostToolUse {
                    tool_name: "Bash".to_string(),
                },
                bash_input,
            )
            .await;

        let result = sync.wait(Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(WaitError::Timeout { .. })),
            "expected Timeout when other tool fired, got {result:?}"
        );

        sync.unregister().await;
    }

    #[tokio::test]
    async fn agent_not_isolation() {
        let core = fresh_core();
        let agent_a = format!("f36-agent-A-{}", uuid::Uuid::new_v4());
        let agent_b = format!("f36-agent-B-{}", uuid::Uuid::new_v4());
        // Install on A; fire B (must miss), then A (must hit).
        let sync = TestSyncHandler::install(core.clone(), agent_a.clone(), "Read").await;

        let b_input = tool_call_input("Read", Some(&agent_b), json!({"path": "/tmp/b"}));
        let _ = core
            .invoke_hook(
                HookPoint::PostToolUse {
                    tool_name: "Read".to_string(),
                },
                b_input,
            )
            .await;

        // Now spawn a background task that fires the A invocation
        // after a short delay so the main test can `wait` for it.
        let core_for_fire = core.clone();
        let agent_a_for_fire = agent_a.clone();
        let fire_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let input = tool_call_input("Read", Some(&agent_a_for_fire), json!({"path": "/tmp/a"}));
            let _ = core_for_fire
                .invoke_hook(
                    HookPoint::PostToolUse {
                        tool_name: "Read".to_string(),
                    },
                    input,
                )
                .await;
        });

        let ctx = sync
            .wait(Duration::from_secs(2))
            .await
            .expect("wait resolves for the matching agent");
        let HookInput::ToolCall { agent_id, .. } = ctx.input else {
            panic!("expected ToolCall input");
        };
        assert_eq!(agent_id.as_deref(), Some(agent_a.as_str()));

        fire_task.await.expect("fire task ok");
        sync.unregister().await;
    }

    #[tokio::test]
    async fn drop_without_wait_cleanup() {
        let core = fresh_core();
        let agent = format!("f36-agent-{}", uuid::Uuid::new_v4());

        // Install, drop WITHOUT calling wait.
        let sync = TestSyncHandler::install(core.clone(), agent.clone(), "Read").await;
        let hook_id = sync.hook_id.clone();
        drop(sync);

        // The handler is still registered (Drop doesn't unregister).
        // Firing it must not panic — the sender is in the closure's
        // Arc<Mutex<Option<...>>>, send fails silently because the
        // receiver was dropped alongside the TestSyncHandler.
        let input = tool_call_input("Read", Some(&agent), json!({"path": "/tmp/x"}));
        let _ = core
            .invoke_hook(
                HookPoint::PostToolUse {
                    tool_name: "Read".to_string(),
                },
                input,
            )
            .await;

        // Explicit unregister is still possible.
        let _ = core.unregister_hook(&hook_id).await;
    }

    #[tokio::test]
    async fn first_fire_match_once_semantics() {
        let core = fresh_core();
        let agent = format!("f36-agent-{}", uuid::Uuid::new_v4());
        let sync = TestSyncHandler::install(core.clone(), agent.clone(), "Read").await;

        // First fire — consumed by the first wait.
        let input1 = tool_call_input("Read", Some(&agent), json!({"path": "/tmp/1"}));
        let _ = core
            .invoke_hook(
                HookPoint::PostToolUse {
                    tool_name: "Read".to_string(),
                },
                input1,
            )
            .await;
        let _ctx = sync
            .wait(Duration::from_secs(1))
            .await
            .expect("first wait resolves");

        // Second fire — closure sender is now empty (was .take()'d).
        let input2 = tool_call_input("Read", Some(&agent), json!({"path": "/tmp/2"}));
        let _ = core
            .invoke_hook(
                HookPoint::PostToolUse {
                    tool_name: "Read".to_string(),
                },
                input2,
            )
            .await;

        // Second wait — receiver was already taken on the first wait.
        let result = sync.wait(Duration::from_millis(50)).await;
        assert!(
            matches!(result, Err(WaitError::HandlerDropped { .. })),
            "expected HandlerDropped on second wait, got {result:?}"
        );

        sync.unregister().await;
    }
}
