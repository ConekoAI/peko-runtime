//! Principal genesis pipeline (ADR-054) — P2 seeding primitives.
//!
//! A principal moves through five phases: **P0 Provision** (workspace
//! tiers, DID, config — model-free), **P1 Definition** (identity,
//! intent, agent prompt — model-free), **P2 Genesis** (the trunk's
//! first self-turn), **P3 Induction** (bounded self-organization) and
//! **P4 Steady state** (the keepalive rhythm). P0 and P1 live in
//! `PrincipalManager::create` and the CLI/definition surfaces; this
//! module owns the P2 runtime half:
//!
//! - [`genesis_brief`] — the runtime-authored message that drives the
//!   trunk's FIRST self-turn. It points the trunk at its own
//!   definition and workspace and asks it to organize itself.
//! - [`keepalive_tick_message`] — the recurring supervision tick that
//!   keeps the principal an *active actor* (PEKO §K).
//! - [`genesis_job`] / [`keepalive_job`] — the two default cron jobs
//!   that carry those messages: a one-shot `At` genesis turn
//!   (`delete_after_run`) and a recurring trunk-targeted `Every` send
//!   (respecting the 60s `TRUNK_MIN_INTERVAL_MS` floor by
//!   construction — the default cadence is 10 minutes).
//! - [`seed_boot_defaults`] — the daemon-boot pass that guarantees a
//!   freshly created (or legacy, pre-ADR-054) principal is never a
//!   passive request handler: for every principal whose
//!   [`BootState`] is not yet `organized`, it ensures both jobs exist
//!   and stamps `genesis_pending`.
//!
//! ## Ownership handoff
//!
//! Once a principal reaches `organized`, the runtime NEVER touches its
//! cron schedule again — the trunk is self-regulating (PEKO.md §K: it
//! holds the cron tools and may retune or replace its own jobs). The
//! runtime only guarantees that a heartbeat EXISTS; what it does is
//! the trunk's business. The state flip to `organized` itself is the
//! engine's job on the first successful trunk turn (a follow-up; the
//! boot pass here is idempotent and re-checks on every boot until
//! then).
//!
//! ## No compat shim
//!
//! Legacy principals (pre-ADR-054 configs without `boot_state`) get
//! the same seeding on their next daemon boot — that is the point:
//! the keepalive gap is exactly what left them passive. The state
//! inference for them lives on [`PrincipalConfig::boot_state`].

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use peko_cron::{CronJob, CronJobAction, ScheduleKind};
use peko_subject::PrincipalId;

use crate::common::paths::PathResolver;
use crate::principal::config::{BootState, PrincipalConfig};
use crate::principal::PrincipalManager;

/// Job id of the one-shot genesis turn.
pub const GENESIS_JOB_ID: &str = "genesis";

/// Job id of the recurring default keepalive.
pub const KEEPALIVE_JOB_ID: &str = "keepalive";

/// Default keepalive cadence. Well above the 60s trunk floor
/// (`TRUNK_MIN_INTERVAL_MS`) — a fresh principal should think
/// periodically, not burn tokens every minute; the trunk retunes the
/// cadence once it is organized.
pub const DEFAULT_KEEPALIVE_EVERY_MS: u64 = 600_000;

/// Delay between daemon boot and the one-shot genesis turn.
pub const GENESIS_DELAY_SECS: u64 = 60;

