//! PekoHub Integration Tests (Layer 2)
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
//!   cargo test --test pekohub_integration -- --ignored
//!
//! Run in container:
//!   PEKOHUB_URL=http://pekohub-test:3000 cargo test --test pekohub_integration -- --ignored

use pekobot::portable::{manifest::AgentLayers, AgentManifest, AgentRegistry, Layer, LayerType};
use pekobot::registry::client::ResourceType;
use pekobot::registry::{
    media_types, RegistryClient, RegistryConfig, RegistryManifest, RegistryRef, RegistrySource,
};
use std::time::Duration;

mod common;
use common::{reset_pekohub, PekohubBackend};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute sha256 digest of data
fn sha256_digest(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{:x}", hasher.finalize())
}

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
// Tests: Health check & basic connectivity
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_pekohub_health_check() {
    let backend = PekohubBackend::start().await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let resp = client
        .get(format!("{}/health", backend.url))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

// ---------------------------------------------------------------------------
// Tests: OCI registry protocol against pekohub
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_pekohub_manifest_roundtrip() {
    let backend = PekohubBackend::start().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Upload a dummy config blob first (pekohub validates blob existence)
    let config_data = b"{}";
    let config_digest = sha256_digest(config_data);
    let post_resp = client
        .post(format!("{}/v2/ns/test-agent/blobs/uploads/", backend.url))
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
        .put(format!("{}/v2/ns/test-agent/manifests/v1.0", backend.url))
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
        .get(format!("{}/v2/ns/test-agent/manifests/v1.0", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.text().await.unwrap();
    assert!(body.contains("schemaVersion"));
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_pekohub_blob_upload_and_download() {
    let backend = PekohubBackend::start().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    let data = b"test blob content for pekohub";
    let digest = sha256_digest(data);

    // Initiate upload
    let post_resp = client
        .post(format!("{}/v2/ns/test/blobs/uploads/", backend.url))
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

    // Complete upload
    let put_resp = client
        .put(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .query(&[("digest", &digest)])
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
        .head(format!("{}/v2/ns/test/blobs/{}", backend.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(head_resp.status(), 200);

    // GET blob
    let get_resp = client
        .get(format!("{}/v2/ns/test/blobs/{}", backend.url, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let body = get_resp.bytes().await.unwrap();
    assert_eq!(body.as_ref(), data.as_slice());
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_pekohub_catalog_and_tags() {
    let backend = PekohubBackend::start().await;
    // The pekohub-test container is long-lived and shared across the
    // whole test run, so earlier binaries/tests in the same `cargo test`
    // invocation can leave repositories in its catalog. Reset before
    // asserting the exact count.
    reset_pekohub(&backend.url).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Upload config blobs and push two manifests
    for (repo, _name) in [("ns/agent-a", "agent-a"), ("ns/agent-b", "agent-b")] {
        let config_data = b"{}";
        let config_digest = sha256_digest(config_data);

        // Upload config blob
        let post_resp = client
            .post(format!("{}/v2/{}/blobs/uploads/", backend.url, repo))
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
        let blob_put = client
            .put(&upload_url)
            .header("Content-Type", "application/octet-stream")
            .query(&[("digest", &config_digest)])
            .body(config_data.as_slice())
            .send()
            .await
            .unwrap();
        assert!(
            blob_put.status().is_success(),
            "Config blob upload failed: {}",
            blob_put.status()
        );

        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[],"annotations":{{"org.opencontainers.image.description":"{_name}"}}}}"#,
            config_digest,
            config_data.len()
        );
        let resp = client
            .put(format!("{}/v2/{}/manifests/v1.0", backend.url, repo))
            .header("Content-Type", media_types::MANIFEST_OCI)
            .body(manifest)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "Push failed for {repo}: {} - {}",
            status,
            body
        );
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
    assert_eq!(repos.len(), 2, "Expected 2 repositories in catalog");

    // Tags
    let tags_resp = client
        .get(format!("{}/v2/ns/agent-a/tags/list", backend.url))
        .send()
        .await
        .unwrap();
    assert_eq!(tags_resp.status(), 200);
    let tags: serde_json::Value = tags_resp.json().await.unwrap();
    assert_eq!(tags["name"], "ns/agent-a");
    let tag_list = tags["tags"].as_array().unwrap();
    assert!(tag_list.iter().any(|t| t == "v1.0"));
}

// ---------------------------------------------------------------------------
// Tests: Migrated from registry_integration.rs (OCI protocol via direct HTTP)
//
// NOTE: RegistryClient push/pull tests are NOT migrated here because
// RegistryClient uses a Peko-specific manifest format (schema_version, size_bytes,
// layer_type) that is NOT OCI-compliant. PekoHub validates manifests with the
// strict OCI schema (schemaVersion, size, mediaType).
//
// Therefore RegistryClient push/pull tests remain in registry_integration.rs
// where they run against a compatible mock registry.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_registry_client_bare_ref_resolution() {
    let backend = PekohubBackend::start().await;
    let host = backend.url.strip_prefix("http://").unwrap_or(&backend.url);

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
    let _backend = PekohubBackend::start().await;

    // Verify that the RegistryClient sends OCI media type on push
    let accepted = RegistryClient::accept_manifest_media_types();
    assert!(accepted.contains(&media_types::MANIFEST_OCI));
    assert!(accepted.contains(&media_types::MANIFEST_PEKO));

    // Verify default is OCI
    assert_eq!(media_types::MANIFEST_DEFAULT, media_types::MANIFEST_OCI);
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_registry_client_pull_missing_manifest() {
    let backend = PekohubBackend::start().await;
    let host = backend.url.strip_prefix("http://").unwrap_or(&backend.url);

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

// ---------------------------------------------------------------------------
// Tests: PekoHub-specific APIs
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires PekoHub backend + fix for null hooks schema validation in search response"]
async fn test_pekohub_search_api() {
    let backend = PekohubBackend::start().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Upload config blob first
    let config_data = b"{}";
    let config_digest = sha256_digest(config_data);
    let post_resp = client
        .post(format!("{}/v2/ns/searchable/blobs/uploads/", backend.url))
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

    // Build OCI manifest with Pekohub metadata annotations. We don't
    // include a `hooks` field anywhere — pekohub's `nullishToUndefined`
    // schema helper (pekohub issue 001) coerces a null `hooks` to
    // undefined in the search response, so the response no longer
    // 500s on this field being absent.
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_data.len()
        },
        "layers": [],
        "annotations": {
            "dev.pekohub.metadata": r#"{"bundleType":"agent","description":"A searchable test agent","author":"test"}"#,
            "org.opencontainers.image.description": "A searchable test agent"
        }
    })
    .to_string();

    let put_resp = client
        .put(format!("{}/v2/ns/searchable/manifests/v1.0", backend.url))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest)
        .send()
        .await
        .unwrap();
    let status = put_resp.status();
    let body = put_resp.text().await.unwrap_or_default();
    assert!(status.is_success(), "Push failed: {} - {}", status, body);

    // Search for the agent
    let search_resp = client
        .get(format!("{}/v1/search?q=searchable", backend.url))
        .send()
        .await
        .unwrap();
    let search_status = search_resp.status();
    let search_body = search_resp.text().await.unwrap_or_default();
    assert_eq!(search_status, 200, "Search failed: {}", search_body);

    let search_result: serde_json::Value = serde_json::from_str(&search_body).unwrap();
    let hits = search_result["items"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| {
            h.get("namespace").map(|v| v == "ns").unwrap_or(false)
                && h.get("name").map(|v| v == "searchable").unwrap_or(false)
        }),
        "Search should find the pushed agent, got: {:?}",
        hits
    );
}

#[tokio::test]
#[ignore = "requires PekoHub backend (Node.js+tsx locally, or PEKOHUB_URL container)"]
async fn test_pekohub_bundle_detail_api() {
    let backend = PekohubBackend::start().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .no_proxy()
        .build()
        .unwrap();

    // Upload config blob first
    let config_data = b"{}";
    let config_digest = sha256_digest(config_data);
    let post_resp = client
        .post(format!("{}/v2/ns/detail-test/blobs/uploads/", backend.url))
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

    // Push a manifest with required metadata for BundleDetail parsing
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config_data.len()
        },
        "layers": [],
        "annotations": {
            "org.opencontainers.image.description": "Bundle detail test",
            "org.opencontainers.image.authors": "test-author"
        }
    })
    .to_string();

    let put_resp = client
        .put(format!(
            "{}/v2/ns/detail-test/manifests/v2.0.0",
            backend.url
        ))
        .header("Content-Type", media_types::MANIFEST_OCI)
        .body(manifest)
        .send()
        .await
        .unwrap();
    let status = put_resp.status();
    let body = put_resp.text().await.unwrap_or_default();
    assert!(status.is_success(), "Push failed: {} - {}", status, body);

    // Get bundle detail
    let detail_resp = client
        .get(format!("{}/v1/bundles/ns/detail-test", backend.url))
        .send()
        .await
        .unwrap();
    let detail_status = detail_resp.status();
    let detail_body = detail_resp.text().await.unwrap_or_default();
    assert_eq!(detail_status, 200, "Bundle detail failed: {}", detail_body);

    let detail: serde_json::Value = serde_json::from_str(&detail_body).unwrap();
    assert_eq!(detail["namespace"], "ns");
    assert_eq!(detail["name"], "detail-test");

    // Get versions
    let versions_resp = client
        .get(format!(
            "{}/v1/bundles/ns/detail-test/versions",
            backend.url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(versions_resp.status(), 200);

    let versions: serde_json::Value = versions_resp.json().await.unwrap();
    let version_list = versions["versions"].as_array().unwrap();
    assert!(version_list.iter().any(|v| v["version"] == "v2.0.0"));
}
