//! Integration tests for tool monitoring, abortion, and progress updates
//!
//! Run with: cargo test tool_monitoring -- --nocapture

#[cfg(test)]
mod tests {
    use pekobot::tools::context::{AbortSignal, ToolContext, ToolError};
    use pekobot::tools::{ProgressDemoTool, Tool};
    use pekobot::{AgenticEvent, LifecyclePhase};
    use serde_json::json;
    use std::time::Duration;
    use tokio::time::{sleep, Instant};

    #[tokio::test]
    async fn test_abort_signal_basic() {
        let signal = AbortSignal::new();
        let ctx = signal.create_context("run-1", "tool-1", "test-tool");

        // Initially not aborted
        assert!(!ctx.is_aborted());
        assert!(!signal.is_aborted());

        // Abort
        signal.abort();

        // Now aborted
        assert!(ctx.is_aborted());
        assert!(signal.is_aborted());
    }

    #[tokio::test]
    async fn test_tool_error_types() {
        // Test Aborted error
        let err = ToolError::Aborted;
        assert!(matches!(err, ToolError::Aborted));
        assert_eq!(err.to_string(), "Tool execution aborted");

        // Test Timeout error
        let timeout_err = ToolError::Timeout(Duration::from_secs(30));
        assert!(matches!(timeout_err, ToolError::Timeout(_)));
        assert!(timeout_err.to_string().contains("timed out"));

        // Test Other error
        let other_err = ToolError::Other("custom error".to_string());
        assert_eq!(other_err.to_string(), "custom error");
    }

    #[tokio::test]
    async fn test_progress_events_emitted() {
        let tool = ProgressDemoTool::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "demo", tx);

        let params = json!({
            "items": ["a", "b", "c"],
            "delay_ms": 10,
            "batch_size": 1,
        });

        // Execute with context
        let result = tool.execute_with_context(params, &ctx).await;

        // Should succeed
        assert!(result.is_ok());

        // Collect events
        let mut progress_count = 0;

        // Wait a bit for events to arrive
        sleep(Duration::from_millis(100)).await;

        while let Ok(event) = rx.try_recv() {
            match event {
                AgenticEvent::ToolUpdate {
                    progress_percent,
                    output,
                    ..
                } => {
                    progress_count += 1;
                    println!("Progress: {:?}% - {}", progress_percent, output);
                }
                _ => {}
            }
        }