/// The runtime-authored brief for the trunk's FIRST self-turn (P2).
///
/// Deterministic, model-free to produce: the only inputs are the
/// principal's own persisted definition. The brief is intentionally
/// bounded — a setup turn, not a work sprint (P3 budgeting is a
/// follow-up; see ADR-054 §Deferred).
#[must_use]
pub fn genesis_brief(config: &PrincipalConfig) -> String {
    let display = config
        .identity
        .display_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&config.name);
    let description = config
        .identity
        .description
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("(no description recorded — adopt one this turn)");

    let mut brief = String::new();
    brief.push_str(
        "[genesis] You have just come into existence. \
This is your first self-turn — the trunk of a new principal.\n\n",
    );
    brief.push_str(&format!("Principal: {display} (`{}`)\n", config.name));
    brief.push_str(&format!("Description: {description}\n"));
    if !config.intent.goals.is_empty() {
        brief.push_str("Goals:\n");
        for goal in &config.intent.goals {
            brief.push_str(&format!("- {goal}\n"));
        }
    }
    brief.push_str(
        "\nYour definition lives in `principal.toml` in your workspace; your agent \
prompts live in `agents/`. Scratch and removal staging sessions `/tmp` \
and `/trash` already exist under you. Your persistent knowledge base \
lives in `kb/` — `kb/MEMORY.md` (hot memory, rides in every prompt), \
`kb/index.md` (its map, also hot), `kb/people/`, `kb/groups/` and \
`kb/agents/` (per-person, per-group and per-agent notes — cold, looked \
up through the index, except that a group note matching a channel \
binding and a named agent's note are injected into that context's \
prompts). It is yours: revise in place, keep the index honest, \
restructure freely.\n\n\
On this turn:\n\
1. Read your own definition (`principal.toml`). If `[identity]` or \
`[intent]` are empty placeholders, adopt a working self-description \
and write it back — your creator will refine it later.\n\
2. Survey your workspace (`agents/`, `skills/`, `tools/`, `kb/` if present) \
so you know what you can do and what you already know.\n\
3. Organize: the `kb/` scaffold is a floor, not a mandate — keep what \
fits, restructure what does not, and record the conventions you adopt \
in `kb/index.md`. Set up any standing structure you still need \
(long-lived children, memory habits) this turn.\n\
4. The runtime gave you a default keepalive job (`keepalive`, every \
10 minutes, targeting you). Adjust the cadence with the cron tools if \
it does not fit — but never leave yourself without a heartbeat.\n\n\
Stay bounded: this is a setup turn, not a work sprint.\n",
    );
    brief
}

/// The recurring supervision tick (P4). Deliberately minimal: after
/// genesis, the trunk knows what it is; the tick only keeps it an
/// active actor.
#[must_use]
pub fn keepalive_tick_message(config: &PrincipalConfig) -> String {
    let display = config
        .identity
        .display_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&config.name);
    format!(
        "[keepalive] Supervision tick for `{display}`. You are the trunk of this \
principal: review your session tree (`session list`), tend your memory \
and workspace, check channels (`channel read`), and continue or adjust \
your standing work. If this cadence is wrong for you, retune or replace \
this job with the cron tools — you own your rhythm."
    )
}

/// Build the one-shot genesis cron job (P2). Fires
/// [`GENESIS_DELAY_SECS`] after `now` and deletes itself after
/// running; the turn lands in the trunk (`target: "trunk"`).
pub fn genesis_job(config: &PrincipalConfig, now: DateTime<Utc>) -> Result<CronJob> {
    let at = now + chrono::Duration::seconds(GENESIS_DELAY_SECS as i64);
    let at_rfc3339 = at.to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut job = job_base(
        config,
        now,
        GENESIS_JOB_ID,
        "genesis turn",
        ScheduleKind::At {
            at: at_rfc3339.clone(),
        },
    );
    job.delete_after_run = true;
    job.action = CronJobAction::Send {
        message: genesis_brief(config),
        target: Some("trunk".to_string()),
    };
    // The engine fires on `next_run <= now` and does NOT recompute it
    // from the schedule at add time — leaving the construction default
    // (`now`) would fire the one-shot immediately instead of at `at`.
    job.next_run = at;
    Ok(job)
}

