//! Common path resolution utilities
//!
//! This module provides standardized path resolution for Peko's
//! directory structure. All components (CLI, API, daemon) should use
//! these utilities for consistent path resolution.
//!
//! # Directory Structure (ADR-031)
//!
//! ## Config Directory (`{config_dir}`, e.g., `~/.peko`)
//! ```text
//! ~/.peko/                          # Config root
//! ├── agents/                       # Top-level agent storage
//! │   └── {agent}/
//! │       ├── config.toml           # Agent configuration
//! │       ├── tools/                # Agent-specific tools
//! │       └── skills/               # Agent-specific skills
//! └── principals/                   # Principal container configs
//!     └── {principal}/
//!         └── principal.toml        # Principal metadata
//!
//! ## Data Directory (`{data_dir}`, e.g., `~/.local/share/peko`)
//! ```text
//! {data_dir}/
//! ├── tools/                        # Downloaded/installed tools
//! ├── cron.json                     # Cron job database
//! ├── sessions/                     # Session history (*.jsonl)
//! │   └── {agent}/                  # Agent-scoped sessions
//! │       └── personal/             # Personal sessions
//! └── workspaces/                   # Agent workspace files
//!     └── {agent}/
//!         └── personal/             # Personal workspace
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;

use peko_session::safe_filename_component;

// =========================================================================
// Three-Tier Storage Layout (Phase A)
//
// Every byte on disk belongs to exactly one of three tiers. The layout
// structs below group the typed paths so the tier boundary is visible at
// the call site — a function taking `&LocalLayout` cannot accidentally be
// passed a `&SharedLayout`.
//
// | Tier    | Owner    | On-disk root                              |
// |---------|----------|-------------------------------------------|
// | Local   | Principal| {data_dir}/principals/{name}/local/       |
// | Shared  | Principal| {config_dir}/principals/{name}/           |
// | Runtime | Runtime  | {data_dir}/runtime/                       |
//
// Local tier contents are runtime-only state — never packaged.
// Shared tier contents are per-principal capability-bearing config — packaged.
// Runtime tier contents are installed once for the runtime; principals
// access them via capability grants recorded as LINKs in the bundle.
// =========================================================================

/// The storage tier a path belongs to.
///
/// `Serialize`/`Deserialize` are added so the layouts can be transported
/// over IPC for Phase E's `principal_inspect` / `runtime_inspect` verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Per-principal runtime state. Never packaged, never shared.
    Local,
    /// Per-principal capability-bearing config. Packaged into bundles.
    Shared,
    /// Runtime-wide state. Installed once; principals access via grants.
    Runtime,
}

/// Typed paths under `{data_dir}/principals/{name}/local/`.
///
/// `root` is the canonical root; every other field is `root.join(...)`.
/// Callers MUST go through `PathResolver::principal_layout(name).local`
/// rather than hand-rolling `.join("local")`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalLayout {
    /// `{data_dir}/principals/{name}/local`
    pub root: PathBuf,
    /// `…/local/sessions/` — append-only JSONL event log.
    pub sessions_dir: PathBuf,
    /// `…/local/memory_index.json` — session metadata index.
    pub memory_index: PathBuf,
    /// `…/local/cron/` — per-principal cron schedule + history.
    pub cron_dir: PathBuf,
    /// `…/local/cron/schedule.toml`
    pub cron_schedule: PathBuf,
    /// `…/local/cron/history.log`
    pub cron_history: PathBuf,
    /// `…/local/cache/` — tool scratch space.
    pub cache_dir: PathBuf,
    /// `…/local/locks/` — principal-scoped lock files.
    pub locks_dir: PathBuf,
    /// `…/local/plans/` — file-backed Plan DAG storage (PR #1 of the
    /// wiring sequence). One `<plan_id>.jsonl` per plan; `FileLock`-
    /// coordinated atomic writes; per-principal scope matches the
    /// runtime-only nature of plan state (never packaged, never shared).
    pub plans_dir: PathBuf,
}

/// Typed paths under `{config_dir}/principals/{name}/`.
///
/// `root` is the canonical root; every other field is `root.join(...)`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SharedLayout {
    /// `{config_dir}/principals/{name}`
    pub root: PathBuf,
    /// `…/principal.toml`
    pub config_file: PathBuf,
    /// `…/identity.json` (public DID). Private keys stay in keychain.
    pub identity_file: PathBuf,
    /// `…/agents/` — agent definitions.
    pub agents_dir: PathBuf,
    /// `…/memory/snapshots/` — optional portable memory snapshots.
    pub memory_snapshots_dir: PathBuf,
    /// `…/mcps/` — principal-owned MCP server configs.
    pub mcps_dir: PathBuf,
}

/// All typed paths for a single principal. The `name` is stored so
/// `PrincipalLayout` is self-describing (no need to thread the principal
/// name alongside the layout when passing across boundaries).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PrincipalLayout {
    pub name: String,
    pub local: LocalLayout,
    pub shared: SharedLayout,
}

/// Typed paths for the runtime-global bucket under `{data_dir}/runtime/`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuntimeLayout {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    /// `{data_dir}/runtime` — bucket root.
    pub runtime_dir: PathBuf,
    /// `{data_dir}/runtime/extensions` — one install per extension id.
    pub extensions_root: PathBuf,
    /// `{data_dir}/runtime/mcps` — one install per MCP server id.
    pub mcps_root: PathBuf,
    /// `{data_dir}/runtime/registry` — OCI layers + manifests cache.
    pub registry_root: PathBuf,
    /// `{data_dir}/runtime/locks` — runtime-wide lock files.
    pub locks_dir: PathBuf,
    /// `{data_dir}/runtime/pending-requests` — ADR-045 PR #3 self-modify
    /// queue. One `<uuid>.json` per pending request, mode 0600. PR #4
    /// adds `peko pending list/decide` and `rehydrate()` at startup.
    pub pending_requests_dir: PathBuf,
    /// `{config_dir}/principals` — convenience accessor for principal index.
    pub principals_root: PathBuf,
}

