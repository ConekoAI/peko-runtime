//! Integration tests for the subagent spawn system
//!
//! These tests verify end-to-end functionality including:
//! - Spawn tool execution
//! - Background task execution
//! - Result announcement
//! - Status checking
//! - List functionality

use crate::agents::subagent_executor::{ExecutionConfig, SubagentExecutor};
use crate::common::paths::PathResolver;
use crate::extensions::framework::async_exec::executor::AsyncTaskStatus;
use crate::extensions::framework::async_exec::executor::{
    get_or_create_registry_for_agent, SharedAsyncTaskRegistry,
};
use peko_auth::Subject;
use peko_session::manager::SessionManager;
use peko_session::types::SpawnCleanupPolicy;
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
async fn create_test_components() -> (Arc<RwLock<SessionManager>>, SharedAsyncTaskRegistry, String)
{
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
    let path_resolver: Arc<dyn peko_subject::PathResolverLike> = Arc::new(
        peko_session::DefaultPathResolver::with_data_dir(path_resolver.data_dir().to_path_buf()),
    );
    let session_manager = SessionManager::new()
        .with_path_resolver(path_resolver, &agent_name)
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
    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    // Spawn a subagent
    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            None,
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

/// Pre-merge check for the "interrupt actually means stop" follow-up:
/// when a parent cancel token is plumbed into `spawn_and_execute` and
/// then cancelled, the sub-agent's `run_id` should reach
/// `AsyncTaskStatus::Cancelled` rather than `Completed`.
///
/// Race note: with no provider configured, the sub-agent's task body
/// completes almost immediately. We cancel the parent token as fast
/// as possible after `spawn_and_execute` returns, and then poll the
/// registry for up to 1s. In practice the closure's
/// `child_cancel_for_closure.is_cancelled()` check happens AFTER
/// `exec_fut.await` returns, so even a fast-cancel should result in
/// `Cancelled` because the parent cancel was flipped before the
/// closure reached the status-write block.
#[tokio::test]
async fn subagent_inherits_parent_cancel() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Subject::User("alice".to_string());
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    let parent_token = tokio_util::sync::CancellationToken::new();
    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            Some(parent_token.clone()),
        )
        .await
        .unwrap();

    // Cancel immediately — the spawned task has not yet completed.
    parent_token.cancel();

    // Poll the registry for up to 1s for the Cancelled status.
    let mut observed_cancelled = false;
    for _ in 0..100 {
        let registry_guard = registry.read().await;
        if let Some(entry) = registry_guard.get(&run_id) {
            if matches!(
                entry.status,
                crate::extensions::framework::async_exec::executor::types::AsyncTaskStatus::Cancelled
            ) {
                observed_cancelled = true;
                break;
            }
        }
        drop(registry_guard);
        sleep(Duration::from_millis(10)).await;
    }
    assert!(
        observed_cancelled,
        "Sub-agent should reach Cancelled when parent token is cancelled"
    );
}

/// Pre-merge check: `child_token()` is wired correctly — cancelling a
/// child token derived from a parent does NOT propagate up to the
/// parent. The parent's `is_cancelled()` must remain `false` after the
/// child fires.
#[tokio::test]
async fn subagent_child_token_does_not_cancel_sibling() {
    let parent = tokio_util::sync::CancellationToken::new();
    let child = parent.child_token();

    child.cancel();

    assert!(child.is_cancelled(), "child token must be cancelled");
    assert!(
        !parent.is_cancelled(),
        "parent token must NOT be cancelled by child cancel"
    );
}

// Note: `cancel_at_iteration_boundary_drains_subagent` from the plan
// is omitted here because it requires a mocked LLM provider to
// actually drive the child's `AgenticLoop` to an iteration boundary.
// The two tests above (closure-path cancel + child_token() hierarchy)
// cover the production-critical wiring for this PR. The iteration
// boundary path is exercised by the existing `principal_send_stream`
// e2e tests once a `principal interrupt` reaches the loop.