/// Build the recurring default keepalive cron job (P4 seed). Targets
/// the trunk; cadence [`DEFAULT_KEEPALIVE_EVERY_MS`].
pub fn keepalive_job(config: &PrincipalConfig, now: DateTime<Utc>) -> Result<CronJob> {
    let mut job = job_base(
        config,
        now,
        KEEPALIVE_JOB_ID,
        "default keepalive",
        ScheduleKind::Every {
            every_ms: DEFAULT_KEEPALIVE_EVERY_MS,
        },
    );
    job.action = CronJobAction::Send {
        message: keepalive_tick_message(config),
        target: Some("trunk".to_string()),
    };
    // First tick one full cadence after seeding (same next_run
    // semantics as `genesis_job` — the engine honors the stored value).
    job.next_run = now + chrono::Duration::milliseconds(DEFAULT_KEEPALIVE_EVERY_MS as i64);
    Ok(job)
}

fn job_base(
    config: &PrincipalConfig,
    now: DateTime<Utc>,
    id: &str,
    name: &str,
    schedule: ScheduleKind,
) -> CronJob {
    // Cron jobs are keyed the way the cron tools key them: the
    // principal's stable runtime id (`ctx.principal_id` — this is what
    // `CronList` filters on and what `CronCreate` stamps). The DID is
    // the fallback for configs that predate id generation; the engine's
    // `resolve_principal` tries both on dispatch.
    let principal_id = config
        .id
        .clone()
        .or_else(|| config.did.clone().map(|d| PrincipalId(d.0)))
        .unwrap_or_else(|| PrincipalId(config.name.clone()));
    CronJob {
        id: id.to_string(),
        name: name.to_string(),
        principal_id,
        schedule,
        action: CronJobAction::Send {
            message: String::new(),
            target: Some("trunk".to_string()),
        },
        delete_after_run: false,
        enabled: true,
        created_at: now,
        next_run: now,
        last_run: None,
        last_status: None,
        run_count: 0,
        consecutive_failures: 0,
        max_retries: None,
        origin_session: None,
    }
}

/// Does `jobs` already carry an enabled RECURRING trunk-targeted send?
///
/// Both `target: Some("trunk")` and `target: None` route to the trunk
/// (Phase 7: the `Send` default target IS the trunk), so any enabled
/// recurring `Send` job counts as a heartbeat. The one-shot genesis
/// job (schedule `At`) deliberately does NOT count — it runs once and
/// self-deletes.
#[must_use]
pub fn has_recurring_trunk_send(jobs: &[CronJob]) -> bool {
    jobs.iter().any(|job| {
        job.enabled
            && matches!(job.schedule, ScheduleKind::Every { .. })
            && matches!(job.action, CronJobAction::Send { .. })
    })
}

/// What one boot-seeding pass did. `Display`-renders as a compact
/// summary line for the daemon boot log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Principals that got the one-shot genesis job.
    pub genesis_seeded: Vec<String>,
    /// Principals that got the recurring keepalive job.
    pub keepalive_seeded: Vec<String>,
    /// Principals whose boot state was stamped `genesis_pending`.
    pub state_flipped: Vec<String>,
    /// Principals whose legacy workspace-root `MEMORY.md` was moved
    /// into `kb/` (ADR-055 D4 one-time migration).
    pub memory_migrated: Vec<String>,
}

impl SeedReport {
    /// `true` when nothing changed (the common boot after the first).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.genesis_seeded.is_empty()
            && self.keepalive_seeded.is_empty()
            && self.state_flipped.is_empty()
            && self.memory_migrated.is_empty()
    }
}

impl std::fmt::Display for SeedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "genesis seeded for [{}], keepalive seeded for [{}], state flipped for [{}], \
             memory migrated for [{}]",
            self.genesis_seeded.join(", "),
            self.keepalive_seeded.join(", "),
            self.state_flipped.join(", "),
            self.memory_migrated.join(", ")
        )
    }
}

