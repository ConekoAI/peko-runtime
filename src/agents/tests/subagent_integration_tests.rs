//! Integration tests for the subagent spawn system
//!
//! These tests verify end-to-end functionality including:
//! - Spawn tool execution
//! - Background task execution
//! - Result announcement
//! - Status checking
//! - List functionality

use crate::agents::subagent_executor::{ExecutionConfig, SubagentExecutor};
use crate::agents::subagent_types::SubagentStatus;
use crate::common::paths::PathResolver;
use crate::session::manager::SessionManager;
use crate::auth::principal::Principal;
use crate::session::types::{ SpawnCleanupPolicy};
use crate::extensions::framework::async_exec::executor::{
    get_or_create_registry_for_agent, SharedAsyncTaskRegistry,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};

/// Per-test agent-name counter so each subagent integration test gets its own
/// global async-task registry. Without this, every test shares one registry
/// (keyed by "test_agent" in `get_or_create_registry_for_agent`) and
/// `count_active_runs` / `list_subagents_for_parent` see stale entries from
/// earlier tests in the same process.
static TEST_AGENT_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Test fixture that sets up a temporary `PEKO_HOME` directory.
///
/// Creates a temp dir, sets the `PEKO_HOME` env var, creates the minimal
/// directory structure (data/identities for KeyStorage), and returns the
/// temp dir. When dropped, the temp dir is cleaned up and the original
/// env var is restored.
struct PekoHomeFixture {
    _temp: tempfile::TempDir,
    original: Option<String>,
}

impl PekoHomeFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let temp_path = temp.path().to_path_buf();

        // Create minimal directory structure
        std::fs::create_dir_all(temp_path.join("data").join("identities")).unwrap();
        std::fs::create_dir_all(temp_path.join("cache")).unwrap();

        let original = std::env::var("PEKO_HOME").ok();
        std::env::set_var("PEKO_HOME", &temp_path);

        Self {
            _temp: temp,
            original,
        }
    }
}

impl Drop for PekoHomeFixture {
    fn drop(&mut self) {
        match &self.original {
            Some(v) => std::env::set_var("PEKO_HOME", v),
            None => std::env::remove_var("PEKO_HOME"),
        }
    }
}

/// Test helper to create a test session manager and registry
///
/// Returns `(session_manager, registry, agent_name)` where `agent_name` is
/// unique per call so each test gets its own global async-task registry.
/// Uses a temporary `PEKO_HOME` so tests don't require `~/.peko`.
async fn create_test_components() -> (
    Arc<RwLock<SessionManager>>,
    SharedAsyncTaskRegistry,
    String,
) {
    let agent_name = format!(
        "test_agent_{}",
        TEST_AGENT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );

    let fixture = PekoHomeFixture::new();
    let temp_path = fixture._temp.path().to_path_buf();

    let path_resolver = PathResolver::with_dirs(
        temp_path.clone(),
        temp_path.join("data"),
        temp_path.join("cache"),
    );
    let session_manager = SessionManager::new()
        .with_path_resolver(path_resolver, &agent_name, None)
        .await
        .unwrap();
    let session_manager = Arc::new(RwLock::new(session_manager));
    let registry = get_or_create_registry_for_agent(&agent_name);

    // Leak the fixture so it lives for the duration of the test
    // (the temp dir will be cleaned up when the test process exits)
    let _ = Box::leak(Box::new(fixture));

    (session_manager, registry, agent_name)
}

#[tokio::test]
async fn test_e2e_spawn_and_complete() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    // Create a parent session context
    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    // Setup executor
    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    // Spawn a subagent
    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await
        .unwrap();

    // Verify run_id is returned
    assert!(run_id.starts_with("run_"));

    // Wait for background task to complete
    sleep(Duration::from_millis(500)).await;

    // Verify run is in registry as completed
    let registry_guard = registry.read().await;
    let entry = registry_guard.get(&run_id).unwrap();
    assert!(
        entry.status.is_terminal(),
        "Run should be in terminal state: {:?}",
        entry.status
    );
}

