//! JWT + test-user helpers for talking to the PekoHub fixture server.

#![allow(dead_code)]

/// Must match `JWT_SECRET` in the fixture server's env
/// (set in `tests/docker/docker-compose.integration.yml`).
pub const PEKOHUB_JWT_SECRET: &str = "test-secret-key-that-is-32-chars-long!!";

/// Mint an HS256 JWT signed with the fixture's `JWT_SECRET`.
pub fn generate_jwt(user_id: i64, namespace: &str) -> String {
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        namespace: String,
        iat: u64,
    }

    let claims = Claims {
        sub: user_id.to_string(),
        namespace: namespace.to_string(),
        iat: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(PEKOHUB_JWT_SECRET.as_bytes()),
    )
    .unwrap()
}

/// Insert a real user row into the fixture's PGlite DB via `/test/create-user`.
/// Needed because PekoHub enforces namespace ownership on pushes.
///
/// The fixture's error handler returns `{ error: error.message }` with no
/// `message` field, so the status alone is opaque. We read the body and
/// include it in the panic message so a future failure surfaces the
/// actual SQL/DB error (e.g. missing column, schema mismatch) instead
/// of just "500 Internal Server Error".
pub async fn create_test_user(client: &reqwest::Client, base_url: &str, namespace: &str) {
    let resp = client
        .post(format!("{base_url}/test/create-user"))
        .json(&serde_json::json!({
            "namespace": namespace,
            "display_name": format!("Test User ({namespace})"),
            "external_id": format!("test-{namespace}"),
            "provider": "github",
        }))
        .send()
        .await
        .expect("create-user transport failed");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "create-user failed: status={status}, body={body}"
    );
}
