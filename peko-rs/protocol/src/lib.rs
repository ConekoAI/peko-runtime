//! `peko-protocol` — IPC + tunnel wire-shape contracts.
//!
//! Phase 11a ships the leaf-level pieces:
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`ipc`] | Auth envelope: `AuthCredential`, `PrincipalSendControlMode`, plus the shared packet/timeout constants. |
//!
//! Future Phase 11 commits lift the bulk of `src/ipc/packet.rs`
//! (`RequestPacket` / `ResponsePacket` — 5000+ lines) and
//! `src/tunnel/protocol.rs` (`TunnelMessage` + instance payloads)
//! once the wider Phase 11 boundary makes the extraction
//! worthwhile.
//!
//! Rule: this crate depends only on `serde` + `serde_json`. It is
//! the only place where CLI and daemon can meet on the wire —
//! neither side may grow its own private variant of the protocol
//! types.

pub mod ipc;
