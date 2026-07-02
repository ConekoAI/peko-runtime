//! Built-in tool implementations
//!
//! This module contains all built-in tools that ship with Peko:
//! - `fs`: Filesystem tools (Read, Write, glob, grep, Edit)
//! - `messaging`: Messaging tools (Agent, a2a_send)
//! - `bash`: Shell execution (`Bash`)
//! - `cron`: Scheduled jobs (shared helpers)
//! - `cron_create`: `CronCreate` tool
//! - `cron_delete`: `CronDelete` tool
//! - `cron_list`: `CronList` tool
//! - `async_*`: Async task control family (AsyncSpawn, AsyncOutput, AsyncStop,
//!   AsyncStatus, AsyncList)
//! - `task_*`: Planning todo family (TaskCreate, TaskGet, TaskList, TaskUpdate)
//! - `session`: Session introspection

pub mod async_common;
pub mod async_list;
pub mod async_output;
pub mod async_spawn;
pub mod async_status;
pub mod async_stop;
pub mod bash;
pub mod cron;
pub mod cron_create;
pub mod cron_delete;
pub mod cron_list;
pub mod fs;
pub mod messaging;
pub mod session;
pub mod agent_catalog;
pub mod principal_memory;
pub mod principal_sessions;

pub use agent_catalog::AgentCatalogTool;
pub use principal_memory::PrincipalMemoryTool;
pub use principal_sessions::PrincipalSessionsTool;
pub mod task_common;
pub mod task_create;
pub mod task_get;
pub mod task_list;
pub mod task_update;

pub use async_list::AsyncListTool;
pub use async_output::AsyncOutputTool;
pub use async_spawn::AsyncSpawnTool;
pub use async_status::AsyncStatusTool;
pub use async_stop::AsyncStopTool;
pub use bash::BashTool;
pub use cron_create::CronCreateTool;
pub use cron_delete::CronDeleteTool;
pub use cron_list::CronListTool;
pub use fs::{EditTool, GlobTool, GrepTool, ReadTool, WriteTool};
pub use messaging::{AgentTool, DynamicSessionKeyProvider};
pub use session::{
    SessionCache, SessionInfo, SessionIntrospector,
    SessionRegistry as SessionIntrospectionRegistry, SessionTool,
};
pub use task_create::TaskCreateTool;
pub use task_get::TaskGetTool;
pub use task_list::TaskListTool;
pub use task_update::TaskUpdateTool;
