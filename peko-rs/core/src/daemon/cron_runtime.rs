//! `DaemonCronAdapter` — implements `peko_cron::CronRuntime` for the daemon.
//!
//! The cron tools in `peko_cron::tools` do not import daemon state.
//! They speak to a runtime port trait ([`peko_cron::CronRuntime`]),
//! and the daemon side implements that trait via this adapter.
//!
//! Construct at daemon startup with the shared `PrincipalManager` and
//! `PathResolver`, then install via
//! [`DaemonCronAdapter::install_as_global`]. Tools read the global
//! via [`peko_cron::global_runtime`] at execute time.
//!
//! 2026-08-25: cron is now an internal principal tool (like Bash,
//! Session). The legacy `Cron action list`/`CronAdd`/... IPC variants and the
//! `peko cron` CLI were deleted; this adapter is the only cron
//! read/write surface in the daemon. Job ids resolve across every
//! loaded principal here; the Cron tool actions scope each call to the
//! calling principal before reaching this adapter.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use peko_cron::{set_global_runtime, CronJob, CronRuntime, CronScheduler};
use peko_subject::PrincipalId;
use tracing::warn;

use crate::common::paths::PathResolver;
use crate::principal::manager::PrincipalManager;

/// `CronRuntime` impl that reads/writes the per-principal
/// `<resolver>.cron_schedule(name)` schedule file directly. A single
/// adapter represents the daemon-side impl for all cron tools.
pub struct DaemonCronAdapter {
    path_resolver: PathResolver,
    principal_manager: Arc<PrincipalManager>,
    /// The daemon's cron engine, bound at startup. Powers
    /// `trigger_job` (manual fires go through the engine's coalescing
    /// + spawn logic, not a scheduler side-channel). `None` in tests.
    cron_engine: Option<Arc<crate::daemon::cron_engine::CronEngine>>,
}

impl DaemonCronAdapter {
    /// Build an adapter bound to the daemon's principal manager and
    /// typed resolver. Install once via
    /// [`DaemonCronAdapter::install_as_global`].
    pub fn new(path_resolver: PathResolver, principal_manager: Arc<PrincipalManager>) -> Self {
        Self {
            path_resolver,
            principal_manager,
            cron_engine: None,
        }
    }

    /// Bind the cron engine (manual-trigger path).
    pub fn with_cron_engine(mut self, engine: Arc<crate::daemon::cron_engine::CronEngine>) -> Self {
        self.cron_engine = Some(engine);
        self
    }

    /// Convenience: install this adapter as the global runtime.
    /// Idempotent for repeated calls with the same adapter.
    pub fn install_as_global(self: Arc<Self>) {
        set_global_runtime(self.clone());
    }

    /// Enumerate the loaded principals (best-effort).
    async fn all_principal_names(&self) -> Vec<String> {
        let principals = self.principal_manager.list_all().await;
        let mut names = Vec::with_capacity(principals.len());
        for p in principals {
            names.push(p.name().await);
        }
        names
    }

    /// Refuse a job model the runtime cannot resolve now, so a typo
    /// fails at `create`/`update` rather than on every fire. Skipped
    /// when no resolver is bound (tests).
    async fn check_model(&self, model: &str) -> Result<()> {
        let Some(resolver) = self.principal_manager.llm_resolver() else {
            return Ok(());
        };
        resolver
            .resolve(peko_providers::resolver::ResolveRequest {
                override_model: Some(model),
                ..Default::default()
            })
            .await
            .map(|_| ())
            .map_err(|e| {
                anyhow::anyhow!(
                    "model '{model}' is not usable: {e}; `ModelList` shows the configured models"
                )
            })
    }

    /// The schedule file of the loaded principal `principal_id` (runtime
    /// id or DID). By-id calls go to the caller's own file: job ids are
    /// unique only per principal, since every principal has the seeded
    /// `keepalive` and `memory` jobs. A lookup across all principals by id
    /// alone once deleted another principal's keepalive while the
    /// caller's kept firing.
    async fn schedule_path(&self, principal_id: &PrincipalId) -> Result<PathBuf> {
        let principal =
            crate::daemon::cron_engine::resolve_principal(&self.principal_manager, principal_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("Principal '{}' is not loaded", principal_id.0))?;
        Ok(self.path_resolver.cron_schedule(&principal.name().await))
    }

