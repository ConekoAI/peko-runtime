//! `peko_channel_read` — read events from a channel the calling principal
//! is a member of.
//!
//! This is the agentic-loop entry point for PR-4a. The principal's
//! agentic loop calls the tool on demand; the audit ring buffer
//! (PR-3c) observes every event regardless of whether the tool fires.
//! No daemon-side cross-principal reach — the principal invokes the
//! tool itself, so the boundary model stays intact.
//!
//! ## Implementation
//!
//! Thin wrapper around [`peko_channel::ChannelPort::peek`]. Takes
//! `channel: String` (parsed via [`ChannelId::parse`]) plus optional
//! `since: String` (opaque `Checkpoint` cursor) + optional
//! `limit: usize` (post-fetch slice; the port returns everything ≥
//! `since` and we trim). Returns the events as a JSON array.
//!
//! The capability gate is the standard `tool:ChannelRead` grant that
//! the principal's capability set already enforces through the F37
//! funnel — this tool itself does not check capabilities, the gate
//! sits at execute-time on the caller's side.

pub mod channel_read;
pub use channel_read::ChannelReadTool;