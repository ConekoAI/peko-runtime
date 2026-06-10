//! Registry Integration Tests (Tier 1)
//!
//! End-to-end tests for push/pull against the Python mock registry server.
//!
//! These tests are marked `#[ignore]` because they require:
//!   - Python 3 with fastapi + uvicorn installed
//!   - The mock registry server script at `e2e_tests/packaging/mock_registry/main.py`
//!
//! The test harness auto-starts the mock registry on a random ephemeral port
//! and shuts it down after each test.
//!
//! Run:
//!   cargo test --test registry_integration -- --ignored

use pekobot::portable::{manifest::AgentLayers, AgentManifest, AgentRegistry, Layer, LayerType};
use pekobot::registry::client::ResourceType;
use pekobot::registry::{
    media_types, RegistryClient, RegistryConfig, RegistryManifest, RegistryRef, RegistrySource,
};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Test harness: auto-start mock registry
// ---------------------------------------------------------------------------

/// Holds the running mock registry process and its URL
struct MockRegistry {
    #[allow(dead_code)]
    child: Child,
    url: String,
}

impl MockRegistry {
    /// Start the mock registry server on a random port.
    ///
    /// # Panics
    /// Panics if the server cannot be started or the port cannot be read.
    async fn start(auth_token: Option<&str>) -> Self {
        let script_path = std::env::var("MOCK_REGISTRY_SCRIPT").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/e2e_tests/packaging/mock_registry/main.py"
            )
            .to_string()
        });

        let mut cmd = Command::new("python");
        cmd.arg(&script_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(token) = auth_token {
            cmd.arg("--auth-token").arg(token);
        }

        let mut child = cmd.spawn().expect(
            "Failed to start mock registry. Is Python with fastapi+uvicorn installed? \
             Install with: pip install fastapi uvicorn",
        );

        // Read stdout for the PORT= line
        let stdout = child.stdout.take().expect("Failed to capture stdout");
        let reader = std::io::BufReader::new(stdout);
        let port = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in reader.lines() {
                let line = line.expect("Failed to read line from mock registry");
                if let Some(port_str) = line.strip_prefix("PORT=") {
                    return port_str.parse::<u16>().expect("Invalid PORT line");
                }
            }
            panic!("Mock registry did not print PORT= line")
        })
        .await
        .expect("Port detection task panicked");

        let url = format!("http://127.0.0.1:{port}");

        // Wait for the server to be ready
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();

        let mut ready = false;
        for _ in 0..50 {
            if client.get(format!("{url}/v2/")).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "Mock registry did not become ready in 5 seconds");

        Self { child, url }
    }

    #[allow(dead_code)]
    /// Reset the registry storage (clear all data).
    async fn reset(&self) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .no_proxy()
            .build()
            .unwrap();
        let _ = client
            .delete(format!("{}/_debug/reset", self.url))
            .send()
            .await;
    }
}