    async fn scheduler(&self, principal_id: &PrincipalId) -> Result<CronScheduler> {
        let path = self.schedule_path(principal_id).await?;
        CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))
    }
}

#[async_trait]
impl CronRuntime for DaemonCronAdapter {
    async fn add_job(&self, job: CronJob) -> Result<String> {
        let job_id = job.id.clone();
        // Resolve the principal that owns this job by wire `PrincipalId`
        // (DID) — the cron runtime is global and the cron tool runs
        // outside the implicit principal context, so we look up by
        // `job.principal_id` and write to the matching per-principal
        // schedule file.
        let principal = crate::daemon::cron_engine::resolve_principal(
            &self.principal_manager,
            &job.principal_id,
        )
        .await;
        let Some(p) = principal else {
            return Err(anyhow::anyhow!(
                "Principal '{}' is not loaded",
                job.principal_id.0
            ));
        };
        if let peko_cron::CronJobAction::Send {
            model: Some(model), ..
        } = &job.action
        {
            self.check_model(model).await?;
        }
        let principal_name = p.name().await;
        let path = self.path_resolver.cron_schedule(&principal_name);
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        scheduler
            .add_job(&job)
            .map_err(|e| anyhow::anyhow!("Failed to add job: {e}"))?;
        Ok(job_id)
    }

    async fn delete_job(&self, principal_id: &PrincipalId, job_id: &str) -> Result<()> {
        let removed = self
            .scheduler(principal_id)
            .await?
            .delete_job(job_id)
            .map_err(|e| anyhow::anyhow!("Failed to remove job: {e}"))?;
        if !removed {
            warn!("cron remove: job {job_id} not found under principal {principal_id}");
        }
        Ok(())
    }

    async fn update_job(
        &self,
        principal_id: &PrincipalId,
        job_id: &str,
        patch: &peko_cron::JobPatch,
    ) -> Result<()> {
        if let Some(Some(model)) = &patch.model {
            self.check_model(model).await?;
        }
        let scheduler = self.scheduler(principal_id).await?;
        let job = scheduler
            .get_job(job_id)
            .map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;
        let mut patch = patch.clone();
        if patch.message.is_some() {
            // ADR-080 D5: the runtime supplies the memory brief at fire
            // time in place of this marker. A replaced marker would freeze
            // the principal's paraphrase and drop every later brief.
            if crate::principal::genesis::carries_memory_brief(&job) {
                anyhow::bail!(
                    "job {job_id} delivers the runtime's current memory brief, so its message \
                     stays; change its schedule, `when` or `model` here, or create a separate \
                     job for a routine of your own"
                );
            }
            // A seeded job without its own `when` takes it from the
            // runtime text it carries (ADR-083); pin it before the text goes.
            if patch.when.is_none() {
                if let peko_cron::CronJobAction::Send { when: None, .. } = &job.action {
                    let derived = crate::principal::genesis::fire_when(&job);
                    if derived != peko_cron::FireWhen::Always {
                        patch.when = Some(derived);
                    }
                }
            }
        }
        let updated = scheduler
            .update_job_fields(job_id, &patch)
            .map_err(|e| anyhow::anyhow!("Failed to update job: {e}"))?;
        if !updated {
            return Err(anyhow::anyhow!("Job {job_id} not found"));
        }
        Ok(())
    }

    async fn trigger_job(&self, principal_id: &PrincipalId, job_id: &str) -> Result<String> {
        let engine = self
            .cron_engine
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("cron engine not available on this runtime"))?;
        engine.execute_job_for_id(principal_id, job_id).await
    }

    async fn run_history(
        &self,
        principal_id: &PrincipalId,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<peko_cron::CronRun>> {
        self.scheduler(principal_id)
            .await?
            .get_run_history(job_id, limit)
            .map_err(|e| anyhow::anyhow!("Failed to read run history: {e}"))
    }

    async fn owns_job_history(&self, principal_id: &PrincipalId, job_id: &str) -> Result<bool> {
        let Some(principal) =
            crate::daemon::cron_engine::resolve_principal(&self.principal_manager, principal_id)
                .await
        else {
            return Ok(false);
        };
        let path = self.path_resolver.cron_schedule(&principal.name().await);
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        if scheduler
            .get_job(job_id)
            .map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?
            .is_some()
        {
            return Ok(true);
        }
        let runs = scheduler
            .get_run_history(job_id, 1)
            .map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        Ok(!runs.is_empty())
    }

    async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        // `include_disabled=true` so the calling tool can do its own
        // filtering (e.g. by principal). The port trait pushes that
        // policy up to the tool.
        let mut jobs: Vec<CronJob> = Vec::new();
        let mut first_err: Option<String> = None;
        for name in self.all_principal_names().await {
            let path = self.path_resolver.cron_schedule(&name);
            match CronScheduler::new(&path) {
                Ok(scheduler) => match scheduler.list_jobs(true) {
                    Ok(mut j) => jobs.append(&mut j),
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(format!("{name}: {e}"));
                        }
                    }
                },
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(format!("{name}: {e}"));
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(anyhow::anyhow!("Cron DB error: {e}")),
            None => Ok(jobs),
        }
    }
}

