//! Compatibility re-export for the `peko-provider-api::cache_retention`
//! module.
//!
//! The `CacheRetention` enum (F23 prompt-cache TTL policy) lives in
//! `peko-provider-api::cache_retention` along with its `is_enabled`
//! predicate and unit tests. Internal consumers continue using
//! `crate::providers::cache_retention::CacheRetention` through this
//! shim so existing call sites don't churn.

pub use peko_provider_api::cache_retention::*;
