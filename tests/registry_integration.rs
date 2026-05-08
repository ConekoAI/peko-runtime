//! Registry Integration Tests
//!
//! End-to-end tests for push/pull against the mock registry server.
//!
//! These tests are marked `#[ignore]` because they require the Python mock
//! registry server to be running. Start it first:
//!
//!   python e2e_tests/mock_registry/main.py --port 18765
//!
//! Then run:
//!
//!   cargo test --test registry_integration -- --ignored

use pekobot::portable::{
    manifest::AgentLayers, AgentManifest, AgentRegistry, Layer, LayerType,
};
use pekobot::registry::{
    RegistryClient, RegistryConfig, RegistryManifest, RegistrySource,
};
use std::time::Duration;

const MOCK_SERVER_URL: &str = "http://127.0.0.1:18765";

/// Create a test registry config pointing at the mock server
fn test_registry_config(host: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: host.to_string(),
        priority: 1,
        auth: None,
    });
    config
}

/// Compute sha256 digest of data
fn sha256_digest(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{:x}", hasher.finalize())
}

/// Create a minimal AgentManifest with layers for testing
fn create_test_manifest(name: &str) -> (AgentManifest, Vec<Layer>) {
    let mut manifest = AgentManifest::new(name, "1.0.0", "did:pekobot:test");

    let config_data = b"config layer content";
    let identity_data = b"identity layer content";
    let skills_data = b"skills layer content";

    let config_digest = sha256_digest(config_data);
    let identity_digest = sha256_digest(identity_data);
    let skills_digest = sha256_digest(skills_data);

    let layers = vec![
        Layer::new(&config_digest, LayerType::Config, config_data.len() as u64),
        Layer::new(&identity_digest, LayerType::Identity, identity_data.len() as u64),
        Layer::new(&skills_digest, LayerType::Skills, skills_data.len() as u64),
    ];

    manifest.layers = Some(AgentLayers {
        config: Some(config_digest),
        identity: Some(identity_digest),
        skills: Some(skills_digest),
        workspace: None,
        sessions: None,
        mcp: None,
    });

    (manifest, layers)
}

#[tokio::test]
#[ignore = "requires Python mock registry server on port 18765"]
async fn test_mock_registry_manifest_roundtrip() {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // PUT manifest
    let manifest_json = r#"{"schema_version":1,"name":"test-agent","version":"1.0.0","ref":"","digest":"sha256:abc123","created_at":"2026-05-08T10:00:00Z","source":"local","layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/test/manifests/latest", MOCK_SERVER_URL))
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert!(
        put_resp.status().is_success(),
        "PUT manifest failed: {}",
        put_resp.status()
    );

    // GET manifest
    let get_resp = client
        .get(format!("{}/v2/test/manifests/latest", MOCK_SERVER_URL))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("test-agent"));
}

