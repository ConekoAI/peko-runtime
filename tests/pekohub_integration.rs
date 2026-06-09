//! PekoHub Integration Tests (Layer 2)
//!
//! End-to-end tests for push/pull against the real PekoHub backend
//! running in test mode (PGlite + mock storage/search).
//!
//! These tests are marked `#[ignore]` because they require:
//!   - Node.js 22+ with tsx installed
//!   - The PekoHub backend source at `../pekohub/backend`
//!
//! The test harness auto-starts the PekoHub backend on a random ephemeral port
//! and shuts it down after each test.
//!
//! Run:
//!   cd peko-runtime
//!   cargo test --test pekohub_integration -- --ignored

use pekobot::registry::media_types;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Test harness: auto-start pekohub backend
// ---------------------------------------------------------------------------

/// Holds the running pekohub backend process and its URL
struct PekohubBackend {
    #[allow(dead_code)]
    child: Child,
    url: String,
}

impl PekohubBackend {
    /// Start the pekohub backend test server on a random port.
    ///
    /// # Panics
    /// Panics if the server cannot be started or the port cannot be read.
    async fn start() -> Self {
        let backend_path = std::env::var("PEKOHUB_BACKEND_PATH").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../pekohub/backend").to_string()
        });

        let script_path = format!("{backend_path}/tests/fixtures/server.ts");

        // Verify the script exists
        if !std::path::Path::new(&script_path).exists() {
            panic!(
                "PekoHub test server script not found at: {script_path}\n\
                 Set PEKOHUB_BACKEND_PATH to the pekohub/backend directory."
            );
        }

        // Resolve tsx CLI path relative to backend node_modules
        let tsx_cli = format!("{backend_path}/node_modules/tsx/dist/cli.mjs");
        if !std::path::Path::new(&tsx_cli).exists() {
            panic!(
                "tsx CLI not found at: {tsx_cli}\n\
                 Run: cd {backend_path} && npm install"
            );
        }

        let mut cmd = Command::new("node");
        cmd.arg(&tsx_cli)
            .arg(&script_path)
            .arg("--port")
            .arg("0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&backend_path);

        let mut child = cmd.spawn().expect(
            "Failed to start PekoHub backend. Is Node.js 22+ with tsx installed? \
             Install with: cd pekohub/backend && npm install",
        );

        // Read stdout for the PORT= line
        let stdout = child.stdout.take().expect("Failed to capture stdout");
        let reader = std::io::BufReader::new(stdout);
        let port = tokio::task::spawn_blocking(move || {
            use std::io::BufRead;
            for line in reader.lines() {
                let line = line.expect("Failed to read line from PekoHub backend");
                if let Some(port_str) = line.strip_prefix("PORT=") {
                    return port_str.parse::<u16>().expect("Invalid PORT line");
                }
            }
            panic!("PekoHub backend did not print PORT= line")
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
            if client.get(format!("{url}/health")).send().await.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(ready, "PekoHub backend did not become ready in 5 seconds");

        Self { child, url }
    }
}

impl Drop for PekohubBackend {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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

// ---------------------------------------------------------------------------
// Tests: Health check & basic connectivity
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires Node.js with tsx and pekohub backend source"]
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
#[ignore = "requires Node.js with tsx and pekohub backend source"]
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
#[ignore = "requires Node.js with tsx and pekohub backend source"]
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
    // Location may be relative; prepend base URL if needed
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
#[ignore = "requires Node.js with tsx and pekohub backend source"]
async fn test_pekohub_catalog_and_tags() {
    let backend = PekohubBackend::start().await;
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
// Note: RegistryClient push/pull tests
// ---------------------------------------------------------------------------
//
// RegistryClient uses a Peko-specific manifest format (schema_version, size_bytes,
// layer_type) that is NOT OCI-compliant. PekoHub validates manifests with the
// strict OCI schema (schemaVersion, size, mediaType).
//
// Therefore RegistryClient push/pull is tested against the mock registry in
// Layer 1 (tests/registry_integration.rs). Full CLI E2E tests against PekoHub
// are in Layer 3 (e2e_tests/packaging/*.ps1) where the CLI handles format
// conversion before calling RegistryClient.

#[tokio::test]
#[ignore = "requires Node.js with tsx and pekohub backend source"]
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

    // Build OCI manifest with Pekohub metadata annotations
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
        .get(format!("{}/api/v1/search?q=searchable", backend.url))
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
#[ignore = "requires Node.js with tsx and pekohub backend source"]
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
        .get(format!("{}/api/v1/bundles/ns/detail-test", backend.url))
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
            "{}/api/v1/bundles/ns/detail-test/versions",
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
