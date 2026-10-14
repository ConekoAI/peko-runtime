//! Principal-owned connections, discovery, and prepared tool bindings (ADR-075).
mod auth;
mod client;
pub mod config;
#[cfg(test)]
mod http_tests;
mod proxy;
#[cfg(test)]
mod tests;

use anyhow::{bail, ensure, Context, Result};
use config::{Authentication, Definition, Startup, ValueBinding};
use futures::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use peko_subject::PrincipalId;
use peko_tools_core::{Tool, ToolContext};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

type Started = std::result::Result<Arc<client::Client>, Arc<String>>;
type StartupFuture = Shared<BoxFuture<'static, Started>>;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Inventory {
    tools: Vec<Value>,
    resources: Vec<Value>,
    prompts: Vec<Value>,
    instructions: Option<String>,
    fetched_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    version: u32,
    owner: PrincipalId,
    identity: String,
    inventory: Inventory,
}

struct Server {
    id: String,
    path: PathBuf,
    definition: Arc<Definition>,
    identity: String,
    config_identity: String,
    current: AtomicBool,
    revoked: CancellationToken,
    admission: Mutex<()>,
    startup: Mutex<Option<StartupFuture>>,
    failures: AtomicU32,
    attempts: AtomicU32,
    retry_after: AtomicU64,
    oauth_refresh: Mutex<()>,
    discovery: Mutex<()>,
    inventory: RwLock<Option<Inventory>>,
    client: RwLock<Option<Arc<client::Client>>>,
    diagnostic: RwLock<Option<String>>,
    fresh: AtomicBool,
}

/// Stable services shared by every session of one principal; never global.
pub struct PrincipalMcpRuntime {
    owner: PrincipalId,
    workspace: PathBuf,
    cache_dir: PathBuf,
    vault: Option<Arc<crate::common::vault::Vault>>,
    resolver: Option<Arc<peko_providers::LlmResolver>>,
    meter: Arc<peko_quota::QuotaMeter>,
    model: Arc<std::sync::RwLock<Option<String>>>,
    servers: RwLock<BTreeMap<String, Arc<Server>>>,
    invalid: RwLock<BTreeMap<String, (PathBuf, String)>>,
    reconcile: Mutex<()>,
    startup_pool: Arc<Semaphore>,
    clients: Mutex<Vec<std::sync::Weak<client::Client>>>,
    stopped: CancellationToken,
    pub(crate) authenticating: Mutex<std::collections::HashSet<String>>,
}

