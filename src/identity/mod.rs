//! DID Identity management

pub mod did;
pub mod keys;
pub mod resolver;
pub mod storage;

pub use did::{DIDDocument, DIDScope, Identity, ParsedDID, Service, VerificationMethod};
pub use keys::{KeyPair, KeyPairExport, PublicKey};
pub use resolver::{resolve_local_sync, verify_signature, DidResolver};
pub use storage::KeyStorage;
