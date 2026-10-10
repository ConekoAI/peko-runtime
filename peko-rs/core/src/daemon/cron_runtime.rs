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

    /// Resolve the loaded principal that owns `job_id`, returning its
    /// name and schedule file path. Falls back to run-history lookup
    /// for one-shot (`delete_after_run=true`) jobs that fire once and
    /// then self-delete — the run record survives the deletion so we
    /// can still resolve owner for `peko cron history` (2026-08-07
    /// field test, Finding 4).
    async fn resolve_owner(&self, job_id: &str) -> Option<(String, PathBuf)> {
        for name in self.all_principal_names().await {
            let path = self.path_resolver.cron_schedule(&name);
            let scheduler = match CronScheduler::new(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Ok(Some(_)) = scheduler.get_job(job_id) {
                return Some((name, path));
            }
            if let Ok(runs) = scheduler.get_run_history(job_id, 1) {
                if !runs.is_empty() {
                    return Some((name, path));
                }
            }
        }
        None
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
        let principal_name = p.name().await;
        let path = self.path_resolver.cron_schedule(&principal_name);
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        scheduler
            .add_job(&job)
            .map_err(|e| anyhow::anyhow!("Failed to add job: {e}"))?;
        Ok(job_id)
    }

    async fn delete_job(&self, job_id: &str) -> Result<()> {
        let (name, path) = self
            .resolve_owner(job_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        let removed = scheduler
            .delete_job(job_id)
            .map_err(|e| anyhow::anyhow!("Failed to remove job: {e}"))?;
        if !removed {
            warn!("cron remove: job {job_id} not found under principal {name}");
        }
        Ok(())
    }

    async fn update_job(
        &self,
        job_id: &str,
        enabled: Option<bool>,
        wake_on_completion: Option<bool>,
    ) -> Result<()> {
        let (_name, path) = self
            .resolve_owner(job_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        let updated = scheduler
            .update_job_fields(job_id, enabled, wake_on_completion)
            .map_err(|e| anyhow::anyhow!("Failed to update job: {e}"))?;
        if !updated {
            return Err(anyhow::anyhow!("Job {job_id} not found"));
        }
        Ok(())
    }

    async fn trigger_job(&self, job_id: &str) -> Result<String> {
        let engine = self
            .cron_engine
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("cron engine not available on this runtime"))?;
        engine.execute_job_for_id(job_id).await
    }

    async fn run_history(&self, job_id: &str, limit: usize) -> Result<Vec<peko_cron::CronRun>> {
        let (_name, path) = self
            .resolve_owner(job_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Job {job_id} not found"))?;
        let scheduler =
            CronScheduler::new(&path).map_err(|e| anyhow::anyhow!("Cron DB error: {e}"))?;
        scheduler
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
                name: name.to_string(),
                id: None,
                did: None,
                owner: peko_subject::Subject::User("owner".into()),
                identity: Default::default(),
                intent: Default::default(),
                governance: Default::default(),
                memory: Default::default(),
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

        async fn call(&self, who: &Principal, params: Value) -> anyhow::Result<Value> {
            let ctx = ToolContext::for_hook_run("run", "call", "Cron")
                .with_principal_id(who.id.0.clone())
                .with_session_id("session-1");
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