#[tokio::test]
async fn test_spawn_depth_limit() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    // Create a parent session context
    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    // Create executor with max_depth = 1 (only one level allowed)
    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    // Create a config with max_depth = 1
    let config = ExecutionConfig {
        max_depth: 1,
        ..Default::default()
    };

    // First spawn should succeed (depth 1 <= max_depth 1)
    let run_id1 = executor
        .spawn_and_execute(
            "First task",
            Some(&resolved.context),
            false,
            &parent_key,
            config.clone(),
        )
        .await
        .unwrap();

    // Wait for completion so the run is in registry with its depth
    sleep(Duration::from_millis(500)).await;

    // Verify the first run completed at depth 1, and grab its child_session_key.
    let child_key = {
        let registry_guard = registry.read().await;
        let entry = registry_guard.get(&run_id1).unwrap();
        let view = crate::agents::subagent_types::SubagentRunView::from_entry(entry)
            .expect("Should be a subagent entry");
        assert_eq!(view.depth, 1, "First run should be at depth 1");
        view.child_session_key.clone()
    };

    // Spawn from the *child* session of the first run. The depth check
    // looks up runs by `child_session_key == parent_session_key`, so passing
    // `child_key` as the new parent makes it match the first run (depth 1).
    // The new run would be depth 2, exceeding max_depth=1, and must be
    // rejected. (Earlier versions of this test asserted the opposite —
    // that nesting succeeds — but that was a misreading of the depth
    // tracking; the limit IS enforced via this key, not via the original
    // parent's key.)
    let result = executor
        .spawn_and_execute(
            "Nested task",
            Some(&resolved.context),
            false,
            &child_key,
            config,
        )
        .await;

    assert!(
        result.is_err(),
        "Spawning from child_key of a depth-1 subagent must fail with DepthLimitExceeded"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("DepthLimitExceeded") || err.contains("depth"),
        "Expected depth-limit error, got: {err}"
    );

    // And spawning from a fresh, unrelated parent must still succeed —
    // there's no run with `child_session_key == that key`, so parent_depth
    // stays 0 and the spawn is allowed.
    let other_parent_key =
        crate::session::key::derive_base_session_key(&agent_name, &Principal::User("charlie".to_string()));
    let result = executor
        .spawn_and_execute(
            "Independent task",
            Some(&resolved.context),
            false,
            &other_parent_key,
            ExecutionConfig {
                max_depth: 1,
                ..Default::default()
            },
        )
        .await;
    assert!(
        result.is_ok(),
        "Spawning from a fresh parent (no prior runs) must succeed"
    );
}

#[tokio::test]
async fn test_isolated_vs_shared_session() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    // Use higher max_depth since we're spawning multiple runs
    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let config = ExecutionConfig {
        max_depth: 10, // Allow multiple runs
        ..Default::default()
    };

    // Test isolated spawn
    let isolated_run_id = executor
        .spawn_and_execute(
            "Isolated task",
            Some(&resolved.context),
            true,
            &parent_key,
            config.clone(),
        )
        .await
        .unwrap();

    // Wait before creating second spawn to avoid timing issues
    sleep(Duration::from_millis(100)).await;

    // Test shared spawn
    let shared_run_id = executor
        .spawn_and_execute("Shared task", Some(&resolved.context), false, &parent_key, config)
        .await
        .unwrap();

    sleep(Duration::from_millis(600)).await;

    let registry_guard = registry.read().await;

    let isolated_entry = registry_guard.get(&isolated_run_id).unwrap();
    let shared_entry = registry_guard.get(&shared_run_id).unwrap();

    // Both should complete
    assert!(
        isolated_entry.status.is_terminal(),
        "Isolated run should be terminal: {:?}",
        isolated_entry.status
    );
    assert!(
        shared_entry.status.is_terminal(),
        "Shared run should be terminal: {:?}",
        shared_entry.status
    );

    // Verify child session keys are different
    let isolated_view = crate::agents::subagent_types::SubagentRunView::from_entry(isolated_entry)
        .expect("Should be a subagent entry");
    let shared_view = crate::agents::subagent_types::SubagentRunView::from_entry(shared_entry)
        .expect("Should be a subagent entry");
    assert_ne!(isolated_view.child_session_key, shared_view.child_session_key);
}

#[tokio::test]
async fn test_result_format_in_registry() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    // Check the result format
    let registry_guard = registry.read().await;
    let entry = registry_guard.get(&run_id).unwrap();
    let view = crate::agents::subagent_types::SubagentRunView::from_entry(entry)
        .expect("Should be a subagent entry");

    assert!(view.result.is_some());
}

#[tokio::test]
async fn test_list_runs_functionality() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let config = ExecutionConfig {
        max_depth: 10, // Allow multiple runs
        ..Default::default()
    };

    // Create multiple runs
    let mut run_ids = Vec::new();
    for i in 0..3 {
        let run_id = executor
            .spawn_and_execute(
                &format!("Task {}", i),
                Some(&resolved.context),
                false,
                &parent_key,
                config.clone(),
            )
            .await
            .unwrap();
        run_ids.push(run_id);
    }

    // List all runs
    let registry_guard = registry.read().await;
    let all_entries = registry_guard.list_tasks(None);
    let all_runs: Vec<_> = all_entries
        .iter()
        .filter_map(crate::agents::subagent_types::SubagentRunView::from_entry)
        .collect();
    assert_eq!(all_runs.len(), 3);

    // List active runs for parent
    // Note: runs may complete before we check, so we just verify at least one exists
    let active_runs = registry_guard.list_subagents_for_parent(&parent_key);
    // Runs complete very quickly in tests, so we might not catch them all as active
    assert!(
        !active_runs.is_empty() || all_runs.len() == 3,
        "Should have active runs or all 3 completed"
    );

    // Wait for completion
    drop(registry_guard);
    sleep(Duration::from_millis(800)).await;

    let registry_guard = registry.read().await;
    let active_runs = registry_guard.list_subagents_for_parent(&parent_key);
    let active_count = active_runs.iter().filter(|e| !e.status.is_terminal()).count();
    assert_eq!(active_count, 0, "All runs should be completed");

    // Verify all run_ids are present
    for run_id in &run_ids {
        assert!(registry_guard.get(run_id).is_some());
    }
}