/// Expand a leading `~` in a path string to the user's home directory.
///
/// - `"~/foo"` → `{home}/foo`
/// - `"~"` → `{home}`
/// - `"/abs/path"` and `"relative"` are returned unchanged
///
/// This is intentionally a string-in/string-out helper so it can be
/// applied before validating whether the result is absolute.
#[must_use]
pub fn expand_tilde(path: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref();
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        let mut home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
        home.push(rest);
        return home;
    }
    PathBuf::from(path)
}

/// Environment variable override for Peko's home directory.
const PEKO_HOME_ENV: &str = "PEKO_HOME";

/// Get the default configuration directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// `~/.peko` or the current directory's `.peko` folder.
#[must_use]
pub fn default_config_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home);
    }
    dirs::home_dir().map_or_else(|| PathBuf::from(".").join(".peko"), |d| d.join(".peko"))
}

/// Get the default data directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// `~/.local/share/peko` on Linux,
/// `~/Library/Application Support/peko` on macOS,
/// or `%APPDATA%/peko` on Windows.
/// Falls back to the config directory if `data_dir` is not available.
pub fn default_data_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home).join("data");
    }
    dirs::data_dir().map_or_else(default_config_dir, |d| d.join("peko"))
}

/// Get the default cache directory
///
/// Checks `PEKO_HOME` environment variable first, then falls back to
/// platform cache directory. Falls back to `{data_dir}/cache` if
/// `cache_dir` is not available.
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    if let Ok(peko_home) = std::env::var(PEKO_HOME_ENV) {
        return PathBuf::from(peko_home).join("cache");
    }
    dirs::cache_dir().map_or_else(|| default_data_dir().join("cache"), |d| d.join("peko"))
}

/// Path resolver for Peko's directory structure
///
/// This struct provides methods to resolve paths for agents,
/// sessions, and workspaces. It uses the configured base directories
/// and applies the personal-only path structure consistently.
///
/// # Path Categories
///
/// - **Config paths** (`config_dir`): Agent configurations, credentials
/// - **Data paths** (`data_dir`): Sessions, workspaces, tools, cron database
/// - **Cache paths** (`cache_dir`): Temporary files, downloaded caches
#[derive(Debug, Clone)]
pub struct PathResolver {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl Default for PathResolver {
    fn default() -> Self {
        Self {
            config_dir: default_config_dir(),
            data_dir: default_data_dir(),
            cache_dir: default_cache_dir(),
        }
    }
}

impl PathResolver {
    /// Create a new path resolver with default directories
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a path resolver with custom directories
    #[must_use]
    pub fn with_dirs(config_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
            cache_dir,
        }
    }

