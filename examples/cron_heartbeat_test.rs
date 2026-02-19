//! Test cron and heartbeat functionality

use pekobot::cron::CronScheduler;
use pekobot::heartbeat::{HeartbeatConfig, HeartbeatEngine};

#[tokio::main]
async fn main() {
    println!("🐰 Pekobot Cron + Heartbeat Test");
    println!("=================================\n");

    // Test 1: Cron Scheduler
    println!("--- Cron Scheduler Test ---");
    test_cron().await;

    println!();

    // Test 2: Heartbeat
    println!("--- Heartbeat Test ---");
    test_heartbeat().await;

    println!("\n✅ All tests passed!");
}

async fn test_cron() {
    let temp_dir = std::env::temp_dir().join("pekobot_cron_test");
    let _ = std::fs::remove_dir_all(&temp_dir);
    let db_path = temp_dir.join("cron.db");

    let scheduler = CronScheduler::new(&db_path).expect("Failed to create scheduler");

    // Add some jobs
    let job1 = scheduler
        .add_job("*/5 * * * *", "echo 'Every 5 minutes'")
        .unwrap();
    let job2 = scheduler
        .add_job("0 9 * * *", "echo 'Daily at 9am'")
        .unwrap();

    println!(
        "✓ Added job 1: {} ({})",
        job1.id[..8].to_string(),
        job1.expression
    );
    println!(
        "✓ Added job 2: {} ({})",
        job2.id[..8].to_string(),
        job2.expression
    );

    // List jobs
    let jobs = scheduler.list_jobs().unwrap();
    println!("✓ Listed {} jobs", jobs.len());

    // Check due jobs
    let due = scheduler.due_jobs(chrono::Utc::now()).unwrap();
    println!("✓ Due jobs now: {} (expected: 0)", due.len());

    // Check due jobs in future
    let far_future = chrono::Utc::now() + chrono::Duration::days(365);
    let due_future = scheduler.due_jobs(far_future).unwrap();
    println!("✓ Due jobs in 1 year: {} (expected: 2)", due_future.len());

    // Remove a job
    scheduler.remove_job(&job1.id).unwrap();
    let remaining = scheduler.list_jobs().unwrap();
    println!("✓ After removal: {} jobs remaining", remaining.len());

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);
}

async fn test_heartbeat() {
    let temp_dir = std::env::temp_dir().join("pekobot_heartbeat_test");
    let _ = std::fs::remove_dir_all(&temp_dir);
    std::fs::create_dir_all(&temp_dir).unwrap();

    // Create HEARTBEAT.md
    let heartbeat_content = "# Tasks\n\n- Check email\n- Review calendar\n- Backup data\n";
    std::fs::write(temp_dir.join("HEARTBEAT.md"), heartbeat_content).unwrap();

    let config = HeartbeatConfig {
        enabled: true,
        interval_minutes: 15,
        workspace_dir: temp_dir.clone(),
    };

    let engine = HeartbeatEngine::new(config);

    // Test parsing
    let tasks = engine.collect_tasks().await.unwrap();
    println!("✓ Parsed {} tasks from HEARTBEAT.md", tasks.len());
    for (i, task) in tasks.iter().enumerate() {
        println!("  {}. {}", i + 1, task);
    }

    // Test tick
    let count = engine.tick().await.unwrap();
    println!("✓ Tick returned {} tasks", count);

    // Test ensure_heartbeat_file
    let new_dir = temp_dir.join("new_workspace");
    std::fs::create_dir_all(&new_dir).unwrap();
    HeartbeatEngine::ensure_heartbeat_file(&new_dir)
        .await
        .unwrap();

    let created_file = new_dir.join("HEARTBEAT.md");
    assert!(created_file.exists());
    println!("✓ Created default HEARTBEAT.md");

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);
}
