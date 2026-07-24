//! CLI integration tests for `peko principal pull` capability selection.
//!
//! These tests require the PekoHub backend fixture. Mark them `#[ignore]`
//! so the default `cargo test` run stays local and fast.

mod common;

use common::{
    create_test_user, reset_pekohub, run_with_stdin, run_with_timeout, DaemonGuard, PekoCli,
    PekohubBackend, PrincipalPackageBuilder,
};
use peko::registry::manifest::RegistryManifest;
use peko::registry::packaging::types::{compute_digest, ImageDigest};
use peko::registry::packaging::PrincipalRegistryDescriptor;
use peko::registry::{AgentRegistry, RegistryClient, RegistryConfig, RegistrySource};
use std::time::Duration;

fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}{}",
        uuid::Uuid::new_v4().to_string().replace('-', "")
    )
}

fn assert_success((output, stdout, stderr): (std::process::Output, Vec<u8>, Vec<u8>)) {
    if !output.status.success() {
        panic!(
            "peko command failed with status {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}\n--- end ---",
            output.status.code(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        );
    }
}

fn test_registry_config(url: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: url.to_string(),
        priority: 1,
        auth: None,
        token: None,
    });
    config
}

/// Write a registry source pointing at the test backend into the isolated
/// `.peko/data/config.toml` (the daemon's workspace) so `peko principal pull`
/// resolves against the fixture rather than the production default.
fn write_registry_config(cli: &PekoCli, url: &str) {
    let toml = format!(
        r#"
[registry]
default = {url:?}
sources = [{{ url = {url:?}, priority = 1 }}]
"#
    );
    let data_dir = cli.peko_dir().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("config.toml"), &toml).unwrap();
    // Also keep a copy at the config root in case other loaders look there.
    std::fs::write(cli.peko_dir().join("config.toml"), toml).unwrap();
}

fn registry_ref(host: &str, namespace: &str, name: &str) -> String {
    format!("{host}/{namespace}/{name}:v1.0")
}

async fn push_signed_descriptor(
    descriptor: &PrincipalRegistryDescriptor,
    host: &str,
    namespace: &str,
    name: &str,
) -> anyhow::Result<String> {
    let registry_dir = tempfile::tempdir()?.path().join("registry");
    let registry = AgentRegistry::new(&registry_dir);
    registry.init().await?;

    let config = test_registry_config(host);
    let client = RegistryClient::new(config, registry);

    let remote_ref = registry_ref(host, namespace, name);
    let mut events = Vec::new();
    client
        .push_principal(descriptor, name, "1.0.0", &remote_ref, |event| {
            events.push(event)
        })
        .await?;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, peko::registry::ProgressEvent::Done { .. })),
        "signed push should complete with Done event"
    );
    Ok(remote_ref)
}

