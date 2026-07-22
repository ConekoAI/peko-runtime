//! Re-export shim — Phase 9b.N.5b.5 lifted `synthesize_stream_from_blocking`
//! into `peko_engine::synthetic_stream` so the helper lives next to its
//! consumer (the agentic loop, soon to arrive in `peko-engine`).
//!
//! Existing call sites that imported via
//! `crate::providers::synthetic_stream::...` keep working through this
//! one-line re-export. New code should import the helper directly from
//! `peko_engine::synthetic_stream::synthesize_stream_from_blocking`.

pub use peko_engine::synthetic_stream::synthesize_stream_from_blocking;