impl std::fmt::Debug for PrincipalMcpRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrincipalMcpRuntime")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl PrincipalMcpRuntime {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: PrincipalId,
        workspace: PathBuf,
        cache_dir: PathBuf,
        vault: Option<Arc<crate::common::vault::Vault>>,
        resolver: Option<Arc<peko_providers::LlmResolver>>,
        meter: Arc<peko_quota::QuotaMeter>,
        model: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            owner,
            workspace,
            cache_dir,
            vault,
            resolver,
            meter,
            model: Arc::new(std::sync::RwLock::new(model)),
            servers: RwLock::new(BTreeMap::new()),
            invalid: RwLock::new(BTreeMap::new()),
            reconcile: Mutex::new(()),
            startup_pool: Arc::new(Semaphore::new(4)),
            clients: Mutex::new(Vec::new()),
            stopped: CancellationToken::new(),
            authenticating: Mutex::new(Default::default()),
        })
    }
    pub fn owner(&self) -> &PrincipalId {
        &self.owner
    }
    pub fn set_model(&self, model: Option<String>) {
        if let Ok(mut current) = self.model.write() {
            *current = model;
        }
    }
    fn namespace(&self, id: &str) -> String {
        format!("mcp:principal:{}:{id}", self.owner.0)
    }
    fn secret(&self, id: &str, slot: &str) -> Result<String> {
        config::validate_id(slot)?;
        self.vault
            .as_ref()
            .context("MCP credential vault unavailable")?
            .get_material_for(&self.namespace(id), slot)?
            .map(|secret| secret.expose_secret().to_string())
            .with_context(|| format!("missing credential slot '{slot}' for MCP server '{id}'"))
    }
    fn values(
        &self,
        id: &str,
        bindings: &BTreeMap<String, ValueBinding>,
    ) -> Result<BTreeMap<String, String>> {
        bindings
            .iter()
            .map(|(key, binding)| {
                Ok((
                    key.clone(),
                    match binding {
                        ValueBinding::Literal { value } => value.clone(),
                        ValueBinding::Credential { credential } => self.secret(id, credential)?,
                    },
                ))
            })
            .collect()
    }
    fn identity(&self, id: &str, definition: &Definition) -> Result<String> {
        let mut revisions = Vec::new();
        if let Some(vault) = &self.vault {
            let mut credentials = vault.list_credentials(&crate::common::vault::CredentialFilter {
                namespace: Some(self.namespace(id)),
                include_system: true,
                ..Default::default()
            });
            credentials.sort_by(|a, b| a.id.cmp(&b.id));
            for credential in credentials {
                if let Some(record) = vault.get_credential(&credential.id) {
                    revisions.push((record.id, record.name, record.updated_at));
                }
            }
        }
        Ok(digest(
            &json!({"owner":self.owner,"server":id,"config":definition.connection_identity()?,"credentials":revisions}),
        ))
    }
    fn validate_owner(&self, context: &ToolContext) -> Result<()> {
        ensure!(
            context.principal_id.as_deref() == Some(&self.owner.0),
            "MCP caller does not own this service"
        );
        ensure!(
            !self.stopped.is_cancelled(),
            "MCP principal was removed or unloaded"
        );
        Ok(())
    }

    /// File content is authoritative; no one-time installation or directory-mtime cache.
    pub async fn reconcile(self: &Arc<Self>) -> Result<()> {
        let _gate = self.reconcile.lock().await;
        ensure!(
            !self.stopped.is_cancelled(),
            "MCP principal was removed or unloaded"
        );
        if let Some(vault) = &self.vault {
            vault.reload_unlocked().context("MCP vault reload failed")?;
        }
        let manifests = config::scan(&self.workspace.join("mcp")).await?;
        let previous = self.servers.read().await.clone();
        let mut next = BTreeMap::new();
        let mut invalid = BTreeMap::new();
        for (id, manifest) in manifests {
            let definition = match manifest.definition {
                Ok(definition) => definition,
                Err(error) => {
                    invalid.insert(id, (manifest.path, format!("{error:#}")));
                    continue;
                }
            };
            let identity = self.identity(&id, &definition)?;
            let config_identity = digest(&serde_json::to_value(&definition)?);
            if let Some(server) = previous.get(&id).filter(|server| {
                server.identity == identity && server.config_identity == config_identity
            }) {
                next.insert(id, server.clone());
                continue;
            }
            let cached = self.read_cache(&id, &identity).await;
            let server = Arc::new(Server {
                id: id.clone(),
                path: manifest.path,
                definition: Arc::new(definition),
                identity,
                config_identity,
                current: AtomicBool::new(true),
                revoked: CancellationToken::new(),
                admission: Mutex::new(()),
                startup: Mutex::new(None),
                failures: AtomicU32::new(0),
                attempts: AtomicU32::new(0),
                retry_after: AtomicU64::new(0),
                oauth_refresh: Mutex::new(()),
                discovery: Mutex::new(()),
                inventory: RwLock::new(cached),
                client: RwLock::new(None),
                diagnostic: RwLock::new(None),
                fresh: AtomicBool::new(false),
            });
            if let Some(old) = previous
                .get(&id)
                .filter(|old| old.identity == server.identity)
            {
                *server.client.write().await = old.client.read().await.clone();
                *server.inventory.write().await = old.inventory.read().await.clone();
                server
                    .fresh
                    .store(old.fresh.load(Ordering::Acquire), Ordering::Release);
                if let Some(client) = server.client.read().await.as_ref() {
                    self.monitor(&server, client);
                }
            }
            next.insert(id, server);
        }
        for (id, old) in &previous {
            if next.get(id).is_none_or(|next| !Arc::ptr_eq(old, next)) {
                let _admission = old.admission.lock().await;
                old.current.store(false, Ordering::Release);
                // Retire startup; admitted requests retain their exact client lease.
                old.revoked.cancel();
                if next.get(id).is_some_and(|new| new.identity != old.identity) {
                    // Credential replacement/removal is distinct from ordinary configuration edits.
                    if self.credential_revoked(&old.id, &old.definition)? {
                        if let Some(client) = old.client.read().await.as_ref() {
                            client.revoke();
                        }
                    }
                }
            }
        }
        *self.servers.write().await = next;
        *self.invalid.write().await = invalid;
        Ok(())
    }

    fn credential_revoked(&self, id: &str, definition: &Definition) -> Result<bool> {
        for binding in definition.env.values().chain(definition.headers.values()) {
            if let ValueBinding::Credential { credential } = binding {
                if self.secret(id, credential).is_err() {
                    return Ok(true);
                }
            }
        }
        for source in definition.reserved_parameters.values() {
            if let config::ReservedParameter::Credential { credential } = source {
                if self.secret(id, credential).is_err() {
                    return Ok(true);
                }
            }
        }
        if let Authentication::Bearer { credential } = &definition.auth {
            return Ok(self.secret(id, credential).is_err());
        }
        if matches!(definition.auth, Authentication::OAuth { .. }) {
            return Ok(self.secret(id, "oauth_token").is_err());
        }
        Ok(false)
    }

    async fn read_cache(&self, id: &str, identity: &str) -> Option<Inventory> {
        let path = self.cache_dir.join(format!("{id}.json"));
        if tokio::fs::metadata(&path).await.ok()?.len() > 8 * 1024 * 1024 {
            return None;
        }
        let cache: Cache = serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()?;
        (cache.version == 1
            && cache.owner == self.owner
            && cache.identity == identity
            && now().saturating_sub(cache.inventory.fetched_at) < 1800)
            .then_some(cache.inventory)
    }
    async fn store_cache(&self, server: &Server, inventory: &Inventory) -> Result<()> {
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        let value = serde_json::to_vec(&Cache {
            version: 1,
            owner: self.owner.clone(),
            identity: server.identity.clone(),
            inventory: inventory.clone(),
        })?;
        let temporary = self
            .cache_dir
            .join(format!(".{}.{}.tmp", server.id, uuid::Uuid::new_v4()));
        tokio::fs::write(&temporary, value).await?;
        tokio::fs::rename(
            temporary,
            self.cache_dir.join(format!("{}.json", server.id)),
        )
        .await?;
        Ok(())
    }

    async fn server(&self, id: &str) -> Result<Arc<Server>> {
        self.servers
            .read()
            .await
            .get(id)
            .cloned()
            .with_context(|| format!("MCP server '{id}' unavailable; use MCP status"))
    }

    async fn start(self: &Arc<Self>, server: &Arc<Server>) -> Result<Arc<client::Client>> {
        if let Some(client) = server
            .client
            .read()
            .await
            .as_ref()
            .filter(|client| !client.is_closed())
        {
            return Ok(client.clone());
        }
        let mut startup = server.startup.lock().await;
        if server.diagnostic.read().await.is_some()
            || server
                .client
                .read()
                .await
                .as_ref()
                .is_some_and(|client| client.is_closed())
        {
            *startup = None;
        }

        if startup.is_none() {
            if server.definition.max_restarts > 0
                && server.attempts.load(Ordering::Acquire) > server.definition.max_restarts
            {
                bail!("MCP restart limit reached; use MCP reconnect");
            }
            ensure!(
                now() >= server.retry_after.load(Ordering::Acquire),
                "MCP reconnect is in backoff; use MCP status or reconnect"
            );
            // Clear failed/closed state before publishing the replacement startup,
            // so sibling callers join it instead of launching another process.
            *server.diagnostic.write().await = None;
            *server.client.write().await = None;
        }

        let future = startup.get_or_insert_with(|| {
            server.attempts.fetch_add(1, Ordering::AcqRel);
            let runtime = self.clone();
            let server = server.clone();
            tokio::spawn(async move {
                let result = tokio::select! {
                    () = runtime.stopped.cancelled() => Err(anyhow::anyhow!("MCP principal stopped")),
                    () = server.revoked.cancelled() => Err(anyhow::anyhow!("MCP server removed")),
                    result = tokio::time::timeout(Duration::from_millis(server.definition.startup_timeout_ms),runtime.connect(&server)) => result.context("MCP startup/discovery timed out").and_then(|result| result),
                };
                match result {
                    Ok(client) => {
                        server.failures.store(0, Ordering::Release);
                        server.retry_after.store(0, Ordering::Release);
                        *server.diagnostic.write().await = None;
                        Ok(client)
                    }
                    Err(error) => {
                        let failures = server.failures.fetch_add(1, Ordering::AcqRel).saturating_add(1);
                        server.retry_after.store(now() + (1u64 << failures.min(6)), Ordering::Release);
                        let error = runtime.sanitize(&server.id, format!("{error:#}"));
                        *server.diagnostic.write().await = Some(error.clone());
                        Err(Arc::new(error))
                    }
                }
            }).map(|result| result.unwrap_or_else(|_| Err(Arc::new("MCP startup task failed".into())))).boxed().shared()
        }).clone();
        drop(startup);
        future.await.map_err(|error| anyhow::anyhow!("{error}"))
    }

    async fn connect(self: &Arc<Self>, server: &Arc<Server>) -> Result<Arc<client::Client>> {
        let _permit = self.startup_pool.acquire().await?;
        let definition = &server.definition;
        let bearer = self.bearer(server).await?;
        let sampling = definition
            .sampling
            .as_ref()
            .map(|sampling| {
                let resolver = self
                    .resolver
                    .clone()
                    .context("MCP sampling resolver unavailable")?;
                let model = sampling.model.as_ref().map_or_else(
                    || self.model.clone(),
                    |model| Arc::new(std::sync::RwLock::new(Some(model.clone()))),
                );
                Ok::<_, anyhow::Error>(Arc::new(
                    crate::extensions::mcp::protocol::sampling::SamplingRequestHandler::new(
                        resolver,
                        self.meter.clone(),
                    )
                    .with_model(model, sampling.max_output_tokens),
                ))
            })
            .transpose()?;
        let client = Arc::new(
            client::Client::connect(
                definition,
                self.values(&server.id, &definition.env)?,
                self.values(&server.id, &definition.headers)?,
                bearer,
                sampling,
            )
            .await?,
        );
        {
            let mut clients = self.clients.lock().await;
            clients.retain(|client| client.strong_count() > 0);
            clients.push(Arc::downgrade(&client));
        }
        self.discover(server, &client).await?;
        ensure!(
            server.current.load(Ordering::Acquire),
            "MCP server removed during discovery"
        );
        *server.client.write().await = Some(client.clone());
        self.monitor(server, &client);
        Ok(client)
    }

    async fn discover(&self, server: &Server, client: &client::Client) -> Result<()> {
        let _discovery = server.discovery.lock().await;
        let result = self.discover_inner(server, client).await;
        if let Err(error) = &result {
            *server.inventory.write().await = None;
            server.fresh.store(false, Ordering::Release);
            client.changed.store(true, Ordering::Release);
            *server.diagnostic.write().await =
                Some(self.sanitize(&server.id, format!("{error:#}")));
            let _ =
                tokio::fs::remove_file(self.cache_dir.join(format!("{}.json", server.id))).await;
        }
        result
    }

    async fn discover_inner(&self, server: &Server, client: &client::Client) -> Result<()> {
        client.changed.store(false, Ordering::Release);
        let capabilities = &client.info["capabilities"];
        let tools = if capabilities.get("tools").is_some() {
            client.list("tools/list", "tools").await?
        } else {
            Vec::new()
        };
        let mut names = std::collections::HashSet::new();
        for tool in &tools {
            let name = tool["name"].as_str().context("MCP tool has no name")?;
            ensure!(
                !name.is_empty() && names.insert(name),
                "MCP catalog contains duplicate/empty tool names"
            );
            ensure!(
                tool["inputSchema"].is_object(),
                "MCP tool has no inputSchema object"
            );
        }
        let resources = if capabilities.get("resources").is_some() {
            client.list("resources/list", "resources").await?
        } else {
            Vec::new()
        };
        let prompts = if capabilities.get("prompts").is_some() {
            client.list("prompts/list", "prompts").await?
        } else {
            Vec::new()
        };
        let inventory = Inventory {
            tools,
            resources,
            prompts,
            instructions: client.info["instructions"].as_str().map(str::to_string),
            fetched_at: now(),
        };
        ensure!(
            server.current.load(Ordering::Acquire),
            "MCP server removed during discovery"
        );
        *server.inventory.write().await = Some(inventory.clone());
        server.fresh.store(true, Ordering::Release);
        if let Err(error) = self.store_cache(server, &inventory).await {
            tracing::warn!(server = %server.id,error = %error,"MCP metadata cache write failed");
        }
        Ok(())
    }

    fn monitor(self: &Arc<Self>, server: &Arc<Server>, client: &Arc<client::Client>) {
        let interval = server.definition.health_check_interval_secs;
        if interval == 0 {
            return;
        }
        let server = Arc::downgrade(server);
        let runtime = Arc::downgrade(self);
        let client = Arc::downgrade(client);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(interval)).await;
                let (Some(server), Some(runtime), Some(client)) =
                    (server.upgrade(), runtime.upgrade(), client.upgrade())
                else {
                    return;
                };
                if !server.current.load(Ordering::Acquire) || runtime.stopped.is_cancelled() {
                    return;
                }
                if client.has_active_request() {
                    continue;
                }
                if let Err(error) = client
                    .request("ping", json!({}), Some(Duration::from_secs(5)))
                    .await
                {
                    *server.diagnostic.write().await = Some(
                        runtime.sanitize(&server.id, format!("MCP health check failed: {error}")),
                    );
                    client.revoke();
                    return;
                }
            }
        });
    }

    /// Cold discovery is independent of whether any method was already advertised.
    pub async fn prepare_catalog(self: &Arc<Self>) -> Result<()> {
        self.reconcile().await?;
        let servers: Vec<_> = self.servers.read().await.values().cloned().collect();
        let work = futures::future::join_all(servers.iter().map(|server| async {
            let catalog = server.inventory.read().await;
            let cold = catalog.is_none();
            let expired = catalog
                .as_ref()
                .is_some_and(|inventory| now().saturating_sub(inventory.fetched_at) >= 1800);
            drop(catalog);
            let connected = server.client.read().await.clone();
            if cold || server.definition.startup == Startup::Eager || connected.is_some() || expired
            {
                if let Ok(client) = self.start(server).await {
                    if expired || client.changed.load(Ordering::Acquire) {
                        if let Err(error) = self.discover(server, &client).await {
                            *server.diagnostic.write().await =
                                Some(self.sanitize(&server.id, format!("{error:#}")));
                        }
                    }
                }
            }
        }));
        let _ = tokio::time::timeout(Duration::from_secs(30), work).await;
        Ok(())
    }

    pub async fn tools(self: &Arc<Self>) -> Vec<(String, bool, Arc<dyn Tool>)> {
        self.catalog().await.0
    }

    /// Every method tool with its server id and the owner's
    /// `untrusted_ok` declaration, plus the prompt context.
    pub async fn catalog(self: &Arc<Self>) -> (Vec<(String, bool, Arc<dyn Tool>)>, String) {
        let mut tools = Vec::new();
        let mut registered_names = std::collections::HashSet::new();
        let mut lines = Vec::new();
        for server in self.servers.read().await.values() {
            if let Some(inventory) = server.inventory.read().await.as_ref() {
                let names: Vec<_> = inventory
                    .tools
                    .iter()
                    .map(|tool| {
                        proxy::registered_name(
                            &server.id,
                            tool["name"].as_str().unwrap_or_default(),
                        )
                    })
                    .collect();
                let mut unique = std::collections::HashSet::new();
                if names
                    .iter()
                    .any(|name| registered_names.contains(name) || !unique.insert(name.clone()))
                {
                    let error = "MCP catalog rejected: registered tool name collision";
                    *server.diagnostic.write().await = Some(error.into());
                    lines.push(format!("- {}: {error}", server.id));
                    continue;
                }
                registered_names.extend(names);
                lines.push(format!(
                    "- {}: {} catalog, {} tools",
                    server.id,
                    if server.fresh.load(Ordering::Acquire) {
                        "fresh"
                    } else {
                        "cached"
                    },
                    inventory.tools.len()
                ));
                if let Some(instructions) = &inventory.instructions {
                    lines.push(format!(
                        "External integration instructions for {}: {}",
                        server.id, instructions
                    ));
                }
                for descriptor in &inventory.tools {
                    tools.push((
                        server.id.clone(),
                        server.definition.untrusted_ok,
                        Arc::new(proxy::MethodTool::new(
                            self.clone(),
                            server.clone(),
                            descriptor.clone(),
                        )) as Arc<dyn Tool>,
                    ));
                }
            } else {
                lines.push(format!(
                    "- {}: unavailable; use MCP status for diagnostics",
                    server.id
                ));
            }
        }
        for id in self.invalid.read().await.keys() {
            lines.push(format!(
                "- {id}: invalid configuration; use MCP status for diagnostics"
            ));
        }
        let mut context = lines.join("\n");
        if context.len() > 8192 {
            let mut boundary = 8192;
            while !context.is_char_boundary(boundary) {
                boundary -= 1;
            }
            context.truncate(boundary);
            context.push_str("\nCatalog context truncated; use MCP status for the full inventory.");
        }
        (
            tools,
            if lines.is_empty() {
                String::new()
            } else {
                format!(
                    "MCP servers (use MCP status/refresh for diagnostics):\n{}",
                    context
                )
            },
        )
    }

    pub async fn status(&self) -> Value {
        let mut states = Vec::new();
        for server in self.servers.read().await.values() {
            let client = server.client.read().await;
            let inventory = server.inventory.read().await;
            let diagnostic = server.diagnostic.read().await.clone();
            states.push(json!({"server":server.id,"path":server.path,"state":if client.as_ref().is_some_and(|client| !client.is_closed()) {"ready"} else if diagnostic.is_some() {"failed"} else if server.startup.lock().await.is_some() {"starting"} else {"dormant"},"catalog":if inventory.is_none() {"unknown"} else if server.fresh.load(Ordering::Acquire) {"fresh"} else {"cached"},"tools":inventory.as_ref().map_or(0,|inventory| inventory.tools.len()),"error":diagnostic}));
        }
        for (id, (path, error)) in self.invalid.read().await.iter() {
            states.push(json!({"server":id,"path":path,"state":"invalid_config","catalog":"unknown","tools":0,"error":error}));
        }
        json!({"servers":states})
    }

    pub async fn context(&self) -> String {
        let states = self.status().await;
        let mut lines = Vec::new();
        if let Some(servers) = states["servers"].as_array() {
            for server in servers {
                lines.push(format!(
                    "- {}: {}, {} catalog, {} tools{}",
                    server["server"].as_str().unwrap_or("?"),
                    server["state"].as_str().unwrap_or("?"),
                    server["catalog"].as_str().unwrap_or("?"),
                    server["tools"],
                    server["error"]
                        .as_str()
                        .map_or(String::new(), |error| format!("; {error}"))
                ));
            }
        }
        for server in self.servers.read().await.values() {
            if let Some(instructions) = server
                .inventory
                .read()
                .await
                .as_ref()
                .and_then(|inventory| inventory.instructions.as_ref())
            {
                lines.push(format!(
                    "External integration instructions for {}: {}",
                    server.id, instructions
                ));
            }
        }
        if lines.is_empty() {
            String::new()
        } else {
            format!(
                "MCP servers (use MCP status/refresh for diagnostics):\n{}",
                lines.join("\n")
            )
        }
    }

    pub async fn operate(
        self: &Arc<Self>,
        action: &str,
        id: Option<&str>,
        params: &Value,
        context: &ToolContext,
    ) -> Result<Value> {
        self.validate_owner(context)?;
        let mut abort = context.abort_signal();
        ensure!(!context.is_aborted(), "MCP request cancelled");
        let cancelled = async {
            loop {
                if *abort.borrow() {
                    return;
                }
                if abort.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            () = self.stopped.cancelled() => bail!("MCP principal stopped"),
            () = cancelled => bail!("MCP request cancelled"),
            result = self.operate_inner(action,id,params,context) => result.map_err(|error| anyhow::anyhow!("{}",self.sanitize(id.unwrap_or_default(),format!("{error:#}")))),
        }
    }

    async fn operate_inner(
        self: &Arc<Self>,
        action: &str,
        id: Option<&str>,
        params: &Value,
        context: &ToolContext,
    ) -> Result<Value> {
        self.reconcile().await?;
        if action == "status" {
            return Ok(self.status().await);
        }
        if action == "refresh" && id.is_none() {
            let ids: Vec<_> = self.servers.read().await.keys().cloned().collect();
            for id in ids {
                let server = self.server(&id).await?;
                let client = self.start(&server).await?;
                self.discover(&server, &client).await?;
            }
            return Ok(self.status().await);
        }
        let id = id.context("this MCP action requires server")?;
        let server = self.server(id).await?;
        if action == "authenticate" {
            return self.authenticate(server).await;
        }
        if action == "reconnect" {
            if let Some(client) = server.client.write().await.take() {
                client.revoke();
            }
            *server.startup.lock().await = None;
            server.failures.store(0, Ordering::Release);
            server.retry_after.store(0, Ordering::Release);
            server.attempts.store(0, Ordering::Release);
        }
        let client = self.start(&server).await?;
        match action {
            "refresh" => { self.discover(&server,&client).await?; Ok(self.status().await) },
            "reconnect" => Ok(self.status().await),
            "resources" => Ok(json!({"resources":client.list("resources/list","resources").await?})),
            "resource_templates" => Ok(json!({"resourceTemplates":client.list("resources/templates/list","resourceTemplates").await?})),
            "prompts" => Ok(json!({"prompts":client.list("prompts/list","prompts").await?})),
            "read_resource" => client.request_with_context("resources/read",json!({"uri":params["uri"]}),None,Some(context)).await,
            "get_prompt" => client.request_with_context("prompts/get",json!({"name":params["name"],"arguments":params.get("arguments").cloned().unwrap_or(json!({}))}),None,Some(context)).await,
            _ => bail!("unknown MCP action"),
        }
    }

    fn sanitize(&self, id: &str, mut message: String) -> String {
        if let Some(vault) = &self.vault {
            for credential in vault.list_credentials(&crate::common::vault::CredentialFilter {
                namespace: Some(self.namespace(id)),
                include_system: true,
                ..Default::default()
            }) {
                if let Some(credential) = vault.get_credential(&credential.id) {
                    let secret = credential.material.expose_secret();
                    if !secret.is_empty() {
                        message = message.replace(secret, "[redacted]");
                    }
                    if let Some(refresh) = credential.metadata["refresh_token"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                    {
                        message = message.replace(refresh, "[redacted]");
                    }
                }
            }
        }
        message
    }

    pub async fn shutdown(&self) {
        self.stopped.cancel();
        for client in self
            .clients
            .lock()
            .await
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            client.revoke();
        }
        for server in self.servers.read().await.values() {
            server.current.store(false, Ordering::Release);
            server.revoked.cancel();
            if let Some(client) = server.client.read().await.as_ref() {
                client.revoke();
            }
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn digest(value: &Value) -> String {
    hex::encode(Sha256::digest(value.to_string().as_bytes()))
}
