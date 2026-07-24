//! `AsyncInboxLike` — engine-side re-export of the trait port defined
//! in `peko_extension_api::async_inbox`.
//!
//! Phase 7 promoted this trait + the `AsyncInboxItem` enum into the
//! `peko-extension-api` crate so that `peko-session` (which owns the
//! daemon-global `InboxRegistry`) can hold
//! `Arc<dyn AsyncInboxLike>` without importing either `peko-engine`
//! or `peko-extension-host`. Engine callers continue to use
//! `peko_engine::AsyncInboxLike` / `peko_engine::AsyncInboxItem`
//! via this re-export, matching the pre-Phase 7 import paths.

pub use peko_extension_api::{
    AsyncInboxItem, AsyncInboxLike, CompletionEnvelope, SteeringEnvelope,
};
