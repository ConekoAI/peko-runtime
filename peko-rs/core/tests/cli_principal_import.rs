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
    assert!(config.capabilities.is_empty());
}
