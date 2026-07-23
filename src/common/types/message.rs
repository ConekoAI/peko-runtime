//! Compatibility re-exports for the neutral `peko-message` crate.
//!
//! The message contract (`ContentBlock`, `LlmMessage`, `MessageRole`,
//! `TokenUsage`, `AgentMessage`, `MessageConverter`, `MessageContext`,
//! `SteeringProvider`, `ContextTransformer`, …) is shared by providers,
//! sessions, quota, extensions, and the agentic loop, so it lives in
//! its own crate. Internal consumers keep the historical
//! `crate::common::types::message::...` import paths; downstream crates
//! that grow out of the workspace migration will depend on `peko-message`
//! directly.
//!
//! ---
//! **Cleanup ledger:** This file is a pure re-export shim and will be
//! **deleted in Phase 15** of the post-migration cleanup plan (see
//! `AGENTS.md` §Cleanup phases). After deletion, every internal caller
//! will import `peko_message::*` (or specific items) directly. The
//! historical `peko::common::types::message::*` import path is
//! intentionally broken.
//! ---

pub use peko_message::*;
