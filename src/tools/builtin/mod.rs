//! Built-in tool implementations
//!
//! This module contains all built-in tools that ship with Peko:
//! - `fs`: Filesystem tools (Read, Write, glob, grep, Edit)
//! - `messaging`: Messaging tools (agent_spawn, a2a_send, message_tool)
//! - `shell`: Shell execution
//! - `cron`: Scheduled jobs
//! - `session`: Session introspection
//! - `task_management`: Async task management

pub mod cron;
pub mod fs;
pub mod messaging;
pub mod session;
pub mod shell;
pub mod task_management;

pub use cron::CronTool;
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{AgentSpawnTool, ChannelType, MessageConfig, MessageResult, MessageTool};
pub use session::{
    SessionCache, SessionInfo, SessionIntrospector,
    SessionRegistry as SessionIntrospectionRegistry, SessionTool,
};
pub use shell::ShellTool;
pub use task_management::TaskTool;
