//! IPC Server — Daemon-Side UDP/Unix Socket Listener
//!
//! The daemon binds a socket and listens for incoming request packets.
//! Each request is dispatched to the appropriate service, and responses
//! are streamed back to the CLI.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::UnixDatagram;
use tokio::time::interval;
use tracing::{error, info, trace, warn};

use super::packet::{RequestPacket, ResponsePacket, HEARTBEAT_INTERVAL_SECS};
use super::{DEFAULT_HOST, DEFAULT_PORT};
use crate::daemon::state::AppState;

/// Platform-specific server socket (wrapped in Arc for shared ownership)
#[derive(Clone)]
enum ServerSocket {
    #[cfg(unix)]
    Unix {
        socket: Arc<UnixDatagram>,
        path: Arc<std::path::PathBuf>,
    },
    Udp {
        socket: Arc<UdpSocket>,
    },
}

impl ServerSocket {
    /// Receive a packet from the socket
    async fn recv_from(
        &self,
        buf: &mut [u8],
    ) -> std::io::Result<(usize, Option<std::net::SocketAddr>)> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket, .. } => {
                let len = socket.recv(buf).await?;
                Ok((len, None))
            }
            Self::Udp { socket } => {
                let (len, addr) = socket.recv_from(buf).await?;
                Ok((len, Some(addr)))
            }
        }
    }

    /// Send a response back to the client
    async fn send_response(
        &self,
        bytes: &[u8],
        addr: Option<std::net::SocketAddr>,
    ) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix { socket, .. } => {
                // For Unix datagram, the socket is connected to the peer
                // when we receive from them (we use recv_from/send_to semantics).
                // Actually UnixDatagram doesn't have recv_from/send_to in tokio.
                // We use the connected peer approach: after recv, we can send back.
                socket.send(bytes).await?;
            }
            Self::Udp { socket } => {
                if let Some(addr) = addr {
                    socket.send_to(bytes, addr).await?;
                }
            }
        }
        Ok(())
    }
}

/// IPC server that handles CLI requests
pub struct IpcServer {
    socket: ServerSocket,
    app_state: AppState,
}