/// The daemon's cron backend behind the real `Cron` tool: two loaded
/// principals, each with its own schedule file.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tool_runtime::ToolRuntime;
    use crate::principal::config::Exposure;
    use crate::principal::{
        DefaultPrincipalMemoryFactory, DefaultPrincipalRouterFactory, Principal, PrincipalConfig,
    };
    use peko_cron::{CronRun, CronTool};
    use peko_tools_core::{Tool, ToolContext};
    use serde_json::{json, Value};
    use std::path::Path;

    struct Fixture {
        _tmp: tempfile::TempDir,
        resolver: PathResolver,
        adapter: Arc<DaemonCronAdapter>,
        tool: CronTool,
        alice: Arc<Principal>,
        bob: Arc<Principal>,
    }

    async fn create_principal(
        manager: &PrincipalManager,
        root: &Path,
        name: &str,
    ) -> Arc<Principal> {
        let roles = root.join("principals").join(name).join("roles");
        tokio::fs::create_dir_all(&roles).await.unwrap();
        tokio::fs::write(
            roles.join("primary.md"),
            "---\ndescription: \"t\"\n---\n\nTest.\n",
        )
        .await
        .unwrap();
        manager
            .create(PrincipalConfig {
                actor_key: None,
                name: name.to_string(),
                id: None,
                did: None,
                owner: peko_subject::ActorRef::User("owner".into()),
                identity: Default::default(),
                intent: Default::default(),
                governance: Default::default(),
                routing: Default::default(),
                exposure: Exposure::Private,
                status: None,
                boot_state: None,
                permissions: Vec::new(),
                preferred_model_id: Some("mock".to_string()),
                quota: None,
                children: Default::default(),
            })
            .await
            .unwrap()
    }

    impl Fixture {
        async fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let resolver = PathResolver::with_dirs(
                tmp.path().join("config"),
                tmp.path().join("data"),
                tmp.path().join("cache"),
            );
            let tool_runtime = ToolRuntime::with_workspace(resolver.clone(), tmp.path())
                .await
                .unwrap();
            let (llm, _adapter) = peko_providers::LlmResolver::mock(
                peko_providers::MockAdapter::new(),
                &tmp.path().join("models.toml"),
            )
            .await;
            let manager = Arc::new(
                PrincipalManager::with_path_resolver(
                    resolver.clone(),
                    Arc::new(DefaultPrincipalMemoryFactory),
                    Arc::new(DefaultPrincipalRouterFactory),
                    crate::async_exec::executor::standalone_inbox_registry(),
                )
                .with_identity_vault(Arc::new(crate::common::vault::Vault::for_test(
                    &tmp.path().join("identity-vault"),
                    "cron-fixture-key",
                )))
                .with_tooling(tool_runtime.tooling().clone())
                .with_resolver(llm),
            );
            let alice = create_principal(&manager, tmp.path(), "alice").await;
            let bob = create_principal(&manager, tmp.path(), "bob").await;
            let adapter = Arc::new(DaemonCronAdapter::new(resolver.clone(), manager));
            Self {
                tool: CronTool::with_runtime(adapter.clone()),
                adapter,
                resolver,
                alice,
                bob,
                _tmp: tmp,
            }
        }

        /// Calls as the owner in person, who may also stop the runtime's
        /// own jobs.
        async fn call(&self, who: &Principal, params: Value) -> anyhow::Result<Value> {
            let owner = peko_tools_core::ActingFor {
                origin: peko_tools_core::Origin::Ingress { channel: None },
                ..peko_tools_core::ActingFor::runtime()
            };
            let ctx = ToolContext::for_hook_run("run", "call", "Cron")
                .with_principal_id(who.id.0.clone())
                .with_session_id("session-1")
                .with_acting_for(owner);
            self.tool.execute_with_context(params, &ctx).await
        }

        async fn scheduler(&self, who: &Principal) -> CronScheduler {
            CronScheduler::new(self.resolver.cron_schedule(&who.name().await)).unwrap()
        }

        async fn create(&self, who: &Principal, label: &str, schedule: Value) -> String {
            let mut params = json!({ "action": "create", "label": label, "message": "ping" });
            params
                .as_object_mut()
                .unwrap()
                .extend(schedule.as_object().unwrap().clone());
            let created = self
                .call(who, params.clone())
                .await
                .unwrap_or_else(|e| panic!("{params}: {e}"));
            created["job_id"].as_str().unwrap().to_string()
        }
    }

    /// Jobs land in, and are read from, their owner's schedule file only.
    #[tokio::test]
    async fn each_principal_manages_jobs_in_its_own_schedule() {
        let fx = Fixture::new().await;
        let id = fx
            .create(&fx.alice, "standup", json!({ "interval_ms": 600_000 }))
            .await;
        assert!(fx
            .scheduler(&fx.alice)
            .await
            .get_job(&id)
            .unwrap()
            .is_some());
        assert!(fx.scheduler(&fx.bob).await.get_job(&id).unwrap().is_none());

        let mine = fx
            .call(&fx.alice, json!({ "action": "list" }))
            .await
            .unwrap();
        assert!(mine.to_string().contains(&id), "{mine}");
        let theirs = fx.call(&fx.bob, json!({ "action": "list" })).await.unwrap();
        assert!(!theirs.to_string().contains(&id), "{theirs}");

        fx.call(
            &fx.alice,
            json!({ "action": "update", "id": id, "enabled": false }),
        )
        .await
        .unwrap();
        assert!(
            !fx.scheduler(&fx.alice)
                .await
                .get_job(&id)
                .unwrap()
                .unwrap()
                .enabled
        );

        fx.call(&fx.alice, json!({ "action": "delete", "label": "standup" }))
            .await
            .unwrap();
        assert!(fx
            .scheduler(&fx.alice)
            .await
            .get_job(&id)
            .unwrap()
            .is_none());
    }

    /// Another principal cannot reach a job by id or label through any
    /// action; the job is left untouched.
    #[tokio::test]
    async fn other_principals_cannot_reach_a_job() {
        let fx = Fixture::new().await;
        let id = fx
            .create(&fx.alice, "private", json!({ "interval_ms": 600_000 }))
            .await;
        for target in [json!({ "id": id }), json!({ "label": "private" })] {
            for action in ["delete", "update", "trigger", "history"] {
                let mut params = target.clone();
                params["action"] = json!(action);
                params["enabled"] = json!(false);
                let error = fx
                    .call(&fx.bob, params.clone())
                    .await
                    .expect_err(&params.to_string());
                assert!(error.to_string().contains("not found"), "{params}: {error}");
            }
        }
        let job = fx.scheduler(&fx.alice).await.get_job(&id).unwrap().unwrap();
        assert!(job.enabled, "bob's update must not apply");
    }

    /// A one-shot job that fired and was removed keeps its history
    /// readable by its owner, and only its owner.
    #[tokio::test]
    async fn history_of_a_removed_one_shot_job_stays_with_its_owner() {
        let fx = Fixture::new().await;
        let id = fx.create(&fx.alice, "once", json!({ "delay": "5m" })).await;
        let scheduler = fx.scheduler(&fx.alice).await;
        let now = chrono::Utc::now();
        scheduler
            .record_run(&CronRun {
                id: "run_1".into(),
                job_id: id.clone(),
                started_at: now,
                finished_at: Some(now),
                status: "success".into(),
                output: None,
                error: None,
            })
            .unwrap();
        scheduler.delete_job(&id).unwrap();

        let history = fx
            .call(&fx.alice, json!({ "action": "history", "id": id }))
            .await
            .unwrap();
        assert!(history.to_string().contains("run_1"), "{history}");
        let error = fx
            .call(&fx.bob, json!({ "action": "history", "id": id }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not found"), "{error}");
    }

    /// Give `who` the runtime-seeded `keepalive` and `memory` jobs.
    async fn seed(fx: &Fixture, who: &Principal) {
        use crate::principal::genesis::{keepalive_job, memory_job};
        let config = who.config.read().await.clone();
        let now = chrono::Utc::now();
        let scheduler = fx.scheduler(who).await;
        scheduler
            .add_job(&keepalive_job(&config, now).unwrap())
            .unwrap();
        scheduler
            .add_job(&memory_job(&config, now).unwrap())
            .unwrap();
    }

    /// Every principal has jobs `keepalive` and `memory`. A by-id call
    /// reaches the caller's own job whichever principal loaded first: the
    /// live MiMo eval saw one principal's `delete keepalive` remove the
    /// other's while its own kept firing.
    #[tokio::test]
    async fn seeded_job_ids_resolve_to_the_callers_schedule() {
        use crate::principal::genesis::{KEEPALIVE_JOB_ID, MEMORY_JOB_ID};
        let fx = Fixture::new().await;
        seed(&fx, &fx.alice).await;
        seed(&fx, &fx.bob).await;

        // Both directions, so the list order cannot hide a wrong lookup.
        fx.call(
            &fx.bob,
            json!({ "action": "delete", "id": KEEPALIVE_JOB_ID }),
        )
        .await
        .unwrap();
        fx.call(
            &fx.alice,
            json!({ "action": "update", "id": KEEPALIVE_JOB_ID, "enabled": false }),
        )
        .await
        .unwrap();
        fx.call(
            &fx.alice,
            json!({ "action": "delete", "id": MEMORY_JOB_ID }),
        )
        .await
        .unwrap();
        fx.call(
            &fx.bob,
            json!({ "action": "update", "id": MEMORY_JOB_ID, "enabled": false }),
        )
        .await
        .unwrap();

        let alice = fx.scheduler(&fx.alice).await;
        let bob = fx.scheduler(&fx.bob).await;
        assert!(!alice.get_job(KEEPALIVE_JOB_ID).unwrap().unwrap().enabled);
        assert!(alice.get_job(MEMORY_JOB_ID).unwrap().is_none());
        assert!(bob.get_job(KEEPALIVE_JOB_ID).unwrap().is_none());
        assert!(!bob.get_job(MEMORY_JOB_ID).unwrap().unwrap().enabled);

        alice
            .record_run(&CronRun {
                id: "run_alice".into(),
                job_id: KEEPALIVE_JOB_ID.into(),
                started_at: chrono::Utc::now(),
                finished_at: Some(chrono::Utc::now()),
                status: "success".into(),
                output: None,
                error: None,
            })
            .unwrap();
        bob.record_run(&CronRun {
            id: "run_bob".into(),
            job_id: KEEPALIVE_JOB_ID.into(),
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            status: "success".into(),
            output: None,
            error: None,
        })
        .unwrap();
        for (who, mine, theirs) in [
            (&fx.alice, "run_alice", "run_bob"),
            (&fx.bob, "run_bob", "run_alice"),
        ] {
            let history = fx
                .call(who, json!({ "action": "history", "id": KEEPALIVE_JOB_ID }))
                .await
                .unwrap()
                .to_string();
            assert!(
                history.contains(mine) && !history.contains(theirs),
                "{history}"
            );
        }
    }

    /// `update` retunes a seeded job's schedule in place: the id, creator,
    /// trunk target and `when` stay, so the runtime still treats it as its
    /// keepalive or memory job. The memory job's marker message stays.
    #[tokio::test]
    async fn update_retunes_seeded_jobs_in_place() {
        use crate::principal::genesis::{
            fire_message, fire_when, has_recurring_trunk_send, KEEPALIVE_JOB_ID,
            MEMORY_BRIEF_MARKER, MEMORY_JOB_ID,
        };
        use peko_cron::{CronJobAction, FireWhen, ScheduleKind};
        let fx = Fixture::new().await;
        seed(&fx, &fx.alice).await;
        let scheduler = fx.scheduler(&fx.alice).await;
        let before = scheduler.get_job(KEEPALIVE_JOB_ID).unwrap().unwrap();

        let out = fx
            .call(
                &fx.alice,
                json!({ "action": "update", "id": KEEPALIVE_JOB_ID, "interval_ms": 1_800_000 }),
            )
            .await
            .unwrap();
        assert_eq!(out["schedule"], "every 30m", "{out}");
        let keepalive = scheduler.get_job(KEEPALIVE_JOB_ID).unwrap().unwrap();
        assert_eq!(
            keepalive.schedule,
            ScheduleKind::Every {
                every_ms: 1_800_000
            }
        );
        assert_eq!(keepalive.created_by, before.created_by);
        assert_eq!(keepalive.created_at, before.created_at);
        assert_eq!(fire_when(&keepalive), FireWhen::Attention);
        assert!(has_recurring_trunk_send(std::slice::from_ref(&keepalive)));
        let lead = keepalive.next_run - chrono::Utc::now();
        assert!(
            lead > chrono::Duration::minutes(29) && lead <= chrono::Duration::minutes(30),
            "next fire one new interval out, got {lead}"
        );

        let error = fx
            .call(
                &fx.alice,
                json!({ "action": "update", "id": KEEPALIVE_JOB_ID, "interval_ms": 1_000 }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("every_ms >= 60000"), "{error}");
        assert_eq!(
            scheduler
                .get_job(KEEPALIVE_JOB_ID)
                .unwrap()
                .unwrap()
                .schedule,
            ScheduleKind::Every {
                every_ms: 1_800_000
            }
        );

        fx.call(
            &fx.alice,
            json!({
                "action": "update", "id": MEMORY_JOB_ID,
                "cron": "0 3 * * *", "timezone": "Asia/Tokyo",
            }),
        )
        .await
        .unwrap();
        fx.call(
            &fx.alice,
            json!({ "action": "update", "id": MEMORY_JOB_ID, "cron": "30 2 * * *" }),
        )
        .await
        .unwrap();
        let memory = scheduler.get_job(MEMORY_JOB_ID).unwrap().unwrap();
        assert_eq!(
            memory.schedule,
            ScheduleKind::Cron {
                expr: "30 2 * * *".into(),
                tz: Some("Asia/Tokyo".into()),
            },
            "a new expression keeps the job's zone"
        );
        assert_eq!(fire_when(&memory), FireWhen::Quiet);
        let config = fx.alice.config.read().await.clone();
        assert_ne!(fire_message(&memory, &config), MEMORY_BRIEF_MARKER);

        let error = fx
            .call(
                &fx.alice,
                json!({ "action": "update", "id": MEMORY_JOB_ID, "message": "do my memory" }),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("memory brief"), "{error}");
        assert!(matches!(
            scheduler.get_job(MEMORY_JOB_ID).unwrap().unwrap().action,
            CronJobAction::Send { message, .. } if message == MEMORY_BRIEF_MARKER
        ));

        // A pre-ADR-083 keepalive reads `attention` from its tick text;
        // a replaced message keeps that by pinning `when`.
        let mut legacy = scheduler.get_job(KEEPALIVE_JOB_ID).unwrap().unwrap();
        if let CronJobAction::Send { when, .. } = &mut legacy.action {
            *when = None;
        }
        scheduler.delete_job(KEEPALIVE_JOB_ID).unwrap();
        scheduler.add_job(&legacy).unwrap();
        assert_eq!(fire_when(&legacy), FireWhen::Attention);
        fx.call(
            &fx.alice,
            json!({ "action": "update", "id": KEEPALIVE_JOB_ID, "message": "my own tick" }),
        )
        .await
        .unwrap();
        let retold = scheduler.get_job(KEEPALIVE_JOB_ID).unwrap().unwrap();
        assert!(matches!(
            &retold.action,
            CronJobAction::Send { message, when: Some(FireWhen::Attention), .. }
                if message == "my own tick"
        ));
    }

    #[tokio::test]
    async fn trigger_without_an_engine_and_unloaded_owners_are_errors() {
        let fx = Fixture::new().await;
        let id = fx
            .create(&fx.alice, "manual", json!({ "interval_ms": 600_000 }))
            .await;
        let error = fx
            .call(&fx.alice, json!({ "action": "trigger", "id": id }))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("cron engine not available"),
            "{error}"
        );

        let mut job = fx.scheduler(&fx.alice).await.get_job(&id).unwrap().unwrap();
        job.id = "ghost-job".into();
        job.principal_id = PrincipalId("did:peko:not-loaded".into());
        let error = fx.adapter.add_job(job).await.unwrap_err();
        assert!(error.to_string().contains("is not loaded"), "{error}");
    }
}
