//! Complete workspace definitions and explicit compatibility normalization.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[serde(deny_unknown_fields)]
pub enum ValueBinding {
    Credential { credential: String },
    Literal { value: String },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    StreamableHttp {
        url: String,
    },
    Sse {
        endpoint: String,
    },
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Authentication {
    #[default]
    None,
    Bearer {
        credential: String,
    },
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default)]
        client_id: Option<String>,
        #[serde(default)]
        authorization_endpoint: Option<String>,
        #[serde(default)]
        token_endpoint: Option<String>,
        #[serde(default)]
        scopes: Vec<String>,
        #[serde(default)]
        resource: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReservedParameter {
    Runtime { field: String },
    Static { value: Value },
    Credential { credential: String },
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Startup {
    #[default]
    Auto,
    Eager,
    Lazy,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default = "output_limit")]
    pub max_output_tokens: u32,
}
fn output_limit() -> u32 {
    4096
}
fn startup_timeout() -> u64 {
    30_000
}
fn schema_version() -> u32 {
    1
}
fn health_interval() -> u64 {
    30
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    #[serde(default = "schema_version")]
    pub schema_version: u32,
    pub transport: Transport,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, ValueBinding>,
    #[serde(default)]
    pub headers: BTreeMap<String, ValueBinding>,
    #[serde(default)]
    pub auth: Authentication,
    #[serde(default)]
    pub startup: Startup,
    #[serde(default = "startup_timeout")]
    pub startup_timeout_ms: u64,
    #[serde(default)]
    pub tool_timeout_ms: Option<u64>,
    #[serde(default = "health_interval")]
    pub health_check_interval_secs: u64,
    #[serde(default)]
    pub max_restarts: u32,
    #[serde(default)]
    pub reserved_parameters: BTreeMap<String, ReservedParameter>,
    #[serde(default)]
    pub sampling: Option<Sampling>,
    /// The owner's declaration that this server's methods are safe for
    /// runs acting for untrusted actors (ADR-078 D6). Off by default:
    /// servers usually carry the owner's credentials.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub untrusted_ok: bool,
}

impl Definition {
    pub fn parse(value: Value, path: &Path, id: &str) -> Result<Self> {
        let native = value.get("schema_version").is_some();
        let mut definition: Self =
            serde_json::from_value(if native { value } else { legacy(value, id)? })
                .context("invalid MCP configuration")?;
        ensure!(
            definition.schema_version == 1,
            "unsupported schema_version; expected 1"
        );
        ensure!(
            definition.startup_timeout_ms > 0,
            "startup_timeout_ms must be positive"
        );
        ensure!(
            definition.tool_timeout_ms != Some(0),
            "tool_timeout_ms must be positive or null"
        );
        let base = path.parent().context("manifest has no parent directory")?;
        definition.cwd = Some(match definition.cwd.take() {
            Some(cwd) if cwd.is_absolute() => cwd,
            Some(cwd) => base.join(cwd),
            None => base.to_path_buf(),
        });
        match &definition.transport {
            Transport::Stdio { command, .. } => {
                ensure!(!command.trim().is_empty(), "transport.command is empty");
                ensure!(
                    definition.headers.is_empty()
                        && matches!(definition.auth, Authentication::None),
                    "stdio cannot configure HTTP headers or authentication"
                );
            }
            Transport::StreamableHttp { url } | Transport::Sse { endpoint: url } => {
                let url = url::Url::parse(url).context("invalid MCP endpoint")?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "endpoint must be HTTP(S) without embedded credentials"
                );
                ensure!(
                    definition.env.is_empty(),
                    "HTTP cannot configure a process environment"
                );
                ensure!(
                    matches!(definition.auth, Authentication::None)
                        || !definition
                            .headers
                            .keys()
                            .any(|name| name.eq_ignore_ascii_case("authorization")),
                    "auth and Authorization header cannot both be configured"
                );
            }
        }
        for binding in definition.env.values().chain(definition.headers.values()) {
            if let ValueBinding::Credential { credential } = binding {
                validate_slot(credential)?;
            }
        }
        if let Authentication::Bearer { credential } = &definition.auth {
            validate_slot(credential)?;
        }
        for reserved in definition.reserved_parameters.values() {
            match reserved {
                ReservedParameter::Credential { credential } => validate_slot(credential)?,
                ReservedParameter::Runtime { field } => ensure!(
                    matches!(
                        field.as_str(),
                        "principal_id"
                            | "principal_name"
                            | "agent_id"
                            | "session_id"
                            | "workspace"
                            | "run_id"
                            | "tool_call_id"
                            | "acting_for"
                            | "acting_for_trust"
                    ),
                    "unsupported runtime field '{field}'"
                ),
                ReservedParameter::Static { .. } => {}
            }
        }
        if let Some(sampling) = &definition.sampling {
            ensure!(
                !matches!(definition.transport, Transport::Sse { .. }),
                "legacy SSE sampling is unsupported; use Streamable HTTP or stdio"
            );
            ensure!(
                sampling.max_output_tokens > 0,
                "sampling.max_output_tokens must be positive"
            );
        }
        Ok(definition)
    }

    pub fn connection_identity(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        let object = value
            .as_object_mut()
            .context("configuration is not an object")?;
        for field in [
            "tool_timeout_ms",
            "startup",
            "reserved_parameters",
            "untrusted_ok",
        ] {
            object.remove(field);
        }
        Ok(value)
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id != "."
            && id != ".."
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')),
        "server/credential ID must contain only ASCII letters, digits, '.', '_' or '-'"
    );
    Ok(())
}
fn validate_slot(slot: &str) -> Result<()> {
    validate_id(slot)
}

