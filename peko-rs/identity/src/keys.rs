//! ed25519 Key management

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use rand::RngCore;

/// Key pair for signing
#[derive(Debug, Clone)]
pub struct KeyPair {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl KeyPair {
    /// Generate a new random keypair
    #[must_use]
    pub fn generate() -> Self {
        // Generate random bytes for secret key
        let mut secret_key_bytes = [0u8; 32];
        OsRng.fill_bytes(&mut secret_key_bytes);

        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    /// Sign a message
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing_key.sign(message)
    }

    /// Verify a signature
    pub fn verify(&self, message: &[u8], signature: &Signature) -> Result<()> {
        self.verifying_key
            .verify(message, signature)
            .map_err(|e| anyhow::anyhow!("Signature verification failed: {e}"))
    }

    /// Get public key as bytes
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.verifying_key.to_bytes()
    }

    /// Get private key as bytes
    #[must_use]
    pub fn private_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Export keypair to base64-encoded strings
    #[must_use]
    pub fn export(&self) -> KeyPairExport {
        KeyPairExport {
            public_key: BASE64.encode(self.public_key_bytes()),
            private_key: BASE64.encode(self.private_key_bytes()),
        }
    }

    /// Import keypair from base64-encoded strings
    pub fn import(export: &KeyPairExport) -> Result<Self> {
        let private_key_bytes = BASE64.decode(&export.private_key)?;
        let public_key_bytes = BASE64.decode(&export.public_key)?;

        if private_key_bytes.len() != 32 || public_key_bytes.len() != 32 {
            anyhow::bail!("Invalid key length");
        }

        let mut sk_bytes = [0u8; 32];
        sk_bytes.copy_from_slice(&private_key_bytes);

        let signing_key = SigningKey::from_bytes(&sk_bytes);
        let verifying_key = signing_key.verifying_key();

        // Verify the public key matches
        if verifying_key.to_bytes() != public_key_bytes.as_slice() {
            anyhow::bail!("Public key does not match private key");
        }

        Ok(Self {
            signing_key,
            verifying_key,
        })
    }
}

/// Exported keypair representation
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeyPairExport {
    pub public_key: String,
    pub private_key: String,
}

/// Standalone verification (when you only have public key)
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// Create from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            anyhow::bail!("Invalid public key length");
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(bytes);
        Ok(Self(VerifyingKey::from_bytes(&arr)?))
    }

    /// Verify a signature
    pub fn verify(&self, message: &[u8], signature_bytes: &[u8; 64]) -> Result<()> {
        let signature = Signature::from_bytes(signature_bytes);
        self.0
            .verify(message, &signature)
            .map_err(|e| anyhow::anyhow!("Signature verification failed: {e}"))
    }

    /// Get as bytes
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generate() {
        let kp = KeyPair::generate();
        assert_eq!(kp.public_key_bytes().len(), 32);
        assert_eq!(kp.private_key_bytes().len(), 32);
    }

    #[test]
    fn test_sign_and_verify() {
        let kp = KeyPair::generate();
        let message = b"Hello, Peko!";

        let signature = kp.sign(message);
        assert!(kp.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_verify_wrong_message() {
        let kp = KeyPair::generate();
        let message1 = b"Hello, Peko!";
        let message2 = b"Goodbye, Peko!";

        let signature = kp.sign(message1);
        assert!(kp.verify(message2, &signature).is_err());
    }

    #[test]
    fn test_import_export() {
        let kp = KeyPair::generate();
        let export = kp.export();

        let imported = KeyPair::import(&export).unwrap();

        // Test that imported key can still sign/verify
        let message = b"Test message";
        let signature = imported.sign(message);
        assert!(imported.verify(message, &signature).is_ok());

        // Original key should also verify
        assert!(kp.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_public_key_standalone() {
        let kp = KeyPair::generate();
        let message = b"Standalone verification";
        let signature = kp.sign(message);

        let pk = PublicKey::from_bytes(&kp.public_key_bytes()).unwrap();
        assert!(pk.verify(message, &signature.to_bytes()).is_ok());
    }
}
