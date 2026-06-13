//! Shared test harness used by all `tests/*.rs` integration crates.
//!
//! Cargo special-cases `tests/common/` — files in this directory are NOT
//! compiled as their own integration test binaries. Each test crate brings
//! the harness in with `mod common;` and `use common::…;`.
//!
//! Because cargo compiles the whole `common` module once per test crate
//! (whether the crate uses everything or not), each submodule needs
//! `#![allow(dead_code)]` to avoid spurious unused-fn warnings.

#![allow(dead_code, unused_imports)]

pub mod agent;
pub mod auth;
pub mod cli;
pub mod crypto;
pub mod daemon;
pub mod harness;
pub mod subprocess;

pub use agent::write_mock_agent;
pub use auth::{create_test_user, generate_jwt, PEKOHUB_JWT_SECRET};
pub use cli::PekoCli;
pub use crypto::{generate_runtime_identity, sign_nonce};
pub use daemon::DaemonGuard;
pub use harness::{reset_pekohub, PekohubBackend};
pub use subprocess::{run_with_timeout, try_run_with_timeout};