    /// Create a path resolver from optional overrides
    ///
    /// Uses the provided overrides if available, otherwise uses defaults.
    pub fn from_overrides(
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            config_dir: config_dir.unwrap_or_else(default_config_dir),
            data_dir: data_dir.unwrap_or_else(default_data_dir),
            cache_dir: cache_dir.unwrap_or_else(default_cache_dir),
        }
    }

    /// Get the config directory
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Get the data directory
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Get the cache directory
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    // ====================================================================================
    // Config Directory Paths (configuration, metadata)
    // ====================================================================================

    /// Get the top-level agents directory
    ///
    /// Path: `{config_dir}/agents`
    #[must_use]
    pub fn agents_root_dir(&self) -> PathBuf {
        self.config_dir.join("agents")
    }

    /// Get the path to an agent's config file
    ///
    /// Path: `{config_dir}/agents/{agent}/config.toml`
    #[must_use]
    pub fn agent_config(&self, agent: &str) -> PathBuf {
        self.agents_root_dir().join(agent).join("config.toml")
    }

    /// Get the MCP configuration file path
    ///
    /// Path: `{config_dir}/mcp.toml`
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.config_dir.join("mcp.toml")
    }

    /// Get the principals configuration directory
    ///
    /// Path: `{config_dir}/principals`
    #[must_use]
    pub fn principals_root_dir(&self) -> PathBuf {
        self.config_dir.join("principals")
    }

    /// Get a specific principal's configuration directory
    ///
    /// Path: `{config_dir}/principals/{principal}`
    #[must_use]
    pub fn principal_dir(&self, principal: &str) -> PathBuf {
        self.principals_root_dir().join(principal)
    }

    /// F20: Get the peers configuration root directory.
    ///
    /// Path: `{config_dir}/peers`
    #[must_use]
    pub fn peers_root_dir(&self) -> PathBuf {
        self.config_dir.join("peers")
    }

    /// F20: Get a specific peer's configuration directory.
    ///
    /// Path: `{config_dir}/peers/{peer_id}`
    #[must_use]
    pub fn peer_dir(&self, peer_id: &str) -> PathBuf {
        self.peers_root_dir().join(peer_id)
    }

    /// F20: Get the path to a peer's config file.
    ///
    /// Path: `{config_dir}/peers/{peer_id}/peer.toml`
    #[must_use]
    pub fn peer_config(&self, peer_id: &str) -> PathBuf {
        self.peer_dir(peer_id).join("peer.toml")
    }

    /// Get the path to a principal's identity storage directory.
    ///
    /// Returns the directory `{config_dir}/principals/{name}/identity/`.
    /// Kept as a thin wrapper because some legacy code paths (Phase A
    /// IPC handlers, manager internals) still reach for the bare path;
    /// migrate them to `principal_layout(name).shared.root.join("identity")`
    /// as you touch them.
    #[must_use]
    pub fn principal_identity_path(&self, principal: &str) -> PathBuf {
        self.principal_layout(principal).shared.root.join("identity")
    }

    // ========================================================================
    // Phase A: Three-Tier Storage Layout — typed accessors
    // ========================================================================

    /// Resolve the full typed layout for a single principal.
    ///
    /// This is the canonical way to ask "where does X for principal Y live?"
    /// after Phase A. The returned struct groups Local and Shared paths so
    /// callers can pass `layout.local` or `layout.shared` to functions that
    /// need a single tier's worth of paths.
    ///
    /// See [`PrincipalLayout`] for the field semantics.
    #[must_use]
    pub fn principal_layout(&self, principal: &str) -> PrincipalLayout {
        let principals_root = self.principals_root_dir();
        let data_root = self.data_dir.join("principals").join(principal);

        let local_root = data_root.join("local");
        let shared_root = principals_root.join(principal);

        PrincipalLayout {
            name: principal.to_string(),
            local: LocalLayout {
                root: local_root.clone(),
                sessions_dir: local_root.join("sessions"),
                memory_index: local_root.join("memory_index.json"),
                cron_dir: local_root.join("cron"),
                cron_schedule: local_root.join("cron").join("schedule.toml"),
                cron_history: local_root.join("cron").join("history.log"),
                cache_dir: local_root.join("cache"),
                locks_dir: local_root.join("locks"),
                plans_dir: local_root.join("plans"),
            },
            shared: SharedLayout {
                root: shared_root.clone(),
                config_file: shared_root.join("principal.toml"),
                identity_file: shared_root.join("identity.json"),
                agents_dir: shared_root.join("agents"),
                memory_snapshots_dir: shared_root.join("memory").join("snapshots"),
                mcps_dir: shared_root.join("mcps"),
            },
        }
    }

    /// Resolve the typed layout for runtime-global state.
    ///
    /// Use this anywhere you need to compute a path under `{data_dir}/runtime/`
    /// (extension install root, MCP server root, OCI registry cache,
    /// runtime locks) so the layout is centralized.
    #[must_use]
    pub fn runtime_layout(&self) -> RuntimeLayout {
        let runtime_dir = self.data_dir.join("runtime");
        RuntimeLayout {
            config_dir: self.config_dir.clone(),
            data_dir: self.data_dir.clone(),
            runtime_dir: runtime_dir.clone(),
            extensions_root: runtime_dir.join("extensions"),
            mcps_root: runtime_dir.join("mcps"),
            registry_root: runtime_dir.join("registry"),
            locks_dir: runtime_dir.join("locks"),
            // ADR-045 PR #3: durable on-disk queue for self-modify
            // requests. Lives under `runtime/` (not `run/`) because it
            // is part of the runtime data plane, not IPC auth state.
            pending_requests_dir: runtime_dir.join("pending-requests"),
            principals_root: self.principals_root_dir(),
        }
    }

    /// Runtime-global extension install root.
    ///
    /// Path: `{data_dir}/runtime/extensions`
    ///
    /// Each extension id gets a subdirectory here. Use
    /// `extensions_root().join(id)` for the per-extension dir.
    #[must_use]
    pub fn extensions_root(&self) -> PathBuf {
        self.runtime_layout().extensions_root
    }

    /// Runtime-global MCP server install root.
    ///
    /// Path: `{data_dir}/runtime/mcps`
    #[must_use]
    pub fn mcps_root(&self) -> PathBuf {
        self.runtime_layout().mcps_root
    }

    /// OCI registry cache root.
    ///
    /// Path: `{data_dir}/runtime/registry`
    #[must_use]
    pub fn registry_root(&self) -> PathBuf {
        self.runtime_layout().registry_root
    }

    /// Runtime-wide lock directory.
    ///
    /// Path: `{data_dir}/runtime/locks`
    #[must_use]
    pub fn runtime_locks_dir(&self) -> PathBuf {
        self.runtime_layout().locks_dir
    }

    /// Pending self-modification request queue (ADR-045 PR #3).
    ///
    /// Path: `{data_dir}/runtime/pending-requests`
    ///
    /// One `<uuid>.json` file per pending request, mode 0600.
    /// The runtime writes here when `peko_self` is called; the
    /// daemon reads it on `peko pending list` (PR #4) and at
    /// startup via `ApprovalQueue::rehydrate` (also PR #4).
    #[must_use]
    pub fn pending_requests_dir(&self) -> PathBuf {
        self.runtime_layout().pending_requests_dir
    }

    /// Per-principal cron schedule file (Local tier).
    ///
    /// Path: `{data_dir}/principals/{principal}/local/cron/schedule.toml`
    #[must_use]
    pub fn cron_schedule(&self, principal: &str) -> PathBuf {
        self.principal_layout(principal).local.cron_schedule
    }

    /// Resolve a `PrincipalId` (DID) to the on-disk principal name.
    ///
    /// Scans `{config_dir}/principals/` for `principal.toml` files whose
    /// `did` field matches the given id. Returns `None` if no
    /// `principal.toml` matches — including the case where the
    /// directory is empty or missing (a freshly initialized runtime).
    ///
    /// **Phase B.** This is the DID → name lookup the tier-typed
    /// authority needs to convert `PrincipalId` accessors into
    /// `PathResolver::principal_layout(name)` calls. The scan is
    /// best-effort and linear in the principal count, which is fine —
    /// there are at most a few dozen principals on a typical install,
    /// and the result is cached inside `RuntimeAuthority` once the
    /// principal layout is materialised.
    ///
    /// The scan reads `principal.toml` as a generic `toml::Value` so
    /// `paths.rs` does not need to depend on `crate::principal::config`
    /// (which would create a cycle — `principal` depends on `common`).
    #[must_use]
    pub fn lookup_principal_name(&self, principal_id: &peko_subject::PrincipalId) -> Option<String> {
        let principals_root = self.principals_root_dir();
        let entries = std::fs::read_dir(&principals_root).ok()?;
        let needle = principal_id.0.as_str();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let config_path = path.join("principal.toml");
            let contents = match std::fs::read_to_string(&config_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let value: toml::Value = match contents.parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let did = value
                .get("did")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if did == needle {
                // The on-disk `name` field is the canonical name; fall
                // back to the directory name if `name` is missing.
                let name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string()
                    });
                return Some(name);
            }
        }
        None
    }

    /// Inverse of [`Self::lookup_principal_name`]: scan for the
    /// `PrincipalId` whose `principal.toml` carries the given name.
    ///
    /// Used by CLI commands that take a principal *name* from `--principal`
    /// but need to populate a wire-shape `PrincipalId`. Returns `None`
    /// if the principal doesn't exist on disk. Cost is the same as the
    /// DID scan: O(principals).
    #[must_use]
    pub fn lookup_principal_id_by_name(&self, principal_name: &str) -> Option<peko_subject::PrincipalId> {
        let principals_root = self.principals_root_dir();
        let entries = std::fs::read_dir(&principals_root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            // Match either by directory name (cheap) or by `name` field
            // (authoritative when both are present).
            if dir_name != principal_name {
                let config_path = path.join("principal.toml");
                let contents = match std::fs::read_to_string(&config_path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let value: toml::Value = match contents.parse() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let declared_name = value
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if declared_name != principal_name {
                    continue;
                }
            }
            let config_path = path.join("principal.toml");
            let contents = std::fs::read_to_string(&config_path).ok()?;
            let value: toml::Value = contents.parse().ok()?;
            let did = value.get("did").and_then(|v| v.as_str()).unwrap_or("");
            return Some(peko_subject::PrincipalId(did.to_string()));
        }
        None
    }

    /// Per-principal cron history log (Local tier).
    ///
    /// Path: `{data_dir}/principals/{principal}/local/cron/history.log`
    #[must_use]
    pub fn cron_history(&self, principal: &str) -> PathBuf {
        self.principal_layout(principal).local.cron_history
    }

    /// Per-principal cron directory (Local tier).
    ///
    /// Path: `{data_dir}/principals/{principal}/local/cron`
    #[must_use]
    pub fn principal_cron_dir(&self, principal: &str) -> PathBuf {
        self.principal_layout(principal).local.cron_dir
    }

    /// Get the runtime directory
    ///
    /// Path: `{config_dir}/runtime`
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.config_dir.join("runtime")
    }

    /// Get the runtime identity file path
    ///
    /// Path: `{config_dir}/runtime/identity.toml`
    #[must_use]
    pub fn runtime_identity(&self) -> PathBuf {
        self.runtime_dir().join("identity.toml")
    }

    /// Get the runtime metadata file path
    ///
    /// Path: `{config_dir}/runtime/runtime.toml`
    #[must_use]
    pub fn runtime_metadata(&self) -> PathBuf {
        self.runtime_dir().join("runtime.toml")
    }

    /// Get the known runtimes file path
    ///
    /// Path: `{config_dir}/runtime/known_runtimes.toml`
    #[must_use]
    pub fn known_runtimes(&self) -> PathBuf {
        self.runtime_dir().join("known_runtimes.toml")
    }

    /// Get the auth config file path
    ///
    /// Path: `{config_dir}/runtime/auth_config.toml`
    #[must_use]
    pub fn auth_config(&self) -> PathBuf {
        self.runtime_dir().join("auth_config.toml")
    }

    /// Get the API keys file path
    ///
    /// Path: `{config_dir}/runtime/api_keys.toml`
    #[must_use]
    pub fn api_keys(&self) -> PathBuf {
        self.runtime_dir().join("api_keys.toml")
    }

    /// Get the pekohub config file path
    ///
    /// Path: `{config_dir}/runtime/pekohub.toml`
    #[must_use]
    pub fn pekohub_config(&self) -> PathBuf {
        self.runtime_dir().join("pekohub.toml")
    }

    /// Get the IPC run directory.
    ///
    /// Path: `{config_dir}/run` — this is the directory the daemon
    /// socket (`daemon.sock`), pid file (`daemon.pid`), and the
    /// session-auth artifacts (`auth-code`, `auth-token-<sid>`)
    /// live in. Distinct from `runtime_dir()` (`{config_dir}/runtime`)
    /// which holds structured config files; `run_dir()` is for
    /// ephemeral sockets and short-lived secret material.
    #[must_use]
    pub fn run_dir(&self) -> PathBuf {
        self.config_dir.join("run")
    }

    /// Get the startup auth-code file path.
    ///
    /// Path: `{config_dir}/run/auth-code` (mode 0600). The daemon
    /// writes the diceware code here at startup and deletes it on
    /// shutdown or after first successful submission.
    #[must_use]
    pub fn auth_code_file(&self) -> PathBuf {
        self.run_dir().join("auth-code")
    }

    /// Get the per-session auth-token file path.
    ///
    /// Path: `{config_dir}/run/auth-token-{sid}` (mode 0600).
    /// Keyed by Unix session ID so multiple terminals (each with
    /// its own SID) keep independent tokens.
    #[must_use]
    pub fn auth_token_file(&self, sid: i32) -> PathBuf {
        self.run_dir().join(format!("auth-token-{sid}"))
    }

    /// Get the encrypted vault file path
    ///
    /// Path: `{config_dir}/vault.enc`
    #[must_use]
    pub fn vault(&self) -> PathBuf {
        self.config_dir.join("vault.enc")
    }

    /// Get the Universal Tools directory
    ///
    /// Path: `{data_dir}/tools`
    #[must_use]
    pub fn universal_tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// Get the Skills directory
    ///
    /// Path: `{data_dir}/skills`
    #[must_use]
    pub fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }

    /// Get the Agents directory
    ///
    /// Path: `{data_dir}/agents`
    #[must_use]
    pub fn agents_dir(&self) -> PathBuf {
        self.data_dir.join("agents")
    }

    /// Get the Slash Commands directory
    ///
    /// Path: `{data_dir}/commands`
    #[must_use]
    pub fn commands_dir(&self) -> PathBuf {
        self.data_dir.join("commands")
    }

    // ====================================================================================
    // Data Directory Paths (sessions, workspaces, runtime data)
    // ====================================================================================

    /// Get the sessions root directory
    ///
    /// Path: `{data_dir}/sessions`
    #[must_use]
    pub fn sessions_root(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    /// Get the agent-scoped sessions root directory
    ///
    /// Path: `{data_dir}/sessions/{agent}`
    #[must_use]
    pub fn agent_sessions_root(&self, agent: &str) -> PathBuf {
        self.sessions_root().join(agent)
    }

    /// Get the personal sessions directory for an agent
    ///
    /// Path: `{data_dir}/sessions/{agent}/personal`
    #[must_use]
    pub fn agent_sessions_dir(&self, agent: &str) -> PathBuf {
        self.agent_sessions_root(agent).join("personal")
    }

    /// Get the path to an agent's session file
    ///
    /// Path: `{data_dir}/sessions/{agent}/personal/{session_id}.jsonl`
    #[must_use]
    pub fn agent_session_file(&self, agent: &str, session_id: &str) -> PathBuf {
        self.agent_sessions_dir(agent)
            .join(format!("{}.jsonl", safe_filename_component(session_id)))
    }

    /// Get the workspaces root directory
    ///
    /// Path: `{data_dir}/workspaces`
    #[must_use]
    pub fn workspaces_root(&self) -> PathBuf {
        self.data_dir.join("workspaces")
    }

    /// Get the agent-scoped workspaces root directory
    ///
    /// Path: `{data_dir}/workspaces/{agent}`
    #[must_use]
    pub fn agent_workspaces_root(&self, agent: &str) -> PathBuf {
        self.workspaces_root().join(agent)
    }

    /// Get the personal workspace directory for an agent
    ///
    /// Path: `{data_dir}/workspaces/{agent}/personal`
    #[must_use]
    pub fn agent_workspace(&self, agent: &str) -> PathBuf {
        self.agent_workspaces_root(agent).join("personal")
    }

    /// Get the tools directory
    ///
    /// Path: `{data_dir}/tools`
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.data_dir.join("tools")
    }

    /// Get the async tasks directory
    ///
    /// Path: `{data_dir}/async_tasks`
    #[must_use]
    pub fn async_tasks_dir(&self) -> PathBuf {
        self.data_dir.join("async_tasks")
    }

    /// Runtime-owned chat-log root directory.
    ///
    /// Distinct from the principal-owned session JSONL: chat logs are
    /// sharded by `(principal_did, peer)` and capture only the consumer-
    /// visible message stream (user↔principal, principal↔principal).
    /// They survive across session resets / compaction because they are
    /// append-only and external to the principal's mutable working
    /// memory. Deleting a principal deletes only that principal's own
    /// chat-log shards.
    ///
    /// Path: `{data_dir}/chat_logs`
    #[must_use]
    pub fn chat_logs_dir(&self) -> PathBuf {
        self.data_dir.join("chat_logs")
    }

    // ====================================================================================
    // Utility Methods
    // ====================================================================================

    /// Ensure all base directories exist
    ///
    /// Creates directories if they don't exist. Returns Ok(()) if successful
    /// or if directories already exist.
    ///
    /// **Phase A.** Also creates the runtime-global bucket at
    /// `{data_dir}/runtime/{extensions,mcps,registry,locks}` so daemon
    /// startup is a no-op when the bucket is fresh.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        std::fs::create_dir_all(self.chat_logs_dir())?;
        // ADR-045 PR #2: `run/` holds the daemon socket, pid file,
        // and session-auth artifacts (auth-code, auth-token-<sid>).
        std::fs::create_dir_all(self.run_dir())?;
        // Phase A: runtime-global bucket.
        let runtime = self.runtime_layout();
        std::fs::create_dir_all(&runtime.extensions_root)?;
        std::fs::create_dir_all(&runtime.mcps_root)?;
        std::fs::create_dir_all(&runtime.registry_root)?;
        std::fs::create_dir_all(&runtime.locks_dir)?;
        // ADR-045 PR #3: durable queue for self-modify requests.
        // Owner-only (0700): the bucket holds per-request 0600 JSON files
        // whose contents reveal the principal's intent (capabilities,
        // edit paths, schedule targets). World-readable `create_dir_all`
        // would defeat the file mode.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&runtime.pending_requests_dir)?;
        Ok(())
    }

    /// Ensure a principal's tier directories exist (Shared + Local).
    ///
    /// Idempotent. Called from `PrincipalManager::create` so each new
    /// principal starts with a fully-formed layout.
    pub fn ensure_principal_dirs(&self, principal: &str) -> std::io::Result<()> {
        let layout = self.principal_layout(principal);
        // Shared tier.
        std::fs::create_dir_all(&layout.shared.root)?;
        std::fs::create_dir_all(&layout.shared.agents_dir)?;
        std::fs::create_dir_all(&layout.shared.memory_snapshots_dir)?;
        std::fs::create_dir_all(&layout.shared.mcps_dir)?;
        // Local tier.
        std::fs::create_dir_all(&layout.local.root)?;
        std::fs::create_dir_all(&layout.local.sessions_dir)?;
        std::fs::create_dir_all(&layout.local.cron_dir)?;
        std::fs::create_dir_all(&layout.local.cache_dir)?;
        std::fs::create_dir_all(&layout.local.locks_dir)?;
        std::fs::create_dir_all(&layout.local.plans_dir)?;
        Ok(())
    }

    /// Ensure an agent's data directories exist
    ///
    /// Creates the personal sessions and workspace directories for the agent.
    pub fn ensure_agent_data_dirs(&self, agent: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(self.agent_sessions_dir(agent))?;
        std::fs::create_dir_all(self.agent_workspace(agent))?;
        Ok(())
    }
}

