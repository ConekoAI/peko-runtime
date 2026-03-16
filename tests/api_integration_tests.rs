//! HTTP API Integration Tests
//!
//! These tests verify the HTTP API endpoints work correctly.
//! They require a running daemon to test against.

use std::time::Duration;

/// Default base URL for API tests
fn base_url() -> String {
    std::env::var("PEKOBOT_TEST_URL").unwrap_or_else(|_| "http://127.0.0.1:11435".to_string())
}

/// Test helper: Wait for server to be ready
async fn wait_for_server() -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!("{}/health", base_url());

    for _ in 0..30 {
        match client
            .get(&url)
            .timeout(Duration::from_secs(1))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }

    anyhow::bail!("Server did not become ready in time")
}

#[tokio::test]
#[ignore = "Requires running daemon"] // Run with: cargo test -- --ignored
async fn test_health_endpoint() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", base_url()))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.expect("Should parse JSON");
    assert_eq!(json["status"], "ok");
    assert!(json["version"].as_str().is_some());
    assert!(json["uptime_seconds"].as_u64().is_some());
    assert!(json["instance_count"].as_u64().is_some());
    assert!(json["team_count"].as_u64().is_some());
}

#[tokio::test]
#[ignore = "Requires running daemon"]
async fn test_info_endpoint() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/info", base_url()))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.expect("Should parse JSON");
    assert!(json["version"].as_str().is_some());
    assert_eq!(json["api_version"], "1.0");
    assert!(json["workspace"].as_str().is_some());
    assert!(json["port"].as_u64().is_some());
    assert!(json["pid"].as_u64().is_some());
    assert!(json["platform"].as_str().is_some());
    assert!(json["capabilities"].is_object());
    assert_eq!(json["capabilities"]["streaming"], true);
    assert_eq!(json["capabilities"]["websocket"], true);
    assert_eq!(json["capabilities"]["teams"], true);
}

#[tokio::test]
#[ignore = "Requires running daemon"]
async fn test_version_header_present() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", base_url()))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let version_header = resp.headers().get("X-Pekobot-Version");
    assert!(
        version_header.is_some(),
        "X-Pekobot-Version header should be present"
    );
}

#[tokio::test]
#[ignore = "Requires running daemon"]
async fn test_request_id_echo() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", base_url()))
        .header("X-Request-ID", "test-request-123")
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let request_id_header = resp.headers().get("X-Request-ID");
    assert_eq!(
        request_id_header.map(|h| h.to_str().unwrap()),
        Some("test-request-123"),
        "X-Request-ID should be echoed back"
    );
}

#[tokio::test]
#[ignore = "Requires running daemon"]
async fn test_request_id_generated() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/health", base_url()))
        .send()
        .await
        .expect("Request should succeed");

    assert_eq!(resp.status(), 200);

    let request_id_header = resp.headers().get("X-Request-ID");
    assert!(
        request_id_header.is_some(),
        "X-Request-ID header should be present even when not provided"
    );

    // Verify it's a valid UUID
    let id = request_id_header.unwrap().to_str().unwrap();
    assert!(
        uuid::Uuid::parse_str(id).is_ok(),
        "Request ID should be a valid UUID"
    );
}

#[tokio::test]
#[ignore = "Requires running daemon"]
async fn test_404_error_format() {
    wait_for_server().await.expect("Server should be running");

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/nonexistent-path", base_url()))
        .send()
        .await
        .expect("Request should complete");

    assert_eq!(resp.status(), 404);

    // Error response should use standard envelope
    let json: serde_json::Value = resp.json().await.expect("Should parse JSON error");
    assert!(json["error"].is_object());
    assert!(json["error"]["code"].as_str().is_some());
    assert!(json["error"]["message"].as_str().is_some());
    assert!(json["error"]["request_id"].as_str().is_some());
}

#[tokio::test]
async fn test_server_creation_with_loopback() {
    // This test doesn't require a running daemon
    use pekobot::api::{state::DaemonConfigSnapshot, ApiServer, ServerConfig};

    let config = ServerConfig {
        host: "127.0.0.1".to_string(),
        port: 0, // Let OS assign port
        workspace_path: std::path::PathBuf::from("/tmp/test"),
        daemon_config: DaemonConfigSnapshot::default(),
    };

    let server = ApiServer::new(config);
    assert_eq!(server.address(), "127.0.0.1:0");
}