/// Push a descriptor whose manifest signature has been cleared. The canonical
/// `push_principal` path rejects unsigned packages, so we stage the layers and
/// manifest directly into a throwaway local registry and use the generic push.
async fn push_unsigned_descriptor(
    descriptor: &PrincipalRegistryDescriptor,
    host: &str,
    namespace: &str,
    name: &str,
) -> anyhow::Result<String> {
    let registry_dir = tempfile::tempdir()?.path().join("registry");
    let registry = AgentRegistry::new(&registry_dir);
    registry.init().await?;

    // Stage every layer blob (including the unsigned config blob).
    for (digest, data) in &descriptor.layer_data {
        if !registry.has_layer(digest) {
            registry.store_layer(digest, data).await?;
        }
    }

    let config_digest = compute_digest(&descriptor.manifest_toml);
    let config_size = descriptor.manifest_toml.len() as u64;
    eprintln!("DEBUG unsigned descriptor: config_digest={config_digest} size={config_size}");
    eprintln!(
        "DEBUG manifest_toml head: {}",
        String::from_utf8_lossy(
            &descriptor.manifest_toml[..descriptor.manifest_toml.len().min(200)]
        )
    );

    let mut manifest = RegistryManifest::new(name, "1.0.0")
        .with_kind("principal")
        .with_ref(registry_ref(host, namespace, name))
        .with_config(
            config_digest,
            config_size,
            Some("application/vnd.peko.config.v1+json"),
        );
    for layer in &descriptor.layers {
        manifest.add_layer(layer.clone());
    }

    let json = manifest.to_json()?;
    eprintln!("DEBUG registry manifest json: {json}");
    let digest = ImageDigest::from_bytes(json.as_bytes());
    manifest.digest = digest.as_str().to_string();

    let manifest_dir = registry
        .root_path()
        .join("registry_manifests")
        .join(digest.dir_name());
    tokio::fs::create_dir_all(&manifest_dir).await?;
    tokio::fs::write(manifest_dir.join("manifest.json"), json).await?;

    let config = test_registry_config(host);
    let client = RegistryClient::new(config, registry);

    let remote_ref = registry_ref(host, namespace, name);
    let mut events = Vec::new();
    client
        .push(&digest, &remote_ref, |event| events.push(event))
        .await?;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, peko::registry::ProgressEvent::Done { .. })),
        "unsigned push should complete with Done event"
    );

    // Sanity-check that the generic push produced a pullable manifest.
    let pull_check = tempfile::tempdir()?.path().join("check.principal");
    let check_config = test_registry_config(host);
    let check_registry = AgentRegistry::new(tempfile::tempdir()?.path().join("check_registry"));
    check_registry.init().await?;
    let check_client = RegistryClient::new(check_config, check_registry);
    if let Err(e) = check_client
        .pull_principal(&remote_ref, &pull_check, |_| {})
        .await
    {
        panic!(
            "unsigned descriptor is not pullable: {e}\nlayer_data digests: {:?}\nlayers: {:?}",
            descriptor.layer_data.keys().collect::<Vec<_>>(),
            descriptor.layers
        );
    }

    Ok(remote_ref)
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial_test::serial]
async fn pull_yes_selects_no_required_capabilities() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, namespace) = create_test_user(&client, &backend.url, "ns").await;

    let name = unique_name("pull-yes-");
    let descriptor = PrincipalPackageBuilder::new(&name)
        .with_skill(
            "fixture-skill",
            &["tool:fixture.exec", "tool:fixture.read"],
            &[],
        )
        .build_descriptor()
        .await
        .expect("build signed descriptor");

    let remote_ref = push_signed_descriptor(&descriptor, &backend.url, &namespace, &name)
        .await
        .expect("push signed descriptor");

    let cli = PekoCli::new();
    write_registry_config(&cli, &backend.url);
    let _daemon = DaemonGuard::spawn(&cli);

    assert_success(
        run_with_timeout(
            || cli.cmd(),
            &["principal", "pull", &remote_ref, "--name", &name, "--yes"],
            Duration::from_secs(60),
        )
        .expect("pull --yes should succeed"),
    );

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("pulled principal.toml should exist");
    let config: peko_principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse pulled principal.toml");

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
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial_test::serial]
async fn pull_interactive_partial_capability_selection() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, namespace) = create_test_user(&client, &backend.url, "ns").await;

    let name = unique_name("pull-partial-");
    let descriptor = PrincipalPackageBuilder::new(&name)
        .with_skill(
            "fixture-skill",
            &["tool:fixture.exec", "tool:fixture.read"],
            &[],
        )
        .build_descriptor()
        .await
        .expect("build signed descriptor");

    let remote_ref = push_signed_descriptor(&descriptor, &backend.url, &namespace, &name)
        .await
        .expect("push signed descriptor");

    let cli = PekoCli::new();
    write_registry_config(&cli, &backend.url);
    let _daemon = DaemonGuard::spawn(&cli);

    // Required capabilities are sorted alphabetically. Defaults are now
    // opt-out (y/N), so explicit answers select the partial set:
    //   1. tool:fixture.exec  -> answer "n"
    //   2. tool:fixture.read  -> answer "y"
    // Then confirm the pull                       -> answer "y"
    let stdin = b"n\ny\ny\n";
    assert_success(
        run_with_stdin(
            || cli.cmd(),
            &["principal", "pull", &remote_ref, "--name", &name],
            stdin,
            Duration::from_secs(60),
        )
        .expect("interactive pull should succeed"),
    );

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("pulled principal.toml should exist");
    let config: peko_principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse pulled principal.toml");

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
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial_test::serial]
async fn pull_unsigned_with_allow_unsigned_yes_selects_none() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, namespace) = create_test_user(&client, &backend.url, "ns").await;

    let name = unique_name("pull-unsigned-");
    let descriptor = PrincipalPackageBuilder::new(&name)
        .with_skill(
            "fixture-skill",
            &["tool:fixture.exec", "tool:fixture.read"],
            &[],
        )
        .unsigned()
        .build_descriptor()
        .await
        .expect("build unsigned descriptor");

    let remote_ref = push_unsigned_descriptor(&descriptor, &backend.url, &namespace, &name)
        .await
        .expect("push unsigned descriptor");

    let cli = PekoCli::new();
    write_registry_config(&cli, &backend.url);
    let _daemon = DaemonGuard::spawn(&cli);

    assert_success(
        run_with_timeout(
            || cli.cmd(),
            &[
                "principal",
                "pull",
                &remote_ref,
                "--name",
                &name,
                "--allow-unsigned",
                "--yes",
            ],
            Duration::from_secs(60),
        )
        .expect("pull --allow-unsigned --yes should succeed"),
    );

    let config_path = cli
        .peko_dir()
        .join("principals")
        .join(&name)
        .join("principal.toml");
    let config_toml = tokio::fs::read_to_string(&config_path)
        .await
        .expect("pulled principal.toml should exist");
    let config: peko_principal::config::PrincipalConfig =
        toml::from_str(&config_toml).expect("parse pulled principal.toml");

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
