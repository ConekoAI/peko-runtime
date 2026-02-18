//! Pekobot - Lightweight Multi-Agent Runtime
//!
//! A Rust-based agent runtime that supports local multi-agent orchestration
//! and optional connection to the Coneko network.
//!
//! ## Quick Start
//!
//! ```bash
//! pekobot agent                    # Run single agent
//! pekobot orchestrate              # Run multi-agent orchestrator
//! pekobot --help                   # See all options
//! ```

#![warn(clippy::all, clippy::pedantic)]

pub mod agent;
pub mod a2a;
pub mod capability_registry;
pub mod channels;
pub mod tool_registry;
pub mod config;
pub mod coneko;
pub mod cron;
pub mod daemon;
pub mod heartbeat;
pub mod identity;
pub mod memory;
pub mod providers;
pub mod security;
pub mod skills;
pub mod tools;
pub mod tunnel;
pub mod types;

pub use agent::Agent;
pub use config::Config;

/// Pekobot version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