fn legacy(mut value: Value, id: &str) -> Result<Value> {
    let object = value
        .as_object_mut()
        .context("manifest must be an object")?;
    if let Some(name) = object.remove("name") {
        ensure!(name == id, "legacy name disagrees with directory ID; rename the directory to preserve callable names");
    }
    object.remove("version");
    if let Some(packages) = object.remove("packages") {
        let packages = packages.as_array().context("packages must be an array")?;
        ensure!(
            packages.len() == 1,
            "multiple packages require an explicit native transport definition"
        );
        if !object.contains_key("transport") {
            object.insert(
                "transport".into(),
                packages[0]
                    .get("transport")
                    .cloned()
                    .context("package has no transport")?,
            );
        }
    }
    let transport = object.remove("transport").context("missing transport")?;
    let mut transport = if let Some(kind) = transport.as_str() {
        json!({"type":kind})
    } else {
        transport
    };
    let transport_object = transport
        .as_object_mut()
        .context("transport must be a type or object")?;
    for key in ["command", "args", "endpoint"] {
        if let Some(value) = object.remove(key) {
            ensure!(
                !transport_object.contains_key(key),
                "duplicate transport.{key}"
            );
            transport_object.insert(key.into(), value);
        }
    }
    for key in ["env", "cwd", "auth", "reserved_parameters"] {
        if let Some(value) = transport_object.remove(key) {
            ensure!(!object.contains_key(key), "duplicate {key}");
            object.insert(key.into(), value);
        }
    }
    for field in ["env", "headers"] {
        if let Some(values) = object.get_mut(field).and_then(Value::as_object_mut) {
            for value in values.values_mut() {
                if value.is_string() {
                    *value = json!({"value":value.take()});
                }
            }
        }
    }
    if let Some(auth) = object.remove("auth") {
        if auth.get("type").is_some() {
            object.insert("auth".into(), auth);
        } else {
            let mut auth = auth.as_object().context("auth must be an object")?.clone();
            ensure!(
                !auth.contains_key("bearer_token"),
                "migrate bearer_token into an owner-scoped credential slot"
            );
            if let Some(headers) = auth.remove("headers") {
                object.insert(
                    "headers".into(),
                    headers
                        .as_object()
                        .context("headers must be an object")?
                        .iter()
                        .map(|(k, v)| (k.clone(), json!({"value":v})))
                        .collect::<serde_json::Map<_, _>>()
                        .into(),
                );
            }
            if !auth.is_empty() {
                if let Some(client_id) = auth.remove("oauth_client_id") {
                    auth.insert("client_id".into(), client_id);
                }
                auth.insert("type".into(), json!("oauth"));
                object.insert("auth".into(), auth.into());
            }
        }
    }
    if let Some(reserved) = object
        .get_mut("reserved_parameters")
        .and_then(Value::as_object_mut)
    {
        for parameter in reserved.values_mut() {
            if matches!(
                parameter.get("source").and_then(Value::as_str),
                Some("vault" | "env")
            ) {
                bail!("migrate reserved vault/env parameters to local credential slots");
            }
        }
    }
    let startup = object.remove("auto_start").unwrap_or(json!(true));
    object.insert(
        "startup".into(),
        json!(if startup == true { "eager" } else { "lazy" }),
    );
    if let Some(seconds) = object.remove("init_timeout_secs") {
        object.insert(
            "startup_timeout_ms".into(),
            json!(seconds
                .as_u64()
                .context("invalid init_timeout_secs")?
                .checked_mul(1000)
                .context("timeout overflow")?),
        );
    }
    if let Some(seconds) = object.remove("tool_timeout_secs") {
        object.insert(
            "tool_timeout_ms".into(),
            json!(seconds
                .as_u64()
                .context("invalid tool_timeout_secs")?
                .checked_mul(1000)
                .context("timeout overflow")?),
        );
    }
    ensure!(
        object.remove("bundle").is_none_or(|v| v == false)
            && object.remove("bundled_path").is_none_or(|v| v.is_null()),
        "legacy binary bundles require re-export as workspace files"
    );
    object.insert("schema_version".into(), json!(1));
    object.insert("transport".into(), transport);
    Ok(value)
}