// =============================================================================
// `PathResolver` impl — narrow cross-boundary view used by the extension
// framework's `ExtensionStore::load_all_with` (host) and any other host
// crate that needs the data-directory layout. The trait ships in the
// `peko-extension-host` crate (Phase 8 commit 2); root's concrete
// `PathResolver` impls it via single-method delegation.
// =============================================================================

impl crate::extensions::framework::paths::PathResolver for PathResolver {
    fn skills_dir(&self) -> PathBuf {
        PathResolver::skills_dir(self)
    }

    fn agents_dir(&self) -> PathBuf {
        PathResolver::agents_dir(self)
    }

    fn commands_dir(&self) -> PathBuf {
        PathResolver::commands_dir(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_tilde_home() {
        let home = dirs::home_dir().expect("should have home dir");
        assert_eq!(expand_tilde("~/foo"), home.join("foo"));
        assert_eq!(expand_tilde("~"), home);
    }

    #[test]
    fn test_expand_tilde_unchanged() {
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("relative"), PathBuf::from("relative"));
        assert_eq!(expand_tilde("./relative"), PathBuf::from("./relative"));
    }

    #[test]
    fn test_path_resolver_default() {
        // Some earlier tests (notably subagent_integration_tests) leak a
        // temp `PEKO_HOME` via `Box::leak`-ed fixtures, so by the time this
        // test runs the env var may already be set. Clear it for the
        // duration of the assertion so we exercise the *default* branch
        // (home_dir().join(".peko")).
        let saved_peko_home = std::env::var("PEKO_HOME").ok();
        // SAFETY: tests in the same process don't run in parallel for
        // env-var-sensitive paths (cargo test default), and we restore the
        // value immediately below.
        unsafe { std::env::remove_var("PEKO_HOME") };
        let resolver = PathResolver::new();
        let config_dir = resolver.config_dir().to_string_lossy().to_string();
        if let Some(v) = saved_peko_home {
            // SAFETY: same as above.
            unsafe { std::env::set_var("PEKO_HOME", v) };
        }
        assert!(
            config_dir.contains(".peko"),
            "default config dir should contain '.peko', got: {config_dir}"
        );
    }

    // ====================================================================================
    // New Layout Path Tests
    // ====================================================================================

    #[test]
    fn test_new_layout_agent_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(resolver.agents_root_dir(), PathBuf::from("/config/agents"));

        assert_eq!(
            resolver.agent_config("alice"),
            PathBuf::from("/config/agents/alice/config.toml")
        );
    }

