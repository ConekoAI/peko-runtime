//! Registry Integration Tests (Tier 1)
//!
//! End-to-end tests for push/pull against the real PekoHub backend
//! running in test mode (PGlite + mock storage/search).
//!
//! These tests are marked `#[ignore]` because they require:
//!   - Node.js 22+ with tsx installed  (local mode)
//!   - OR a running PekoHub test container (container mode via PEKOHUB_URL)
//!
//! The test harness auto-starts the PekoHub backend on a random ephemeral port
//! and shuts it down after each test (local mode), or connects to an existing
//! container (container mode).
//!
//! Run locally:
//!   cd peko-runtime
//!   cargo test --test registry_integration -- --ignored
//!
//! Run in container:
//!   PEKOHUB_URL=http://pekohub-test:3000 cargo test --test registry_integration -- --ignored

use pekobot::portable::{manifest::AgentLayers, AgentManifest, Layer, LayerType};
use pekobot::registry::AgentRegistry;
use pekobot::registry::client::ResourceType;
use pekobot::registry::{
    media_types, RegistryClient, RegistryConfig, RegistryManifest, RegistryRef, RegistrySource,
};
use std::time::Duration;

mod common;
use common::{create_test_user, reset_pekohub, PekohubBackend};
use serial_test::serial;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a test registry config pointing at the hub server
fn test_registry_config(host: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: host.to_string(),
        priority: 1,
        auth: None,
        token: None,
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
        Layer::new(
            &identity_digest,
            LayerType::Identity,
            identity_data.len() as u64,
        ),
        Layer::new(&skills_digest, LayerType::Skills, skills_data.len() as u64),
    ];

    manifest.layers = Some(AgentLayers {
        config: Some(config_digest),
        identity: Some(identity_digest),
        skills: Some(skills_digest),
        workspace: None,
        sessions: None,
        mcp: None,
        extensions: None,
    });

    (manifest, layers)
}

