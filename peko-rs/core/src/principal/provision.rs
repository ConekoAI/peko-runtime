//! Creating a peko: the one place a new peko's folder is laid down and
//! its first turn is scheduled (ADR-105).
//!
//! `peko create` and Peko Desktop both reach this through the daemon's
//! `principal_create` request. Each used to write the folder itself, and
//! the two drifted: a peko made through the daemon got no knowledge
//! scaffold, no birth journal entry, and no first turn until the daemon
//! was restarted.

use std::sync::Arc;

use peko_auth::ActorRef;

use crate::common::paths::PathResolver;
use crate::principal::config::{
    Exposure, PrincipalConfig, PrincipalGovernanceConfig, PrincipalIdentityConfig,
    PrincipalIntentConfig, PrincipalRoutingConfig,
};
use crate::principal::manager::{PrincipalManager, PrincipalManagerError};
use crate::principal::seed::Seed;
use crate::principal::Principal;

/// What a new peko is made from.
#[derive(Debug, Clone)]
pub struct NewPeko {
    pub name: String,
    /// Who owns it: whoever asked for it.
    pub owner: ActorRef,
    /// The configured model it runs on. Overrides the seed's.
    pub model_id: String,
    /// Overrides the seed's description; blank is ignored.
    pub description: Option<String>,
    /// The seed it grows from (ADR-060), if any. Its setup steps must
    /// already have been accepted by the person creating it (ADR-104 D3).
    pub seed: Option<Seed>,
}

/// The definition of a peko created without a seed. Its display name is
/// its name; everything else is left for its first turn to adopt.
#[must_use]
pub fn default_config(
    name: &str,
    description: Option<String>,
    owner: ActorRef,
    model_id: String,
) -> PrincipalConfig {
    PrincipalConfig {
        actor_key: None,
        name: name.to_string(),
        id: None,
        did: None,
        owner,
        identity: PrincipalIdentityConfig {
            display_name: Some(name.to_string()),
            description: description.filter(|d| !d.trim().is_empty()),
            avatar: None,
        },
        intent: PrincipalIntentConfig::default(),
        governance: PrincipalGovernanceConfig::default(),
        routing: PrincipalRoutingConfig::default(),
        exposure: Exposure::Private,
        status: None,
        boot_state: None,
        permissions: Vec::new(),
        preferred_model_id: Some(model_id),
        quota: None,
        children: Default::default(),
    }
}

fn config_for(new: &NewPeko) -> (PrincipalConfig, Option<String>) {
    match new.seed.clone() {
        Some(seed) => {
            let persona = seed.persona.clone();
            // A fresh identity, always, and no authority: whoever creates
            // it owns what grows from the seed (ADR-060, ADR-081 D4).
            let mut config = seed.grown(&new.name, new.owner.clone());
            config.preferred_model_id = Some(new.model_id.clone());
            if let Some(description) = new.description.clone().filter(|d| !d.trim().is_empty()) {
                config.identity.description = Some(description);
            }
            (config, persona)
        }
        None => (
            default_config(
                &new.name,
                new.description.clone(),
                new.owner.clone(),
                new.model_id.clone(),
            ),
            None,
        ),
    }
}