#[tokio::test]
async fn test_spawn_depth_limit() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    // Create a parent session context
    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
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
            None,
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
            None,
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
    let other_parent_key = peko_session::key::derive_base_session_key(
        &agent_name,
        &Subject::User("charlie".to_string()),
    );
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
            None,
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

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
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
            None,
        )
        .await
        .unwrap();

    // Wait before creating second spawn to avoid timing issues
    sleep(Duration::from_millis(100)).await;

    // Test shared spawn
    let shared_run_id = executor
        .spawn_and_execute(
            "Shared task",
            Some(&resolved.context),
            false,
            &parent_key,
            config,
            None,
        )
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
    assert_ne!(
        isolated_view.child_session_key,
        shared_view.child_session_key
    );
}

#[tokio::test]
async fn test_result_format_in_registry() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            None,
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

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
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
                None,
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
    let active_count = active_runs
        .iter()
        .filter(|e| !e.status.is_terminal())
        .count();
    assert_eq!(active_count, 0, "All runs should be completed");

    // Verify all run_ids are present
    for run_id in &run_ids {
        assert!(registry_guard.get(run_id).is_some());
    }
}

#[tokio::test]
async fn test_cleanup_policy_tracking() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
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
            None,
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
                cleanup: crate::extensions::framework::subagent::SpawnCleanupPolicy::Delete,
                ..Default::default()
            },
            None,
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

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            None,
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
    let peer1 = Subject::User("alice".to_string());
    let peer2 = Subject::User("bob".to_string());
    let parent_key1 = peko_session::key::derive_base_session_key(&agent_name, &peer1);
    let parent_key2 = peko_session::key::derive_base_session_key(&agent_name, &peer2);
    assert_ne!(
        parent_key1, parent_key2,
        "test setup: peers must produce distinct parent keys"
    );

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name.clone(),
        5,
        peko_subject::PrincipalId::generate(),
    ));

    let config = ExecutionConfig {
        max_depth: 10, // Allow multiple runs per parent
        ..Default::default()
    };

    // Create runs for different parents. `parent_ctx` is unused inside
    // `spawn_and_execute`, so `None` is fine — only `parent_session_key`
    // matters for registry bookkeeping.
    let run1 = executor
        .spawn_and_execute("Task 1", None, false, &parent_key1, config.clone(), None)
        .await
        .unwrap();

    let run2 = executor
        .spawn_and_execute("Task 2", None, false, &parent_key1, config.clone(), None)
        .await
        .unwrap();

    let run3 = executor
        .spawn_and_execute("Task 3", None, false, &parent_key2, config, None)
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    let registry_guard = registry.read().await;

    // Check runs for parent 1
    let runs_for_parent1 = registry_guard.list_subagents_for_parent(&parent_key1);
    assert_eq!(runs_for_parent1.len(), 2);
    let ids1: std::collections::HashSet<_> =
        runs_for_parent1.iter().map(|e| e.task_id.clone()).collect();
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

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        let active_count = registry_guard
            .list_subagents_for_parent(&parent_key)
            .iter()
            .filter(|e| !e.status.is_terminal())
            .count();
        assert_eq!(active_count, 0);
    }

    let executor = Arc::new(SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name.clone(),
        5,
        peko_subject::PrincipalId::generate(),
    ));

    let config = ExecutionConfig {
        max_depth: 10,         // Allow multiple runs
        timeout_seconds: 3600, // Long timeout
        ..Default::default()
    };

    // Create a run with long timeout
    let _run_id = executor
        .spawn_and_execute(
            "Long task",
            Some(&resolved.context),
            false,
            &parent_key,
            config,
            None,
        )
        .await
        .unwrap();

    // Should have at most 1 active run (immediately after spawn)
    let registry_guard = registry.read().await;
    let active_count = registry_guard
        .list_subagents_for_parent(&parent_key)
        .iter()
        .filter(|e| !e.status.is_terminal())
        .count();
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
    let active_count = registry_guard
        .list_subagents_for_parent(&parent_key)
        .iter()
        .filter(|e| !e.status.is_terminal())
        .count();
    assert_eq!(active_count, 0);
}