impl IpcServer {
    /// Create and bind the IPC server
    ///
    /// Tries Unix socket first (on Unix), falls back to UDP.
    ///
    /// # Errors
    /// Returns error if socket binding fails
    pub async fn new(app_state: AppState) -> anyhow::Result<Self> {
        // Try Unix socket on Unix platforms
        #[cfg(unix)]
        {
            let run_dir = ensure_run_dir()?;
            let sock_path = run_dir.join("daemon.sock");

            // Remove stale socket file
            let _ = std::fs::remove_file(&sock_path);

            match UnixDatagram::bind(&sock_path) {
                Ok(socket) => {
                    info!("IPC server bound to Unix socket: {}", sock_path.display());
                    return Ok(Self {
                        socket: ServerSocket::Unix {
                            socket: Arc::new(socket),
                            path: Arc::new(sock_path),
                        },
                        app_state,
                    });
                }
                Err(e) => {
                    warn!("Failed to bind Unix socket ({}), falling back to UDP", e);
                }
            }
        }

        // Fall back to UDP
        let addr = format!("{}:{}", DEFAULT_HOST, DEFAULT_PORT);
        let socket = UdpSocket::bind(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind UDP socket to {}: {}", addr, e))?;

        info!("IPC server bound to UDP: {}", addr);
        Ok(Self {
            socket: ServerSocket::Udp {
                socket: Arc::new(socket),
            },
            app_state,
        })
    }

    /// Run the IPC server loop
    ///
    /// This method runs until the daemon shuts down or the shutdown signal is received.
    pub async fn run(
        &self,
        mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
    ) -> anyhow::Result<()> {
        let mut buf = vec![0u8; 65536];

        info!("IPC server ready, waiting for requests...");

        loop {
            tokio::select! {
                result = self.socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            if len == 0 {
                                continue;
                            }

                            match RequestPacket::from_bytes(&buf[..len]) {
                                Ok(request) => {
                                    trace!("Received request: {:?}", request);
                                    let request_id = request.request_id();

                                    // Spawn a task to handle the request
                                    let state = self.app_state.clone();
                                    let socket = self.socket.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = Self::handle_request(request, state, socket, addr).await {
                                            error!("Error handling request {}: {}", request_id, e);
                                        }
                                    });
                                }
                                Err(e) => {
                                    warn!("Failed to parse request packet: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            error!("Socket receive error: {}", e);
                            // Brief pause to avoid tight error loop
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("IPC server received shutdown signal, stopping...");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single request
    async fn handle_request(
        request: RequestPacket,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        match request {
            RequestPacket::Ping { request_id } => {
                let uptime = state.uptime_seconds();
                let response = ResponsePacket::Pong {
                    request_id,
                    uptime_secs: uptime,
                    version: crate::VERSION.to_string(),
                };
                Self::send_packet(&socket, response, addr).await?;
            }

            RequestPacket::Shutdown { request_id, force } => {
                info!("Shutdown request received via IPC (force={})", force);
                let response = ResponsePacket::ShuttingDown { request_id };
                Self::send_packet(&socket, response, addr).await?;
                state.request_shutdown(force).await;
            }

            RequestPacket::Execute {
                request_id,
                agent,
                team,
                message,
                session_id,
                new_session,
                stream,
                user,
            } => {
                Self::handle_execute(
                    request_id,
                    agent,
                    team,
                    message,
                    session_id,
                    new_session,
                    stream,
                    user,
                    state,
                    socket,
                    addr,
                )
                .await?;
            }

            RequestPacket::AsyncSpawn {
                request_id,
                tool_name,
                params,
                session_key,
                workspace,
            } => {
                Self::handle_async_spawn(
                    request_id,
                    tool_name,
                    params,
                    session_key,
                    workspace,
                    state,
                    socket,
                    addr,
                )
                .await?;
            }

            RequestPacket::AsyncCancel {
                request_id,
                task_id,
            } => {
                Self::handle_async_cancel(request_id, task_id, state, socket, addr).await?;
            }

            RequestPacket::CronList {
                request_id,
                include_disabled,
            } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.list_jobs(include_disabled) {
                        Ok(jobs) => {
                            let response = ResponsePacket::CronList { request_id, jobs };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to list jobs: {e}"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::CronAdd { request_id, job } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.add_job(&job) {
                        Ok(()) => {
                            let response = ResponsePacket::CronAdded {
                                request_id,
                                job_id: job.id,
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to add job: {e}"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::CronRemove { request_id, job_id } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.delete_job(&job_id) {
                        Ok(true) => {
                            let response = ResponsePacket::CronRemoved { request_id, job_id };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Ok(false) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to remove job: {e}"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::CronRun { request_id, job_id } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_job(&job_id) {
                        Ok(Some(_job)) => {
                            let now = chrono::Utc::now();
                            if let Err(e) =
                                scheduler.update_job_after_run(&job_id, "triggered", now)
                            {
                                let response = ResponsePacket::Error {
                                    request_id,
                                    message: format!("Failed to trigger job: {e}"),
                                };
                                Self::send_packet(&socket, response, addr).await?;
                            } else {
                                let run_id = uuid::Uuid::new_v4().to_string();
                                let response = ResponsePacket::CronRunStarted {
                                    request_id,
                                    job_id,
                                    run_id,
                                };
                                Self::send_packet(&socket, response, addr).await?;
                            }
                        }
                        Ok(None) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Job {job_id} not found"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to get job: {e}"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::CronHistory {
                request_id,
                job_id,
                limit,
            } => {
                let cron_db = state.data_dir.join("cron.json");
                match crate::cron::CronScheduler::new(&cron_db) {
                    Ok(scheduler) => match scheduler.get_run_history(&job_id, limit) {
                        Ok(runs) => {
                            let response = ResponsePacket::CronHistory { request_id, runs };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                        Err(e) => {
                            let response = ResponsePacket::Error {
                                request_id,
                                message: format!("Failed to get history: {e}"),
                            };
                            Self::send_packet(&socket, response, addr).await?;
                        }
                    },
                    Err(e) => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: format!("Cron DB error: {e}"),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            // ─── Extension Runtime Lifecycle (ADR-026) ───────────────────────
            RequestPacket::ExtStart {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_start(request_id, extension_id, state, socket, addr).await?;
            }

            RequestPacket::ExtStop {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_stop(request_id, extension_id, state, socket, addr).await?;
            }

            RequestPacket::ExtRestart {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_restart(request_id, extension_id, state, socket, addr).await?;
            }

            RequestPacket::ExtStatus {
                request_id,
                extension_id,
            } => {
                Self::handle_ext_status(request_id, extension_id, state, socket, addr).await?;
            }

            // ─── Agent CRUD ─────────────────────────────────────────────────
            RequestPacket::AgentList { request_id, team_filter } => {
                let service = state.agent_mgmt_service();
                match service.list_agents(team_filter.as_deref()).await {
                    Ok(agents) => {
                        let response = ResponsePacket::AgentList { request_id, agents };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::AgentGet { request_id, name, team } => {
                let service = state.agent_mgmt_service();
                match service.get_agent(&name, team.as_deref()).await {
                    Ok(agent) => {
                        let response = ResponsePacket::AgentGet { request_id, agent };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::AgentCreate { request_id, request } => {
                let service = state.agent_mgmt_service();
                match service.create_agent(request).await {
                    Ok(result) => {
                        let response = ResponsePacket::AgentCreated { request_id, result };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::AgentDelete { request_id, name, team, force } => {
                let service = state.agent_mgmt_service();
                let opts = crate::common::types::agent::AgentDeleteOptions {
                    force,
                    ..Default::default()
                };
                match service.delete_agent(&name, team.as_deref(), opts).await {
                    Ok(result) => {
                        let response = ResponsePacket::AgentDeleted { request_id, result };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            // ─── Team CRUD ──────────────────────────────────────────────────
            RequestPacket::TeamList { request_id } => {
                let service = state.team_service();
                match service.list_teams().await {
                    Ok(teams) => {
                        let response = ResponsePacket::TeamList { request_id, teams };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::TeamGet { request_id, name } => {
                let service = state.team_service();
                match service.get_team(&name).await {
                    Ok(team) => {
                        let response = ResponsePacket::TeamGet { request_id, team };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                    Err(e) => {
                        let response = ResponsePacket::Error { request_id, message: e.to_string() };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            // ─── Session CRUD ───────────────────────────────────────────────
            RequestPacket::SessionList { request_id, agent } => {
                let service = state.session_service();
                match agent {
                    Some(agent_name) => {
                        match service.list_sessions(&agent_name, None).await {
                            Ok(sessions) => {
                                let response = ResponsePacket::SessionList { request_id, sessions };
                                Self::send_packet(&socket, response, addr).await?;
                            }
                            Err(e) => {
                                let response = ResponsePacket::Error { request_id, message: e.to_string() };
                                Self::send_packet(&socket, response, addr).await?;
                            }
                        }
                    }
                    None => {
                        let response = ResponsePacket::Error {
                            request_id,
                            message: "Agent name is required for session listing".to_string(),
                        };
                        Self::send_packet(&socket, response, addr).await?;
                    }
                }
            }

            RequestPacket::SessionGet { request_id, id: _ } => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: "SessionGet requires both agent name and session ID. Use the HTTP API for detailed session lookups.".to_string(),
                };
                Self::send_packet(&socket, response, addr).await?;
            }

            RequestPacket::SystemStatus { request_id } => {
                let response = ResponsePacket::SystemStatus {
                    request_id,
                    version: crate::VERSION.to_string(),
                    uptime_secs: state.uptime_seconds(),
                    degraded: state.is_degraded().await,
                    instance_count: state.instance_count().await,
                    team_count: state.team_count().await,
                    ready: state.is_ready().await,
                };
                Self::send_packet(&socket, response, addr).await?;
            }

            RequestPacket::SystemDoctor { request_id } => {
                let mut checks = Vec::new();

                let ready = state.is_ready().await;
                checks.push(super::packet::DoctorCheck {
                    name: "daemon_ready".to_string(),
                    status: if ready { "pass".to_string() } else { "fail".to_string() },
                    message: if ready { "Daemon is ready to serve requests".to_string() } else { "Daemon is not ready".to_string() },
                    suggestion: if !ready { Some("Check daemon logs for startup errors".to_string()) } else { None },
                });

                let degraded = state.is_degraded().await;
                checks.push(super::packet::DoctorCheck {
                    name: "not_degraded".to_string(),
                    status: if !degraded { "pass".to_string() } else { "warn".to_string() },
                    message: if !degraded { "Daemon is operating normally".to_string() } else { "Daemon is in degraded mode".to_string() },
                    suggestion: if degraded { Some("Check resource usage and consider restarting".to_string()) } else { None },
                });

                let uptime = state.uptime_seconds();
                checks.push(super::packet::DoctorCheck {
                    name: "uptime".to_string(),
                    status: "pass".to_string(),
                    message: format!("Daemon uptime: {} seconds", uptime),
                    suggestion: None,
                });

                let passed = checks.iter().filter(|c| c.status == "pass").count() as u32;
                let failed = checks.iter().filter(|c| c.status == "fail").count() as u32;
                let warnings = checks.iter().filter(|c| c.status == "warn").count() as u32;

                let response = ResponsePacket::SystemDoctor { request_id, checks, passed, failed, warnings };
                Self::send_packet(&socket, response, addr).await?;
            }
        }

        Ok(())
    }

    /// Handle an Execute request — run the agentic loop and stream responses
    async fn handle_execute(
        request_id: u64,
        agent: String,
        team: String,
        message: String,
        session_id: Option<String>,
        new_session: bool,
        stream_enabled: bool,
        user: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        use crate::agent::stateless_service::MessageRequest;
        use crate::engine::{AgenticEvent, LifecyclePhase};

        tracing::info!(
            "IPC handle_execute started: request_id={}, agent={}, user={}, stream={}",
            request_id,
            agent,
            user,
            stream_enabled
        );

        let agent_service = state.agent_service().clone();

        let request = MessageRequest::new(&agent, message)
            .with_team(&team)
            .with_session_opt(session_id)
            .with_new_session(new_session)
            .with_user(&user);

        // Start the agentic loop — wrap in catch_unwind-like error handling
        // so the client always gets a response even if execution fails
        let mut event_stream = match agent_service.execute_message_streaming(request).await {
            Ok(stream) => stream,
            Err(e) => {
                let error_packet = ResponsePacket::Error {
                    request_id,
                    message: format!("Failed to start agent execution: {e}"),
                };
                Self::send_packet(&socket, error_packet, addr).await?;
                let done_packet = ResponsePacket::Done {
                    request_id,
                    success: false,
                    error: Some(e.to_string()),
                };
                Self::send_packet(&socket, done_packet, addr).await?;
                return Ok(());
            }
        };

        // Stream events back as packets
        let mut seq = 0u32;
        let mut heartbeat = interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        // Buffer for non-streaming mode: accumulate all text and send at the end
        let mut non_streaming_buffer = String::new();

        loop {
            info!("IPC: waiting for event...");
            tokio::select! {
                maybe_event = event_stream.receiver.recv() => {
                    info!("IPC: received event from channel: {:?}", maybe_event.is_some());
                    match maybe_event {
                        Some(event) => {
                            match event {
                                AgenticEvent::AssistantDelta { text, .. } => {
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: text,
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                        seq += 1;
                                    } else {
                                        // Accumulate for non-streaming mode
                                        non_streaming_buffer.push_str(&text);
                                    }
                                }
                                AgenticEvent::AssistantText { text, .. } => {
                                    // Full block text (non-streaming mode)
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: text,
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                        seq += 1;
                                    } else {
                                        non_streaming_buffer.push_str(&text);
                                    }
                                }
                                AgenticEvent::ToolStart { name, .. } => {
                                    if stream_enabled {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: format!("\n[Running tool: {}]\n", name),
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                        seq += 1;
                                    }
                                }
                                AgenticEvent::ToolEnd { result, success, .. } => {
                                    info!("IPC: received ToolEnd event, stream_enabled={}", stream_enabled);
                                    if stream_enabled {
                                        let output = if success {
                                            result.to_string()
                                        } else {
                                            format!("[Tool failed: {}]", result)
                                        };
                                        info!("Sending ToolEnd result to client: len={}, output={}", output.len(), output);
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: format!("\n[Tool result]: {}\n", output),
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                    }
                                }
                                AgenticEvent::Lifecycle { phase: LifecyclePhase::End, .. } => {
                                    // In non-streaming mode, send accumulated text before Done
                                    if !stream_enabled && !non_streaming_buffer.is_empty() {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: std::mem::take(&mut non_streaming_buffer),
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                    }
                                    let packet = ResponsePacket::Done {
                                        request_id,
                                        success: true,
                                        error: None,
                                    };
                                    Self::send_packet(&socket, packet, addr).await?;
                                    break;
                                }
                                AgenticEvent::Lifecycle { phase: LifecyclePhase::Error, error, .. } => {
                                    // In non-streaming mode, send accumulated text before Done (even on error)
                                    if !stream_enabled && !non_streaming_buffer.is_empty() {
                                        let packet = ResponsePacket::Text {
                                            request_id,
                                            seq,
                                            chunk: std::mem::take(&mut non_streaming_buffer),
                                        };
                                        Self::send_packet(&socket, packet, addr).await?;
                                    }
                                    let packet = ResponsePacket::Done {
                                        request_id,
                                        success: false,
                                        error,
                                    };
                                    Self::send_packet(&socket, packet, addr).await?;
                                    break;
                                }
                                _ => {
                                    // Ignore other events (Thinking, Status, Usage, etc.)
                                }
                            }
                        }
                        None => {
                            // In non-streaming mode, send accumulated text before Done
                            if !stream_enabled && !non_streaming_buffer.is_empty() {
                                let packet = ResponsePacket::Text {
                                    request_id,
                                    seq,
                                    chunk: std::mem::take(&mut non_streaming_buffer),
                                };
                                Self::send_packet(&socket, packet, addr).await?;
                            }
                            let packet = ResponsePacket::Done {
                                request_id,
                                success: true,
                                error: None,
                            };
                            Self::send_packet(&socket, packet, addr).await?;
                            break;
                        }
                    }
                }

                _ = heartbeat.tick() => {
                    let packet = ResponsePacket::Heartbeat { request_id };
                    Self::send_packet(&socket, packet, addr).await?;
                }
            }
        }

        Ok(())
    }

    /// Handle an AsyncSpawn request
    async fn handle_async_spawn(
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: std::path::PathBuf,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        use crate::extension::async_exec::executor::{AsyncTaskId, AsyncToolConfig};

        let tool_runtime = state.tool_runtime.clone();
        let executor = state.async_task_executor.clone();

        let config = AsyncToolConfig::default();
        let task_id = AsyncTaskId::new();

        let receipt = executor
            .execute(
                task_id,
                tool_name.clone(),
                params.clone(),
                session_key,
                config,
                move || {
                    let runtime = tool_runtime.clone();
                    let ws = workspace.clone();
                    let name = tool_name.clone();
                    let p = params.clone();
                    Box::pin(async move {
                        match runtime.execute_tool_with_workspace(&name, p, &ws).await {
                            Ok(value) => Ok(value),
                            Err(e) => Err(e),
                        }
                    })
                },
            )
            .await?;

        let response = ResponsePacket::AsyncReceipt {
            request_id,
            receipt,
        };
        Self::send_packet(&socket, response, addr).await?;

        Ok(())
    }

    /// Handle an AsyncCancel request
    async fn handle_async_cancel(
        request_id: u64,
        task_id: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let executor = state.async_task_executor.clone();
        let cancelled = executor.cancel(&task_id).await.unwrap_or(false);

        let response = ResponsePacket::Done {
            request_id,
            success: cancelled,
            error: if cancelled {
                None
            } else {
                Some(format!("Task {} not found or already completed", task_id))
            },
        };
        Self::send_packet(&socket, response, addr).await?;

        Ok(())
    }

    /// Handle an ExtStart request — start a background runtime for an extension
    async fn handle_ext_start(
        request_id: u64,
        extension_id: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.start(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtStarted {
                    request_id,
                    extension_id,
                };
                Self::send_packet(&socket, response, addr).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_packet(&socket, response, addr).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtStop request
    async fn handle_ext_stop(
        request_id: u64,
        extension_id: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.stop(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtStopped {
                    request_id,
                    extension_id,
                };
                Self::send_packet(&socket, response, addr).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_packet(&socket, response, addr).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtRestart request
    async fn handle_ext_restart(
        request_id: u64,
        extension_id: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let registry = state.runtime_starter_registry().clone();
        let ctx = state.starter_context();

        match registry.restart(&extension_id, &ctx).await {
            Ok(()) => {
                let response = ResponsePacket::ExtRestarted {
                    request_id,
                    extension_id,
                };
                Self::send_packet(&socket, response, addr).await?;
            }
            Err(e) => {
                let response = ResponsePacket::Error {
                    request_id,
                    message: e.to_string(),
                };
                Self::send_packet(&socket, response, addr).await?;
            }
        }

        Ok(())
    }

    /// Handle an ExtStatus request
    async fn handle_ext_status(
        request_id: u64,
        extension_id: String,
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let manager = state.background_runtime_manager().clone();

        match manager.get_state(&extension_id).await {
            Some(runtime_state) => {
                // Also get summary for restart_count and last_error
                let summaries = manager.list().await;
                let summary = summaries.iter().find(|s| s.id == extension_id);
                let restart_count = summary.map(|s| s.restart_count).unwrap_or(0);
                let last_error = summary.and_then(|s| s.last_error.clone());

                let response = ResponsePacket::ExtStatus {
                    request_id,
                    extension_id,
                    state: runtime_state.to_string(),
                    restart_count,
                    last_error,
                };
                Self::send_packet(&socket, response, addr).await?;
            }
            None => {
                let response = ResponsePacket::ExtStatus {
                    request_id,
                    extension_id,
                    state: "not_found".to_string(),
                    restart_count: 0,
                    last_error: None,
                };
                Self::send_packet(&socket, response, addr).await?;
            }
        }

        Ok(())
    }

    /// Send a response packet back to the client
    async fn send_packet(
        socket: &ServerSocket,
        packet: ResponsePacket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        let bytes = packet.to_bytes()?;
        trace!("Sending response: {:?} ({} bytes)", packet, bytes.len());
        socket.send_response(&bytes, addr).await?;
        Ok(())
    }
}
