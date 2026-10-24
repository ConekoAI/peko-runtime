//! Local CLI snapshot import: operator inventory appears before completion.
mod common;
use common::{run_with_timeout, DaemonGuard, PekoCli, PrincipalPackageBuilder};
use std::time::Duration;

#[tokio::test]
async fn import_displays_inventory_and_restores_workspace_without_confirmation() {
    let name = format!("imp-{}", uuid::Uuid::new_v4().simple());
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);
    let package = PrincipalPackageBuilder::new(&name)
        .with_skill("fixture-skill")
        .build()
        .await
        .unwrap();
    let (output, stdout, stderr) = run_with_timeout(
        || cli.cmd(),
        &["import", package.to_str().unwrap(), "--name", &name],
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&stderr)
    );
    let text = String::from_utf8_lossy(&stdout);
    assert!(
        text.find("Snapshot inventory:").unwrap() < text.find("Imported peko").unwrap(),
        "{text}"
    );
    assert!(text.contains("fixture-skill"), "{text}");
    assert!(!text.contains("[y/N]"), "{text}");
    let root = cli.peko_dir().join("principals").join(&name);
    assert!(root.join("skills/fixture-skill/SKILL.md").exists());
    let config: peko_core::principal::config::PrincipalConfig =
        toml::from_str(&std::fs::read_to_string(root.join("principal.toml")).unwrap()).unwrap();
    assert!(!toml::to_string(&config).unwrap().contains("capabilities"));
}

/// ADR-081 D6 step 1: a runtime hosts one copy of an actor. Re-importing a
/// hosted peko's export is refused; `--rotate-keys` makes a second actor.
#[tokio::test]
async fn reimport_of_hosted_peko_needs_rotate_keys_and_makes_a_new_actor() {
    let name = format!("imp-{}", uuid::Uuid::new_v4().simple());
    let copy = format!("{name}-copy");
    let cli = PekoCli::new();
    let _daemon = DaemonGuard::spawn(&cli);
    let package = PrincipalPackageBuilder::new(&name).build().await.unwrap();
    let run =
        |args: &[&str]| run_with_timeout(|| cli.cmd(), args, Duration::from_secs(30)).unwrap();

    let (output, _, stderr) = run(&["import", package.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&stderr)
    );
    let export = cli.peko_dir().join("hosted.peko");
    let (output, _, stderr) = run(&["export", &name, "-o", export.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&stderr)
    );

    let (output, _, stderr) = run(&["import", export.to_str().unwrap(), "--name", &copy]);
    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&stderr);
    assert!(err.contains("already hosted here"), "{err}");
    assert!(err.contains("--rotate-keys"), "{err}");

    let (output, stdout, stderr) = run(&[
        "import",
        export.to_str().unwrap(),
        "--rotate-keys",
        "--name",
        &copy,
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&stderr)
    );
    assert!(String::from_utf8_lossy(&stdout).contains("New actor"));

    let read = |peko: &str| -> peko_core::principal::config::PrincipalConfig {
        let path = cli
            .peko_dir()
            .join("principals")
            .join(peko)
            .join("principal.toml");
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let (hosted, rotated) = (read(&name), read(&copy));
    assert!(rotated.actor_key.is_some());
    assert_ne!(rotated.actor_key, hosted.actor_key);
    assert_ne!(rotated.id, hosted.id);
    assert_ne!(rotated.did, hosted.did);
}