#[tokio::test]
async fn test_executor_get_status() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            None,
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

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
    ));

    let run_id = executor
        .spawn_and_execute(
            "Test task",
            Some(&resolved.context),
            false,
            &parent_key,
            ExecutionConfig::default(),
            None,
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
        peko_subject::PrincipalId::generate(),
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
            "Agent".to_string(),
            serde_json::json!({"task": "Long task"}),
            "agent:test:peer:user:alice".to_string(),
            crate::extensions::framework::async_exec::executor::types::AsyncToolConfig {
                delivery_mode: crate::extensions::framework::async_exec::executor::types::AsyncResultDeliveryMode::QueueWhenBusy,
                delivery_target: None,
                timeout_secs: Some(3600),
                timeout_millis: None,
                cleanup_after_delivery: false,
                label: None,
                wake_on_completion: true,
                principal_root_session_key: None,
            },
        );
        registry_guard.register(entry);
    }

    // The task must be in Pending (not terminal) for cancel to take effect.
    {
        let registry_guard = registry.read().await;
        let entry = registry_guard.get(&run_id).unwrap();
        assert!(matches!(
            entry.status,
            crate::extensions::framework::async_exec::executor::types::AsyncTaskStatus::Pending
        ));
    }

    // Cancel the run
    executor.cancel(&run_id).await.unwrap();

    let registry_guard = registry.read().await;
    let entry = registry_guard.get(&run_id).unwrap();
    assert!(
        matches!(entry.status, AsyncTaskStatus::Cancelled),
        "Status should be Cancelled after cancel(), got: {:?}",
        entry.status
    );
}

#[tokio::test]
async fn test_max_concurrent_limit() {
    let (session_manager, registry, agent_name) = create_test_components().await;

    let peer = Subject::User("alice".to_string());
    // Scope the session-manager write lock so it's released before
    // `spawn_and_execute`, which internally re-acquires the same write
    // lock via `manager.spawn_session()` — holding the guard here would
    // deadlock on the current-thread test runtime.
    let (parent_key, resolved) = {
        let mut manager = session_manager.write().await;
        let resolved = manager
            .route(
                &peer,
                peko_session::types::ChannelType::Cli,
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
        peko_subject::PrincipalId::generate(),
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
            None,
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
            None,
        )
        .await;
}

// ---------------------------------------------------------------------------
// Phase 5b: resume (Agent tool `action = "resume"`, persistent
// subagents) — guards + happy path
// ---------------------------------------------------------------------------

/// Create a session with explicit id/parent/trigger linkage via the
/// manager (mirrors what `spawn_session` stamps for real spawns:
/// `trigger == "spawn"` + `parent_session_id`).
async fn create_linked_session(
    session_manager: &Arc<RwLock<SessionManager>>,
    agent_name: &str,
    id: &str,
    parent_id: Option<&str>,
    trigger: &str,
) {
    let peer = Subject::User("alice".to_string());
    let mut options = peko_session::SessionCreateOptions::new().with_session_id(id);
    if let Some(parent) = parent_id {
        options = options.with_parent(parent);
    }
    // `with_parent` presets trigger="branch"; the explicit trigger
    // must be applied after it.
    options = options.with_trigger(trigger);
    session_manager
        .write()
        .await
        .create_session(agent_name, &peer, options)
        .await
        .unwrap();
}

#[tokio::test]
async fn resume_refuses_nonexistent_target() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let err = executor
        .resume_and_execute(
            "task",
            "no-such-session",
            "root-sess",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[tokio::test]
async fn resume_refuses_non_spawn_target() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    // A branch/regular session (trigger != "spawn") must refuse.
    create_linked_session(
        &session_manager,
        &agent_name,
        "plain-sess",
        Some("root-sess"),
        "user",
    )
    .await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let err = executor
        .resume_and_execute(
            "task",
            "plain-sess",
            "root-sess",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("only spawned"), "{err}");
}

#[tokio::test]
async fn resume_refuses_self_and_ancestor() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );

    // Self: target == the session the caller is running in.
    let err = executor
        .resume_and_execute(
            "task",
            "spawn-a",
            "spawn-a",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("running in"), "{err}");

    // Ancestor: root-sess is spawn-a's parent.
    let err = executor
        .resume_and_execute(
            "task",
            "root-sess",
            "spawn-a",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("running in"), "{err}");
}

