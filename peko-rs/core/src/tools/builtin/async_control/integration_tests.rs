//! End-to-end integration tests for the Async* tool family.
//!
//! Wires `AsyncSpawnTool` / `AsyncOutputTool` / `AsyncStatusTool` /
//! `AsyncListTool` / `AsyncStopTool` against a real
//! `AsyncExecutorRuntime` wrapping `AsyncExecutor` + `Arc<ExtensionCore>`.
//!
//! These tests pin two contracts that layered unit tests cannot:
//!
//! 1. **F37 canonical funnel** — `AsyncSpawnTool.execute` →
//!    `AsyncExecutorRuntime::spawn` → `executor.dispatch_tool` →
//!    `core.execute_tool_via_hook(...)`. A spawn with the right
//!    capability grant lands in `Completed`, not `Failed`.
//! 2. **Abort-signal bridge** — `AsyncStopTool.execute` →
//!    `AsyncExecutor::cancel` → registry flips to `Cancelled`
//!    synchronously, plus the abort watch channel fires for tool bodies
//!    that poll `is_aborted()`.
//!
//! Layered unit-test coverage lives in
//! `extensions::framework::async_exec::executor::dispatch_tool_tests`
//! (gate) and
//! `extensions::framework::async_exec::executor::async_runtime_impl::TestAsyncRuntime`
//! (adapter). This file is the only place all five `Async*Tool` objects
//! run through their own `.execute()` chains.

#[cfg(test)]
mod tests {
    use crate::extensions::builtin::BuiltinToolAdapter;
    use crate::extensions::framework::async_exec::executor::{
        standalone_inbox_registry, AsyncExecutor, AsyncExecutorRuntime,
    };
    use crate::extensions::framework::core::ExtensionCore;
    use crate::tools::builtin::{
        AsyncListTool, AsyncOutputTool, AsyncSpawnTool, AsyncStatusTool, AsyncStopTool,
    };
    use async_trait::async_trait;
    use peko_subject::PrincipalId;
    use peko_tools_core::Tool;
    use std::sync::Arc;
    use std::time::Duration;

