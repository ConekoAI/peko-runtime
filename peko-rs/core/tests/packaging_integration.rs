//! Local snapshot container is directly inspectable with ordinary tar.
mod common;
use common::PrincipalPackageBuilder;

#[tokio::test]
async fn snapshot_tar_lists_flat_manifest_and_workspace_files() {
    let path = PrincipalPackageBuilder::new("inspectable")
        .with_skill("skill-one")
        .build()
        .await
        .unwrap();
    let output = std::process::Command::new("tar")
        .arg("-tf")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    for file in [
        "manifest.toml",
        "config/principal.toml",
        "identity/did.json",
        "identity/keys.enc",
        "skills/skill-one/SKILL.md",
    ] {
        assert!(listing.lines().any(|line| line == file), "{listing}");
    }
    let output = std::process::Command::new("tar")
        .arg("-xOf")
        .arg(&path)
        .arg("manifest.toml")
        .output()
        .unwrap();
    assert!(output.status.success());
    let manifest = peko_core::registry::packaging::PrincipalManifest::from_toml(
        std::str::from_utf8(&output.stdout).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest.name, "inspectable");
    assert!(manifest.files.contains_key("skills/skill-one/SKILL.md"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("layers"));
    std::fs::remove_file(path).unwrap();
}