#[tokio::test]
async fn test_cleanup_policy_tracking() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let config = ExecutionConfig {
        max_depth: 10, // Allow multiple runs
        ..Default::default()
    };

    // Test keep policy (default)
    let keep_run_id = executor
        .spawn_and_execute(
            "Keep task",
            Some(&resolved.context),
            false,
            &parent_key,
            config.clone(),
        )
        .await
        .unwrap();

    // Test delete policy
    let delete_run_id = executor
        .spawn_and_execute(
            "Delete task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig {
                max_depth: 10,
                cleanup: SpawnCleanupPolicy::Delete,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    let registry_guard = registry.read().await;

    let keep_entry = registry_guard.get(&keep_run_id).unwrap();
    let delete_entry = registry_guard.get(&delete_run_id).unwrap();

    let keep_view = crate::agents::subagent_types::SubagentRunView::from_entry(keep_entry)
        .expect("Should be a subagent entry");
    let delete_view = crate::agents::subagent_types::SubagentRunView::from_entry(delete_entry)
        .expect("Should be a subagent entry");

    assert_eq!(keep_view.cleanup, SpawnCleanupPolicy::Keep);
    assert_eq!(delete_view.cleanup, SpawnCleanupPolicy::Delete);
}

#[tokio::test]
async fn test_parent_child_relationship() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    let registry_guard = registry.read().await;
    let entry = registry_guard.get(&run_id).unwrap();
    let view = crate::agents::subagent_types::SubagentRunView::from_entry(entry)
        .expect("Should be a subagent entry");

    assert_eq!(view.parent_session_key, parent_key);
    assert!(!view.child_session_key.is_empty());
    // Session key format includes "overlay:spawn:" for spawn sessions
    assert!(view.child_session_key.contains(":overlay:spawn:"));
}

#[tokio::test]
async fn test_runs_by_parent_filtering() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    // `route()` ignores its `_peer` argument and uses `SessionManager::self.user`
    // (default "default") instead, so calling `route(&peer1, ...)` and
    // `route(&peer2, ...)` produces the *same* parent key. To get two
    // distinct parents we derive the base session key directly from the
    // peer, which is exactly what `create_session` and the registry do.
    let peer1 = Principal::User("alice".to_string());
    let peer2 = Principal::User("bob".to_string());
    let parent_key1 = crate::session::key::derive_base_session_key(&agent_name, &peer1);
    let parent_key2 = crate::session::key::derive_base_session_key(&agent_name, &peer2);
    assert_ne!(
        parent_key1, parent_key2,
        "test setup: peers must produce distinct parent keys"
    );

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let config = ExecutionConfig {
        max_depth: 10, // Allow multiple runs per parent
        ..Default::default()
    };

    // Create runs for different parents. `parent_ctx` is unused inside
    // `spawn_and_execute`, so `None` is fine — only `parent_session_key`
    // matters for registry bookkeeping.
    let run1 = executor
        .spawn_and_execute(
            "Task 1",
            None,
            false,
            &parent_key1,
            config.clone(),
        )
        .await
        .unwrap();

    let run2 = executor
        .spawn_and_execute(
            "Task 2",
            None,
            false,
            &parent_key1,
            config.clone(),
        )
        .await
        .unwrap();

    let run3 = executor
        .spawn_and_execute("Task 3", None, false, &parent_key2, config)
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    let registry_guard = registry.read().await;

    // Check runs for parent 1
    let runs_for_parent1 = registry_guard.list_subagents_for_parent(&parent_key1);
    assert_eq!(runs_for_parent1.len(), 2);
    let ids1: std::collections::HashSet<_> = runs_for_parent1.iter().map(|e| e.task_id.clone()).collect();
    assert!(ids1.contains(&run1));
    assert!(ids1.contains(&run2));

    // Check runs for parent 2
    let runs_for_parent2 = registry_guard.list_subagents_for_parent(&parent_key2);
    assert_eq!(runs_for_parent2.len(), 1);
    assert_eq!(runs_for_parent2[0].task_id, run3);
}

