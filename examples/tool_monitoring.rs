//! Example: Tool Monitoring and Abortion
//!
//! This example demonstrates:
//! - Running a tool with progress updates
//! - Monitoring tool execution via events
//! - Aborting a long-running tool
//! - Setting timeouts on tool execution
//!
//! Run with: cargo run --example tool_monitoring

use pekobot::{AgenticEvent, LifecyclePhase};
use pekobot::tools::context::AbortSignal;
use pekobot::tools::{ProgressDemoTool, Tool};
use serde_json::json;
use std::time::Duration;
use tokio::time::{sleep, Instant};

#[tokio::main]
async fn main() {
    println!("=== Tool Monitoring Example ===\n");

    // Create the demo tool
    let tool = ProgressDemoTool::new();

    // Example 1: Normal execution with progress monitoring
    println!("1. Running tool with progress monitoring...");
    run_with_progress(&tool).await;

    println!("\n---\n");

    // Example 2: Aborting a long-running tool
    println!("2. Running tool and aborting mid-execution...");
    run_and_abort(&tool).await;

    println!("\n---\n");

    // Example 3: Tool timeout
    println!("3. Running tool with timeout (will timeout)...");
    run_with_timeout(&tool).await;

    println!("\n---\n");

    // Example 4: Progress throttling
    println!("4. Running tool with throttled progress updates...");
    run_with_throttling(&tool).await;

    println!("\n=== Example Complete ===");
}

async fn run_with_progress(tool: &ProgressDemoTool) {
    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);

    // Create abort signal and context
    let signal = AbortSignal::new();
    let ctx = signal.create_context_with_events("example-run", "demo-1", "progress-demo", event_tx);

    // Set up parameters for a medium-length operation
    let params = json!({
        "items": vec!["file1.txt", "file2.txt", "file3.txt", "file4.txt", "file5.txt"],
        "batch_size": 2,
        "delay_ms": 200, // 200ms per item
    });

    println!("   Processing 5 items in batches of 2 (200ms per item)...\n");

    // Spawn event processor
    let event_handle = tokio::spawn(async move {
        let mut progress_count = 0;

        while let Some(event) = event_rx.recv().await {
            match event {
                AgenticEvent::ToolStart { name, tool_id, .. } => {
                    println!("   🚀 Tool '{}' started (id: {})", name, tool_id);
                }
                AgenticEvent::ToolUpdate { output, progress_percent, .. } => {
                    progress_count += 1;
                    if let Some(percent) = progress_percent {
                        let bar = progress_bar(percent);
                        println!("   📊 {} {}% - {}", bar, percent, output);
                    } else {
                        println!("   📝 {}", output);
                    }
                }
                AgenticEvent::ToolEnd { success, duration_ms, .. } => {
                    let icon = if success { "✅" } else { "❌" };
                    println!(
                        "   {} Tool completed: success={}, duration={}ms",
                        icon, success, duration_ms
                    );
                    break;
                }
                _ => {}
            }
        }

        progress_count
    });

    // Run the tool
    let result = tool.execute_with_context(params, &ctx).await;

    // Wait for event processor
    let progress_count = event_handle.await.unwrap();

    // Show result
    match result {
        Ok(value) => {
            let processed = value.get("processed").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("\n   ✅ Successfully processed {} items", processed);
        }
        Err(e) => {
            println!("\n   ❌ Error: {}", e);
        }
    }

    println!("   (Received {} progress updates)", progress_count);
}

async fn run_and_abort(tool: &ProgressDemoTool) {
    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);

    // Create abort signal and context
    let signal = AbortSignal::new();
    let ctx = signal.create_context_with_events("example-run", "demo-2", "progress-demo", event_tx);

    // Set up parameters for a long operation
    let items: Vec<String> = (0..20).map(|i| format!("file{}.txt", i)).collect();
    let params = json!({
        "items": items,
        "batch_size": 3,
        "delay_ms": 150, // 150ms per item
    });

    println!("   Processing 20 items (will abort after 500ms)...\n");

    // Spawn event processor
    let event_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgenticEvent::ToolStart { name, .. } => {
                    println!("   🚀 Tool '{}' started", name);
                }
                AgenticEvent::ToolUpdate { output, progress_percent, .. } => {
                    if let Some(percent) = progress_percent {
                        let bar = progress_bar(percent);
                        println!("   📊 {} {}% - {}", bar, percent, output);
                    }
                }
                AgenticEvent::ToolEnd { success, .. } => {
                    let icon = if success { "✅" } else { "❌" };
                    println!("   {} Tool ended", icon);
                    break;
                }
                _ => {}
            }
        }
    });

    // Spawn abort timer
    let abort_signal = signal.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(500)).await;
        println!("   🛑 Sending abort signal...");
        abort_signal.abort();
    });

    // Run the tool
    let start = Instant::now();
    let result = tool.execute_with_context(params, &ctx).await;
    let elapsed = start.elapsed();

    // Wait for event processor
    let _ = event_handle.await;

    // Show result
    match result {
        Ok(_) => {
            println!("\n   ⚠️  Tool completed before abort (took {:?})", elapsed);
        }
        Err(e) => {
            println!("\n   ✅ Tool aborted as expected: {}", e);
            println!("   ⏱️  Time to abort: {:?}", elapsed);
        }
    }
}