pub struct Manifest {
    pub path: PathBuf,
    pub definition: Result<Definition>,
}

pub async fn scan(dir: &Path) -> Result<BTreeMap<String, Manifest>> {
    let mut out = BTreeMap::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let (id, path, ambiguous) = if entry.file_type().await?.is_dir() {
            let json = path.join("server.json");
            let yaml = path.join("manifest.yaml");
            let has_json = json.is_file();
            let has_yaml = yaml.is_file();
            if !has_json && !has_yaml {
                continue;
            }
            (
                name,
                if has_json { json } else { yaml },
                has_json && has_yaml,
            )
        } else if entry.file_type().await?.is_file() {
            if !matches!(
                path.extension().and_then(|p| p.to_str()),
                Some("json" | "yaml" | "yml")
            ) {
                continue;
            }
            (
                path.file_stem()
                    .context("manifest has no name")?
                    .to_string_lossy()
                    .into_owned(),
                path,
                false,
            )
        } else {
            continue;
        };
        let definition = async {
            validate_id(&id)?;
            ensure!(
                !ambiguous,
                "both server.json and manifest.yaml exist; retain one definition"
            );
            ensure!(
                tokio::fs::metadata(&path).await?.len() <= 1024 * 1024,
                "manifest exceeds 1 MiB"
            );
            let text = tokio::fs::read_to_string(&path).await?;
            let value = if path.extension().is_some_and(|ext| ext == "json") {
                serde_json::from_str(&text)?
            } else {
                let mut yaml: Value = serde_yaml::from_str(&text)?;
                let servers = yaml
                    .get_mut("mcp_servers")
                    .and_then(Value::as_object_mut)
                    .context("manifest.yaml missing mcp_servers mapping")?;
                ensure!(
                    servers.len() == 1 && servers.contains_key(&id),
                    "YAML must contain exactly the directory's server ID"
                );
                servers.remove(&id).context("missing server definition")?
            };
            Definition::parse(value, &path, &id)
        }
        .await;
        if let Some(previous) = out.insert(
            id.clone(),
            Manifest {
                path: path.clone(),
                definition,
            },
        ) {
            out.insert(
                id,
                Manifest {
                    path,
                    definition: Err(anyhow::anyhow!(
                        "duplicate server ID (also {})",
                        previous.path.display()
                    )),
                },
            );
        }
    }
    Ok(out)
}
