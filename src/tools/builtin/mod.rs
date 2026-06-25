//! Built-in tool implementations
//!
//! This module contains all built-in tools that ship with Peko:
//! - `fs`: Filesystem tools (Read, Write, glob, grep, Edit)
//! - `messaging`: Messaging tools (agent_spawn, a2a_send, message_tool)
//! - `bash`: Shell execution (`Bash`)
//! - `cron`: Scheduled jobs (shared helpers)
//! - `cron_create`: `CronCreate` tool
//! - `cron_delete`: `CronDelete` tool
//! - `cron_list`: `CronList` tool
//! - `session`: Session introspection
//! - `task_management`: Async task management

pub mod bash;
pub mod cron;
pub mod cron_create;
pub mod cron_delete;
pub mod cron_list;
pub mod fs;
pub mod messaging;
pub mod session;
pub mod task_management;

pub use bash::BashTool;
pub use cron_create::CronCreateTool;
pub use cron_delete::CronDeleteTool;
pub use cron_list::CronListTool;
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{AgentSpawnTool, ChannelType, MessageConfig, MessageResult, MessageTool};
pub use session::{
    SessionCache, SessionInfo, SessionIntrospector,
    SessionRegistry as SessionIntrospectionRegistry, SessionTool,
};
pub use task_management::TaskTool;
