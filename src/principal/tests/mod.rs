//! Same-runtime offline `principal_send` integration test (gated, opt-in via
//! `--features test-utils`). Originally `tests/principal_send_offline.rs`;
//! moved inline as part of F9 so `peko::daemon::AppState` exposure is no
//! longer required just to host this test.

#[cfg(all(test, feature = "test-utils"))]
mod principal_send_offline;