    #[test]
    fn test_new_layout_session_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_sessions_dir("alice"),
            PathBuf::from("/data/sessions/alice/personal")
        );

        assert_eq!(
            resolver.agent_session_file("alice", "sess-1"),
            PathBuf::from("/data/sessions/alice/personal/sess-1.jsonl")
        );
    }

    #[test]
    fn test_new_layout_workspace_paths() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );

        assert_eq!(
            resolver.agent_workspace("alice"),
            PathBuf::from("/data/workspaces/alice/personal")
        );
    }

    // ====================================================================================
    // Phase A three-tier storage layout tests
    //
    // PR 3: previously the only paths.rs tests covered the legacy
    // `agents/`, `sessions/`, and `workspaces/` accessors. The
    // principal-scoped `Local`/`Shared`/`Runtime` layouts had no
    // coverage. These tests pin the field presence and root splits
    // so a future refactor can't silently move a subdirectory
    // between tiers.
    // ====================================================================================

    #[test]
    fn three_tier_principal_layout_local_field_presence() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let layout = resolver.principal_layout("alice");
        let local = layout.local;
        // Local root lives under `<data_dir>/principals/<name>/local/`.
        assert!(local.root.ends_with("principals/alice/local"));
        assert!(local.sessions_dir.ends_with("alice/local/sessions"));
        assert!(local.memory_index.ends_with("alice/local/memory_index.json"));
        assert!(local.cron_dir.ends_with("alice/local/cron"));
        assert!(local.cron_schedule.ends_with("alice/local/cron/schedule.toml"));
        assert!(local.cron_history.ends_with("alice/local/cron/history.log"));
        assert!(local.cache_dir.ends_with("alice/local/cache"));
        assert!(local.locks_dir.ends_with("alice/local/locks"));
        assert!(local.plans_dir.ends_with("alice/local/plans"));
    }

    #[test]
    fn three_tier_principal_layout_shared_field_presence() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let layout = resolver.principal_layout("alice");
        let shared = layout.shared;
        // Shared root lives under `<config_dir>/principals/<name>/`.
        assert!(shared.root.ends_with("principals/alice"));
        assert!(shared.config_file.ends_with("principals/alice/principal.toml"));
        assert!(shared.agents_dir.ends_with("principals/alice/agents"));
        assert!(shared.identity_file.ends_with("principals/alice/identity.json"));
        assert!(shared.memory_snapshots_dir
            .ends_with("principals/alice/memory/snapshots"));
        assert!(shared.mcps_dir.ends_with("principals/alice/mcps"));
    }

    #[test]
    fn three_tier_principal_layout_local_and_shared_roots_disjoint() {
        // The Local and Shared roots MUST live on different filesystem
        // hierarchies — Local is per-host per-uid (data_dir) while
        // Shared is per-principal-portable (config_dir). A future
        // refactor that moves one of these would break the
        // Local/Shared tier split; this test pins the contract.
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let layout = resolver.principal_layout("alice");
        assert!(layout.local.root.starts_with("/data/"));
        assert!(layout.shared.root.starts_with("/config/"));
        assert_ne!(layout.local.root, layout.shared.root);
    }

    #[test]
    fn three_tier_runtime_layout_field_presence() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let runtime = resolver.runtime_layout();
        // Runtime root lives under `<data_dir>/runtime/`.
        assert!(runtime.runtime_dir.ends_with("/data/runtime"));
        assert!(runtime
            .extensions_root
            .ends_with("/data/runtime/extensions"));
        assert!(runtime.mcps_root.ends_with("/data/runtime/mcps"));
        assert!(runtime.registry_root.ends_with("/data/runtime/registry"));
        assert!(runtime.locks_dir.ends_with("/data/runtime/locks"));
        assert!(runtime
            .pending_requests_dir
            .ends_with("/data/runtime/pending-requests"));
        assert!(runtime.principals_root.ends_with("/config/principals"));
    }

    #[test]
    fn pending_requests_dir_lives_under_data_runtime_not_config() {
        // ADR-045 PR #3 invariant: pending-request artifacts are
        // runtime data (ephemeral-but-durable, queue-shaped) so they
        // belong under `<data_dir>/runtime/`, NOT under
        // `<config_dir>/runtime/` (portable config) or `<run_dir>/`
        // (IPC auth state). This test pins the contract so a future
        // refactor that swaps to `config_dir.join("runtime")` is caught.
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let dir = resolver.pending_requests_dir();
        assert!(dir.starts_with("/data/runtime/"));
        assert!(!dir.starts_with("/config/"));
        assert!(!dir.starts_with("/data/run/"));
    }

    #[test]
    fn pending_requests_dir_is_distinct_from_other_runtime_buckets() {
        // Distinct from `extensions_root`, `mcps_root`, `registry_root`,
        // `locks_dir`. Each bucket owns its own subdirectory name.
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let layout = resolver.runtime_layout();
        let other = [
            &layout.extensions_root,
            &layout.mcps_root,
            &layout.registry_root,
            &layout.locks_dir,
        ];
        for o in other {
            assert_ne!(
                &layout.pending_requests_dir, o,
                "pending-requests must be its own directory, not collide with {o:?}",
            );
        }
    }

    #[test]
    fn three_tier_runtime_extensions_root_accessor_matches_layout() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        let via_accessor = resolver.extensions_root();
        let via_layout = resolver.runtime_layout().extensions_root;
        assert_eq!(via_accessor, via_layout);
    }

    #[test]
    fn three_tier_principals_root_under_config_dir() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        assert_eq!(
            resolver.principals_root_dir(),
            PathBuf::from("/config/principals")
        );
    }

    // ADR-045 PR #2: session-auth artifact paths live under
    // `{config_dir}/run/`, NOT under the existing `{config_dir}/runtime/`
    // bucket. These tests pin the path split so a future refactor
    // can't silently move auth-code / auth-token between the two.

    #[test]
    fn run_dir_is_under_config_not_runtime() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        assert_eq!(resolver.run_dir(), PathBuf::from("/config/run"));
        // The runtime/ bucket holds structured config and must
        // remain distinct from run/.
        assert_ne!(resolver.run_dir(), resolver.runtime_dir());
    }

    #[test]
    fn auth_code_file_lives_under_run() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        assert_eq!(
            resolver.auth_code_file(),
            PathBuf::from("/config/run/auth-code")
        );
    }

    #[test]
    fn auth_token_file_is_sid_keyed_under_run() {
        let resolver = PathResolver::with_dirs(
            PathBuf::from("/config"),
            PathBuf::from("/data"),
            PathBuf::from("/cache"),
        );
        assert_eq!(
            resolver.auth_token_file(1234),
            PathBuf::from("/config/run/auth-token-1234")
        );
        // Negative SIDs are not produced by the kernel — we still
        // format them rather than reject so unit tests can exercise
        // the helper with any i32 value.
        assert_eq!(
            resolver.auth_token_file(-1),
            PathBuf::from("/config/run/auth-token--1")
        );
    }
}

