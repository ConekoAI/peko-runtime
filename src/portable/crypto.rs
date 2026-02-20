//! Encryption utilities for portable agent packages
//!
//! Uses AES-256-GCM for symmetric encryption with Argon2id for key derivation.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::Argon2;
use rand::RngCore;

/// Encryption result containing ciphertext and metadata
#[derive(Debug, Clone)]
pub struct EncryptedData {
    /// Salt for key derivation
    pub salt: Vec<u8>,
    /// Nonce for AES-GCM
    pub nonce: Vec<u8>,
    /// Ciphertext
    pub ciphertext: Vec<u8>,
    /// Argon2 memory cost (KB)
    pub memory_cost: u32,
    /// Argon2 time cost (iterations)
    pub time_cost: u32,
    /// Argon2 parallelism
    pub parallelism: u32,
}

/// Default Argon2 parameters (secure but reasonable)
pub const DEFAULT_MEMORY_COST: u32 = 65536; // 64 MB
pub const DEFAULT_TIME_COST: u32 = 3;
pub const DEFAULT_PARALLELISM: u32 = 4;
pub const SALT_LENGTH: usize = 32;
pub const NONCE_LENGTH: usize = 12;

/// Derive encryption key from passphrase using Argon2id
pub fn derive_key(
    passphrase: &str,
    salt: &[u8],
    memory_cost: u32,
    time_cost: u32,
    parallelism: u32,
) -> anyhow::Result<Vec<u8>> {
    let argon2 = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(memory_cost, time_cost, parallelism, None)
            .map_err(|e| anyhow::anyhow!("Invalid Argon2 params: {}", e))?,
    );

    let mut key = vec![0u8; 32]; // 256 bits for AES-256
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow::anyhow!("Argon2 hashing failed: {:?}", e))?;

    Ok(key)
}

/// Encrypt data with a passphrase
///
/// Returns encrypted data with salt and nonce for decryption
pub fn encrypt_with_passphrase(
    plaintext: &[u8],
    passphrase: &str,
) -> anyhow::Result<EncryptedData> {
    encrypt_with_params(
        plaintext,
        passphrase,
        DEFAULT_MEMORY_COST,
        DEFAULT_TIME_COST,
        DEFAULT_PARALLELISM,
    )
}

/// Encrypt data with custom Argon2 parameters
pub fn encrypt_with_params(
    plaintext: &[u8],
    passphrase: &str,
    memory_cost: u32,
    time_cost: u32,
    parallelism: u32,
) -> anyhow::Result<EncryptedData> {
    // Generate random salt
    let mut salt = vec![0u8; SALT_LENGTH];
    OsRng.fill_bytes(&mut salt);

    // Derive key
    let key_bytes = derive_key(passphrase, &salt, memory_cost, time_cost, parallelism)?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);

    // Generate random nonce
    let mut nonce_bytes = vec![0u8; NONCE_LENGTH];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    // Encrypt
    let cipher = Aes256Gcm::new(key);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("Encryption failed: {:?}", e))?;

    Ok(EncryptedData {
        salt,
        nonce: nonce_bytes,
        ciphertext,
        memory_cost,
        time_cost,
        parallelism,
    })
}

/// Decrypt data with a passphrase
pub fn decrypt_with_passphrase(
    encrypted: &EncryptedData,
    passphrase: &str,
) -> anyhow::Result<Vec<u8>> {
    // Derive key with stored parameters
    let key_bytes = derive_key(
        passphrase,
        &encrypted.salt,
        encrypted.memory_cost,
        encrypted.time_cost,
        encrypted.parallelism,
    )?;
    let key = Key::<Aes256Gcm>::from_slice(&key_bytes);

    // Decrypt
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&encrypted.nonce);

    let plaintext = cipher
        .decrypt(nonce, encrypted.ciphertext.as_ref())
        .map_err(|e| anyhow::anyhow!("Decryption failed (wrong passphrase?): {:?}", e))?;

    Ok(plaintext)
}

/// Serialize encrypted data to bytes for storage
pub fn serialize_encrypted(data: &EncryptedData) -> Vec<u8> {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct SerializedEncryptedData {
        salt: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        memory_cost: u32,
        time_cost: u32,
        parallelism: u32,
    }

    let serializable = SerializedEncryptedData {
        salt: data.salt.clone(),
        nonce: data.nonce.clone(),
        ciphertext: data.ciphertext.clone(),
        memory_cost: data.memory_cost,
        time_cost: data.time_cost,
        parallelism: data.parallelism,
    };

    serde_json::to_vec(&serializable).expect("Failed to serialize encrypted data")
}

/// Deserialize encrypted data from bytes
pub fn deserialize_encrypted(bytes: &[u8]) -> anyhow::Result<EncryptedData> {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct SerializedEncryptedData {
        salt: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        memory_cost: u32,
        time_cost: u32,
        parallelism: u32,
    }

    let deserialized: SerializedEncryptedData = serde_json::from_slice(bytes)?;

    Ok(EncryptedData {
        salt: deserialized.salt,
        nonce: deserialized.nonce,
        ciphertext: deserialized.ciphertext,
        memory_cost: deserialized.memory_cost,
        time_cost: deserialized.time_cost,
        parallelism: deserialized.parallelism,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt() {
        let plaintext = b"Hello, World!";
        let passphrase = "my secret passphrase";

        let encrypted = encrypt_with_passphrase(plaintext, passphrase).unwrap();
        assert!(!encrypted.ciphertext.is_empty());
        assert_ne!(encrypted.ciphertext, plaintext.to_vec());

        let decrypted = decrypt_with_passphrase(&encrypted, passphrase).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }

    #[test]
    fn test_wrong_passphrase() {
        let plaintext = b"Secret data";
        let encrypted = encrypt_with_passphrase(plaintext, "correct").unwrap();

        let result = decrypt_with_passphrase(&encrypted, "wrong");
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_deserialize() {
        let plaintext = b"Test data";
        let passphrase = "test";

        let encrypted = encrypt_with_passphrase(plaintext, passphrase).unwrap();
        let serialized = serialize_encrypted(&encrypted);
        let deserialized = deserialize_encrypted(&serialized).unwrap();

        let decrypted = decrypt_with_passphrase(&deserialized, passphrase).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }

    #[test]
    fn test_custom_params() {
        let plaintext = b"Custom params test";
        let passphrase = "test";

        let encrypted = encrypt_with_params(
            plaintext, passphrase, 32768, // 32 MB
            2,     // 2 iterations
            2,     // 2 threads
        )
        .unwrap();

        assert_eq!(encrypted.memory_cost, 32768);
        assert_eq!(encrypted.time_cost, 2);
        assert_eq!(encrypted.parallelism, 2);

        let decrypted = decrypt_with_passphrase(&encrypted, passphrase).unwrap();
        assert_eq!(decrypted, plaintext.to_vec());
    }
}
