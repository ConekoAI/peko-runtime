//! Tunnel Security Integration Tests (Issue #4)
//!
//! Tests that verify the local credential and identity storage layers:
//!   (a) `pekohub.toml` never contains a raw private_key field
//!   (b) Legacy credential files are parsed gracefully (migration may or may not
//!       succeed depending on keychain availability)
//!   (c) Identity files use keychain storage when available, or encrypted-file
//!       fallback when `with_passphrase()` is used
//!   (d) An attacker reading `~/.peko/identity/` and `~/.peko/pekohub.toml`
//!       cannot recover the private key without the OS keychain or passphrase
//!
//! These tests do NOT require a PekoHub backend; they test the local credential
//! and identity storage layers only. They do NOT test the actual tunnel start
//! flow (that is covered by higher-level acceptance tests).

use secrecy::SecretString;

use peko::common::vault::Vault;
use peko::identity::keychain::{EncryptedKeyStorage, KeyStorageRef};
use peko::identity::storage::KeyStorage;
use peko::identity::{DIDScope, Identity};
use peko::tunnel::PekoHubCredential;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_pekohub_credential_does_not_contain_raw_key() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pekohub.toml");

    let cred = PekoHubCredential {
        url: "wss://pekohub.org/v1/tunnel".to_string(),
        runtime_id: "did:key:z6MkTest".to_string(),
        tls: None,
    };

    cred.save_to_file(&path).unwrap();

    let toml_content = std::fs::read_to_string(&path).unwrap();
    assert!(
        !toml_content.contains("private_key"),
        "pekohub.toml must never contain a raw private_key field"
    );
}

#[test]
fn test_pekohub_credential_legacy_file_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pekohub.toml");

    // Write a legacy credential file that contains a raw private key.
    let legacy_toml = r#"
url = "wss://pekohub.org/v1/tunnel"
runtime_id = "did:key:z6MkTest"
private_key = "dGVzdC1rZXk="
"#;
    std::fs::write(&path, legacy_toml).unwrap();

    // The new format no longer supports a raw private_key field. The private
    // key now lives in the encrypted vault.
    let loaded = PekoHubCredential::from_file(&path);
    assert!(
        loaded.is_err(),
        "Legacy credential file with raw private_key must be rejected"
    );
}

#[test]
fn test_identity_storage_uses_encrypted_file_fallback() {
    let temp_dir = tempfile::tempdir().unwrap();
    let passphrase = SecretString::new("test-passphrase".into());
    let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

    let identity = Identity::generate(DIDScope::Public, None).unwrap();
    let did = identity.did.clone();

    storage.store(&identity).unwrap();

    // The identity JSON file should exist
    let identity_path = temp_dir
        .path()
        .join(format!("{}.json", did.replace(':', "_")));
    assert!(identity_path.exists(), "Identity file should exist");

    // The identity file should NOT contain a plaintext private_key
    let json = std::fs::read_to_string(&identity_path).unwrap();
    assert!(
        !json.contains("private_key"),
        "Identity file must not contain plaintext private_key"
    );
    assert!(
        json.contains("key_storage"),
        "Identity file should contain key_storage reference"
    );

    // An encrypted key file should exist (because we used test passphrase fallback)
    let enc_path = temp_dir
        .path()
        .join(format!("{}.enc", did.replace(':', "_")));
    assert!(
        enc_path.exists(),
        "Encrypted key file should exist when using fallback"
    );

    // Loading should work
    let loaded = storage.load(&did).unwrap();
    assert_eq!(loaded.did, did);
    assert!(loaded.keypair.is_some());
}

#[test]
fn test_identity_file_is_useless_without_passphrase() {
    let temp_dir = tempfile::tempdir().unwrap();
    let passphrase = SecretString::new("correct-passphrase".into());
    let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

    let identity = Identity::generate(DIDScope::Public, None).unwrap();
    let did = identity.did.clone();

    storage.store(&identity).unwrap();

    // Try to load with a WRONG passphrase
    let wrong_passphrase = SecretString::new("wrong-passphrase".into());
    let bad_storage =
        KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), wrong_passphrase).unwrap();

    let result = bad_storage.load(&did);
    assert!(result.is_err(), "Loading with wrong passphrase must fail");
}

