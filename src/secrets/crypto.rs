//! Encryption utilities for secret manager
//!
//! Uses AES-256-GCM for symmetric encryption with Argon2id for key derivation.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::{Argon2, Params};
use hkdf::Hkdf;
use rand::RngCore;
use secrecy::{ExposeSecret, SecretBox, SecretString};
use sha2::Sha256;

/// Encrypted secret data
#[derive(Debug, Clone)]
pub struct EncryptedSecret {
    /// Salt for key derivation
    pub salt: Vec<u8>,
    /// Nonce for AES-GCM
    pub nonce: Vec<u8>,
    /// Ciphertext
    pub ciphertext: Vec<u8>,
}

/// Default Argon2 parameters (secure but reasonable)
pub const DEFAULT_MEMORY_COST: u32 = 65536; // 64 MB
pub const DEFAULT_TIME_COST: u32 = 3;
pub const DEFAULT_PARALLELISM: u32 = 4;
pub const SALT_LENGTH: usize = 32;
pub const NONCE_LENGTH: usize = 12;

/// Master key storage
pub struct MasterKey {
    key: SecretBox<[u8; 32]>,
}

impl MasterKey {
    /// Create a new master key from a password using Argon2id
    pub fn from_password(
        password: &str,
        salt: &[u8],
    ) -> anyhow::Result<Self> {
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(DEFAULT_MEMORY_COST, DEFAULT_TIME_COST, DEFAULT_PARALLELISM, None)
                .map_err(|e| anyhow::anyhow!("Invalid Argon2 params: {}", e))?,
        );

        let mut key = [0u8; 32];
        argon2
            .hash_password_into(password.as_bytes(), salt, &mut key)
            .map_err(|e| anyhow::anyhow!("Argon2 hashing failed: {:?}", e))?;

        Ok(Self {
            key: SecretBox::new(Box::new(key)),
        })
    }

    /// Derive a per-secret key from the master key
    fn derive_secret_key(&self,
        secret_name: &str,
        scope: &str,
    ) -> anyhow::Result<[u8; 32]> {
        let hkdf = Hkdf::<Sha256>::new(None, self.key.expose_secret().as_slice());
        let info = format!("{}:{}", scope, secret_name);
        
        let mut secret_key = [0u8; 32];
        hkdf.expand(info.as_bytes(), &mut secret_key)
            .map_err(|e| anyhow::anyhow!("HKDF expand failed: {}", e))?;
        
        Ok(secret_key)
    }

    /// Encrypt a secret value
    pub fn encrypt(
        &self,
        plaintext: &str,
        secret_name: &str,
        scope: &str,
    ) -> anyhow::Result<EncryptedSecret> {
        // Derive per-secret key
        let secret_key = self.derive_secret_key(secret_name, scope)?;
        let key = Key::<Aes256Gcm>::from_slice(&secret_key);

        // Generate random nonce
        let mut nonce_bytes = [0u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt
        let cipher = Aes256Gcm::new(key);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("Encryption failed: {:?}", e))?;

        // Generate random salt for this encryption
        let mut salt = vec![0u8; SALT_LENGTH];
        OsRng.fill_bytes(&mut salt);

        Ok(EncryptedSecret {
            salt,
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        })
    }

    /// Decrypt a secret value
    pub fn decrypt(
        &self,
        encrypted: &EncryptedSecret,
        secret_name: &str,
        scope: &str,
    ) -> anyhow::Result<SecretString> {
        // Derive per-secret key
        let secret_key = self.derive_secret_key(secret_name, scope)?;
        let key = Key::<Aes256Gcm>::from_slice(&secret_key);

        // Decrypt
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&encrypted.nonce);

        let plaintext = cipher
            .decrypt(nonce, encrypted.ciphertext.as_ref())
            .map_err(|e| anyhow::anyhow!("Decryption failed (wrong password?): {:?}", e))?;

        Ok(SecretString::from(String::from_utf8(plaintext)?))
    }
}

/// Generate a random master key (for OS keychain storage)
pub fn generate_random_master_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() {
        let salt = vec![0u8; SALT_LENGTH];
        let master_key = MasterKey::from_password("test-password", &salt).unwrap();

        let plaintext = "my-secret-api-key";
        let encrypted = master_key
            .encrypt(plaintext, "OPENAI_KEY", "global")
            .unwrap();

        assert!(!encrypted.ciphertext.is_empty());
        assert_ne!(encrypted.ciphertext, plaintext.as_bytes());

        let decrypted = master_key
            .decrypt(&encrypted, "OPENAI_KEY", "global")
            .unwrap();

        assert_eq!(decrypted.expose_secret(), plaintext);
    }

    #[test]
    fn test_wrong_password() {
        let salt = vec![0u8; SALT_LENGTH];
        let master_key = MasterKey::from_password("correct-password", &salt).unwrap();

        let encrypted = master_key
            .encrypt("secret", "KEY", "global")
            .unwrap();

        // Try to decrypt with wrong password
        let wrong_key = MasterKey::from_password("wrong-password", &salt).unwrap();
        let result = wrong_key.decrypt(&encrypted, "KEY", "global");
        assert!(result.is_err());
    }

    #[test]
    fn test_different_scopes_different_keys() {
        let salt = vec![0u8; SALT_LENGTH];
        let master_key = MasterKey::from_password("password", &salt).unwrap();

        let plaintext = "secret";
        
        // Encrypt with global scope
        let encrypted_global = master_key
            .encrypt(plaintext, "KEY", "global")
            .unwrap();

        // Encrypt with agent scope
        let encrypted_agent = master_key
            .encrypt(plaintext, "KEY", "agent:did:123")
            .unwrap();

        // Should produce different ciphertexts due to different derived keys
        assert_ne!(encrypted_global.ciphertext, encrypted_agent.ciphertext);

        // Both should decrypt correctly with their respective scopes
        let decrypted_global = master_key
            .decrypt(&encrypted_global, "KEY", "global")
            .unwrap();
        assert_eq!(decrypted_global.expose_secret(), plaintext);

        let decrypted_agent = master_key
            .decrypt(&encrypted_agent, "KEY", "agent:did:123")
            .unwrap();
        assert_eq!(decrypted_agent.expose_secret(), plaintext);
    }
}
