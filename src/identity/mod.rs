//! DID Identity management

pub mod crypto;
pub mod did;
pub mod keychain;
pub mod keys;
pub mod resolver;
pub mod runtime;
pub mod runtime_metadata;
pub mod storage;

pub use did::{DIDDocument, DIDScope, Identity, ParsedDID, Service, VerificationMethod};
pub use keychain::{EncryptedKeyStorage, KeyStorageRef, KeychainStorage};
pub use keys::{KeyPair, KeyPairExport, PublicKey};
pub use resolver::{resolve_local_sync, verify_signature, DidResolver};
pub use storage::KeyStorage;

/// Test-only environment setup: force the encrypted-file identity fallback
/// for every test in this process.
///
/// ## Why this exists
/// `KeyStorage::new()` and `KeyStorage::with_path()` ([storage.rs:64-82])
/// consult the `PEKO_IDENTITY_PASSPHRASE` env var. When unset, `store()`
/// falls through to the OS keychain via the `keyring` crate, and on
/// Windows-headless (non-interactive sessions, Server Core, CI runners
/// without Credential Manager) the `keyring` v2.3.3 FFI calls can panic
/// inside `CredWriteW` instead of returning a `Result::Err`. That panic
/// surfaces as a hung test process.
///
/// CI works around this by exporting `PEKO_IDENTITY_PASSPHRASE` in
/// `.github/workflows/integration.yml:35`; local dev shells on Windows
/// usually don't, which is why the 13 `agent::agent` / `engine::agentic_loop`
/// tests fail there with keychain panics. Calling `init_test_env()` from
/// the first line of each affected test ensures the encrypted-file path
/// is taken on every platform, with no production-code change.
///
/// ## Why `Once` and not a `ctor` / build script
/// `set_var` is safe to call under the 2021 edition (it's only `unsafe` in
/// the 2024 edition). The `Once` guarantees the env var is set exactly
/// once even though the helper is called from many test functions across
/// the process, and the existing `#[serial_test::serial(core)]` attributes
/// on the failing tests prevent concurrent invocations during the
/// (very brief) write window.
#[cfg(test)]
pub fn init_test_env() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        std::env::set_var(
            "PEKO_IDENTITY_PASSPHRASE",
            "test-passphrase-do-not-use-in-production",
        );
    });
}