#[test]
fn test_legacy_identity_auto_migration() {
    let temp_dir = tempfile::tempdir().unwrap();
    let passphrase = SecretString::new("migration-passphrase".into());
    let storage = KeyStorage::with_passphrase(temp_dir.path().to_path_buf(), passphrase).unwrap();

    // Create a legacy plaintext identity file manually
    let identity = Identity::generate(DIDScope::Public, None).unwrap();
    let did = identity.did.clone();
    let keypair = identity.keypair.as_ref().unwrap();
    let export = keypair.export();

    let legacy_json = serde_json::json!({
        "did": did,
        "document": identity.document,
        "private_key": export.private_key,
        "public_key": export.public_key,
        "created_at": identity.document.created,
        "last_used": chrono::Utc::now().to_rfc3339(),
    });

    let file_path = temp_dir
        .path()
        .join(format!("{}.json", did.replace(':', "_")));
    std::fs::write(
        &file_path,
        serde_json::to_string_pretty(&legacy_json).unwrap(),
    )
    .unwrap();

    // Load should auto-migrate
    let loaded = storage.load(&did).unwrap();
    assert_eq!(loaded.did, did);
    assert!(loaded.keypair.is_some());

    // Verify the file was rewritten without plaintext private_key
    let json = std::fs::read_to_string(&file_path).unwrap();
    assert!(
        !json.contains("private_key"),
        "Migrated identity file must not contain plaintext private_key"
    );
}

#[test]
fn test_headless_store_without_passphrase_fails() {
    let temp_dir = tempfile::tempdir().unwrap();
    let storage = KeyStorage::with_path(temp_dir.path().to_path_buf()).unwrap();
    let result = storage.generate_identity(DIDScope::Local, Some("test"));
    // If keychain is unavailable, this should fail with a clear error
    if let Err(e) = result {
        let err = format!("{e}");
        assert!(
            err.contains("keychain") || err.contains("passphrase"),
            "Expected keychain/passphrase error, got: {err}"
        );
    }
    // If keychain IS available (local dev), the test passes trivially
}

#[test]
fn test_keychain_storage_ref_serialization() {
    let keychain_ref = KeyStorageRef::Keychain {
        service: "peko-runtime".to_string(),
        account: "did:key:z6MkTest".to_string(),
    };

    let json = serde_json::to_string(&keychain_ref).unwrap();
    let deserialized: KeyStorageRef = serde_json::from_str(&json).unwrap();

    match deserialized {
        KeyStorageRef::Keychain { service, account } => {
            assert_eq!(service, "peko-runtime");
            assert_eq!(account, "did:key:z6MkTest");
        }
        other => panic!("Expected Keychain, got: {:?}", other),
    }
}

#[test]
fn test_encrypted_key_storage_roundtrip() {
    let temp_dir = tempfile::tempdir().unwrap();
    let file_path = temp_dir.path().join("test_key.enc");
    let private_key = "dGVzdC1wcml2YXRlLWtleQ==";
    let passphrase = SecretString::new("super-secret-passphrase".into());

    let storage_ref = EncryptedKeyStorage::store_key(&file_path, private_key, &passphrase).unwrap();

    match &storage_ref {
        KeyStorageRef::EncryptedFile { file_name } => {
            assert_eq!(file_name, "test_key.enc");
        }
        other => panic!("Expected EncryptedFile, got: {:?}", other),
    }

    let retrieved = EncryptedKeyStorage::retrieve_key(&file_path, &passphrase).unwrap();
    assert_eq!(retrieved, private_key);
}

#[test]
fn test_credential_resolve_private_key_from_vault() {
    let temp = tempfile::tempdir().unwrap();
    let vault = Vault::for_test(temp.path(), "tunnel-security-test");

    let cred = PekoHubCredential {
        url: "wss://example.com".to_string(),
        runtime_id: "did:key:z6MkTest".to_string(),
        tls: None,
    };

    vault
        .set_tunnel_private_key(&cred.runtime_id, "dGVzdC1rZXk=")
        .unwrap();

    let resolved = cred.resolve_private_key(&vault).unwrap();
    assert_eq!(resolved, "dGVzdC1rZXk=");
}

#[test]
fn test_credential_resolve_private_key_missing() {
    let temp = tempfile::tempdir().unwrap();
    let vault = Vault::for_test(temp.path(), "tunnel-security-test");

    let cred = PekoHubCredential {
        url: "wss://example.com".to_string(),
        runtime_id: "did:key:z6MkTest".to_string(),
        tls: None,
    };

    let result = cred.resolve_private_key(&vault);
    assert!(result.is_err());
}