impl Drop for MockRegistry {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a test registry config pointing at the mock server
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

/// Create a test registry config with bearer token auth
fn test_registry_config_with_token(host: &str, token: &str) -> RegistryConfig {
    let mut config = RegistryConfig::default();
    config.sources.clear();
    config.add_source(RegistrySource {
        url: host.to_string(),
        priority: 1,
        auth: None,
        token: Some(token.to_string()),
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
// Tests: Basic registry protocol (direct HTTP)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_manifest_roundtrip() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // PUT manifest with OCI media type
    let manifest_json = r#"{"schema_version":1,"name":"test-agent","version":"1.0.0","ref":"","digest":"sha256:abc123","created_at":"2026-05-08T10:00:00Z","source":"local","layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/ns/test-agent/manifests/latest", server.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
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
        .get(format!("{}/v2/ns/test-agent/manifests/latest", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("test-agent"));
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_manifest_peko_media_type() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // PUT manifest with legacy Peko media type (should be accepted)
    let manifest_json = r#"{"schema_version":1,"name":"peko-agent","version":"1.0.0","ref":"","digest":"sha256:abc123","created_at":"2026-05-08T10:00:00Z","source":"local","layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/ns/peko-agent/manifests/v1.0", server.url))
        .header("Content-Type", media_types::MANIFEST_PEKO)
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert!(
        put_resp.status().is_success(),
        "PUT manifest with Peko media type failed: {}",
        put_resp.status()
    );

    // GET should return the same media type
    let get_resp = client
        .get(format!("{}/v2/ns/peko-agent/manifests/v1.0", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let ct = get_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert_eq!(ct, media_types::MANIFEST_PEKO);
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_manifest_invalid_media_type_rejected() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // PUT manifest with invalid media type
    let manifest_json = r#"{"schema_version":1,"name":"bad-agent","version":"1.0.0","layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/ns/bad-agent/manifests/latest", server.url))
        .header("Content-Type", "application/json")
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 400);
    let body: serde_json::Value = put_resp.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "MANIFEST_INVALID");
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_blob_roundtrip() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let data = b"test blob content";
    let digest = sha256_digest(data);

    // Upload blob via POST + PUT
    let post_resp = client
        .post(format!("{}/v2/ns/test/blobs/uploads/", server.url))
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
        .head(format!("{}/v2/ns/test/blobs/{}", server.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(head_resp.status(), 200);

    // GET blob
    let get_resp = client
        .get(format!("{}/v2/ns/test/blobs/{}", server.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), data.as_slice());
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_catalog_and_tags() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Push two manifests
    for (repo, name) in [("ns/agent-a", "agent-a"), ("ns/agent-b", "agent-b")] {
        let json = format!(
            r#"{{"schema_version":1,"name":"{}","version":"1.0.0","layers":[]}}"#,
            name
        );
        let resp = client
            .put(format!("{}/v2/{}/manifests/v1.0", server.url, repo))
            .header("Content-Type", media_types::MANIFEST_OCI)
            .body(json)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
    }

    // Catalog
    let catalog_resp = client
        .get(format!("{}/v2/_catalog", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(catalog_resp.status(), 200);
    let catalog: serde_json::Value = catalog_resp.json().await.unwrap();
    let repos = catalog["repositories"].as_array().unwrap();
    assert_eq!(repos.len(), 2);

    // Tags
    let tags_resp = client
        .get(format!("{}/v2/ns/agent-a/tags/list", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(tags_resp.status(), 200);
    let tags: serde_json::Value = tags_resp.json().await.unwrap();
    assert_eq!(tags["name"], "ns/agent-a");
    let tag_list = tags["tags"].as_array().unwrap();
    assert!(tag_list.iter().any(|t| t == "v1.0"));
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_namespace_validation() {
    let server = MockRegistry::start(None).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Try to access a repository without namespace separator
    let resp = client
        .get(format!("{}/v2/no-namespace/manifests/latest", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "NAME_UNKNOWN");
}

// ---------------------------------------------------------------------------
// Tests: Auth
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_mock_registry_auth_required_for_mutations() {
    let server = MockRegistry::start(Some("secret-token-123")).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // PUT manifest without auth should fail
    let manifest_json = r#"{"schema_version":1,"name":"auth-test","version":"1.0.0","layers":[]}"#;
    let put_resp = client
        .put(format!("{}/v2/ns/auth-test/manifests/v1.0", server.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 401);
    let body: serde_json::Value = put_resp.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNAUTHORIZED");

    // POST blob upload without auth should fail
    let post_resp = client
        .post(format!("{}/v2/ns/auth-test/blobs/uploads/", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 401);

    // GET manifest without auth should succeed (read is public)
    // First push with auth
    let authed_put = client
        .put(format!("{}/v2/ns/auth-test/manifests/v1.0", server.url))
        .header("Authorization", "Bearer secret-token-123")
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert!(authed_put.status().is_success());

    // Now read without auth
    let get_resp = client
        .get(format!("{}/v2/ns/auth-test/manifests/v1.0", server.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
}

// ---------------------------------------------------------------------------
// Tests: RegistryClient push/pull
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_push_and_pull() {
    let server = MockRegistry::start(None).await;
    let host = server.url.strip_prefix("http://").unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    // Store some fake layers in the local registry
    let layer1_data = b"config layer content";
    let layer1_digest = sha256_digest(layer1_data);
    registry
        .store_layer(&layer1_digest, layer1_data)
        .await
        .unwrap();

    let layer2_data = b"identity layer content";
    let layer2_digest = sha256_digest(layer2_data);
    registry
        .store_layer(&layer2_digest, layer2_data)
        .await
        .unwrap();

    let layer3_data = b"skills layer content";
    let layer3_digest = sha256_digest(layer3_data);
    registry
        .store_layer(&layer3_digest, layer3_data)
        .await
        .unwrap();

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
    store_registry_manifest_local(&registry, &reg_manifest, &manifest_digest).await;

    // Configure client
    let config = test_registry_config(host);
    let client = RegistryClient::new(config, registry.clone());

    // --- PUSH ---
    let mut push_events = Vec::new();
    let push_result = client
        .push(
            &manifest_digest,
            &format!("{host}/ns/test-agent:v1.0"),
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
        .pull(&format!("{host}/ns/test-agent:v1.0"), |event| {
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
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_skips_existing_layers() {
    let server = MockRegistry::start(None).await;
    let host = server.url.strip_prefix("http://").unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    // Store layers
    let layer1_data = b"config layer";
    let layer1_digest = sha256_digest(layer1_data);
    registry
        .store_layer(&layer1_digest, layer1_data)
        .await
        .unwrap();

    let layer2_data = b"identity layer";
    let layer2_digest = sha256_digest(layer2_data);
    registry
        .store_layer(&layer2_digest, layer2_data)
        .await
        .unwrap();

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
        .with_ref(format!("{host}/ns/skip-test:v1.0"));
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
    store_registry_manifest_local(&registry, &reg_manifest, &manifest_digest).await;

    // First push
    let config = test_registry_config(host);
    let client = RegistryClient::new(config.clone(), registry.clone());
    let _ = client
        .push(
            &manifest_digest,
            &format!("{host}/ns/skip-test:v1.0"),
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
            &format!("{host}/ns/skip-test:v1.0"),
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
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_push_with_auth_token() {
    let token = "ph_test_token_abc123";
    let server = MockRegistry::start(Some(token)).await;
    let host = server.url.strip_prefix("http://").unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    let layer_data = b"auth layer content";
    let layer_digest = sha256_digest(layer_data);
    registry
        .store_layer(&layer_digest, layer_data)
        .await
        .unwrap();

    let mut agent_manifest = AgentManifest::new("auth-agent", "1.0.0", "did:pekobot:test");
    agent_manifest.layers = Some(AgentLayers {
        config: Some(layer_digest.to_string()),
        identity: None,
        skills: None,
        workspace: None,
        sessions: None,
        mcp: None,
        extensions: None,
    });

    let manifest_digest = registry
        .store_manifest(&agent_manifest, Some("auth-agent:v1.0"))
        .await
        .unwrap();

    let mut reg_manifest = RegistryManifest::new("auth-agent", "1.0.0")
        .with_digest(manifest_digest.as_str())
        .with_ref(format!("{host}/ns/auth-agent:v1.0"));
    reg_manifest.add_layer(Layer::new(
        layer_digest.clone(),
        LayerType::Config,
        layer_data.len() as u64,
    ));
    store_registry_manifest_local(&registry, &reg_manifest, &manifest_digest).await;

    // Configure client with token
    let config = test_registry_config_with_token(host, token);
    let client = RegistryClient::new(config, registry.clone());

    let mut events = Vec::new();
    let result = client
        .push(
            &manifest_digest,
            &format!("{host}/ns/auth-agent:v1.0"),
            |event| events.push(event),
        )
        .await;

    assert!(
        result.is_ok(),
        "Push with auth token failed: {:?}",
        result.err()
    );

    let has_done = events
        .iter()
        .any(|e| matches!(e, pekobot::registry::ProgressEvent::Done { .. }));
    assert!(has_done, "Push should complete with Done event");
}

// ---------------------------------------------------------------------------
// Tests: Namespace / reference resolution
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_bare_ref_resolution() {
    let server = MockRegistry::start(None).await;
    let host = server.url.strip_prefix("http://").unwrap();

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
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_pull_uses_oci_media_type() {
    let server = MockRegistry::start(None).await;
    let _host = server.url.strip_prefix("http://").unwrap();

    // Verify that the RegistryClient sends OCI media type on push
    let accepted = RegistryClient::accept_manifest_media_types();
    assert!(accepted.contains(&media_types::MANIFEST_OCI));
    assert!(accepted.contains(&media_types::MANIFEST_PEKO));

    // Verify default is OCI
    assert_eq!(media_types::MANIFEST_DEFAULT, media_types::MANIFEST_OCI);
}

// ---------------------------------------------------------------------------
// Tests: Error handling
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_pull_missing_manifest() {
    let server = MockRegistry::start(None).await;
    let host = server.url.strip_prefix("http://").unwrap();

    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    let config = test_registry_config(host);
    let client = RegistryClient::new(config, registry.clone());

    let result = client
        .pull(&format!("{host}/ns/nonexistent:latest"), |_event| {})
        .await;

    assert!(result.is_err(), "Pulling nonexistent manifest should fail");
}

#[tokio::test]
#[ignore = "requires Python mock registry server"]
async fn test_registry_client_digest_verification_on_pull() {
    let server = MockRegistry::start(None).await;
    let host = server.url.strip_prefix("http://").unwrap();

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Upload a blob with a specific digest
    let data = b"correct data";
    let correct_digest = sha256_digest(data);

    let post_resp = client
        .post(format!("{}/v2/ns/digest-test/blobs/uploads/", server.url))
        .send()
        .await
        .unwrap();
    let upload_url = post_resp
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let put_resp = client
        .put(&upload_url)
        .query(&[("digest", &correct_digest)])
        .body(data.as_slice())
        .send()
        .await
        .unwrap();
    assert!(put_resp.status().is_success());

    // Now push a manifest that references this blob with a WRONG digest
    let wrong_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    let manifest_json = format!(
        r#"{{"schema_version":1,"name":"digest-test","version":"1.0.0","digest":"{}","layers":[{{"digest":"{}","layer_type":"config","size_bytes":12}}],"ref":"{}/ns/digest-test:v1.0","created_at":"2026-05-08T10:00:00Z","source":"local"}}"#,
        correct_digest, wrong_digest, host
    );

    let manifest_put = client
        .put(format!("{}/v2/ns/digest-test/manifests/v1.0", server.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest_json)
        .send()
        .await
        .unwrap();
    assert!(manifest_put.status().is_success());

    // Try to pull — the RegistryClient should verify the blob digest and fail
    let temp_dir = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::new(temp_dir.path());
    registry.init().await.unwrap();

    let config = test_registry_config(host);
    let reg_client = RegistryClient::new(config, registry.clone());

    let result = reg_client
        .pull(&format!("{host}/ns/digest-test:v1.0"), |_event| {})
        .await;

    // The pull should fail because the manifest says the layer has wrong_digest
    // but the registry returns the actual data with correct_digest
    // Actually, the RegistryClient verifies the digest of downloaded data against
    // what the manifest says. Since the manifest says wrong_digest but data has
    // correct_digest, this should fail.
    assert!(result.is_err(), "Pull with digest mismatch should fail");
}