// =============================================================================
// GlobalPaths — CLI-side wrapper (Phase 0.Z-B lifted here from commands/mod.rs)
// =============================================================================

/// CLI-side global paths helper.
///
/// Wraps `PathResolver` with a service container + user identifier so
/// CLI commands can resolve paths and reach into the shared service
/// graph with one struct. Lives in `common` (not `commands`) because
/// the core `credentials_service` takes `GlobalPaths` as a parameter,
/// and after Phase 0.Z-B the CLI binary lives in `peko-rs/cli/` —
/// keeping this type in core prevents a circular dep.
#[derive(Clone, Debug)]
pub struct GlobalPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    resolver: PathResolver,
    services: crate::common::services::ServiceContainer,
    user: String,
    /// **Phase B.** Tier-typed authority that hands out
    /// `LocalPath`/`SharedPath`/`RuntimePath` newtypes. Mirrors
    /// `AppState::authority` on the daemon side. CLI is its own
    /// subject (`Subject::User(self.user.clone())`) so it cannot
    /// obtain `LocalPath` for principals it doesn't own.
    authority: Arc<crate::common::authority::RuntimeAuthority>,
}

impl GlobalPaths {
    /// Build a `GlobalPaths` from already-resolved directory paths + user.
    ///
    /// Callers (CLI `commands/mod.rs::from_cli`, daemon entry point,
    /// `credentials_service` tests) supply the four fields directly. This
    /// keeps `GlobalPaths` free of any reference to the clap `Cli` struct,
    /// which after Phase 0.Z-B lives in the `peko-cli` satellite rather than
    /// the `peko_core` lib. The CLI crate owns a thin `from_cli(&Cli)`
    /// wrapper that resolves defaults via `default_config_dir` /
    /// `default_data_dir` / `default_cache_dir` and then calls `new`.
    ///
    /// Path resolution rule (highest to lowest precedence) is applied by
    /// the caller:
    ///   1. Explicit `--config-dir` / `--data-dir` / `--cache-dir` CLI args
    ///   2. The `PEKO_HOME` env var (delegated to `default_config_dir` /
    ///      `default_data_dir` so this matches what the rest of the codebase
    ///      — and external tools like the daemon's IPC layer — expect)
    ///   3. The XDG defaults (`~/.peko` for config, `~/.local/share/peko`
    ///      for data on Linux)
    ///
    /// Before this was fixed, the fallback was `dirs::home_dir()` directly,
    /// which silently bypassed `PEKO_HOME` and made the daemon's
    /// `data_dir` (used for `cron.json`, `announcements/`, agent state)
    /// resolve to the host default even when callers set `PEKO_HOME` to
    /// isolate the daemon in a tempdir. Caught by `tests/cli_cron.rs`'s
    /// `cron_announce_writes_file_on_run` (announcement file was being
    /// written to the host's `~/.local/share/peko/announcements/`, not
    /// the test tempdir).
    #[must_use]
    pub fn new(config_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf, user: String) -> Self {
        // Ensure directories exist
        let _ = std::fs::create_dir_all(&config_dir);
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::fs::create_dir_all(&cache_dir);

        let resolver =
            PathResolver::with_dirs(config_dir.clone(), data_dir.clone(), cache_dir.clone());

        let services = crate::common::services::ServiceContainer::new(resolver.clone());

        let authority = Arc::new(
            crate::common::authority::RuntimeAuthority::for_caller(
                resolver.clone(),
                peko_subject::Subject::User(user.clone()),
            ),
        );

        Self {
            config_dir,
            data_dir,
            cache_dir,
            resolver,
            services,
            user,
            authority,
        }
    }

