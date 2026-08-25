//! CLI integration tests for `peko principal import` capability selection.
//!
//! These tests run by default: they are fully local and use a mocked
//! daemon environment.

mod common;

use common::{run_with_stdin, run_with_timeout, DaemonGuard, PekoCli, PrincipalPackageBuilder};
use std::time::Duration;

fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        uuid::Uuid::new_v4().to_string().replace('-', "")
    )
}

#[tokio::test]
async fn import_yes_selects_no_required_capabilities() {
    let name = unique_name("imp-yes-");
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let package = PrincipalPackageBuilder::new(&name)
        .with_skill(
            "fixture-skill",
            &["tool:fixture.exec", "tool:fixture.read"],
            &[],
        )
        .build()
        .await
        .expect("build signed principal package");

    run_with_timeout(
        || cli.cmd(),
        &[
            "principal",
            "import",
            package.to_str().unwrap(),
            "--name",
            &name,
            "--yes",
        ],
        Duration::from_secs(30),
    )
    .expect("import --yes should succeed");

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("imported principal.toml should exist");
    let config: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse imported principal.toml");

    assert!(
        !config.capabilities.contains_str("tool:fixture.exec"),
        "--yes should not grant tool:fixture.exec; got {:?}",
        config.capabilities
    );
    assert!(
        !config.capabilities.contains_str("tool:fixture.read"),
        "--yes should not grant tool:fixture.read; got {:?}",
        config.capabilities
    );
}

// `import_interactive_partial_capability_selection` removed in
// Phase 2 PR 1 (ADR-047 §2.4): the interactive prompt for required
// capabilities was driven by `with_skill(...)` building a skill
// extension through `ExtensionStore`, which required the
// `SkillAdapter`. Phase 7 deletes the entire `.principal`
// extensions-layer packaging path (workspace tools are packaged as a
// literal tar of `<workspace>/`, not as a separate extensions
// layer). The interactive capability-selection UX is redesigned
// in Phase 7 to operate on workspace-resident tools; that PR
// will restore an equivalent test.

#[tokio::test]
async fn import_unsigned_with_allow_unsigned_yes_selects_none() {
    let name = unique_name("imp-unsigned-");
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);

    let package = PrincipalPackageBuilder::new(&name)
        .with_skill(
            "fixture-skill",
            &["tool:fixture.exec", "tool:fixture.read"],
            &[],
        )
        .unsigned()
        .build()
        .await
        .expect("build unsigned principal package");

    run_with_timeout(
        || cli.cmd(),
        &[
            "principal",
            "import",
            package.to_str().unwrap(),
            "--name",
            &name,
            "--allow-unsigned",
            "--yes",
        ],
        Duration::from_secs(30),
    )
    .expect("import --allow-unsigned --yes should succeed");

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("imported principal.toml should exist");
    let config: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse imported principal.toml");

    assert!(
        !config.capabilities.contains_str("tool:fixture.exec"),
        "unsigned --yes should not grant tool:fixture.exec; got {:?}",
        config.capabilities
    );
    assert!(
        !config.capabilities.contains_str("tool:fixture.read"),
        "unsigned --yes should not grant tool:fixture.read; got {:?}",
        config.capabilities
    );
}
