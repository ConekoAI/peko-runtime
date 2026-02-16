//! DID Identity management

pub mod did;
pub mod keys;
pub mod storage;
pub mod resolver;

pub use did::{Identity, DIDDocument, DIDScope, ParsedDID, VerificationMethod, Service};
pub use keys::{KeyPair, KeyPairExport, PublicKey};
pub use storage::KeyStorage;
pub use resolver::{DidResolver, resolve_local_sync, verify_signature};