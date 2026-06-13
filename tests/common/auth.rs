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
/// The namespace is auto-suffixed with a process-wide counter so
/// parallel tests in the same binary don't collide on the
/// `users_external_id_key` / `users_namespace_key` unique
/// constraints. The `namespace` argument is preserved as a prefix
/// for readable test logs / database inspection. The function
/// returns the actual namespace that was inserted so the caller
/// can use it in pekohub push URLs (which must match the user's
/// namespace exactly — see
/// `backend/src/routes/oci/manifests.ts:172`).
///
/// The fixture's error handler returns `{ error: error.message }`
/// with no `message` field, so the status alone is opaque. We
/// read the body and include it in the panic message so a
/// future failure surfaces the actual SQL/DB error (e.g. missing
/// column, schema mismatch) instead of just "500 Internal Server
/// Error".
pub async fn create_test_user(
    client: &reqwest::Client,
    base_url: &str,
    namespace: &str,
) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let unique = format!("{namespace}_{pid}_{seq}");

    let resp = client
        .post(format!("{base_url}/test/create-user"))
        .json(&serde_json::json!({
            "namespace": unique,
            "display_name": format!("Test User ({namespace})"),
            "external_id": format!("test-{unique}"),
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
    unique
}
