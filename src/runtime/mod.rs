//! Shared runtime components for agent and daemon
//!
//! This module provides components that can be used by both the agent
//! and the daemon, enabling the daemon to execute tools without
//! instantiating a full agent.

pub mod facade;
pub mod tool_runtime;

pub use facade::RuntimeFacade;
pub use tool_runtime::ToolRuntime;
