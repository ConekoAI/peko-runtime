//! SDK transport boundary. Raw protocol results remain structured.
#![allow(deprecated)] // Sampling remains supported for negotiated legacy MCP peers.
use super::config::{Definition, Transport};
use crate::extensions::mcp::protocol::{
    client::{McpClient, ServerRequestHandler},
    config::McpAuthConfig,
    sampling::SamplingRequestHandler,
    transport::SseTransport,
};
use anyhow::{bail, ensure, Context, Result};
use rmcp::{
    model::{
        ClientInfo, ClientRequest, CreateMessageRequestParams, CreateMessageResult, ErrorData,
        ServerResult,
    },
    service::{
        NotificationContext, Peer, PeerRequestOptions, RequestContext, RoleClient, RunningService,
    },
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
        TokioChildProcess,
    },
    ClientHandler, ServiceExt,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub struct Callbacks {
    progress: Arc<
        std::sync::Mutex<
            std::collections::HashMap<rmcp::model::ProgressToken, peko_tools_core::ToolContext>,
        >,
    >,
    pub changed: Arc<AtomicBool>,
    pub active: Arc<AtomicUsize>,
    pub sampling: Option<Arc<SamplingRequestHandler>>,
}

impl ClientHandler for Callbacks {
    async fn on_progress(
        &self,
        params: rmcp::model::ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) {
        let context = self
            .progress
            .lock()
            .expect("MCP progress lock poisoned")
            .get(&params.progress_token)
            .cloned();
        if let Some(context) = context {
            context
                .report_progress(
                    params.progress.max(0.0) as usize,
                    params.total.unwrap_or(0.0).max(0.0) as usize,
                    params.message,
                )
                .await;
        }
    }
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.client_info.name = "peko".into();
        info.client_info.version = env!("CARGO_PKG_VERSION").into();
        if self.sampling.is_some() {
            info.capabilities.sampling = Some(Default::default());
        }
        info
    }
    async fn on_tool_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.changed.store(true, Ordering::Release);
    }
    async fn on_resource_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.changed.store(true, Ordering::Release);
    }
    async fn on_prompt_list_changed(&self, _: NotificationContext<RoleClient>) {
        self.changed.store(true, Ordering::Release);
    }
    async fn create_message(
        &self,
        params: CreateMessageRequestParams,
        context: RequestContext<RoleClient>,
    ) -> std::result::Result<CreateMessageResult, ErrorData> {
        if self.active.load(Ordering::Acquire) == 0 {
            return Err(ErrorData::invalid_request(
                "sampling requires an active owner-attributed request",
                None,
            ));
        }
        let handler = self
            .sampling
            .as_ref()
            .ok_or_else(|| ErrorData::invalid_request("sampling is not configured", None))?;
        let params = serde_json::to_value(params)
            .map_err(|_| ErrorData::invalid_params("invalid sampling parameters", None))?;
        let result = tokio::select! {
            () = context.ct.cancelled() => return Err(ErrorData::internal_error("sampling cancelled", None)),
            result = handler.handle_request("sampling/createMessage", Some(params)) => result.map_err(|e| ErrorData::internal_error(e.message, None))?,
        };
        serde_json::from_value(result)
            .map_err(|_| ErrorData::internal_error("unsupported sampling result", None))
    }
}

pub enum Connection {
    Sdk(RunningService<RoleClient, Callbacks>),
    Legacy(Arc<RwLock<McpClient>>),
}

pub struct Client {
    progress: Arc<
        std::sync::Mutex<
            std::collections::HashMap<rmcp::model::ProgressToken, peko_tools_core::ToolContext>,
        >,
    >,
    connection: Connection,
    pub info: Value,
    pub changed: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    pub cancellation: CancellationToken,
}

struct ActiveRequest(Arc<AtomicUsize>);
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct CancelRequest {
    peer: Peer<RoleClient>,
    id: Option<rmcp::model::RequestId>,
}

