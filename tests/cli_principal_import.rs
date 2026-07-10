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
    let config: peko::principal::config::PrincipalConfig =
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

#[tokio::test]
async fn import_interactive_partial_capability_selection() {
    let name = unique_name("imp-partial-");
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

    // Required capabilities are sorted alphabetically. Defaults are now
    // opt-out (y/N), so explicit answers select the partial set:
    //   1. tool:fixture.exec  -> answer "n"
    //   2. tool:fixture.read  -> answer "y"
    // Then confirm the import                       -> answer "y"
    let stdin = b"n\ny\ny\n";
    run_with_stdin(
        || cli.cmd(),
        &[
            "principal",
            "import",
            package.to_str().unwrap(),
            "--name",
            &name,
        ],
        stdin,
        Duration::from_secs(30),
    )
    .expect("interactive import should succeed");

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("imported principal.toml should exist");
    let config: peko::principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse imported principal.toml");

    assert!(
        !config.capabilities.contains_str("tool:fixture.exec"),
        "tool:fixture.exec should not be granted; got {:?}",
        config.capabilities
    );
    assert!(
        config.capabilities.contains_str("tool:fixture.read"),
        "expected tool:fixture.read to be granted; got {:?}",
        config.capabilities
    );
}

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
    let config: peko::principal::config::PrincipalConfig =
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
