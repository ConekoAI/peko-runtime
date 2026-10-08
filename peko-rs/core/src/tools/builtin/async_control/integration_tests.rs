//! Async domain-tool integration tests against the real executor and funnel.
//!
//! Calls all five actions through AsyncTool. Pin completion/result propagation,
//! cancellation, and per-call spawning-session attribution across sequential runs.

#[cfg(test)]
mod tests {
    use crate::async_exec::executor::{
        standalone_inbox_registry, AsyncExecutor, AsyncExecutorRuntime,
    };

    use crate::tools::builtin::AsyncTool;
    use crate::tools::runtime::ToolingRuntime;
    use async_trait::async_trait;
    use peko_subject::PrincipalId;
    use peko_tools_core::Tool;
    use std::sync::Arc;
    use std::time::Duration;

    /// Tool stub that returns `{"ok": true}` immediately.
    ///
    /// Mirrors the `StubTool` in
    /// `async_exec::executor::dispatch_tool_tests`.
    /// Distinct type so the registry sees two separate tools.
    struct StubTool;

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &'static str {
            "stub_tool"
        }
        fn description(&self) -> String {
            "stub for Async action spawn happy-path round-trip".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    /// Tool stub that sleeps ~200ms before completing.
    ///
    /// Used to exercise the cancel path: `AsyncStopAction` flips the
    /// registry to `Cancelled`; this stub doesn't poll `is_aborted()`
    /// so it runs to natural completion — that's the F38 two-layer
    /// contract being pinned here.
    struct AbortableStubTool;

    #[async_trait]
    impl Tool for AbortableStubTool {
        fn name(&self) -> &'static str {
            "abortable_stub"
        }
        fn description(&self) -> String {
            "long-running stub that ignores the abort channel".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(serde_json::json!({"ran_to_completion": true}))
        }
    }

    /// Bundle the constructed runtime + tools + core so a test can call
    /// each Async action against the same backing state.
    struct AsyncToolRig {
        #[allow(dead_code)] // retained for diagnostic future tests
        core: Arc<ToolingRuntime>,
        tool: AsyncTool,
    }

    /// Construct a runtime with `StubTool` (and optionally
    /// `AbortableStubTool`) registered. Returns an `AsyncToolRig`
    /// ready to drive every Async action from the same backing runtime.
    async fn setup_with_stop(register_abortable: bool) -> AsyncToolRig {
        let core = crate::tools::runtime::ToolingRuntime::standalone();
        // Spawned calls resolve this registration through the canonical dispatcher.
        core.catalog()
            .register(
                Arc::new(StubTool),
                crate::tools::metadata::ToolSource::BuiltIn,
                peko_subject::PrincipalId::system(),
            )
            .await;
        if register_abortable {
            core.catalog()
                .register(
                    Arc::new(AbortableStubTool),
                    crate::tools::metadata::ToolSource::BuiltIn,
                    peko_subject::PrincipalId::system(),
                )
                .await;
        }
        core.session_keys()
            .set("test_agent", Some("session_under_test".to_string()));

        let executor = Arc::new(AsyncExecutor::new(standalone_inbox_registry()));
        let runtime = Arc::new(AsyncExecutorRuntime::new(
            executor,
            Arc::downgrade(&core),
            Some("test_agent".to_string()),
            PrincipalId("principal_test".to_string()),
        ));
        let handle = runtime.as_shared();

        AsyncToolRig {
            core,
            tool: AsyncTool::new(handle),
        }
    }

    /// Pin: a spawn of an installed tool reaches
    /// `Completed`, lands a terminal result in `Async action output` output,
    /// and `Async action status` reports `is_terminal=true`.
    #[tokio::test]
    async fn test_async_spawn_then_output_blocks_for_terminal_result() {
        let rig = setup_with_stop(false).await;

        let receipt = rig
            .tool
            .execute(serde_json::json!({"action":"spawn",
                "tool": "stub_tool",
                "params": {},
                "label": "happy-path",
            }))
            .await
            .expect("Async action spawn returns receipt");
        assert_eq!(
            receipt["status"], "running",
            "receipt status runs while dispatched"
        );
        assert_eq!(receipt["tool"], "stub_tool", "receipt echoes the tool name");
        let task_id = receipt["task_id"].as_str().expect("task_id is a string");
        assert!(
            task_id.starts_with("stub_tool:"),
            "task_id shape is tool_name:uuid, got: {task_id}",
        );

        // Wait for the spawned task to reach terminal via Async action status,
        // then read the result via Async action output. This avoids the
        // block:true path's known lock-held-during-sleep interaction
        // (see test_async_output_block_false_does_not_wait for the
        // targeted shape assertion on block:false).
        let status = poll_terminal(&rig, task_id, "completed").await;
        assert_eq!(status["is_terminal"], serde_json::json!(true));

        // Async action output now reads a terminal entry.
        let output = rig
            .tool
            .execute(serde_json::json!({"action":"output","task_id": task_id}))
            .await
            .expect("Async action output reads terminal entry");
        assert_eq!(output["is_terminal"], serde_json::json!(true));
        assert_eq!(output["status"], "completed");
        assert_eq!(
            output["result"],
            serde_json::json!({"ok": true}),
            "tool return value flows through the funnel intact",
        );

        // Async action status sees the same terminal entry.
        let status = rig
            .tool
            .execute(serde_json::json!({"action":"status","task_id": task_id}))
            .await
            .expect("Async action status returns entry");
        assert_eq!(status["is_terminal"], serde_json::json!(true));
        assert_eq!(status["status"], "completed");
        assert_eq!(
            status["parent_session_key"], "session_under_test",
            "no ctx: legacy session-key cell remains the fallback stamp",
        );
    }

    /// ADR-061 follow-up: `ToolContext.session_id` stamps the parent
    /// session PER CALL — the request's `parent_session_id` beats the
    /// legacy session-key cell.
    #[tokio::test]
    async fn test_spawn_stamps_ctx_session_id_over_cell() {
        let rig = setup_with_stop(false).await;
        // The cell holds the legacy stamp; the ctx-carried id must win.
        let receipt = rig
            .tool
            .execute_with_context(
                serde_json::json!({"action":"spawn","tool": "stub_tool", "params": {}}),
                &peko_tools_core::ToolContext::default_for_tool("Async")
                    .with_session_id("ctx-session-A"),
            )
            .await
            .expect("spawn with ctx");
        let task_id = receipt["task_id"].as_str().expect("task_id");
        let status = poll_terminal(&rig, task_id, "completed").await;
        assert_eq!(
            status["parent_session_key"], "ctx-session-A",
            "ctx session id must stamp, not the cell value"
        );
    }

    /// The staleness regression pin: two sequential spawns carrying
    /// different ctx ids each stamp their own — nothing is sticky
    /// between runs the way the session-key cell was.
    #[tokio::test]
    async fn test_spawn_sequential_ctx_ids_stamp_independently() {
        let rig = setup_with_stop(false).await;
        let mut task_ids = Vec::new();
        for id in ["ctx-run-1", "ctx-run-2"] {
            let receipt = rig
                .tool
                .execute_with_context(
                    serde_json::json!({"action":"spawn","tool": "stub_tool", "params": {}}),
                    &peko_tools_core::ToolContext::default_for_tool("Async").with_session_id(id),
                )
                .await
                .expect("spawn with ctx");
            task_ids.push((
                receipt["task_id"].as_str().expect("task_id").to_string(),
                id,
            ));
        }
        for (task_id, expected) in &task_ids {
            let status = poll_terminal(&rig, task_id, "completed").await;
            assert_eq!(
                status["parent_session_key"],
                serde_json::json!(expected),
                "task {task_id} must carry its own caller's stamp"
            );
        }
    }

    /// Pin: `block:false` returns immediately. `block:true` with a
    /// holding-read-lock-during-sleep interaction with the spawned
    /// task's write-lock update is exercised via the Async action status
    /// polling pattern instead (see test_async_spawn_then_output_...).
    #[tokio::test]
    async fn test_async_output_block_false_does_not_wait() {
        // 200ms-stub means it will likely still be running when we
        // poll; block:false must return immediately either way.
        let rig = setup_with_stop(true).await;

        let receipt = rig
            .tool
            .execute(serde_json::json!({"action":"spawn",
                "tool": "abortable_stub",
                "params": {},
            }))
            .await
            .expect("Async action spawn returns receipt");
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Race-tolerant: assert that block:false did NOT block for
        // completion. We allow either is_terminal (200ms is short and
        // poll scheduling can race) but the call itself must have
        // returned within the timeout window.
        let start = std::time::Instant::now();
        let output = rig
            .tool
            .execute(serde_json::json!({"action":"output",
                "task_id": task_id,
                "block": false,
            }))
            .await
            .expect("block:false returns immediately");
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "block:false must return without waiting, took {elapsed:?}",
        );
        // Sanity: shape matches either terminal or not.
        assert!(output.get("is_terminal").is_some());
        assert!(output.get("status").is_some());
    }

    /// Pin: `Async action stop` against an already-terminal task returns
    /// `success:true, already_terminal:true` per the Claude-Code
    /// `TaskStop` shape — never `success:false` for "task already
    /// done". See `common::build_cancel_response`.
    #[tokio::test]
    async fn test_async_stop_on_already_terminal_returns_success_no_op() {
        let rig = setup_with_stop(false).await;

        let receipt = rig
            .tool
            .execute(serde_json::json!({"action":"spawn","tool": "stub_tool", "params": {}}))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Drain to completion via Async action status polling.
        let _ = poll_terminal(&rig, &task_id, "completed").await;

        let result = rig
            .tool
            .execute(serde_json::json!({"action":"stop","task_id": task_id}))
            .await
            .expect("Async action stop returns response");
        assert_eq!(result["success"], serde_json::json!(true));
        assert_eq!(result["already_terminal"], serde_json::json!(true));
        assert_eq!(result["previous_status"], "completed");
    }

    /// Pin the abort-signal bridge: cancelling a long-running task
    /// flips the registry to `Cancelled` synchronously and
    /// `Async action status` reports `cancelled` (the task itself continues
    /// to run because `AbortableStubTool` doesn't poll `is_aborted()`
    /// — the F38 two-layer contract).
    #[tokio::test]
    async fn test_async_stop_cancels_long_running_task() {
        let rig = setup_with_stop(true).await;

        let receipt = rig
            .tool
            .execute(serde_json::json!({"action":"spawn","tool": "abortable_stub", "params": {}}))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Cancel immediately — before the 200ms stub finishes.
        // previous_status can be "pending" or "running" depending on
        // scheduler timing; we accept either so the test isn't flaky.
        let result = rig
            .tool
            .execute(serde_json::json!({"action":"stop","task_id": task_id}))
            .await
            .unwrap();
        assert_eq!(result["success"], serde_json::json!(true));
        assert_eq!(result["already_terminal"], serde_json::json!(false));
        let previous = result["previous_status"].as_str().unwrap();
        assert!(
            previous == "pending" || previous == "running",
            "previous_status should be pending or running pre-cancel, got: {previous}",
        );

        // Registry flip is synchronous per executor.rs:725 — poll a
        // little for any scheduler delay, then assert.
        for _ in 0..40 {
            let status = rig
                .tool
                .execute(serde_json::json!({"action":"status","task_id": task_id}))
                .await
                .unwrap();
            if status["status"] == "cancelled" {
                assert_eq!(status["is_terminal"], serde_json::json!(true));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("status never reported `cancelled` after Async action stop succeeded");
    }

    /// Pin: `Async action status` against an unknown task_id returns
    /// `{error, task_id}` rather than Err — the tool body explicitly
    /// produces this JSON shape (see `status.rs:65-69`).
    #[tokio::test]
    async fn test_async_status_returns_not_found_for_unknown_task() {
        let rig = setup_with_stop(false).await;
        let result = rig
            .tool
            .execute(serde_json::json!({"action":"status","task_id": "ghost:task-id"}))
            .await
            .expect("Async action status returns JSON, not Err, on missing tasks");
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "ghost:task-id");
    }

    /// Pin: `Async action list` filters by tool_name; only matching entries
    /// appear under `tasks[]`, and `total` reflects the filtered
    /// count.
    #[tokio::test]
    async fn test_async_list_filters_by_tool_name() {
        let rig = setup_with_stop(true).await;

        // One of each.
        let r1 = rig
            .tool
            .execute(serde_json::json!({"action":"spawn","tool": "stub_tool", "params": {}}))
            .await
            .unwrap();
        let r2 = rig
            .tool
            .execute(serde_json::json!({"action":"spawn","tool": "abortable_stub", "params": {}}))
            .await
            .unwrap();

        // Drain both to terminal so the list returns terminal entries
        // (otherwise the stub_tool one could still be running and the
        // Async action stop call against it just flips the registry to
        // Cancelled).
        let _ = poll_terminal(&rig, r1["task_id"].as_str().unwrap(), "completed").await;
        let _ = poll_terminal(&rig, r2["task_id"].as_str().unwrap(), "completed").await;

        let list_all = rig
            .tool
            .execute(serde_json::json!({"action":"list",}))
            .await
            .unwrap();
        // `Async action list` merges the process-global per-agent registries
        // (background `Bash` tasks, subagent runs from other tests in
        // this binary), so the total is a lower bound — pin that both
        // spawned tasks are present instead of an exact count.
        let all_tasks = list_all["tasks"].as_array().expect("tasks is array");
        assert!(
            all_tasks
                .iter()
                .any(|t| t["task_id"] == r1["task_id"] && t["tool_name"] == "stub_tool"),
            "list_all must contain the stub_tool task: {list_all}"
        );
        assert!(
            all_tasks
                .iter()
                .any(|t| t["task_id"] == r2["task_id"] && t["tool_name"] == "abortable_stub"),
            "list_all must contain the abortable_stub task: {list_all}"
        );

        let list_stub = rig
            .tool
            .execute(serde_json::json!({"action":"list","tool_filter": "stub_tool"}))
            .await
            .unwrap();
        assert_eq!(list_stub["total"], serde_json::json!(1));
        let tasks = list_stub["tasks"].as_array().expect("tasks is array");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["tool_name"], "stub_tool");
        assert_eq!(tasks[0]["task_id"], r1["task_id"]);

        // Other filter returns the other one.
        let list_aborter = rig
            .tool
            .execute(serde_json::json!({"action":"list","tool_filter": "abortable_stub"}))
            .await
            .unwrap();
        assert_eq!(list_aborter["total"], serde_json::json!(1));
        assert_eq!(list_aborter["tasks"][0]["task_id"], r2["task_id"],);
    }

    /// F37 success-path test: the test the doc comment on
    /// `executor.rs:1188` claimed existed "outside the framework
    /// boundary." It exercises the full chain — `AsyncSpawnAction`
    /// through `AsyncExecutorRuntime::spawn` → `dispatch_tool` →
    /// `core.execute_tool_via_hook` — and asserts the spawn reaches
    /// `completed` (not `failed`). ADR-066 P2: no capability gate.
    #[tokio::test]
    async fn test_async_spawn_through_funnel_completes() {
        let rig = setup_with_stop(false).await;

        let receipt = rig
            .tool
            .execute(serde_json::json!({"action":"spawn",
                "tool": "stub_tool",
                "params": {},
                "label": "f37-allow",
            }))
            .await
            .expect("Async action spawn returns receipt");
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Poll for terminal via Async action status. A funnel-level rejection
        // would surface as "failed" rather than "completed".
        let status = poll_terminal(&rig, &task_id, "completed").await;
        assert_eq!(status["status"], "completed");
        assert_eq!(status["result"], serde_json::json!({"ok": true}));
    }

    /// Poll `Async action status` until the task's status matches `expected`,
    /// or panic after ~2s. Returns the terminal `TaskView` JSON.
    async fn poll_terminal(rig: &AsyncToolRig, task_id: &str, expected: &str) -> serde_json::Value {
        for _ in 0..100 {
            let status = rig
                .tool
                .execute(serde_json::json!({"action":"status","task_id": task_id}))
                .await
                .expect("Async action status returns JSON");
            if status["status"] == expected {
                return status;
            }
            // Don't loop forever on cancelled/failed entries — bail
            // out early so the failure message names the actual state.
            if status["status"] == "cancelled"
                || status["status"] == "failed"
                || status["status"] == "timed_out"
            {
                panic!("task reached {status:?}, expected {expected}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("task {task_id} never reached status {expected} within ~2s");
    }
}