#[tokio::test]
#[ignore = "requires Python mock registry server on port 18765"]
async fn test_mock_registry_blob_roundtrip() {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let digest = "sha256:deadbeef00000000000000000000000000000000000000000000000000000000";
    let data = b"test blob content";

    // Upload blob via POST + PUT
    let post_resp = client
        .post(format!("{}/v2/test/blobs/uploads/", MOCK_SERVER_URL))
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 202);
    let upload_url = post_resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let put_resp = client
        .put(&upload_url)
        .query(&[("digest", digest)])
        .body(data.as_slice())
        .send()
        .await
        .unwrap();
    assert!(
        put_resp.status().is_success(),
        "Blob upload failed: {}",
        put_resp.status()
    );

    // HEAD check
    let head_resp = client
        .head(format!("{}/v2/test/blobs/{}", MOCK_SERVER_URL, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(head_resp.status(), 200);

    // GET blob
    let get_resp = client
        .get(format!("{}/v2/test/blobs/{}", MOCK_SERVER_URL, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), data.as_slice());
}

#[tokio::test]
#[ignore = "requires Python mock registry server on port 18765"]
async fn test_registry_client_push_and_pull() {
    let host = "127.0.0.1:18765";

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    // Store some fake layers in the local registry
    let layer1_data = b"config layer content";
    let layer1_digest = sha256_digest(layer1_data);
    registry.store_layer(&layer1_digest, layer1_data).await.unwrap();

    let layer2_data = b"identity layer content";
    let layer2_digest = sha256_digest(layer2_data);
    registry.store_layer(&layer2_digest, layer2_data).await.unwrap();

    let layer3_data = b"skills layer content";
    let layer3_digest = sha256_digest(layer3_data);
    registry.store_layer(&layer3_digest, layer3_data).await.unwrap();

    // Create and store a local manifest
    let (agent_manifest, layers) = create_test_manifest("test-agent");
    let manifest_digest = registry
        .store_manifest(&agent_manifest, Some("test-agent:v1.0"))
        .await
        .unwrap();

    // Also store the RegistryManifest JSON for the client
    let mut reg_manifest = RegistryManifest::new("test-agent", "1.0.0")
        .with_digest(manifest_digest.as_str())
        .with_ref(format!("{}/test-agent:v1.0", host));
    for layer in &layers {
        reg_manifest.add_layer(layer.clone());
    }

    let reg_manifests_dir = temp_dir
        .path()
        .join("registry_manifests")
        .join(manifest_digest.dir_name());
    tokio::fs::create_dir_all(&reg_manifests_dir).await.unwrap();
    tokio::fs::write(
        reg_manifests_dir.join("manifest.json"),
        reg_manifest.to_json().unwrap(),
    )
    .await
    .unwrap();

    // Configure client
    let config = test_registry_config(host);
    let client = RegistryClient::new(config, registry.clone());

    // --- PUSH ---
    let mut push_events = Vec::new();
    let push_result = client
        .push(
            &manifest_digest,
            &format!("{}/test-agent:v1.0", host),
            |event| push_events.push(event),
        )
        .await;

    assert!(push_result.is_ok(), "Push failed: {:?}", push_result.err());

    let has_done = push_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Push should complete with Done event");

    // --- PULL ---
    let pull_temp = tempfile::tempdir().unwrap();
    let pull_registry = AgentRegistry::new(pull_temp.path());
    pull_registry.init().await.unwrap();

    let config2 = test_registry_config(host);
    let client2 = RegistryClient::new(config2, pull_registry.clone());

    let mut pull_events = Vec::new();
    let pull_result = client2
        .pull(
            &format!("{}/test-agent:v1.0", host),
            |event| pull_events.push(event),
        )
        .await;

    assert!(pull_result.is_ok(), "Pull failed: {:?}", pull_result.err());

    let has_done = pull_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Pull should complete with Done event");

    // Verify layers were pulled
    assert!(pull_registry.has_layer(&layer1_digest));
    assert!(pull_registry.has_layer(&layer2_digest));
    assert!(pull_registry.has_layer(&layer3_digest));

    // Verify layer content
    assert_eq!(
        pull_registry.get_layer(&layer1_digest).await.unwrap(),
        layer1_data
    );
    assert_eq!(
        pull_registry.get_layer(&layer2_digest).await.unwrap(),
        layer2_data
    );
    assert_eq!(
        pull_registry.get_layer(&layer3_digest).await.unwrap(),
        layer3_data
    );
}

#[tokio::test]
#[ignore = "requires Python mock registry server on port 18765"]
async fn test_registry_client_skips_existing_layers() {
    let host = "127.0.0.1:18765";

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    // Store layers
    let layer1_data = b"config layer";
    let layer1_digest = sha256_digest(layer1_data);
    registry.store_layer(&layer1_digest, layer1_data).await.unwrap();

    let layer2_data = b"identity layer";
    let layer2_digest = sha256_digest(layer2_data);
    registry.store_layer(&layer2_digest, layer2_data).await.unwrap();

    // Create manifest
    let mut agent_manifest = AgentManifest::new("skip-test", "1.0.0", "did:pekobot:test");
    agent_manifest.layers = Some(AgentLayers {
        config: Some(layer1_digest.to_string()),
        identity: Some(layer2_digest.to_string()),
        skills: None,
        workspace: None,
        sessions: None,
        mcp: None,
    });

    let manifest_digest = registry
        .store_manifest(&agent_manifest, Some("skip-test:v1.0"))
        .await
        .unwrap();

    // Store RegistryManifest JSON
    let mut reg_manifest = RegistryManifest::new("skip-test", "1.0.0")
        .with_digest(manifest_digest.as_str())
        .with_ref(format!("{}/skip-test:v1.0", host));
    reg_manifest.add_layer(Layer::new(
        layer1_digest.clone(),
        LayerType::Config,
        layer1_data.len() as u64,
    ));
    reg_manifest.add_layer(Layer::new(
        layer2_digest.clone(),
        LayerType::Identity,
        layer2_data.len() as u64,
    ));

    let reg_manifests_dir = temp_dir
        .path()
        .join("registry_manifests")
        .join(manifest_digest.dir_name());
    tokio::fs::create_dir_all(&reg_manifests_dir).await.unwrap();
    tokio::fs::write(
        reg_manifests_dir.join("manifest.json"),
        reg_manifest.to_json().unwrap(),
    )
    .await
    .unwrap();

    // First push
    let config = test_registry_config(host);
    let client = RegistryClient::new(config.clone(), registry.clone());
    let _ = client
        .push(
            &manifest_digest,
            &format!("{}/skip-test:v1.0", host),
            |_event| {},
        )
        .await
        .unwrap();

    // Second push — should skip layers
    let client2 = RegistryClient::new(config, registry.clone());
    let mut second_push_events = Vec::new();
    let _ = client2
        .push(
            &manifest_digest,
            &format!("{}/skip-test:v1.0", host),
            |event| second_push_events.push(event),
        )
        .await
        .unwrap();

    let has_done = second_push_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Second push should complete");
}
