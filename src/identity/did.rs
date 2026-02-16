//! DID creation and validation

use tracing::info;

/// DID Identity
#[derive(Debug, Clone)]
pub struct Identity {
    pub did: String,
    pub public_key: String,
}

impl Identity {
    pub fn generate() -> Self {
        info!("Generating new DID identity");
        // TODO: Generate ed25519 keypair
        Self {
            did: format!("did:pekobot:local:{}", uuid::Uuid::new_v4()),
            public_key: "placeholder".to_string(),
        }
    }
}