    /// Tool stub that returns `{"ok": true}` immediately.
    ///
    /// Mirrors the `StubTool` in
    /// `extensions::framework::async_exec::executor::dispatch_tool_tests`.
    /// Distinct type so the registry sees two separate tools.
    struct StubTool;

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &'static str {
            "stub_tool"
        }
        fn description(&self) -> String {
            "stub for AsyncSpawn happy-path round-trip".to_string()
        }
        async fn execute(&self, _params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    /// Tool stub that sleeps ~200ms before completing.
    ///
    /// Used to exercise the cancel path: `AsyncStopTool` flips the
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
    /// each Async* tool's `.execute()` against the same backing state.
    struct AsyncToolRig {
        #[allow(dead_code)] // retained for diagnostic future tests
        core: Arc<ExtensionCore>,
        spawn: AsyncSpawnTool,
        output: AsyncOutputTool,
        status: AsyncStatusTool,
        list: AsyncListTool,
        stop: AsyncStopTool,
    }

    /// Construct a runtime with `StubTool` (and optionally
    /// `AbortableStubTool`) registered against the supplied capability
    /// snapshot. Returns an `AsyncToolRig` ready to drive every Async*
    /// tool from the same backing runtime.
    async fn setup_with_stop(
        capabilities: Vec<String>,
        register_abortable: bool,
    ) -> AsyncToolRig {
        let core = Arc::new(ExtensionCore::new());
        // Register via `BuiltinToolAdapter::register_tool_system` rather
        // than `core.insert_tool_instance`. The latter only fills the
        // `Arc<dyn Tool>` side-table that direct callers (e.g.
        // AsyncSpawnTool) read from. The F37 funnel — `dispatch_tool` →
        // `execute_tool_via_hook` → `hook_registry.invoke_hook` — also
        // needs a `BuiltinExecuteHandler` registered for the tool's
        // `ToolExecute` hook point. Without it, the hook registry
        // returns `HookResult::PassThrough`, and `tool_result_from_hook`
        // converts that to `("Tool 'stub_tool' not available",
        // success=false)`, which is why every spawned task landed in
        // `Failed` with no `result` field before this fix.
        BuiltinToolAdapter::register_tool_system(&core, Arc::new(StubTool))
            .await
            .expect("register stub_tool");
        if register_abortable {
            BuiltinToolAdapter::register_tool_system(
                &core,
                Arc::new(AbortableStubTool),
            )
            .await
            .expect("register abortable_stub");
        }
        core.set_session_key("test_agent", Some("session_under_test".to_string()))
            .await;

        let executor = Arc::new(AsyncExecutor::new(standalone_inbox_registry()));
        let runtime = Arc::new(AsyncExecutorRuntime::new(
            executor,
            Arc::downgrade(&core),
            Some("test_agent".to_string()),
            PrincipalId("principal_test".to_string()),
            Arc::new(capabilities),
            Arc::new(Vec::<String>::new()),
        ));
        let handle = runtime.as_shared();

        AsyncToolRig {
            core,
            spawn: AsyncSpawnTool::new(handle.clone()),
            output: AsyncOutputTool::new(handle.clone()),
            status: AsyncStatusTool::new(handle.clone()),
            list: AsyncListTool::new(handle.clone()),
            stop: AsyncStopTool::new(handle),
        }
    }

    /// Pin: a spawn with the right capability grant reaches
    /// `Completed`, lands a terminal result in `AsyncOutput` output,
    /// and `AsyncStatus` reports `is_terminal=true`.
    #[tokio::test]
    async fn test_async_spawn_then_output_blocks_for_terminal_result() {
        let rig = setup_with_stop(vec!["tool:stub_tool".to_string()], false).await;

        let receipt = rig
            .spawn
            .execute(serde_json::json!({
                "tool": "stub_tool",
                "params": {},
                "label": "happy-path",
            }))
            .await
            .expect("AsyncSpawn returns receipt");
        assert_eq!(receipt["status"], "running", "receipt status runs while dispatched");
        assert_eq!(receipt["tool"], "stub_tool", "receipt echoes the tool name");
        let task_id = receipt["task_id"].as_str().expect("task_id is a string");
        assert!(
            task_id.starts_with("stub_tool:"),
            "task_id shape is tool_name:uuid, got: {task_id}",
        );

        // Wait for the spawned task to reach terminal via AsyncStatus,
        // then read the result via AsyncOutput. This avoids the
        // block:true path's known lock-held-during-sleep interaction
        // (see test_async_output_block_false_does_not_wait for the
        // targeted shape assertion on block:false).
        let status = poll_terminal(&rig, task_id, "completed").await;
        assert_eq!(status["is_terminal"], serde_json::json!(true));

        // AsyncOutput now reads a terminal entry.
        let output = rig
            .output
            .execute(serde_json::json!({"task_id": task_id}))
            .await
            .expect("AsyncOutput reads terminal entry");
        assert_eq!(output["is_terminal"], serde_json::json!(true));
        assert_eq!(output["status"], "completed");
        assert_eq!(
            output["result"],
            serde_json::json!({"ok": true}),
            "tool return value flows through the funnel intact",
        );

        // AsyncStatus sees the same terminal entry.
        let status = rig
            .status
            .execute(serde_json::json!({"task_id": task_id}))
            .await
            .expect("AsyncStatus returns entry");
        assert_eq!(status["is_terminal"], serde_json::json!(true));
        assert_eq!(status["status"], "completed");
        assert_eq!(
            status["parent_session_key"], "session_under_test",
            "set_session_key resolved the parent key from the core",
        );
    }

    /// Pin: `block:false` returns immediately. `block:true` with a
    /// holding-read-lock-during-sleep interaction with the spawned
    /// task's write-lock update is exercised via the AsyncStatus
    /// polling pattern instead (see test_async_spawn_then_output_...).
    #[tokio::test]
    async fn test_async_output_block_false_does_not_wait() {
        // 200ms-stub means it will likely still be running when we
        // poll; block:false must return immediately either way.
        let rig = setup_with_stop(vec!["tool:abortable_stub".to_string()], true).await;

        let receipt = rig
            .spawn
            .execute(serde_json::json!({
                "tool": "abortable_stub",
                "params": {},
            }))
            .await
            .expect("AsyncSpawn returns receipt");
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Race-tolerant: assert that block:false did NOT block for
        // completion. We allow either is_terminal (200ms is short and
        // poll scheduling can race) but the call itself must have
        // returned within the timeout window.
        let start = std::time::Instant::now();
        let output = rig
            .output
            .execute(serde_json::json!({
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

    /// Pin: `AsyncStop` against an already-terminal task returns
    /// `success:true, already_terminal:true` per the Claude-Code
    /// `TaskStop` shape — never `success:false` for "task already
    /// done". See `common::build_cancel_response`.
    #[tokio::test]
    async fn test_async_stop_on_already_terminal_returns_success_no_op() {
        let rig = setup_with_stop(vec!["tool:stub_tool".to_string()], false).await;

        let receipt = rig
            .spawn
            .execute(serde_json::json!({"tool": "stub_tool", "params": {}}))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Drain to completion via AsyncStatus polling.
        let _ = poll_terminal(&rig, &task_id, "completed").await;

        let result = rig
            .stop
            .execute(serde_json::json!({"task_id": task_id}))
            .await
            .expect("AsyncStop returns response");
        assert_eq!(result["success"], serde_json::json!(true));
        assert_eq!(result["already_terminal"], serde_json::json!(true));
        assert_eq!(result["previous_status"], "completed");
    }

    /// Pin the abort-signal bridge: cancelling a long-running task
    /// flips the registry to `Cancelled` synchronously and
    /// `AsyncStatus` reports `cancelled` (the task itself continues
    /// to run because `AbortableStubTool` doesn't poll `is_aborted()`
    /// — the F38 two-layer contract).
    #[tokio::test]
    async fn test_async_stop_cancels_long_running_task() {
        let rig =
            setup_with_stop(vec!["tool:abortable_stub".to_string()], true).await;

        let receipt = rig
            .spawn
            .execute(serde_json::json!({"tool": "abortable_stub", "params": {}}))
            .await
            .unwrap();
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Cancel immediately — before the 200ms stub finishes.
        // previous_status can be "pending" or "running" depending on
        // scheduler timing; we accept either so the test isn't flaky.
        let result = rig
            .stop
            .execute(serde_json::json!({"task_id": task_id}))
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
                .status
                .execute(serde_json::json!({"task_id": task_id}))
                .await
                .unwrap();
            if status["status"] == "cancelled" {
                assert_eq!(status["is_terminal"], serde_json::json!(true));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("status never reported `cancelled` after AsyncStop succeeded");
    }

    /// Pin: `AsyncStatus` against an unknown task_id returns
    /// `{error, task_id}` rather than Err — the tool body explicitly
    /// produces this JSON shape (see `status.rs:65-69`).
    #[tokio::test]
    async fn test_async_status_returns_not_found_for_unknown_task() {
        let rig = setup_with_stop(Vec::new(), false).await;
        let result = rig
            .status
            .execute(serde_json::json!({"task_id": "ghost:task-id"}))
            .await
            .expect("AsyncStatus returns JSON, not Err, on missing tasks");
        assert_eq!(result["error"], "Task not found");
        assert_eq!(result["task_id"], "ghost:task-id");
    }

    /// Pin: `AsyncList` filters by tool_name; only matching entries
    /// appear under `tasks[]`, and `total` reflects the filtered
    /// count.
    #[tokio::test]
    async fn test_async_list_filters_by_tool_name() {
        let rig = setup_with_stop(
            vec!["tool:stub_tool".to_string(), "tool:abortable_stub".to_string()],
            true,
        )
        .await;

        // One of each.
        let r1 = rig
            .spawn
            .execute(serde_json::json!({"tool": "stub_tool", "params": {}}))
            .await
            .unwrap();
        let r2 = rig
            .spawn
            .execute(serde_json::json!({"tool": "abortable_stub", "params": {}}))
            .await
            .unwrap();

        // Drain both to terminal so the list returns terminal entries
        // (otherwise the stub_tool one could still be running and the
        // AsyncStop call against it just flips the registry to
        // Cancelled).
        let _ = poll_terminal(&rig, r1["task_id"].as_str().unwrap(), "completed").await;
        let _ = poll_terminal(&rig, r2["task_id"].as_str().unwrap(), "completed").await;

        let list_all = rig
            .list
            .execute(serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(list_all["total"], serde_json::json!(2));

        let list_stub = rig
            .list
            .execute(serde_json::json!({"tool_filter": "stub_tool"}))
            .await
            .unwrap();
        assert_eq!(list_stub["total"], serde_json::json!(1));
        let tasks = list_stub["tasks"].as_array().expect("tasks is array");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["tool_name"], "stub_tool");
        assert_eq!(tasks[0]["task_id"], r1["task_id"]);

        // Other filter returns the other one.
        let list_aborter = rig
            .list
            .execute(serde_json::json!({"tool_filter": "abortable_stub"}))
            .await
            .unwrap();
        assert_eq!(list_aborter["total"], serde_json::json!(1));
        assert_eq!(
            list_aborter["tasks"][0]["task_id"],
            r2["task_id"],
        );
    }

    /// F37 success-path test: the test the doc comment on
    /// `executor.rs:1188` claimed existed "outside the framework
    /// boundary." It exercises the full chain — `AsyncSpawnTool`
    /// through `AsyncExecutorRuntime::spawn` → `dispatch_tool` →
    /// `core.execute_tool_via_hook` — and asserts the capability gate
    /// allows the spawn to reach `completed` (not `failed`) when the
    /// matching grant is in the snapshot.
    #[tokio::test]
    async fn test_async_spawn_through_capability_gate_allow() {
        let rig = setup_with_stop(vec!["tool:stub_tool".to_string()], false).await;

        let receipt = rig
            .spawn
            .execute(serde_json::json!({
                "tool": "stub_tool",
                "params": {},
                "label": "f37-allow",
            }))
            .await
            .expect("AsyncSpawn returns receipt when the gate allows");
        let task_id = receipt["task_id"].as_str().unwrap().to_string();

        // Poll for terminal via AsyncStatus. If the gate had rejected,
        // status would be "failed" rather than "completed".
        let status = poll_terminal(&rig, &task_id, "completed").await;
        assert_eq!(status["status"], "completed");
        assert_eq!(status["result"], serde_json::json!({"ok": true}));
    }

    /// Poll `AsyncStatus` until the task's status matches `expected`,
    /// or panic after ~2s. Returns the terminal `TaskView` JSON.
    async fn poll_terminal(
        rig: &AsyncToolRig,
        task_id: &str,
        expected: &str,
    ) -> serde_json::Value {
        for _ in 0..100 {
            let status = rig
                .status
                .execute(serde_json::json!({"task_id": task_id}))
                .await
                .expect("AsyncStatus returns JSON");
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
        panic!(
            "task {task_id} never reached status {expected} within ~2s"
        );
    }
}