        assert!(
            progress_count > 0,
            "Should have received progress events, got {}",
            progress_count
        );
    }

    #[tokio::test]
    async fn test_tool_abort_mid_execution() {
        let tool = ProgressDemoTool::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "demo", tx);

        // Create many items that will take time to process
        let items: Vec<String> = (0..100).map(|i| format!("item_{}", i)).collect();

        let params = json!({
            "items": items,
            "delay_ms": 50, // 50ms per item
            "batch_size": 5,
        });

        // Abort after 150ms
        let abort_signal = signal.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(150)).await;
            println!("Aborting tool execution...");
            abort_signal.abort();
        });

        // Execute should fail with abort error
        let result = tool.execute_with_context(params, &ctx).await;

        assert!(
            result.is_err(),
            "Tool should have returned an error when aborted"
        );

        // Check that it's a ToolError::Aborted
        let err_string = result.unwrap_err().to_string();
        assert!(
            err_string.contains("aborted"),
            "Error message should mention abortion, got: {}",
            err_string
        );
    }

    #[tokio::test]
    async fn test_tool_timeout() {
        let tool = ProgressDemoTool::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();

        // Set a short timeout
        let ctx = signal
            .create_context_with_events("run-1", "tool-1", "demo", tx)
            .with_timeout(Duration::from_millis(200));

        // Create items that would take longer than timeout
        let items: Vec<String> = (0..20).map(|i| format!("item_{}", i)).collect();

        let params = json!({
            "items": items,
            "delay_ms": 50, // 50ms per item = 1000ms total
            "batch_size": 1,
        });

        // Execute should fail with timeout
        let result = tool.execute_with_context(params, &ctx).await;

        assert!(
            result.is_err(),
            "Tool should have returned an error when timed out"
        );

        let err_string = result.unwrap_err().to_string();
        assert!(
            err_string.contains("timeout") || err_string.contains("timed out"),
            "Error should mention timeout, got: {}",
            err_string
        );
    }

    #[tokio::test]
    async fn test_progress_throttling() {
        let tool = ProgressDemoTool::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();

        // Set aggressive throttling
        let ctx = signal
            .create_context_with_events("run-1", "tool-1", "demo", tx)
            .with_throttle(1000); // 1 second between updates

        let params = json!({
            "items": ["a", "b", "c", "d", "e"],
            "delay_ms": 50,
            "batch_size": 1,
        });

        let result = tool.execute_with_context(params, &ctx).await;
        assert!(result.is_ok());

        // Count events - should be throttled
        let mut event_count = 0;
        sleep(Duration::from_millis(100)).await;

        while rx.try_recv().is_ok() {
            event_count += 1;
        }

        println!("Received {} events (should be throttled)", event_count);
        // With 1 second throttle and ~250ms total work, we expect 1-2 events
        assert!(
            event_count <= 3,
            "Events should be throttled, got {}",
            event_count
        );
    }

    #[tokio::test]
    async fn test_tool_without_progress_support() {
        use pekobot::tools::{FileSystemTool, Tool};

        let tool = FileSystemTool::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let signal = AbortSignal::new();
        let ctx = signal.create_context_with_events("run-1", "tool-1", "fs", tx);

        // FileSystemTool doesn't support progress by default
        assert!(!tool.supports_progress());

        // Should still work with context (fallback to basic execute)
        let params = json!({
            "operation": "read",
            "path": "/tmp/test_file.txt"
        });

        // This will fail because file doesn't exist, but should not panic
        let _ = tool.execute_with_context(params, &ctx).await;

        // Should still receive status events
        sleep(Duration::from_millis(50)).await;

        let mut status_received = false;
        while let Ok(event) = rx.try_recv() {
            if let AgenticEvent::ToolUpdate { output, .. } = event {
                println!("Status: {}", output);
                status_received = true;
            }
        }

        // Status events should be emitted even without progress support
        assert!(status_received, "Should have received status events");
    }

    #[tokio::test]
    async fn test_multiple_tools_with_different_abort_signals() {
        let tool1 = ProgressDemoTool::new();
        let tool2 = ProgressDemoTool::new();

        let (tx1, mut rx1) = tokio::sync::mpsc::channel(100);
        let (tx2, mut rx2) = tokio::sync::mpsc::channel(100);

        let signal1 = AbortSignal::new();
        let signal2 = AbortSignal::new();

        let ctx1 = signal1.create_context_with_events("run-1", "tool-1", "demo1", tx1);
        let ctx2 = signal2.create_context_with_events("run-1", "tool-2", "demo2", tx2);

        let params = json!({
            "items": ["a", "b", "c", "d", "e"],
            "delay_ms": 30,
            "batch_size": 1,
        });

        // Run both tools concurrently
        let handle1 = tokio::spawn({
            let tool = tool1;
            let ctx = ctx1;
            let params = params.clone();
            async move { tool.execute_with_context(params, &ctx).await }
        });

        let handle2 = tokio::spawn({
            let tool = tool2;
            let ctx = ctx2;
            let params = params.clone();
            async move { tool.execute_with_context(params, &ctx).await }
        });

        // Abort only tool1 after 100ms
        let abort_signal = signal1.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            abort_signal.abort();
        });

        // Wait for both to complete
        let result1 = handle1.await.unwrap();
        let result2 = handle2.await.unwrap();

        // Tool1 should have aborted
        assert!(result1.is_err(), "Tool1 should have aborted");
        assert!(result1.unwrap_err().to_string().contains("aborted"));

        // Tool2 should have completed successfully
        assert!(result2.is_ok(), "Tool2 should have completed");

        // Count events
        let mut tool1_events = 0;
        let mut tool2_events = 0;

        while rx1.try_recv().is_ok() {
            tool1_events += 1;
        }
        while rx2.try_recv().is_ok() {
            tool2_events += 1;
        }

        println!(
            "Tool1 events: {}, Tool2 events: {}",
            tool1_events, tool2_events
        );
        assert!(
            tool1_events > 0,
            "Tool1 should have emitted events before abort"
        );
        assert!(tool2_events > 0, "Tool2 should have completed with events");
    }

    #[tokio::test]
    async fn test_progress_demo_estimated_duration() {
        let tool = ProgressDemoTool::new();

        let params = json!({
            "items": (0..10).map(|i| format!("item_{}", i)).collect::<Vec<_>>(),
            "delay_ms": 100,
        });

        let estimated = tool.estimated_duration_ms(&params);
        // 10 items * 100ms + 500ms overhead = 1500ms
        assert_eq!(estimated, 1500);

        // Empty params should still return a value
        let empty_params = json!({});
        let estimated_empty = tool.estimated_duration_ms(&empty_params);
        assert_eq!(estimated_empty, 500); // Just overhead
    }

    #[tokio::test]
    async fn test_tool_context_clone() {
        let signal = AbortSignal::new();
        let ctx = signal.create_context("run-1", "tool-1", "test");

        // Clone the context (used internally when spawning tasks)
        let ctx_clone = ctx.clone();

        // Both should see the same abort state
        assert!(!ctx.is_aborted());
        assert!(!ctx_clone.is_aborted());

        signal.abort();

        assert!(ctx.is_aborted());
        assert!(ctx_clone.is_aborted());
    }
}
