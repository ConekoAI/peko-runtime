//! Credential setup for CLI unit tests. The core dependency is compiled
//! without cfg(test), so its test-only vault fallback does not apply here.

pub const MASTER_PASSPHRASE: &str = "test-cli-passphrase-do-not-use-in-production";

pub fn init_credentials() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var(
            peko_core::common::vault::MASTER_PASSPHRASE_ENV,
            MASTER_PASSPHRASE,
        );
        // A passphrase alone still leaves production vault creation in
        // auto mode. Assert passphrase mode to bypass all keychain probes.
        std::env::set_var(peko_core::common::vault::UNLOCK_METHOD_ENV, "passphrase");
        peko_identity::init_test_env();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_core::common::vault::{UnlockMethod, Vault};
    use secrecy::SecretString;

    #[test]
    fn default_vault_load_uses_only_the_test_master_passphrase() {
        init_credentials();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("vault.enc");
        let vault = Vault::load(&path).unwrap();
        assert_eq!(vault.unlock_method(), UnlockMethod::Passphrase);
        drop(vault);
        let reopened =
            Vault::load_with_passphrase(&path, &SecretString::new(MASTER_PASSPHRASE.into()))
                .unwrap();
        assert_eq!(reopened.unlock_method(), UnlockMethod::Passphrase);
        assert_eq!(
            Vault::load(&path).unwrap().unlock_method(),
            UnlockMethod::Passphrase
        );
    }
}
