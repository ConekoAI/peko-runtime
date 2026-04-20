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
    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, Option<std::net::SocketAddr>)> {
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
    pub async fn run(&self, mut shutdown_rx: tokio::sync::broadcast::Receiver<()>) -> anyhow::Result<()> {
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
            } => {
                Self::handle_execute(
                    request_id, agent, team, message, session_id, new_session, stream, state, socket, addr,
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
                    request_id, tool_name, params, session_key, workspace, state, socket, addr,
                )
                .await?;
            }

            RequestPacket::AsyncCancel { request_id, task_id } => {
                Self::handle_async_cancel(request_id, task_id, state, socket, addr).await?;
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
        state: AppState,
        socket: ServerSocket,
        addr: Option<std::net::SocketAddr>,
    ) -> anyhow::Result<()> {
        use crate::agent::stateless_service::MessageRequest;
        use crate::engine::{AgenticEvent, LifecyclePhase};
        
        tracing::info!("IPC handle_execute started: request_id={}, agent={}, stream={}", request_id, agent, stream_enabled);

        let agent_service = state.agent_service().clone();

        let request = MessageRequest::new(&agent, message)
            .with_team(&team)
            .with_session_opt(session_id)
            .with_new_session(new_session);

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
                                        seq += 1;
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
                                        seq += 1;
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
                                        seq += 1;
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
                                seq += 1;
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
        use crate::agent::async_tool_framework::{AsyncTaskId, AsyncTaskResult, AsyncToolConfig};

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
                            Ok(value) => Ok(AsyncTaskResult::Generic { data: value }),
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


