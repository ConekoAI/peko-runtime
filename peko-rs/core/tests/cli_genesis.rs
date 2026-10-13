//! The one mock-tier test that intentionally waits for CLI creation/genesis.
mod common;

use common::{run_with_timeout, seed_mock_provider_in_catalog, DaemonGuard, PekoCli};
use peko_core::principal::config::{BootState, PrincipalConfig};
use peko_core::principal::genesis::GENESIS_JOB_ID;
use std::time::Duration;

#[test]
#[ignore = "requires MOCK_LLM_URL and peko daemon"]
fn create_waits_for_successful_genesis_and_persists_organized_state() {
    let mock_url = std::env::var("MOCK_LLM_URL").expect("MOCK_LLM_URL required");
    let cli = PekoCli::new();
    seed_mock_provider_in_catalog(cli.home(), &mock_url);
    // Start before create so the entire daemon lifecycle belongs to the guard.
    let _daemon = DaemonGuard::spawn(&cli);
    let (output, stdout, stderr) = run_with_timeout(
        || cli.cmd(),
        &["create", "genesis-test", "--model", "mock-llm"],
        Duration::from_secs(150),
    )
    .expect("blocking create");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    let root = cli.peko_dir().join("principals/genesis-test");
    let config: PrincipalConfig =
        toml::from_str(&std::fs::read_to_string(root.join("principal.toml")).unwrap()).unwrap();
    assert_eq!(config.boot_state(), BootState::Organized);
    let paths = peko_core::common::paths::PathResolver::with_dirs(
        cli.peko_dir(),
        cli.peko_dir().join("data"),
        cli.peko_dir().join("cache"),
    );
    let scheduler = peko_cron::CronScheduler::new(paths.cron_schedule("genesis-test")).unwrap();
    let history = scheduler.get_run_history(GENESIS_JOB_ID, 1).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].status, "success");
    assert!(history[0].finished_at.is_some());
    assert!(!scheduler
        .list_jobs(true)
        .unwrap()
        .iter()
        .any(|job| job.id == GENESIS_JOB_ID));
}