    /// Build a `GlobalPaths` from directory paths (user defaults to `"local"`).
    #[must_use]
    pub fn with_default_user(config_dir: PathBuf, data_dir: PathBuf, cache_dir: PathBuf) -> Self {
        Self::new(config_dir, data_dir, cache_dir, "local".to_string())
    }

    /// Get the underlying path resolver.
    #[must_use]
    pub fn resolver(&self) -> &PathResolver {
        &self.resolver
    }

    /// Get the tier-typed authority (**Phase B**). The CLI builds
    /// this once at startup with `Subject::User(...)` so a CLI
    /// command never silently gets `LocalPath` access.
    #[must_use]
    pub fn authority(&self) -> &Arc<crate::common::authority::RuntimeAuthority> {
        &self.authority
    }

    /// Get the service container.
    #[must_use]
    pub fn services(&self) -> &crate::common::services::ServiceContainer {
        &self.services
    }

    /// Get the top-level agents root directory.
    #[must_use]
    pub fn agents_root_dir(&self) -> PathBuf {
        self.resolver.agents_root_dir()
    }

    /// Get the principals configuration directory.
    #[must_use]
    pub fn principals_root_dir(&self) -> PathBuf {
        self.resolver.principals_root_dir()
    }

    /// Get agent config file path.
    #[must_use]
    pub fn agent_config(&self, name: &str) -> PathBuf {
        self.resolver.agent_config(name)
    }