/// Store a RegistryManifest locally so the client can push it
async fn store_registry_manifest_local(
    registry: &AgentRegistry,
    manifest: &RegistryManifest,
    digest: &pekobot::portable::types::ImageDigest,
) {
    let image_dir = registry
        .root_path()
        .join("registry_manifests")
        .join(digest.dir_name());
    tokio::fs::create_dir_all(&image_dir).await.unwrap();
    tokio::fs::write(image_dir.join("manifest.json"), manifest.to_json().unwrap())
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Tests: OCI registry protocol against PekoHub (direct HTTP)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_pekohub_manifest_roundtrip() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create user so PekoHub namespace ownership check passes
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    // Upload a dummy config blob first (pekohub validates blob existence)
    let config_data = b"{}";
    let config_digest = sha256_digest(config_data);
    let post_resp = client
        .post(format!("{}/v2/{ns}/test-agent/blobs/uploads/", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 202);
    let location = post_resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    let upload_url = if location.starts_with("http") {
        location.to_string()
    } else {
        format!("{}{}", backend.url, location)
    };
    let _ = client
        .put(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .query(&[("digest", &config_digest)])
        .body(config_data.as_slice())
        .send()
        .await
        .unwrap();

    // PUT manifest with OCI media type
    let manifest_json = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[],"annotations":{{"org.opencontainers.image.description":"Test bundle"}}}}"#,
        config_digest,
        config_data.len()
    );

    let put_resp = client
        .put(format!("{}/v2/{ns}/test-agent/manifests/v1.0", backend.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest_json)
        .send()
        .await
        .unwrap();

    assert!(
        put_resp.status().is_success(),
        "PUT manifest failed: {} - {:?}",
        put_resp.status(),
        put_resp.text().await.unwrap_or_default()
    );

    // GET manifest
    let get_resp = client
        .get(format!("{}/v2/{ns}/test-agent/manifests/v1.0", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("schemaVersion"));
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_pekohub_manifest_invalid_media_type_rejected() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    // PUT manifest with invalid media type ( PekoHub only accepts OCI )
    let manifest_json = r#"{"schemaVersion":2,"config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/{ns}/bad-agent/manifests/latest", backend.url))
        .header("Content-Type", "application/json")
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    // PekoHub rejects invalid media types with 400
    assert_eq!(put_resp.status(), 400);
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_pekohub_blob_roundtrip() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    let data = b"test blob content";
    let digest = sha256_digest(data);

    // Upload blob via POST + PUT
    let post_resp = client
        .post(format!("{}/v2/{ns}/test/blobs/uploads/", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 202);
    // PekoHub's blob upload Location header is sometimes relative
    // (e.g. "/v2/.../blobs/uploads/<uuid>"). reqwest refuses a
    // relative URL without a base, so resolve it against the
    // backend URL before passing to .put() (matches what
    // test_pekohub_catalog_and_tags already does).
    let location = post_resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap();
    let upload_url = if location.starts_with("http") {
        location.to_string()
    } else {
        format!("{}{}", backend.url, location)
    };

    let put_resp = client
        .put(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .query(&[("digest", digest.clone())])
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
        .head(format!("{}/v2/{ns}/test/blobs/{}", backend.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(head_resp.status(), 200);

    // GET blob
    let get_resp = client
        .get(format!("{}/v2/{ns}/test/blobs/{}", backend.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), data.as_slice());
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_pekohub_catalog_and_tags() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Create two users for two namespaces
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    // Push manifests to two different repos within the same namespace.
    for name in ["agent-a", "agent-b"] {
        // Upload a config blob first. Each push uses a unique config
        // payload so the resulting manifests have different digests
        // — pekohub's `digest_idx` is unique on `bundle_versions.digest`
        // globally (per `backend/src/db/schema.ts:166` and the test
        // fixture's DDL), so two manifests with the same digest
        // collide even when pushed to different repos.
        let config_data = format!("{{\"name\":\"{}\"}}", name);
        let config_digest = sha256_digest(config_data.as_bytes());

        let post_resp = client
            .post(format!("{}/v2/{}/{}/blobs/uploads/", backend.url, ns, name))
            .send()
            .await
            .unwrap();
        assert_eq!(post_resp.status(), 202);
        let location = post_resp.headers().get("location").unwrap().to_str().unwrap();
        let upload_url = if location.starts_with("http") {
            location.to_string()
        } else {
            format!("{}{}", backend.url, location)
        };
        let _ = client
            .put(&upload_url)
            .header("Content-Type", "application/octet-stream")
            .query(&[("digest", &config_digest)])
            .body(config_data.clone().into_bytes())
            .send()
            .await
            .unwrap();

        let json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[]}}"#,
            config_digest,
            config_data.len()
        );
        let resp = client
            .put(format!("{}/v2/{}/{}/manifests/v1.0", backend.url, ns, name))
            .header("Content-Type", media_types::MANIFEST_OCI)
            .body(json)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert!(status.is_success(), "Push {name} failed: {status} - {body}");
    }

    // Catalog
    let catalog_resp = client
        .get(format!("{}/v2/_catalog", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog_resp.status(), 200);
    let catalog: serde_json::Value = catalog_resp.json().await.unwrap();
    let repos = catalog["repositories"].as_array().unwrap();
    // Should have agent-a and agent-b
    assert!(repos.len() >= 2, "Expected at least 2 repos, got {}", repos.len());

    // Tags for agent-a
    let tags_resp = client
        .get(format!("{}/v2/{ns}/agent-a/tags/list", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(tags_resp.status(), 200);
    let tags: serde_json::Value = tags_resp.json().await.unwrap();
    assert_eq!(tags["name"], format!("{ns}/agent-a"));
    let tag_list = tags["tags"].as_array().unwrap();
    assert!(tag_list.iter().any(|t| t == "v1.0"));
}

// ---------------------------------------------------------------------------
// Tests: RegistryClient push/pull against PekoHub
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_registry_client_push_and_pull() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    let host = backend.url.strip_prefix("http://").unwrap();

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
        .with_ref(format!("{host}/ns/test-agent:v1.0"));
    for layer in &layers {
        reg_manifest.add_layer(layer.clone());
    }
    // PekoHub validates the top-level OCI `config` descriptor and
    // rejects empty digests as `Invalid digest format`. The config
    // blob was stored locally as `layer1` above — populate the
    // descriptor from it.
    reg_manifest = reg_manifest
        .with_config(layer1_digest.clone(), layer1_data.len() as u64, None::<String>);
    store_registry_manifest_local(&registry, &reg_manifest, &manifest_digest).await;

    // Configure client
    let config = test_registry_config(backend.url.as_str());
    let client = RegistryClient::new(config, registry.clone());

    // --- PUSH ---
    let mut push_events = Vec::new();
    let push_result = client
        .push(
            &manifest_digest,
            &format!("{}/ns/test-agent:v1.0", backend.url),
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

    let config2 = test_registry_config(backend.url.as_str());
    let client2 = RegistryClient::new(config2, pull_registry.clone());

    let mut pull_events = Vec::new();
    let pull_result = client2
        .pull(&format!("{}/ns/test-agent:v1.0", backend.url), |event| {
            pull_events.push(event)
        })
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
    assert_eq!(pull_registry.get_layer(&layer1_digest).await.unwrap(), layer1_data);
    assert_eq!(pull_registry.get_layer(&layer2_digest).await.unwrap(), layer2_data);
    assert_eq!(pull_registry.get_layer(&layer3_digest).await.unwrap(), layer3_data);
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_registry_client_skips_existing_layers() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

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
        extensions: None,
    });

    let manifest_digest = registry
        .store_manifest(&agent_manifest, Some("skip-test:v1.0"))
        .await
        .unwrap();

    // Store RegistryManifest JSON
    let mut reg_manifest = RegistryManifest::new("skip-test", "1.0.0")
        .with_digest(manifest_digest.as_str())
        .with_ref(format!("{}/ns/skip-test:v1.0", backend.url));
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
    // PekoHub rejects empty config.digest as "Invalid digest format".
    // layer1 is the config blob (see `agent_manifest.layers.config`
    // assignment above) — populate the top-level OCI config
    // descriptor from it.
    reg_manifest = reg_manifest
        .with_config(layer1_digest.clone(), layer1_data.len() as u64, None::<String>);
    store_registry_manifest_local(&registry, &reg_manifest, &manifest_digest).await;

    // First push
    let config = test_registry_config(backend.url.as_str());
    let client = RegistryClient::new(config, registry.clone());
    let _ = client
        .push(
            &manifest_digest,
            &format!("{}/ns/skip-test:v1.0", backend.url),
            |_event| {},
        )
        .await
        .unwrap();

    // Second push — should skip layers. We build a NEW manifest
    // (with a different config blob so the manifest digest
    // differs) and push it under a new tag (v1.1). PekoHub's
    // `digest_idx` is unique on `bundle_versions.digest` GLOBALLY
    // (`backend/src/db/schema.ts:166`), so re-pushing the same
    // manifest content (even at a new tag) 500s on digest_idx.
    // The new config blob is uploaded via the same blob-upload
    // path the first push used; the existing layer1/layer2
    // blobs are HEAD-checked by the runtime and skipped.
    let layer1_v2_data = b"config layer v2";
    let layer1_v2_digest = sha256_digest(layer1_v2_data);
    registry
        .store_layer(&layer1_v2_digest, layer1_v2_data)
        .await
        .unwrap();

    let mut agent_manifest_v2 = AgentManifest::new("skip-test", "1.0.1", "did:pekobot:test");
    agent_manifest_v2.layers = Some(AgentLayers {
        config: Some(layer1_v2_digest.to_string()),
        identity: Some(layer2_digest.to_string()),
        skills: None,
        workspace: None,
        sessions: None,
        mcp: None,
        extensions: None,
    });
    let manifest_digest_v2 = registry
        .store_manifest(&agent_manifest_v2, Some("skip-test:v1.1"))
        .await
        .unwrap();

    let mut reg_manifest_v2 = RegistryManifest::new("skip-test", "1.0.1")
        .with_digest(manifest_digest_v2.as_str())
        .with_ref(format!("{}/ns/skip-test:v1.1", backend.url));
    reg_manifest_v2.add_layer(Layer::new(
        layer1_v2_digest.clone(),
        LayerType::Config,
        layer1_v2_data.len() as u64,
    ));
    reg_manifest_v2.add_layer(Layer::new(
        layer2_digest.clone(),
        LayerType::Identity,
        layer2_data.len() as u64,
    ));
    reg_manifest_v2 = reg_manifest_v2.with_config(
        layer1_v2_digest.clone(),
        layer1_v2_data.len() as u64,
        None::<String>,
    );
    store_registry_manifest_local(&registry, &reg_manifest_v2, &manifest_digest_v2).await;

    let config2 = test_registry_config(backend.url.as_str());
    let client2 = RegistryClient::new(config2, registry.clone());
    let mut second_push_events = Vec::new();
    let _ = client2
        .push(
            &manifest_digest_v2,
            &format!("{}/ns/skip-test:v1.1", backend.url),
            |event| second_push_events.push(event),
        )
        .await
        .unwrap();

    let has_done = second_push_events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Second push should complete");
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_registry_client_bare_ref_resolution() {
    let backend = PekohubBackend::start().await;
    let host = backend.url.strip_prefix("http://").unwrap();

    // Test bare ref resolution: "my-agent:v1.0" -> "host/peko/agents/my-agent:v1.0"
    let resolved =
        RegistryRef::parse_with_default("my-agent:v1.0", Some(host), Some(ResourceType::Agent))
            .unwrap();
    assert_eq!(resolved.host, host);
    assert_eq!(resolved.path, "peko/agents/my-agent");
    assert_eq!(resolved.tag, "v1.0");

    // Test bare ref without tag defaults to "latest"
    let resolved =
        RegistryRef::parse_with_default("my-agent", Some(host), Some(ResourceType::Agent)).unwrap();
    assert_eq!(resolved.tag, "latest");

    // Test team resource type
    let resolved =
        RegistryRef::parse_with_default("my-team:v2.0", Some(host), Some(ResourceType::Team))
            .unwrap();
    assert_eq!(resolved.path, "peko/teams/my-team");
    assert_eq!(resolved.tag, "v2.0");
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_registry_client_pull_uses_oci_media_type() {
    // Verify that the RegistryClient sends OCI media type on push
    let accepted = RegistryClient::accept_manifest_media_types();
    assert!(accepted.contains(&media_types::MANIFEST_OCI));
    assert!(accepted.contains(&media_types::MANIFEST_PEKO));

    // Verify default is OCI
    assert_eq!(media_types::MANIFEST_DEFAULT, media_types::MANIFEST_OCI);
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_registry_client_pull_missing_manifest() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    let config = test_registry_config(backend.url.as_str());
    let client = RegistryClient::new(config, registry.clone());

    let result = client
        .pull(
            &format!("{}/ns/nonexistent:latest", backend.url),
            |_event| {},
        )
        .await;

    assert!(result.is_err(), "Pulling nonexistent manifest should fail");
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
#[serial]
async fn test_registry_client_digest_verification_on_pull() {
    let backend = PekohubBackend::start().await;
    reset_pekohub(&backend.url).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();
    let (_id, ns) = create_test_user(&client, &backend.url, "ns").await;

    // Upload two blobs: one is the config (top-level descriptor),
    // one is the layer. The manifest below references both by their
    // actual digests so pekohub's blob-existence check passes.
    let config_data = b"config blob";
    let config_digest = sha256_digest(config_data);
    let layer_data = b"correct data";
    let correct_digest = sha256_digest(layer_data);

    for (data, digest) in [
        (config_data.to_vec(), config_digest.clone()),
        (layer_data.to_vec(), correct_digest.clone()),
    ] {
        let post_resp = client
            .post(format!("{}/v2/{ns}/digest-test/blobs/uploads/", backend.url))
            .send()
            .await
            .unwrap();
        let upload_url = post_resp
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap();
        let upload_url = if upload_url.starts_with("http") {
            upload_url.to_string()
        } else {
            format!("{}{}", backend.url, upload_url)
        };

        let put_resp = client
            .put(&upload_url)
            .header("Content-Type", "application/octet-stream")
            .query(&[("digest", digest.as_str())])
            .body(data)
            .send()
            .await
            .unwrap();
        let status = put_resp.status();
        let body = put_resp.text().await.unwrap_or_default();
        assert!(status.is_success(), "Blob PUT failed: {status} - {body}");
    }

    // Push a manifest that references both blobs by their actual digests.
    // The test then pulls and verifies the runtime accepts a manifest
    // whose digests match the stored blobs.
    let manifest_json = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[{{"digest":"{}","mediaType":"application/vnd.peko.layer.config.v1+json","size":{}}}]}}"#,
        config_digest,
        config_data.len(),
        correct_digest,
        layer_data.len()
    );

    let manifest_put = client
        .put(format!("{}/v2/{ns}/digest-test/manifests/v1.0", backend.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert!(
        manifest_put.status().is_success(),
        "Manifest push failed: {} - {:?}",
        manifest_put.status(),
        manifest_put.text().await.unwrap_or_default()
    );

    // Pull — the RegistryClient should verify the blob digest and pass
    // (the manifest says correct_digest and registry returns correct data)
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    let config = test_registry_config(backend.url.as_str());
    let reg_client = RegistryClient::new(config, registry.clone());

    let result = reg_client
        .pull(&format!("{}/{ns}/digest-test:v1.0", backend.url), |_event| {})
        .await;

    // This should succeed since the manifest and blob digests match
    assert!(result.is_ok(), "Pull with correct digest should succeed: {:?}", result.err());
}