/// Daemon-boot seeding pass (ADR-054 P2 entry).
///
/// For every loaded principal whose [`BootState`] is not yet
/// `organized`:
///
/// 1. Ensure the recurring keepalive job exists (any enabled
///    recurring `Send` counts — the trunk may already have made its
///    own).
/// 2. Ensure the one-shot genesis job exists — only for principals
///    that have not had their genesis turn seeded yet (`provisioned`
///    / `defined` states; a `genesis_pending` principal whose
///    one-shot already fired simply has neither job re-added).
/// 3. Stamp `boot_state = genesis_pending` and persist.
///
/// Additionally — and for every principal regardless of boot state —
/// the one-time ADR-055 memory migration runs: a legacy
/// workspace-root `MEMORY.md` is moved into `kb/MEMORY.md` so the
/// contract change never orphans a principal's memory.
///
/// Failures are per-principal and non-fatal (warn-and-continue): a
/// broken schedule file must never block daemon boot — the same
/// posture as `/tmp` + `/trash` seeding.
pub async fn seed_boot_defaults(
    manager: &PrincipalManager,
    path_resolver: &PathResolver,
) -> Result<SeedReport> {
    let now = Utc::now();
    let mut report = SeedReport::default();

    for principal in manager.list_all().await {
        let name = principal.name().await;

        // ADR-055 D4: one-time legacy memory migration. Runs for EVERY
        // principal (including `organized` — the runtime never touches
        // their cron schedule, but this is not a schedule fix), and is
        // a no-op once performed.
        let shared_root = path_resolver.principal_layout(&name).shared.root;
        match crate::principal::kb::migrate_legacy_memory(&shared_root) {
            Ok(true) => {
                tracing::info!(
                    "genesis: migrated legacy MEMORY.md into kb/ for principal '{name}'"
                );
                report.memory_migrated.push(name.clone());
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("genesis: memory migration failed for '{name}': {e}"),
        }

        let state = principal.config.read().await.boot_state();
        if state == BootState::Organized {
            continue;
        }

        let schedule_path = path_resolver.cron_schedule(&name);
        let scheduler = peko_cron::CronScheduler::new(&schedule_path)
            .with_context(|| format!("cron scheduler init for '{name}'"))?;

        let jobs = scheduler
            .list_jobs(true)
            .with_context(|| format!("cron job listing for '{name}'"))?;
        let has_heartbeat = has_recurring_trunk_send(&jobs);
        let has_genesis = jobs.iter().any(|job| job.id == GENESIS_JOB_ID);

        let config_snapshot = principal.config.read().await.clone();

        if !has_heartbeat {
            let job = keepalive_job(&config_snapshot, now)
                .with_context(|| format!("keepalive job build for '{name}'"))?;
            scheduler
                .add_job(&job)
                .with_context(|| format!("keepalive job add for '{name}'"))?;
            tracing::info!(
                "genesis: seeded default keepalive for principal '{name}' \
                 (every {}ms, target trunk)",
                DEFAULT_KEEPALIVE_EVERY_MS
            );
            report.keepalive_seeded.push(name.clone());
        }

        if !has_genesis && matches!(state, BootState::Provisioned | BootState::Defined) {
            let job = genesis_job(&config_snapshot, now)
                .with_context(|| format!("genesis job build for '{name}'"))?;
            scheduler
                .add_job(&job)
                .with_context(|| format!("genesis job add for '{name}'"))?;
            tracing::info!(
                "genesis: scheduled first self-turn for principal '{name}' at {}",
                job.next_run.to_rfc3339()
            );
            report.genesis_seeded.push(name.clone());
        }

        if state != BootState::GenesisPending {
            manager
                .update_config(&name, |config| {
                    config.set_boot_state(BootState::GenesisPending);
                })
                .await
                .with_context(|| format!("boot state stamp for '{name}'"))?;
            report.state_flipped.push(name);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::PrincipalIdentityConfig;
    use chrono::TimeZone;

    fn bare_config() -> PrincipalConfig {
        PrincipalConfig {
            name: "seedling".into(),
            boot_state: None,
            ..make_default()
        }
    }

    fn make_default() -> PrincipalConfig {
        // A full-literal default so the test does not depend on any
        // one helper existing elsewhere.
        use crate::principal::config::{
            PrincipalGovernanceConfig, PrincipalIntentConfig, PrincipalMemoryConfig,
            PrincipalRoutingConfig,
        };
        PrincipalConfig {
            name: String::new(),
            id: None,
            did: None,
            owner: peko_auth::Subject::User("local".into()),
            identity: PrincipalIdentityConfig::default(),
            intent: PrincipalIntentConfig::default(),
            governance: PrincipalGovernanceConfig::default(),
            memory: PrincipalMemoryConfig::default(),
            routing: PrincipalRoutingConfig::default(),
            capabilities: Default::default(),
            exposure: peko_auth::Exposure::Private,
            status: None,
            boot_state: None,
            permissions: Vec::new(),
            preferred_model_id: None,
            transport_preference: Default::default(),
            quota: None,
            children: Default::default(),
        }
    }

    fn defined_config() -> PrincipalConfig {
        let mut cfg = bare_config();
        cfg.identity = PrincipalIdentityConfig {
            display_name: Some("Seedling".into()),
            description: Some("A test principal".into()),
            avatar: None,
        };
        cfg.intent.goals.push("grow".into());
        cfg.id = Some(PrincipalId("prin_seedling".into()));
        cfg.did = Some(peko_subject::PrincipalDID("did:peko:seedling".into()));
        cfg
    }

    #[test]
    fn genesis_job_is_oneshot_trunk_targeted() {
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 5, 0, 0).unwrap();
        let job = genesis_job(&defined_config(), now).unwrap();

        assert_eq!(job.id, GENESIS_JOB_ID);
        assert!(job.delete_after_run, "genesis turn is one-shot");
        assert!(job.enabled);
        // Jobs key on the runtime PrincipalId (what CronList filters
        // on), not the DID.
        assert_eq!(job.principal_id.0, "prin_seedling");
        match &job.action {
            CronJobAction::Send { target, message } => {
                assert_eq!(target.as_deref(), Some("trunk"));
                assert!(message.contains("[genesis]"), "got: {message}");
                assert!(message.contains("Seedling"), "brief names the principal");
                assert!(message.contains("- grow"), "brief lists goals");
            }
            other => panic!("expected Send action, got {other:?}"),
        }
        match &job.schedule {
            ScheduleKind::At { at } => {
                let parsed = chrono::DateTime::parse_from_rfc3339(at).unwrap();
                assert_eq!(parsed, now + chrono::Duration::seconds(60));
            }
            other => panic!("expected At schedule, got {other:?}"),
        }
        // The engine honors the stored `next_run` verbatim — it must
        // match the At instant, not the construction time (an
        // immediate fire defeats the one-shot's delay).
        assert_eq!(job.next_run, now + chrono::Duration::seconds(60));
    }

    #[test]
    fn keepalive_job_is_recurring_trunk_targeted_above_floor() {
        let now = Utc.with_ymd_and_hms(2026, 9, 13, 5, 0, 0).unwrap();
        let job = keepalive_job(&bare_config(), now).unwrap();

        assert_eq!(job.id, KEEPALIVE_JOB_ID);
        assert!(!job.delete_after_run);
        match &job.action {
            CronJobAction::Send { target, message } => {
                assert_eq!(target.as_deref(), Some("trunk"));
                assert!(message.contains("[keepalive]"), "got: {message}");
            }
            other => panic!("expected Send action, got {other:?}"),
        }
        match &job.schedule {
            ScheduleKind::Every { every_ms } => {
                assert!(
                    *every_ms >= 60_000,
                    "keepalive cadence must respect the trunk floor"
                );
            }
            other => panic!("expected Every schedule, got {other:?}"),
        }
        // First tick one full cadence out, not immediately (the engine
        // fires on `next_run <= now` with no recompute at add time).
        assert_eq!(
            job.next_run,
            now + chrono::Duration::milliseconds(DEFAULT_KEEPALIVE_EVERY_MS as i64)
        );
    }

    #[test]
    fn heartbeat_detection_ignores_oneshot_and_disabled() {
        let now = Utc::now();
        let genesis = genesis_job(&defined_config(), now).unwrap();
        let keepalive = keepalive_job(&defined_config(), now).unwrap();

        // A genesis one-shot alone is NOT a heartbeat.
        assert!(!has_recurring_trunk_send(std::slice::from_ref(&genesis)));

        // The keepalive IS.
        assert!(has_recurring_trunk_send(&[genesis, keepalive.clone()]));

        // A disabled keepalive is not.
        let mut disabled = keepalive;
        disabled.enabled = false;
        assert!(!has_recurring_trunk_send(&[disabled]));
    }

    #[test]
    fn seed_report_display_is_compact() {
        let report = SeedReport {
            genesis_seeded: vec!["a".into()],
            keepalive_seeded: vec!["a".into(), "b".into()],
            state_flipped: vec![],
            memory_migrated: vec!["c".into()],
        };
        let text = report.to_string();
        assert!(text.contains("genesis seeded for [a]"), "got: {text}");
        assert!(text.contains("keepalive seeded for [a, b]"), "got: {text}");
        assert!(text.contains("memory migrated for [c]"), "got: {text}");
        assert!(!report.is_empty());
        assert!(SeedReport::default().is_empty());
    }

    // ─── boot-pass integration (daemon boot shape) ──────────────────

    /// The full P2 seeding pass over a real manager: a fresh bare
    /// principal gets both jobs, its state is stamped and persisted,
    /// and a second pass is a no-op (idempotence).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn seed_boot_defaults_is_idempotent_and_persists_state() {
        let temp = tempfile::TempDir::new().unwrap();
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();

        let path_resolver = crate::common::paths::PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime = crate::engine::tool_runtime::ToolRuntime::with_workspace(
            path_resolver.clone(),
            temp.path(),
        )
        .await
        .expect("tool runtime should initialize");
        crate::extensions::framework::core::init_global_core(tool_runtime.extension_core().clone());

        let (resolver, _adapter) = peko_providers::resolver::LlmResolver::mock(
            peko_providers::mock::MockAdapter::new(),
            temp.path().join("models.toml"),
        )
        .await;
        let manager = PrincipalManager::with_path_resolver(
            path_resolver.clone(),
            std::sync::Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
            std::sync::Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
            crate::extensions::framework::async_exec::executor::standalone_inbox_registry(),
        )
        .with_resolver(resolver);

        let mut config = bare_config();
        config.preferred_model_id = Some("mock".into());
        manager.create(config).await.expect("create");

        // Fresh principal: create stamped `provisioned` (bare config).
        let principal = manager.get_by_name("seedling").await.unwrap();
        assert_eq!(
            principal.config.read().await.boot_state(),
            BootState::Provisioned
        );

        let report = seed_boot_defaults(&manager, &path_resolver)
            .await
            .expect("seed pass");
        assert_eq!(report.genesis_seeded, vec!["seedling"]);
        assert_eq!(report.keepalive_seeded, vec!["seedling"]);
        assert_eq!(report.state_flipped, vec!["seedling"]);

        // State stamped AND persisted to principal.toml.
        let principal = manager.get_by_name("seedling").await.unwrap();
        assert_eq!(
            principal.config.read().await.boot_state(),
            BootState::GenesisPending
        );
        let persisted = tokio::fs::read_to_string(
            path_resolver
                .principal_layout("seedling")
                .shared
                .config_file,
        )
        .await
        .unwrap();
        assert!(
            persisted.contains("boot_state = \"genesis_pending\""),
            "got: {persisted}"
        );

        // Both jobs on disk; the recurring one counts as a heartbeat.
        let scheduler =
            peko_cron::CronScheduler::new(path_resolver.cron_schedule("seedling")).unwrap();
        let jobs = scheduler.list_jobs(true).unwrap();
        assert_eq!(jobs.len(), 2, "genesis + keepalive");
        assert!(has_recurring_trunk_send(&jobs));

        // Second pass: nothing to do.
        let report2 = seed_boot_defaults(&manager, &path_resolver)
            .await
            .expect("second seed pass");
        assert!(report2.is_empty(), "idempotent: {report2}");
        let jobs2 = scheduler.list_jobs(true).unwrap();
        assert_eq!(jobs2.len(), 2, "no duplicate jobs");
    }
}