    /// Get agent sessions directory.
    #[must_use]
    pub fn agent_sessions_dir(&self, name: &str) -> PathBuf {
        self.resolver.agent_sessions_dir(name)
    }

    /// Get tools directory.
    #[must_use]
    pub fn tools_dir(&self) -> PathBuf {
        self.resolver.tools_dir()
    }

    /// Get MCP configuration file path.
    #[must_use]
    pub fn mcp_config(&self) -> PathBuf {
        self.resolver.mcp_config()
    }

    /// Get agent workspace directory.
    ///
    /// Returns the path to an agent's workspace directory.
    /// Format: `<data_dir>/workspaces/<agent>/personal`
    #[must_use]
    pub fn agent_workspace(&self, agent: &str) -> PathBuf {
        self.resolver.agent_workspace(agent)
    }

    /// Get the user identifier for session isolation.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Load registry configuration from the config directory.
    ///
    /// Reads `[registry]` section from `~/.peko/config.toml`,
    /// falling back to defaults if the file or section doesn't exist.
    #[must_use]
    pub fn registry_config(&self) -> crate::registry::config::RegistryConfig {
        crate::registry::config::load_from_config_dir(&self.config_dir)
    }

    /// Get the runtime directory.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.resolver.runtime_dir()
    }

    /// Get the runtime identity file path.
    #[must_use]
    pub fn runtime_identity(&self) -> PathBuf {
        self.resolver.runtime_identity()
    }

    /// Get the runtime metadata file path.
    #[must_use]
    pub fn runtime_metadata(&self) -> PathBuf {
        self.resolver.runtime_metadata()
    }

    /// Get the known runtimes file path.
    #[must_use]
    pub fn known_runtimes(&self) -> PathBuf {
        self.resolver.known_runtimes()
    }

    // ========================================================================
    // Phase A: Three-Tier Storage Layout — typed accessors (mirror)
    // ========================================================================

    /// Resolve the full typed layout for a principal.
    #[must_use]
    pub fn principal_layout(&self, principal: &str) -> PrincipalLayout {
        self.resolver.principal_layout(principal)
    }

    /// Resolve the typed layout for runtime-global state.
    #[must_use]
    pub fn runtime_layout(&self) -> RuntimeLayout {
        self.resolver.runtime_layout()
    }

    /// Runtime-global extension install root.
    #[must_use]
    pub fn extensions_root(&self) -> PathBuf {
        self.resolver.extensions_root()
    }

    /// Runtime-global MCP server install root.
    #[must_use]
    pub fn mcps_root(&self) -> PathBuf {
        self.resolver.mcps_root()
    }

    /// OCI registry cache root.
    #[must_use]
    pub fn registry_root(&self) -> PathBuf {
        self.resolver.registry_root()
    }

    /// Runtime-wide lock directory.
    #[must_use]
    pub fn runtime_locks_dir(&self) -> PathBuf {
        self.resolver.runtime_locks_dir()
    }

    /// Pending self-modification request queue (ADR-045 PR #3).
    ///
    /// Path: `{data_dir}/runtime/pending-requests`
    #[must_use]
    pub fn pending_requests_dir(&self) -> PathBuf {
        self.resolver.pending_requests_dir()
    }

    /// Per-principal cron schedule file (Local tier).
    #[must_use]
    pub fn cron_schedule(&self, principal: &str) -> PathBuf {
        self.resolver.cron_schedule(principal)
    }

    /// Per-principal cron history log (Local tier).
    #[must_use]
    pub fn cron_history(&self, principal: &str) -> PathBuf {
        self.resolver.cron_history(principal)
    }

    /// Per-principal cron directory (Local tier).
    #[must_use]
    pub fn principal_cron_dir(&self, principal: &str) -> PathBuf {
        self.resolver.principal_cron_dir(principal)
    }

    /// Resolve a principal's `PrincipalId` (DID) from its on-disk
    /// directory name.
    ///
    /// **Phase B.** CLI commands receive a principal *name* via `--principal`
    /// but cron jobs are keyed by `PrincipalId` on the wire. This helper
    /// scans `principals_root_dir` for `principal.toml` whose `[name]`
    /// matches and returns the corresponding `PrincipalId`. Returns
    /// `None` if the principal directory doesn't exist or has no DID
    /// configured — callers should surface the error to the user.
    #[must_use]
    pub fn principal_id_for(&self, principal_name: &str) -> Option<peko_subject::PrincipalId> {
        self.resolver.lookup_principal_id_by_name(principal_name)
    }
}