/// Create a peko: lay down its folder, register it, and schedule its
/// first turn.
///
/// The folder gets the knowledge scaffold (ADR-055), the seed's persona
/// and capability files (ADR-107), the owner's policy file (ADR-078 D7) and the journal's birth entry;
/// [`PrincipalManager::create`] adds the identity, the seeded roles and
/// `peko.toml`. Then the genesis, keepalive and memory jobs are seeded,
/// so the first turn runs now and not at the next daemon start.
///
/// Nothing is written when a peko of that name already exists.
///
/// # Errors
/// [`PrincipalManagerError::AlreadyExists`] for a taken name, and
/// whatever the folder writes or the manager's create report.
pub async fn provision(
    manager: &PrincipalManager,
    paths: &PathResolver,
    new: NewPeko,
) -> Result<Arc<Principal>, PrincipalManagerError> {
    let shared = paths.principal_layout(&new.name).shared;
    if shared.config_file.exists() || manager.get_by_name(&new.name).await.is_some() {
        return Err(PrincipalManagerError::AlreadyExists(new.name));
    }
    let (config, persona) = config_for(&new);
    let files = new
        .seed
        .as_ref()
        .map(|seed| seed.files.clone())
        .unwrap_or_default();
    let failed = |what: &str, e: &dyn std::fmt::Display| {
        PrincipalManagerError::Config(format!("{what}: {e:#}"))
    };

    std::fs::create_dir_all(&shared.root).map_err(|e| failed("create the peko's folder", &e))?;
    crate::principal::kb::seed_kb_scaffold(&shared.root)
        .map_err(|e| failed("lay down the knowledge scaffold", &e))?;
    if let Some(persona) = &persona {
        crate::principal::seed::add_persona(&shared.root, persona)
            .map_err(|e| failed("write the persona", &e))?;
    }
    // ADR-107: the seed's capability files, before the manager seeds the
    // default roles (create-if-missing), so a role the seed carries wins.
    crate::principal::seed::write_files(&shared.root, &files)
        .map_err(|e| failed("write the seed's files", &e))?;
    crate::security::policy::seed(&shared.root).map_err(|e| failed("write POLICY.md", &e))?;
    crate::principal::kb::seed_journal_birth_entry(&shared.root, &new.name, chrono::Utc::now())
        .map_err(|e| failed("write the journal's first entry", &e))?;

    let principal = manager.create(config).await?;

    // The first turn is scheduled here. A failure leaves a peko that the
    // next daemon start seeds, so it is logged and the create stands.
    match crate::principal::genesis::seed_boot_defaults(manager, paths).await {
        Ok(report) if !report.is_empty() => tracing::info!("genesis: {report}"),
        Ok(_) => {}
        Err(e) => tracing::warn!(
            peko = %new.name,
            "could not schedule the first turn; the next daemon start will: {e:#}"
        ),
    }
    Ok(principal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::config::BootState;
    use crate::principal::genesis::{GENESIS_JOB_ID, KEEPALIVE_JOB_ID};

    async fn manager(temp: &tempfile::TempDir) -> (PrincipalManager, PathResolver) {
        std::env::set_var("PEKO_HOME", temp.path());
        peko_identity::init_test_env();
        let paths = PathResolver::with_dirs(
            temp.path().join("config"),
            temp.path().join("data"),
            temp.path().join("cache"),
        );
        let tool_runtime =
            crate::engine::tool_runtime::ToolRuntime::with_workspace(paths.clone(), temp.path())
                .await
                .expect("tool runtime should initialize");
        let (resolver, _adapter) = peko_providers::resolver::LlmResolver::mock(
            peko_providers::mock::MockAdapter::new(),
            temp.path().join("models.toml"),
        )
        .await;
        let manager = PrincipalManager::with_path_resolver(
            paths.clone(),
            Arc::new(crate::principal::factory::DefaultPrincipalMemoryFactory),
            Arc::new(crate::principal::factory::DefaultPrincipalRouterFactory),
            crate::async_exec::executor::standalone_inbox_registry(),
        )
        .with_tooling(tool_runtime.tooling().clone())
        .with_resolver(resolver);
        (manager, paths)
    }

    fn new_peko(name: &str, seed: Option<&str>) -> NewPeko {
        NewPeko {
            name: name.to_string(),
            owner: ActorRef::User("local".into()),
            model_id: "mock".to_string(),
            description: None,
            seed: seed.map(|text| crate::principal::seed::parse_seed(text).unwrap()),
        }
    }

    /// A new peko has its whole folder and a scheduled first turn,
    /// whoever asked for it.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn a_new_peko_gets_its_folder_and_a_scheduled_first_turn() {
        let temp = tempfile::TempDir::new().unwrap();
        let (manager, paths) = manager(&temp).await;
        let principal = provision(&manager, &paths, new_peko("nova", None))
            .await
            .expect("created");
        let root = &principal.workspace_path;

        for file in [
            "peko.toml",
            "POLICY.md",
            "kb/MEMORY.md",
            "kb/index.md",
            "kb/CONVENTIONS.md",
            "roles/root.md",
            "roles/conversation.md",
            "roles/worker.md",
        ] {
            assert!(root.join(file).is_file(), "{file} is missing");
        }
        let journal: Vec<_> = std::fs::read_dir(root.join("kb/journal"))
            .expect("journal dir")
            .collect();
        assert_eq!(journal.len(), 1, "the birth entry");
        assert!(
            !root.join("roles/primary.md").exists(),
            "the retired default role is no longer written"
        );

        // The first turn and the heartbeat are scheduled now.
        let scheduler = peko_cron::CronScheduler::new(paths.cron_schedule("nova")).unwrap();
        assert!(scheduler.get_job(GENESIS_JOB_ID).unwrap().is_some());
        assert!(scheduler.get_job(KEEPALIVE_JOB_ID).unwrap().is_some());
        let config = principal.config.read().await;
        assert_eq!(config.boot_state(), BootState::GenesisPending);
        assert_eq!(config.identity.display_name.as_deref(), Some("nova"));
        assert_eq!(config.preferred_model_id.as_deref(), Some("mock"));
        // Clients see the state, so they can show a peko waking up.
        drop(config);
        assert_eq!(
            principal.summary().await.boot_state,
            Some(BootState::GenesisPending)
        );
    }

    /// A seed gives the definition, the persona and the setup steps, and
    /// none of its authority (ADR-060, ADR-081 D4).
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn a_seeded_peko_takes_the_definition_and_none_of_the_authority() {
        let temp = tempfile::TempDir::new().unwrap();
        let (manager, paths) = manager(&temp).await;
        let seed = r#"
name = "ignored"
preferred_model_id = "someone-elses-model"
persona = "Sign replies with a fox."
owner = { kind = "user", id = "hub:seed-author" }

[identity]
description = "Reads papers"

[intent]
goals = ["Help with research"]
setup = ["Install ripgrep"]

[files]
"roles/researcher.md" = "---\nname: researcher\ndescription: Reads papers\n---\nCite sources.\n"
"roles/worker.md" = "---\nname: worker\ndescription: This seed's worker\n---\nWork carefully.\n"
"skills/greet/SKILL.md" = "---\nname: greet\ndescription: Greets\n---\nHello $ARGUMENTS\n"

[[permissions]]
subject = { kind = "user", id = "hub:friend" }
permission = "trusted"
granted_at = ""
granted_by = { kind = "user", id = "hub:seed-author" }
"#;
        let mut new = new_peko("sprout", Some(seed));
        new.description = Some("  ".into());
        let principal = provision(&manager, &paths, new).await.expect("created");
        let config = principal.config.read().await;
        assert_eq!(config.name, "sprout");
        assert_eq!(config.owner, ActorRef::User("local".into()));
        assert!(config.permissions.is_empty(), "{:?}", config.permissions);
        assert_eq!(config.preferred_model_id.as_deref(), Some("mock"));
        assert_eq!(
            config.identity.description.as_deref(),
            Some("Reads papers"),
            "a blank description does not replace the seed's"
        );
        assert_eq!(config.intent.setup, ["Install ripgrep".to_string()]);
        let conventions =
            std::fs::read_to_string(principal.workspace_path.join("kb/CONVENTIONS.md")).unwrap();
        assert!(conventions.contains("## Persona\n\nSign replies with a fox."));
        // ADR-107: the seed's files are in place, a role it carries wins
        // over the default, and the roles it does not carry are seeded.
        let read = |path: &str| std::fs::read_to_string(principal.workspace_path.join(path));
        assert!(read("roles/researcher.md")
            .unwrap()
            .contains("Cite sources."));
        assert!(read("roles/worker.md").unwrap().contains("Work carefully."));
        assert!(read("skills/greet/SKILL.md")
            .unwrap()
            .contains("Hello $ARGUMENTS"));
        assert!(read("roles/root.md").is_ok() && read("roles/conversation.md").is_ok());
        assert!(
            principal.agent_prompts.contains_key("researcher"),
            "the seed's role is loaded: {:?}",
            principal.agent_prompts.keys().collect::<Vec<_>>()
        );
    }

    /// A taken name is refused before anything is written.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn a_taken_name_is_refused_and_nothing_is_touched() {
        let temp = tempfile::TempDir::new().unwrap();
        let (manager, paths) = manager(&temp).await;
        let first = provision(&manager, &paths, new_peko("nova", None))
            .await
            .expect("created");
        let conventions = first.workspace_path.join("kb/CONVENTIONS.md");
        let before = std::fs::read_to_string(&conventions).unwrap();

        let again = provision(
            &manager,
            &paths,
            new_peko("nova", Some("name = \"x\"\npersona = \"Overwrite me.\"\n")),
        )
        .await;
        assert!(
            matches!(again, Err(PrincipalManagerError::AlreadyExists(ref name)) if name == "nova"),
            "{:?}",
            again.err()
        );
        assert_eq!(std::fs::read_to_string(&conventions).unwrap(), before);
    }
}