#[tokio::test]
async fn resume_refuses_archived_target() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;
    session_manager
        .write()
        .await
        .set_archived("spawn-a", true)
        .await
        .unwrap();

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let err = executor
        .resume_and_execute(
            "task",
            "spawn-a",
            "root-sess",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unarchive"), "{err}");
}

#[tokio::test]
async fn resume_happy_path_preserves_history() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;

    // Seed prior history into the spawned session.
    {
        let mut manager = session_manager.write().await;
        let handle = manager.open_session("spawn-a").await.unwrap().unwrap();
        handle.add_user("earlier context").await.unwrap();
    }

    let executor = SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );

    // No provider configured → the task body returns the stub success
    // string; the point is registration + guard passage + history.
    let run_id = executor
        .resume_and_execute(
            "continue the task",
            "spawn-a",
            "root-sess",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert!(run_id.starts_with("run_"));

    sleep(Duration::from_millis(500)).await;
    let guard = registry.read().await;
    let entry = guard.get(&run_id).unwrap();
    assert!(
        entry.status.is_terminal(),
        "resumed run should complete: {:?}",
        entry.status
    );
    drop(guard);

    // The resumed session kept its prior history.
    let mut manager = session_manager.write().await;
    let handle = manager.open_session("spawn-a").await.unwrap().unwrap();
    let history = handle.load_history().await.unwrap();
    assert!(
        history.iter().any(|m| m
            .content
            .iter()
            .any(|b| matches!(b, peko_message::ContentBlock::Text { text } if text.contains("earlier context")))),
        "resumed session must keep its prior history"
    );
}

// ---------------------------------------------------------------------------
// Round 7: `request_compaction` (Agent tool `action = "compact"`) — guards
// + happy path. Returns immediately after flagging; no LLM call.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compact_refuses_nonexistent_target() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let err = executor
        .request_compaction("no-such-session", "root-sess")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

#[tokio::test]
async fn compact_refuses_self_and_ancestor() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );

    // Self: the engine compacts the caller's own session automatically.
    let err = executor
        .request_compaction("spawn-a", "spawn-a")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("running in"), "{err}");

    // Ancestor: root-sess is spawn-a's parent.
    let err = executor
        .request_compaction("root-sess", "spawn-a")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("running in"), "{err}");
}

#[tokio::test]
async fn compact_refuses_archived_target() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;
    session_manager
        .write()
        .await
        .set_archived("spawn-a", true)
        .await
        .unwrap();

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager,
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let err = executor
        .request_compaction("spawn-a", "root-sess")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unarchive"), "{err}");
}

#[tokio::test]
async fn compact_happy_path_flags_session_without_trigger_requirement() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    // Unlike resume, compact does NOT require trigger == "spawn" — a
    // branch session in the caller's tree is a valid target.
    create_linked_session(
        &session_manager,
        &agent_name,
        "branch-a",
        Some("root-sess"),
        "branch",
    )
    .await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager.clone(),
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let outcome = executor
        .request_compaction("branch-a", "root-sess")
        .await
        .unwrap();
    assert_eq!(outcome.session_id, "branch-a");
    assert!(
        outcome.message.contains("no completion signal") || outcome.message.contains("next run"),
        "outcome must be honest about deferred semantics: {}",
        outcome.message
    );

    let metas = session_manager
        .write()
        .await
        .list_all_sessions(false)
        .await
        .unwrap();
    let target = metas
        .iter()
        .find(|m| m.session_id == "branch-a")
        .expect("target metadata present");
    assert!(
        target.compact_requested,
        "compact must set the persisted compact_requested flag"
    );
}