struct ProgressGuard {
    progress: Arc<
        std::sync::Mutex<
            std::collections::HashMap<rmcp::model::ProgressToken, peko_tools_core::ToolContext>,
        >,
    >,
    token: rmcp::model::ProgressToken,
}
impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.progress
            .lock()
            .expect("MCP progress lock poisoned")
            .remove(&self.token);
    }
}
impl Drop for CancelRequest {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let peer = self.peer.clone();
            tokio::spawn(async move {
                if let Ok(notification) = serde_json::from_value(
                    json!({"method":"notifications/cancelled","params":{"requestId":id,"reason":"Peko invocation ended"}}),
                ) {
                    let _ = peer.send_notification(notification).await;
                }
            });
        }
    }
}

impl Client {
    pub async fn connect(
        definition: &Definition,
        env: BTreeMap<String, String>,
        headers: BTreeMap<String, String>,
        bearer: Option<String>,
        sampling: Option<Arc<SamplingRequestHandler>>,
    ) -> Result<Self> {
        let changed = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let progress = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let handler = Callbacks {
            progress: progress.clone(),
            changed: changed.clone(),
            active: active.clone(),
            sampling,
        };
        let (connection, info) = match &definition.transport {
            Transport::Stdio { command, args } => {
                let mut command = tokio::process::Command::new(command);
                command.args(args).env_clear().envs(env).kill_on_drop(true);
                for name in [
                    "PATH",
                    "HOME",
                    "SystemRoot",
                    "COMSPEC",
                    "PATHEXT",
                    "TEMP",
                    "TMP",
                ] {
                    if let Some(value) = std::env::var_os(name) {
                        if !definition.env.contains_key(name) {
                            command.env(name, value);
                        }
                    }
                }
                if let Some(cwd) = &definition.cwd {
                    command.current_dir(cwd);
                }
                let mut command = process_wrap::tokio::CommandWrap::from(command);
                command.wrap(process_wrap::tokio::KillOnDrop);
                #[cfg(unix)]
                command.wrap(process_wrap::tokio::ProcessGroup::leader());
                #[cfg(windows)]
                command.wrap(process_wrap::tokio::JobObject);
                let transport =
                    TokioChildProcess::new(command).context("failed to start MCP process")?;
                let client = handler
                    .serve(transport)
                    .await
                    .context("MCP initialization failed")?;
                let info =
                    serde_json::to_value(client.peer_info()).context("invalid server info")?;
                (Connection::Sdk(client), info)
            }
            Transport::StreamableHttp { url } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(url.clone())
                    .reinit_on_expired_session(false);
                if let Some(bearer) = bearer {
                    config = config.auth_header(bearer);
                }
                config = config.custom_headers(
                    headers
                        .into_iter()
                        .map(|(name, value)| {
                            Ok((
                                name.parse::<http::HeaderName>()?,
                                value.parse::<http::HeaderValue>()?,
                            ))
                        })
                        .collect::<Result<_>>()?,
                );
                let http = reqwest_mcp::Client::builder()
                    .redirect(reqwest_mcp::redirect::Policy::none())
                    .build()?;
                let transport = StreamableHttpClientTransport::with_client(http, config);
                let client = handler
                    .serve(transport)
                    .await
                    .context("MCP initialization failed")?;
                let info = serde_json::to_value(client.peer_info())?;
                (Connection::Sdk(client), info)
            }
            Transport::Sse { endpoint } => {
                let auth = McpAuthConfig {
                    bearer_token: bearer,
                    headers: headers.into_iter().collect(),
                    ..Default::default()
                };
                let transport =
                    SseTransport::connect_with_auth(endpoint, auth, None, "legacy").await?;
                transport.require_message_endpoint().await?;
                let mut client = McpClient::with_capabilities(
                    Box::new(transport),
                    crate::extensions::mcp::protocol::types::ClientCapabilities {
                        experimental: None,
                        sampling: None,
                    },
                    crate::extensions::mcp::protocol::types::Implementation {
                        name: "peko".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                );
                client.initialize().await?;
                let server_info = client.server_info().context("missing legacy server info")?;
                let info = json!({"capabilities":server_info.capabilities,"instructions":server_info.instructions});
                (Connection::Legacy(Arc::new(RwLock::new(client))), info)
            }
        };
        Ok(Self {
            progress,
            connection,
            info,
            changed,
            active,
            cancellation: CancellationToken::new(),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.cancellation.is_cancelled()
            || matches!(&self.connection,Connection::Sdk(client) if client.is_closed())
    }

    pub fn has_active_request(&self) -> bool {
        self.active.load(Ordering::Acquire) > 0
    }
    pub fn revoke(&self) {
        self.cancellation.cancel();
        match &self.connection {
            Connection::Sdk(client) => client.cancellation_token().cancel(),
            Connection::Legacy(client) => {
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = client.write().await.shutdown().await;
                });
            }
        }
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        self.request_with_context(method, params, timeout, None)
            .await
    }

