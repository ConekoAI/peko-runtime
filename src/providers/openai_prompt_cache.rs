//! Re-export shim — Phase 9b.N.5b.5 lifted the pure helper
//! `clamp_openai_prompt_cache_key` into `peko_provider_api::prompt_cache`
//! so the agentic loop can depend on the API crate instead of the
//! concrete `crate::providers` module. Existing call sites that
//! imported via `crate::providers::openai_prompt_cache::...` keep
//! working through this one-line re-export.

pub use peko_provider_api::clamp_openai_prompt_cache_key;