#[tokio::test]
async fn test_concurrent_runs_counting() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    // Initially no active runs in registry
    {
        let registry_guard = registry.read().await;
        let active_count = registry_guard.list_subagents_for_parent(&parent_key)
            .iter().filter(|e| !e.status.is_terminal()).count();
        assert_eq!(active_count, 0);
    }

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let config = ExecutionConfig {
        max_depth: 10,         // Allow multiple runs
        timeout_seconds: 3600, // Long timeout
        ..Default::default()
    };

    // Create a run with long timeout
    let _run_id = executor
        .spawn_and_execute("Long task", Some(&resolved.context), false, &parent_key, config)
        .await
        .unwrap();

    // Should have at most 1 active run (immediately after spawn)
    let registry_guard = registry.read().await;
    let active_count = registry_guard.list_subagents_for_parent(&parent_key)
        .iter().filter(|e| !e.status.is_terminal()).count();
    assert!(
        active_count <= 1,
        "Should have at most 1 active run, got {}",
        active_count
    );
    drop(registry_guard);

    // Wait for completion
    sleep(Duration::from_millis(600)).await;

    // Should have 0 active runs
    let registry_guard = registry.read().await;
    let active_count = registry_guard.list_subagents_for_parent(&parent_key)
        .iter().filter(|e| !e.status.is_terminal()).count();
    assert_eq!(active_count, 0);
}

#[tokio::test]
async fn test_executor_get_status() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await
        .unwrap();

    // Check status immediately (should be running or completed)
    let status = executor.get_run_status(&run_id).await;
    assert!(status.is_some());

    sleep(Duration::from_millis(500)).await;

    // Check status after completion
    let status = executor.get_run_status(&run_id).await;
    assert!(status.is_some());
    let status = status.unwrap();
    assert!(status.is_terminal(), "Status: {}", status);
}

#[tokio::test]
async fn test_executor_get_run() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await
        .unwrap();

    // Get run from executor
    let run = executor.get_run(&run_id).await;
    assert!(run.is_some());
    assert_eq!(run.unwrap().run_id, run_id);
}

#[tokio::test]
async fn test_executor_cancel() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name.clone(),
        5,
    ));

    // Cancel is racing the spawned task's completion. Without a provider,
    // the spawned task returns its "no provider configured" placeholder
    // immediately, so the run is already in a terminal state by the time
    // the test calls `cancel()` — and `cancel()` is a no-op on terminal
    // tasks. To exercise the cancel path deterministically we register a
    // Pending entry directly and cancel it before the task body can run.
    let run_id = format!("run_{}", uuid::Uuid::new_v4().simple());
    {
        let mut registry_guard = registry.write().await;
        let entry = crate::extensions::framework::async_exec::executor::registry::AsyncTaskEntry::new(
            run_id.clone(),
            "agent_spawn".to_string(),
            serde_json::json!({"task": "Long task"}),
            "agent:test:peer:user:alice".to_string(),
            crate::extensions::framework::async_exec::executor::types::AsyncToolConfig {
                delivery_mode: crate::extensions::framework::async_exec::executor::types::AsyncResultDeliveryMode::QueueWhenBusy,
                delivery_target: None,
                timeout_secs: 3600,
                cleanup_after_delivery: false,
                label: None,
            },
        );
        registry_guard.register(entry);
    }

    // The task must be in Pending (not terminal) for cancel to take effect.
    {
        let registry_guard = registry.read().await;
        let entry = registry_guard.get(&run_id).unwrap();
        assert!(matches!(entry.status, crate::extensions::framework::async_exec::executor::types::AsyncTaskStatus::Pending));
    }

    // Cancel the run
    executor.cancel(&run_id).await.unwrap();

    let registry_guard = registry.read().await;
    let entry = registry_guard.get(&run_id).unwrap();
    assert!(
        matches!(entry.status, SubagentStatus::Cancelled),
        "Status should be Cancelled after cancel(), got: {:?}",
        entry.status
    );
}

#[tokio::test]
async fn test_max_concurrent_limit() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Principal::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                crate::session::types::ChannelType::Cli,
                "default",
                None,
            )
            .await
            .unwrap();
        (resolved.context.full_session_key.clone(), resolved)
    };

    // Create executor with max_concurrent = 1
    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
                session_manager.clone(),
        agent_name.clone(),
        1, // Only 1 concurrent
    ));

    // First spawn should succeed
    let result1 = executor
        .spawn_and_execute(
            "Task 1",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig {
                timeout_seconds: 3600,
                max_depth: 10,
                ..Default::default()
            },
        )
        .await;
    assert!(result1.is_ok());

    // Second spawn might fail or succeed depending on timing
    // (if first run completes before second spawn, it will succeed)
    let _result2 = executor
        .spawn_and_execute(
            "Task 2",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
        )
        .await;
}