async fn run_with_timeout(tool: &ProgressDemoTool) {
    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);

    // Create abort signal and context with timeout
    let signal = AbortSignal::new();
    let ctx = signal
        .create_context_with_events("example-run", "demo-3", "progress-demo", event_tx)
        .with_timeout(Duration::from_millis(300)); // 300ms timeout

    // Set up parameters that would take longer than timeout
    let items: Vec<String> = (0..15).map(|i| format!("file{}.txt", i)).collect();
    let params = json!({
        "items": items,
        "batch_size": 2,
        "delay_ms": 50, // 50ms per item = 750ms total
    });

    println!("   Processing 15 items with 300ms timeout (will timeout)...\n");

    // Spawn event processor
    let event_handle = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgenticEvent::ToolStart { name, .. } => {
                    println!("   🚀 Tool '{}' started", name);
                }
                AgenticEvent::ToolUpdate { output, progress_percent, .. } => {
                    if let Some(percent) = progress_percent {
                        let bar = progress_bar(percent);
                        println!("   📊 {} {}% - {}", bar, percent, output);
                    }
                }
                AgenticEvent::ToolEnd { success, .. } => {
                    let icon = if success { "✅" } else { "❌" };
                    println!("   {} Tool ended", icon);
                    break;
                }
                _ => {}
            }
        }
    });

    // Run the tool
    let start = Instant::now();
    let result = tool.execute_with_context(params, &ctx).await;
    let elapsed = start.elapsed();

    // Wait for event processor
    let _ = event_handle.await;

    // Show result
    match result {
        Ok(_) => {
            println!("\n   ⚠️  Tool completed before timeout (took {:?})", elapsed);
        }
        Err(e) => {
            if e.to_string().contains("timeout") || e.to_string().contains("timed out") {
                println!("\n   ✅ Tool timed out as expected: {}", e);
                println!("   ⏱️  Time before timeout: {:?}", elapsed);
            } else {
                println!("\n   ❌ Unexpected error: {}", e);
            }
        }
    }
}

async fn run_with_throttling(tool: &ProgressDemoTool) {
    // Create event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);

    // Create abort signal and context with aggressive throttling
    let signal = AbortSignal::new();
    let ctx = signal
        .create_context_with_events("example-run", "demo-4", "progress-demo", event_tx)
        .with_throttle(1000); // 1 second between progress updates

    // Set up parameters for quick processing
    let items: Vec<String> = (0..10).map(|i| format!("item{}", i)).collect();
    let params = json!({
        "items": items,
        "batch_size": 1,
        "delay_ms": 100, // 100ms per item = 1000ms total
    });

    println!("   Processing 10 items with 1 second progress throttling...\n");
    println!("   (Total work ~1000ms, but progress updates throttled to 1/sec)\n");

    // Collect events
    let start = Instant::now();
    let event_handle = tokio::spawn(async move {
        let mut event_count = 0;
        while let Some(event) = event_rx.recv().await {
            match event {
                AgenticEvent::ToolUpdate { output, progress_percent, .. } => {
                    event_count += 1;
                    if let Some(percent) = progress_percent {
                        println!("   📊 {}% - {} (event #{})", percent, output, event_count);
                    }
                }
                AgenticEvent::ToolEnd { .. } => break,
                _ => {}
            }
        }
        event_count
    });

    // Run the tool
    let _ = tool.execute_with_context(params, &ctx).await;
    let elapsed = start.elapsed();

    // Wait for event processor
    let event_count = event_handle.await.unwrap();

    println!("\n   ✅ Completed in {:?}", elapsed);
    println!("   📊 Received only {} progress events (throttled)", event_count);
    println!("   (Without throttling, would have received ~10 events)");
}

fn progress_bar(percent: u8) -> String {
    let filled = (percent as usize) / 10;
    let empty = 10 - filled;
    let filled_str = "█".repeat(filled);
    let empty_str = "░".repeat(empty);
    format!("[{}{}]", filled_str, empty_str)
}