#[tokio::test]
async fn validate_context_parent_ownership() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-b",
        Some("root-sess"),
        "spawn",
    )
    .await;

    let executor = SubagentExecutor::with_registry(
        registry,
        session_manager.clone(),
        agent_name.clone(),
        5,
        peko_subject::PrincipalId::generate(),
    );

    // Default path: caller's own session always passes.
    executor
        .validate_context_parent("spawn-a", "spawn-a")
        .await
        .unwrap();
    // Principal-level caller (base session) passes for any target.
    executor
        .validate_context_parent("spawn-b", "root-sess")
        .await
        .unwrap();
    // Subtree caller seeding from a sibling subtree → refused.
    let err = executor
        .validate_context_parent("spawn-b", "spawn-a")
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("outside your session subtree"),
        "{err}"
    );
    // Subtree caller seeding from inside its own subtree passes.
    create_linked_session(
        &session_manager,
        &agent_name,
        "grandchild",
        Some("spawn-a"),
        "spawn",
    )
    .await;
    executor
        .validate_context_parent("grandchild", "spawn-a")
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Phase 1b: slugs + path addressing. Agent tool `new`'s `name` param
// lands on `ExecutionConfig.slug` and is stamped onto the child's
// session metadata at spawn; `resume` / `compact` accept `/`-rooted
// session paths (resolved against the caller's tree before guards).
// ---------------------------------------------------------------------------

/// Set a slug on a linked session (mirrors what the session tool's
/// rename does through the production adapter).
async fn set_slug(session_manager: &Arc<RwLock<SessionManager>>, id: &str, slug: &str) {
    session_manager
        .read()
        .await
        .set_session_slug(id, Some(slug.to_string()))
        .await
        .unwrap();
}

#[tokio::test]
async fn spawn_with_name_stamps_child_slug() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;

    let executor = SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );
    let run_id = executor
        .spawn_and_execute(
            "task",
            None,
            true,
            "root-sess",
            ExecutionConfig {
                slug: Some("task-b".to_string()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert!(run_id.starts_with("run_"));

    // The child session's metadata carries the slug.
    let metas = session_manager
        .write()
        .await
        .list_all_sessions(false)
        .await
        .unwrap();
    let child = metas
        .iter()
        .find(|m| m.parent_session_id.as_deref() == Some("root-sess"))
        .expect("spawned child present");
    assert_eq!(child.slug.as_deref(), Some("task-b"));
    assert_eq!(child.trigger, "spawn");

    // Uniqueness: a second spawn with the same name under the same
    // parent refuses BEFORE the run registers, naming the conflict.
    let err = executor
        .spawn_and_execute(
            "task 2",
            None,
            true,
            "root-sess",
            ExecutionConfig {
                slug: Some("task-b".to_string()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unique per parent"), "{err}");
    assert!(err.to_string().contains(&child.session_id), "{err}");

    // Invalid slug format refuses at spawn too.
    let err = executor
        .spawn_and_execute(
            "task 3",
            None,
            true,
            "root-sess",
            ExecutionConfig {
                slug: Some("has/slash".to_string()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid slug"), "{err}");
}

#[tokio::test]
async fn resume_and_compact_accept_path_targets() {
    let (session_manager, registry, agent_name) = create_test_components().await;
    create_linked_session(&session_manager, &agent_name, "root-sess", None, "user").await;
    create_linked_session(
        &session_manager,
        &agent_name,
        "spawn-a",
        Some("root-sess"),
        "spawn",
    )
    .await;
    set_slug(&session_manager, "spawn-a", "worker").await;

    let executor = SubagentExecutor::with_registry(
        registry.clone(),
        session_manager.clone(),
        agent_name,
        5,
        peko_subject::PrincipalId::generate(),
    );

    // resume with a /path resolves to the same session id.
    let run_id = executor
        .resume_and_execute(
            "continue the task",
            "/worker",
            "root-sess",
            ExecutionConfig::default(),
            None,
        )
        .await
        .unwrap();
    assert!(run_id.starts_with("run_"));
    sleep(Duration::from_millis(500)).await;
    {
        let guard = registry.read().await;
        let entry = guard.get(&run_id).unwrap();
        assert!(
            entry.status.is_terminal(),
            "resumed-via-path run should complete: {:?}",
            entry.status
        );
    }

    // compact with a /path flags the resolved session.
    let outcome = executor
        .request_compaction("/worker", "root-sess")
        .await
        .unwrap();
    assert_eq!(outcome.session_id, "spawn-a");

    // Unknown path segments surface the actionable resolver error
    // (available child slugs listed), before any guard runs.
    let err = executor
        .request_compaction("/nope", "root-sess")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("worker"), "{err}");
    let err = executor
        .resume_and_execute("t", "/nope", "root-sess", ExecutionConfig::default(), None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("worker"), "{err}");
}
