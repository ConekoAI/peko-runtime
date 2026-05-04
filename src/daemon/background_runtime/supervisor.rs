//! Runtime supervisor — manages the lifecycle of individual runtimes

use crate::common::process::{graceful_shutdown, spawn_process, ProcessSpawnConfig, RestartPolicy, RuntimeSpawnConfig};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Child;


use super::adapter::BackgroundRuntimeAdapter;

/// A supervised background runtime — may be a process, an async task, or an external connection
pub struct ManagedRuntime {
    pub id: String,
    pub kind: RuntimeKind,
    pub state: RuntimeState,
    pub restart_policy: RestartPolicy,
    pub restart_count: u32,
    pub last_error: Option<String>,
    /// The adapter is boxed to allow different types per runtime
    pub adapter: Arc<dyn BackgroundRuntimeAdapter>,
    /// Spawn configuration used to create this runtime (needed for restart)
    pub spawn_config: RuntimeSpawnConfig,
}

impl std::fmt::Debug for ManagedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedRuntime")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("state", &self.state)
            .field("restart_count", &self.restart_count)
            .field("last_error", &self.last_error)
            .field("adapter", &"<dyn BackgroundRuntimeAdapter>")
            .field("spawn_config", &self.spawn_config)
            .finish()
    }
}

/// The concrete runtime implementation
pub enum RuntimeKind {
    /// Child process (MCP server, out-of-process gateway, universal tool)
    Process {
        child: Child,
        pid: u32,
        /// stdin of the child process (for sending data)
        /// Wrapped in Option so adapters can take() ownership during initialize()
        stdin: Option<tokio::process::ChildStdin>,
        /// stdout of the child process (for receiving data)
        /// Wrapped in Option so adapters can take() ownership during initialize()
        stdout: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    },
    /// In-process async task (Rust-native HTTP server, TUI)
    Task {
        handle: tokio::task::JoinHandle<()>,
        abort_tx: Option<tokio::sync::oneshot::Sender<()>>,
    },
    /// External connection the daemon connects to (HTTP webhook, SSE stream)
    External {
        endpoint: String,
        connected: bool,
    },
}

impl std::fmt::Debug for RuntimeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process { pid, .. } => f.debug_struct("Process").field("pid", pid).finish(),
            Self::Task { handle, .. } => f
                .debug_struct("Task")
                .field("finished", &handle.is_finished())
                .finish(),
            Self::External { endpoint, connected } => f
                .debug_struct("External")
                .field("endpoint", endpoint)
                .field("connected", connected)
                .finish(),
        }
    }
}

/// Runtime lifecycle state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeState {
    Starting,
    Running,
    Healthy,
    Unhealthy,
    Crashed,
    Stopping,
    Stopped,
}

impl std::fmt::Display for RuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Running => write!(f, "running"),
            Self::Healthy => write!(f, "healthy"),
            Self::Unhealthy => write!(f, "unhealthy"),
            Self::Crashed => write!(f, "crashed"),
            Self::Stopping => write!(f, "stopping"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Spawn a new process-based runtime
pub async fn spawn_runtime_process(
    id: &str,
    config: &ProcessSpawnConfig,
    adapter: Arc<dyn BackgroundRuntimeAdapter>,
    restart_policy: RestartPolicy,
    spawn_config: RuntimeSpawnConfig,
) -> anyhow::Result<ManagedRuntime> {
    let (child, stdin, stdout, pid) = spawn_process(config).await?;

    Ok(ManagedRuntime {
        id: id.to_string(),
        kind: RuntimeKind::Process { child, pid, stdin: Some(stdin), stdout: Some(stdout) },
        state: RuntimeState::Starting,
        restart_policy,
        restart_count: 0,
        last_error: None,
        adapter,
        spawn_config,
    })
}

/// Spawn a new task-based runtime
pub fn spawn_runtime_task(
    id: &str,
    task: tokio::task::JoinHandle<()>,
    abort_tx: tokio::sync::oneshot::Sender<()>,
    adapter: Arc<dyn BackgroundRuntimeAdapter>,
    restart_policy: RestartPolicy,
    spawn_config: RuntimeSpawnConfig,
) -> ManagedRuntime {
    ManagedRuntime {
        id: id.to_string(),
        kind: RuntimeKind::Task {
            handle: task,
            abort_tx: Some(abort_tx),
        },
        state: RuntimeState::Starting,
        restart_policy,
        restart_count: 0,
        last_error: None,
        adapter,
        spawn_config,
    }
}

/// Spawn an external connection runtime
pub fn spawn_runtime_external(
    id: &str,
    endpoint: String,
    adapter: Arc<dyn BackgroundRuntimeAdapter>,
    restart_policy: RestartPolicy,
    spawn_config: RuntimeSpawnConfig,
) -> ManagedRuntime {
    ManagedRuntime {
        id: id.to_string(),
        kind: RuntimeKind::External {
            endpoint,
            connected: false,
        },
        state: RuntimeState::Starting,
        restart_policy,
        restart_count: 0,
        last_error: None,
        adapter,
        spawn_config,
    }
}

/// Stop a runtime gracefully
pub async fn stop_runtime(runtime: &mut ManagedRuntime) -> anyhow::Result<()> {
    runtime.state = RuntimeState::Stopping;

    // Let the adapter do domain-specific cleanup first
    let adapter = runtime.adapter.clone();
    adapter.shutdown(runtime).await?;

    // Then handle kind-specific termination
    // We replace the kind with a dummy value to take ownership
    let kind = std::mem::replace(&mut runtime.kind, RuntimeKind::External { endpoint: String::new(), connected: false });
    match kind {
        RuntimeKind::Process { child, pid, .. } => {
            // stdin/stdout may have been taken() by the adapter; that's fine
            // because the adapter's transport owns them and will close them
            // when the adapter's shutdown() is called (which happens above).
            let kill_timeout = Duration::from_secs(5);
            graceful_shutdown(child, kill_timeout, pid).await?;
        }
        RuntimeKind::Task { handle, abort_tx } => {
            if let Some(tx) = abort_tx {
                let _ = tx.send(());
            }
            handle.abort();
        }
        RuntimeKind::External { .. } => {
            // Nothing to do
        }
    }

    runtime.state = RuntimeState::Stopped;
    Ok(())
}

/// Check if a runtime is still alive
pub fn is_runtime_alive(runtime: &ManagedRuntime) -> bool {
    match &runtime.kind {
        RuntimeKind::Process { child, .. } => child.id().is_some(),
        RuntimeKind::Task { handle, .. } => !handle.is_finished(),
        RuntimeKind::External { connected, .. } => *connected,
    }
}