    pub async fn request_with_context(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
        context: Option<&peko_tools_core::ToolContext>,
    ) -> Result<Value> {
        ensure!(
            !context.is_some_and(|context| context.is_aborted()),
            "MCP request cancelled"
        );
        ensure!(!self.is_closed(), "MCP connection closed");
        let _active = if method != "ping" {
            self.active.fetch_add(1, Ordering::AcqRel);
            Some(ActiveRequest(self.active.clone()))
        } else {
            None
        };
        let operation = async {
            match &self.connection {
                Connection::Sdk(client) => {
                    let request: ClientRequest =
                        serde_json::from_value(json!({"method":method,"params":params}))
                            .context("unsupported MCP method/parameters")?;
                    let mut options = PeerRequestOptions::default();
                    options.timeout = timeout;
                    let handle = client.send_request_with_option(request, options).await?;
                    let mut cancel = CancelRequest {
                        peer: handle.peer.clone(),
                        id: Some(handle.id.clone()),
                    };
                    let _progress = context.map(|context| {
                        self.progress
                            .lock()
                            .expect("MCP progress lock poisoned")
                            .insert(handle.progress_token.clone(), context.clone());
                        ProgressGuard {
                            progress: self.progress.clone(),
                            token: handle.progress_token.clone(),
                        }
                    });
                    let result: ServerResult = handle.await_response().await?;
                    cancel.id = None;
                    Ok(serde_json::to_value(result)?)
                }
                Connection::Legacy(client) => client
                    .read()
                    .await
                    .request_json(method, params, timeout)
                    .await
                    .map_err(Into::into),
            }
        };
        let abort = async {
            if let Some(context) = context {
                let mut signal = context.abort_signal();
                loop {
                    if *signal.borrow() {
                        return;
                    }
                    if signal.changed().await.is_err() {
                        break;
                    }
                }
            }
            std::future::pending::<()>().await;
        };
        tokio::select! {
            () = self.cancellation.cancelled() => bail!("MCP connection revoked"),
            () = abort => bail!("MCP request cancelled"),
            result = operation => result,
        }
    }

    pub async fn list(&self, method: &str, field: &str) -> Result<Vec<Value>> {
        let fetch = async {
            let mut out = Vec::new();
            let mut cursor = None;
            let mut seen = std::collections::HashSet::new();
            for _ in 0..100 {
                let result = self
                    .request(
                        method,
                        cursor
                            .as_ref()
                            .map_or(json!({}), |cursor| json!({"cursor":cursor})),
                        Some(Duration::from_secs(30)),
                    )
                    .await?;
                let items = result
                    .get(field)
                    .and_then(Value::as_array)
                    .context("MCP catalog response has no item array")?;
                anyhow::ensure!(
                    out.len() + items.len() <= 2048,
                    "MCP catalog exceeds 2048 items"
                );
                out.extend(items.iter().cloned());
                anyhow::ensure!(
                    serde_json::to_vec(&out)?.len() <= 8 * 1024 * 1024,
                    "MCP catalog exceeds 8 MiB"
                );
                match result.get("nextCursor") {
                    Some(Value::String(next)) => {
                        anyhow::ensure!(
                            next.len() <= 64 * 1024 && seen.insert(next.clone()),
                            "invalid or repeated pagination cursor"
                        );
                        cursor = Some(next.clone());
                    }
                    None | Some(Value::Null) => return Ok(out),
                    _ => bail!("invalid pagination cursor"),
                }
            }
            bail!("MCP catalog exceeds 100 pages")
        };
        tokio::time::timeout(Duration::from_secs(30), fetch)
            .await
            .context("MCP catalog discovery timed out")?
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        match &self.connection {
            Connection::Sdk(client) => client.cancellation_token().cancel(),
            Connection::Legacy(client) => {
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = client.write().await.shutdown().await;
                });
            }
        }
    }
}
