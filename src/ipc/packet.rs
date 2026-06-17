//! IPC Packet Types
//!
//! Defines the request/response protocol between CLI and daemon.
//! All packets are serialized with JSON for simplicity (local IPC overhead
//! is negligible; JSON is human-debuggable with netcat/socat).
//!
//! Packet size is limited to ~60KB to stay well under UDP MTU.
//! Larger payloads are chunked at the application layer.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Maximum packet size in bytes (conservative UDP limit)
pub const MAX_PACKET_SIZE: usize = 60_000;

// ============================================================================
// Auth Credential Types (ADR-034)
// ============================================================================

/// Authentication credential sent with every request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "token")]
pub enum AuthCredential {
    /// Local trust — no token provided.
    /// Allowed only for Unix-socket or localhost-UDP connections.
    #[serde(rename = "none")]
    None,
    /// pekohub-issued JWT (short-lived).
    #[serde(rename = "jwt")]
    Jwt(String),
    /// Long-lived programmatic key.
    #[serde(rename = "api_key")]
    ApiKey(String),
}

impl Default for AuthCredential {
    fn default() -> Self {
        Self::None
    }
}

/// Authentication header appended to every request.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthHeader {
    pub credential: AuthCredential,
}

/// Authenticated request envelope (ADR-034).
///
/// New clients wrap their `RequestPacket` in this envelope.
/// Old clients send bare `RequestPacket`s, which deserialize with
/// `auth = AuthCredential::None` when parsed as this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticatedRequest {
    #[serde(default)]
    pub auth: AuthHeader,
    #[serde(flatten)]
    pub packet: RequestPacket,
}

/// Heartbeat interval from daemon to CLI during streams (seconds)
pub const HEARTBEAT_INTERVAL_SECS: u64 = 2;

/// CLI timeout if no packet received (seconds)
/// Set to 60s to allow for agent initialization time before heartbeats start.
pub const CLI_TIMEOUT_SECS: u64 = 60;

/// Request sent from CLI → Daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestPacket {
    /// Execute an agent message and stream the response
    #[serde(rename = "execute")]
    Execute {
        /// Unique request ID (monotonic counter or random)
        request_id: u64,
        /// Agent name
        agent: String,
        /// Team name
        team: String,
        /// Message to send
        message: String,
        /// Optional session ID to resume
        session_id: Option<String>,
        /// Start a new session
        new_session: bool,
        /// Enable streaming response
        stream: bool,
        /// User identifier for session isolation
        user: String,
    },

    /// Spawn an async background task
    #[serde(rename = "async_spawn")]
    AsyncSpawn {
        request_id: u64,
        tool_name: String,
        params: serde_json::Value,
        session_key: String,
        workspace: PathBuf,
    },

    /// Cancel an async task
    #[serde(rename = "async_cancel")]
    AsyncCancel { request_id: u64, task_id: String },

    /// Health check / status ping
    #[serde(rename = "ping")]
    Ping { request_id: u64 },

    /// Request graceful daemon shutdown
    #[serde(rename = "shutdown")]
    Shutdown { request_id: u64, force: bool },

    /// List cron jobs
    #[serde(rename = "cron_list")]
    CronList {
        request_id: u64,
        include_disabled: bool,
    },

    /// Add a cron job
    #[serde(rename = "cron_add")]
    CronAdd {
        request_id: u64,
        job: crate::cron::CronJob,
    },

    /// Remove a cron job
    #[serde(rename = "cron_remove")]
    CronRemove { request_id: u64, job_id: String },

    /// Run a cron job immediately
    #[serde(rename = "cron_run")]
    CronRun { request_id: u64, job_id: String },

    /// Get cron job history
    #[serde(rename = "cron_history")]
    CronHistory {
        request_id: u64,
        job_id: String,
        limit: usize,
    },

    /// Get system status
    #[serde(rename = "system_status")]
    SystemStatus { request_id: u64 },

    /// Run system doctor
    #[serde(rename = "system_doctor")]
    SystemDoctor { request_id: u64 },

    /// Start a background runtime (extension lifecycle — ADR-026)
    #[serde(rename = "ext_start")]
    ExtStart {
        request_id: u64,
        extension_id: String,
    },

    /// Stop a background runtime (extension lifecycle — ADR-026)
    #[serde(rename = "ext_stop")]
    ExtStop {
        request_id: u64,
        extension_id: String,
    },

    /// Restart a background runtime (extension lifecycle — ADR-026)
    #[serde(rename = "ext_restart")]
    ExtRestart {
        request_id: u64,
        extension_id: String,
    },

    /// Get background runtime status (extension lifecycle — ADR-026)
    #[serde(rename = "ext_status")]
    ExtStatus {
        request_id: u64,
        extension_id: String,
    },

    // ─── Agent CRUD ─────────────────────────────────────────────────
    #[serde(rename = "agent_list")]
    AgentList {
        request_id: u64,
        team_filter: Option<String>,
    },

    #[serde(rename = "agent_get")]
    AgentGet {
        request_id: u64,
        name: String,
        team: Option<String>,
    },

    #[serde(rename = "agent_create")]
    AgentCreate {
        request_id: u64,
        request: crate::common::types::agent::AgentCreateRequest,
    },

    #[serde(rename = "agent_delete")]
    AgentDelete {
        request_id: u64,
        name: String,
        team: Option<String>,
        force: bool,
    },

    #[serde(rename = "agent_move")]
    AgentMove {
        request_id: u64,
        old_name: String,
        new_name: String,
        team: Option<String>,
    },

    #[serde(rename = "agent_update")]
    AgentUpdate {
        request_id: u64,
        name: String,
        team: Option<String>,
        model: Option<String>,
        description: Option<String>,
        system_prompt: Option<String>,
        config: Option<serde_json::Value>,
    },

    // ─── Team CRUD ──────────────────────────────────────────────────
    #[serde(rename = "team_list")]
    TeamList { request_id: u64 },

    #[serde(rename = "team_get")]
    TeamGet { request_id: u64, name: String },

    #[serde(rename = "team_create")]
    TeamCreate {
        request_id: u64,
        name: String,
        description: Option<String>,
        members: Option<Vec<String>>,
    },

    #[serde(rename = "team_join")]
    TeamJoin {
        request_id: u64,
        team: String,
        agent: String,
    },

    #[serde(rename = "team_leave")]
    TeamLeave {
        request_id: u64,
        team: String,
        agent: String,
    },

    #[serde(rename = "team_delete")]
    TeamDelete {
        request_id: u64,
        name: String,
        force: bool,
    },

    #[serde(rename = "team_move")]
    TeamMove {
        request_id: u64,
        old_name: String,
        new_name: String,
    },

    // ─── Session CRUD ───────────────────────────────────────────────
    #[serde(rename = "session_list")]
    SessionList {
        request_id: u64,
        agent: Option<String>,
        team: Option<String>,
    },

    #[serde(rename = "session_get")]
    SessionGet { request_id: u64, id: String },

    #[serde(rename = "session_show")]
    SessionShow {
        request_id: u64,
        agent: String,
        team: Option<String>,
        session_id: String,
        history: bool,
    },

    #[serde(rename = "session_remove")]
    SessionRemove {
        request_id: u64,
        agent: String,
        team: Option<String>,
        session_id: String,
        force: bool,
    },

    #[serde(rename = "session_switch")]
    SessionSwitch {
        request_id: u64,
        agent: String,
        team: Option<String>,
        session_id: String,
        user: String,
    },

    // ─── Provider listing ───────────────────────────────────────────
    #[serde(rename = "provider_list")]
    ProviderList { request_id: u64 },

    // ─── Extension CRUD (ADR-030 Tier 1) ────────────────────────────
    #[serde(rename = "extension_list")]
    ExtensionList {
        request_id: u64,
        enabled_only: bool,
        ext_type: Option<String>,
    },

    #[serde(rename = "extension_enable")]
    ExtensionEnable {
        request_id: u64,
        id: String,
        target: Option<String>,
    },

    #[serde(rename = "extension_disable")]
    ExtensionDisable {
        request_id: u64,
        id: String,
        target: Option<String>,
    },

    #[serde(rename = "extension_validate")]
    ExtensionValidate {
        request_id: u64,
        path: String,
        verbose: bool,
        #[serde(default)]
        semantic: bool,
    },

    #[serde(rename = "extension_debug")]
    ExtensionDebug { request_id: u64, id: String },

    #[serde(rename = "extension_info")]
    ExtensionInfo { request_id: u64, id: String },

    #[serde(rename = "extension_export")]
    ExtensionExport {
        request_id: u64,
        id: String,
        output: String,
    },

    #[serde(rename = "extension_bundle")]
    ExtensionBundle {
        request_id: u64,
        name: String,
        ids: Vec<String>,
    },

    #[serde(rename = "system_clean")]
    SystemClean {
        request_id: u64,
        scope: Option<String>,
    },

    /// Add a cron job (simplified — daemon constructs the CronJob)
    #[serde(rename = "cron_add_simple")]
    CronAddSimple {
        request_id: u64,
        name: String,
        schedule: String, // cron expression
        message: String,
    },

    /// Branch a session
    #[serde(rename = "session_branch")]
    SessionBranch {
        request_id: u64,
        agent: String,
        team: Option<String>,
        session_id: String,
        label: Option<String>,
    },

    /// Compact a session
    #[serde(rename = "session_compact")]
    SessionCompact {
        request_id: u64,
        agent: String,
        team: Option<String>,
        session_id: String,
        dry_run: bool,
        instruction: Option<String>,
    },

    /// Install an extension from a path
    #[serde(rename = "extension_install")]
    ExtensionInstall { request_id: u64, path: String },

    /// Uninstall an extension by ID
    #[serde(rename = "extension_uninstall")]
    ExtensionUninstall { request_id: u64, id: String },

    /// Export an agent to a file
    #[serde(rename = "agent_export")]
    AgentExport {
        request_id: u64,
        name: String,
        team: Option<String>,
        output: Option<String>,
        include_sessions: bool,
        /// Embed extension packages in an `extensions/` layer (ADR-037).
        with_extensions: bool,
    },

    /// Import an agent from a file
    #[serde(rename = "agent_import")]
    AgentImport {
        request_id: u64,
        file_path: String,
        name: Option<String>,
        team: Option<String>,
    },

    /// Export a team
    #[serde(rename = "team_export")]
    TeamExport {
        request_id: u64,
        name: String,
        output: Option<String>,
        include_sessions: bool,
    },

    /// Import a team
    #[serde(rename = "team_import")]
    TeamImport {
        request_id: u64,
        file_path: String,
        name: Option<String>,
        force: bool,
    },

    /// Pull an agent from a registry
    #[serde(rename = "registry_pull")]
    RegistryPull {
        request_id: u64,
        registry_ref: String,
        team: Option<String>,
        force: bool,
        registry_token: Option<String>,
        registry_host: Option<String>,
    },

    // ── Runtime (ADR-032) ──
    #[serde(rename = "runtime_id")]
    RuntimeId { request_id: u64 },
    #[serde(rename = "runtime_info")]
    RuntimeInfo { request_id: u64 },
    #[serde(rename = "runtime_rename")]
    RuntimeRename {
        request_id: u64,
        display_name: String,
    },
    #[serde(rename = "runtime_list")]
    RuntimeList { request_id: u64 },
    #[serde(rename = "runtime_register")]
    RuntimeRegister {
        request_id: u64,
        runtime_id: String,
        display_name: String,
    },
    #[serde(rename = "runtime_trust")]
    RuntimeTrust { request_id: u64, runtime_id: String },
    #[serde(rename = "runtime_remove")]
    RuntimeRemove { request_id: u64, runtime_id: String },

    // ── Tunnel (ADR-035) ──
    #[serde(rename = "tunnel_stop")]
    TunnelStop { request_id: u64 },
    #[serde(rename = "tunnel_status")]
    TunnelStatus { request_id: u64 },

    /// Comprehensive daemon status (issue #8). Returns uptime, version, and
    /// tunnel health snapshot. Used by `peko daemon status --json`.
    #[serde(rename = "status")]
    Status { request_id: u64 },

    // ── Instance status ──
    #[serde(rename = "instance_set_status")]
    InstanceSetStatus {
        request_id: u64,
        agent_name: String,
        status: String,
    },
    #[serde(rename = "instance_set_exposure")]
    InstanceSetExposure {
        request_id: u64,
        agent_name: String,
        exposure: String,
    },

    // ── Auth management (ADR-034) ──
    #[serde(rename = "auth_api_key_create")]
    AuthApiKeyCreate {
        request_id: u64,
        name: String,
        scopes: Vec<String>,
    },
    #[serde(rename = "auth_api_key_list")]
    AuthApiKeyList { request_id: u64 },
    #[serde(rename = "auth_api_key_revoke")]
    AuthApiKeyRevoke { request_id: u64, key_id: String },
    #[serde(rename = "auth_status")]
    AuthStatus { request_id: u64 },

    // ── Ownership and Permission (ADR-033) ──
    #[serde(rename = "agent_transfer_owner")]
    AgentTransferOwner {
        request_id: u64,
        agent: String,
        new_owner_id: String,
    },
    #[serde(rename = "agent_grant_permission")]
    AgentGrantPermission {
        request_id: u64,
        agent: String,
        subject_id: String,
        subject_type: crate::auth::ownership::SubjectType,
        permission: crate::auth::ownership::Permission,
    },
    #[serde(rename = "agent_revoke_permission")]
    AgentRevokePermission {
        request_id: u64,
        agent: String,
        subject_id: String,
        permission: crate::auth::ownership::Permission,
    },
    #[serde(rename = "team_transfer_owner")]
    TeamTransferOwner {
        request_id: u64,
        team: String,
        new_owner_id: String,
    },
    #[serde(rename = "team_grant_permission")]
    TeamGrantPermission {
        request_id: u64,
        team: String,
        subject_id: String,
        subject_type: crate::auth::ownership::SubjectType,
        permission: crate::auth::ownership::Permission,
    },
    #[serde(rename = "team_revoke_permission")]
    TeamRevokePermission {
        request_id: u64,
        team: String,
        subject_id: String,
        permission: crate::auth::ownership::Permission,
    },
}

impl RequestPacket {
    /// Get the request ID from any variant
    #[must_use]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Execute { request_id, .. }
            | Self::AsyncSpawn { request_id, .. }
            | Self::AsyncCancel { request_id, .. }
            | Self::Ping { request_id }
            | Self::Shutdown { request_id, .. }
            | Self::CronList { request_id, .. }
            | Self::CronAdd { request_id, .. }
            | Self::CronRemove { request_id, .. }
            | Self::CronRun { request_id, .. }
            | Self::CronHistory { request_id, .. }
            | Self::ExtStart { request_id, .. }
            | Self::ExtStop { request_id, .. }
            | Self::ExtRestart { request_id, .. }
            | Self::ExtStatus { request_id, .. }
            | Self::AgentList { request_id, .. }
            | Self::AgentGet { request_id, .. }
            | Self::AgentCreate { request_id, .. }
            | Self::AgentDelete { request_id, .. }
            | Self::TeamList { request_id }
            | Self::TeamGet { request_id, .. }
            | Self::TeamCreate { request_id, .. }
            | Self::TeamDelete { request_id, .. }
            | Self::TeamMove { request_id, .. }
            | Self::SessionList { request_id, .. }
            | Self::SessionGet { request_id, .. }
            | Self::SessionShow { request_id, .. }
            | Self::SessionRemove { request_id, .. }
            | Self::SessionSwitch { request_id, .. }
            | Self::ProviderList { request_id }
            | Self::SystemStatus { request_id }
            | Self::SystemDoctor { request_id }
            | Self::ExtensionList { request_id, .. }
            | Self::ExtensionEnable { request_id, .. }
            | Self::ExtensionDisable { request_id, .. }
            | Self::ExtensionValidate { request_id, .. }
            | Self::ExtensionDebug { request_id, .. }
            | Self::ExtensionInfo { request_id, .. }
            | Self::ExtensionExport { request_id, .. }
            | Self::ExtensionBundle { request_id, .. }
            | Self::SystemClean { request_id, .. }
            | Self::CronAddSimple { request_id, .. }
            | Self::SessionBranch { request_id, .. }
            | Self::SessionCompact { request_id, .. }
            | Self::ExtensionInstall { request_id, .. }
            | Self::ExtensionUninstall { request_id, .. }
            | Self::AgentExport { request_id, .. }
            | Self::AgentImport { request_id, .. }
            | Self::AgentMove { request_id, .. }
            | Self::AgentUpdate { request_id, .. }
            | Self::TeamExport { request_id, .. }
            | Self::TeamImport { request_id, .. }
            | Self::TeamJoin { request_id, .. }
            | Self::TeamLeave { request_id, .. }
            | Self::RegistryPull { request_id, .. }
            | Self::RuntimeId { request_id }
            | Self::RuntimeInfo { request_id }
            | Self::RuntimeRename { request_id, .. }
            | Self::RuntimeList { request_id }
            | Self::RuntimeRegister { request_id, .. }
            | Self::RuntimeTrust { request_id, .. }
            | Self::RuntimeRemove { request_id, .. }
            | Self::AuthApiKeyCreate { request_id, .. }
            | Self::AuthApiKeyList { request_id }
            | Self::AuthApiKeyRevoke { request_id, .. }
            | Self::AuthStatus { request_id }
            | Self::TunnelStop { request_id }
            | Self::TunnelStatus { request_id }
            | Self::Status { request_id }
            | Self::InstanceSetStatus { request_id, .. }
            | Self::InstanceSetExposure { request_id, .. }
            | Self::AgentTransferOwner { request_id, .. }
            | Self::AgentGrantPermission { request_id, .. }
            | Self::AgentRevokePermission { request_id, .. }
            | Self::TeamTransferOwner { request_id, .. }
            | Self::TeamGrantPermission { request_id, .. }
            | Self::TeamRevokePermission { request_id, .. } => *request_id,
        }
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }

    /// Deserialize from JSON bytes
    ///
    /// # Errors
    /// Returns error if deserialization fails
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// Extract the auth credential from this request.
    ///
    /// For v0.1.0, this always returns `AuthCredential::None` because
    /// `RequestPacket` variants do not carry auth directly. Use
    /// `AuthenticatedRequest::from_bytes` to parse requests that include auth.
    #[must_use]
    pub fn auth(&self) -> AuthCredential {
        AuthCredential::None
    }
}

/// Response sent from Daemon → CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResponsePacket {
    /// Streaming text chunk
    #[serde(rename = "text")]
    Text {
        request_id: u64,
        /// Sequence number for ordering (per-request, monotonic)
        seq: u32,
        chunk: String,
    },

    /// Async task receipt
    #[serde(rename = "async_receipt")]
    AsyncReceipt {
        request_id: u64,
        receipt: crate::extension::async_exec::executor::AsyncTaskReceipt,
    },

    /// Final success/failure marker
    #[serde(rename = "done")]
    Done {
        request_id: u64,
        success: bool,
        error: Option<String>,
    },

    /// Error response
    #[serde(rename = "error")]
    Error { request_id: u64, message: String },

    /// Ping response
    #[serde(rename = "pong")]
    Pong {
        request_id: u64,
        uptime_secs: u64,
        version: String,
    },

    /// Heartbeat — sent during long streams so CLI can detect dead daemon
    #[serde(rename = "heartbeat")]
    Heartbeat { request_id: u64 },

    /// Shutdown acknowledgement
    #[serde(rename = "shutting_down")]
    ShuttingDown { request_id: u64 },

    /// Cron job list response
    #[serde(rename = "cron_list")]
    CronList {
        request_id: u64,
        jobs: Vec<crate::cron::CronJob>,
    },

    /// Cron job added response
    #[serde(rename = "cron_added")]
    CronAdded { request_id: u64, job_id: String },

    /// Cron job removed response
    #[serde(rename = "cron_removed")]
    CronRemoved { request_id: u64, job_id: String },

    /// Cron job run started response
    #[serde(rename = "cron_run_started")]
    CronRunStarted {
        request_id: u64,
        job_id: String,
        run_id: String,
    },

    /// Cron job history response
    #[serde(rename = "cron_history")]
    CronHistory {
        request_id: u64,
        runs: Vec<crate::cron::CronRun>,
    },

    /// Background runtime started (ADR-026)
    #[serde(rename = "ext_started")]
    ExtStarted {
        request_id: u64,
        extension_id: String,
    },

    /// Background runtime stopped (ADR-026)
    #[serde(rename = "ext_stopped")]
    ExtStopped {
        request_id: u64,
        extension_id: String,
    },

    /// Background runtime restarted (ADR-026)
    #[serde(rename = "ext_restarted")]
    ExtRestarted {
        request_id: u64,
        extension_id: String,
    },

    /// Background runtime status response (ADR-026)
    #[serde(rename = "ext_status")]
    ExtStatus {
        request_id: u64,
        extension_id: String,
        state: String,
        restart_count: u32,
        last_error: Option<String>,
    },

    /// Agent list response
    #[serde(rename = "agent_list")]
    AgentList {
        request_id: u64,
        agents: Vec<crate::common::types::agent::AgentSummary>,
    },

    /// Agent detail response
    #[serde(rename = "agent_get")]
    AgentGet {
        request_id: u64,
        agent: Option<crate::common::types::agent::AgentInfo>,
    },

    /// Agent created response
    #[serde(rename = "agent_created")]
    AgentCreated {
        request_id: u64,
        result: crate::common::types::agent::AgentCreationResult,
    },

    /// Agent deleted response
    #[serde(rename = "agent_deleted")]
    AgentDeleted {
        request_id: u64,
        result: crate::common::types::agent::AgentDeleteResult,
    },

    /// Agent moved/renamed response
    #[serde(rename = "agent_moved")]
    AgentMoved {
        request_id: u64,
        result: crate::common::types::agent::AgentRenameResult,
    },

    /// Agent updated response
    #[serde(rename = "agent_updated")]
    AgentUpdated {
        request_id: u64,
        name: String,
    },

    /// Team list response
    #[serde(rename = "team_list")]
    TeamList {
        request_id: u64,
        teams: Vec<crate::common::types::team::TeamInfo>,
    },

    /// Team detail response
    #[serde(rename = "team_get")]
    TeamGet {
        request_id: u64,
        team: Option<crate::common::types::team::TeamInfo>,
    },

    /// Team created response
    #[serde(rename = "team_created")]
    TeamCreated {
        request_id: u64,
        result: crate::common::types::team::TeamCreationResult,
    },

    /// Team deleted response
    #[serde(rename = "team_deleted")]
    TeamDeleted {
        request_id: u64,
        result: crate::common::types::team::TeamDeletionResult,
    },

    /// Team moved response
    #[serde(rename = "team_moved")]
    TeamMoved {
        request_id: u64,
        old_name: String,
        new_name: String,
    },

    /// Team join response
    #[serde(rename = "team_joined")]
    TeamJoined {
        request_id: u64,
        agent: String,
        team: String,
    },

    /// Team leave response
    #[serde(rename = "team_left")]
    TeamLeft {
        request_id: u64,
        agent: String,
        team: String,
        was_member: bool,
    },

    /// Session list response
    #[serde(rename = "session_list")]
    SessionList {
        request_id: u64,
        sessions: Vec<crate::common::services::session_service::SessionInfo>,
        active_session: Option<String>,
    },

    /// Session detail response
    #[serde(rename = "session_get")]
    SessionGet {
        request_id: u64,
        session: Option<crate::common::services::session_service::SessionDetails>,
    },

    /// Session shown response
    #[serde(rename = "session_shown")]
    SessionShown {
        request_id: u64,
        session: crate::common::services::session_service::SessionDetails,
        history: Option<Vec<crate::common::services::session_service::HistoryEvent>>,
    },

    /// Session removed response
    #[serde(rename = "session_removed")]
    SessionRemoved {
        request_id: u64,
        session_id: String,
        deleted: bool,
    },

    /// Session switched response
    #[serde(rename = "session_switched")]
    SessionSwitched {
        request_id: u64,
        session_id: String,
        agent: String,
        team: String,
    },

    /// System status response
    #[serde(rename = "system_status")]
    SystemStatus {
        request_id: u64,
        version: String,
        uptime_secs: u64,
        degraded: bool,
        instance_count: u64,
        team_count: u64,
        ready: bool,
    },

    /// System doctor response
    #[serde(rename = "system_doctor")]
    SystemDoctor {
        request_id: u64,
        checks: Vec<DoctorCheck>,
        passed: u32,
        failed: u32,
        warnings: u32,
    },

    /// Provider list response
    #[serde(rename = "provider_list")]
    ProviderList {
        request_id: u64,
        providers: Vec<ProviderInfo>,
    },

    /// Extension list response
    #[serde(rename = "extension_list")]
    ExtensionList {
        request_id: u64,
        extensions: Vec<ExtensionSummary>,
        total: usize,
    },

    /// Extension enabled response
    #[serde(rename = "extension_enabled")]
    ExtensionEnabled {
        request_id: u64,
        id: String,
        message: String,
    },

    /// Extension disabled response
    #[serde(rename = "extension_disabled")]
    ExtensionDisabled {
        request_id: u64,
        id: String,
        message: String,
    },

    /// Extension validated response
    #[serde(rename = "extension_validated")]
    ExtensionValidated {
        request_id: u64,
        valid: bool,
        errors: Vec<String>,
        warnings: Vec<String>,
    },

    /// Extension debug info response
    #[serde(rename = "extension_debug_info")]
    ExtensionDebugInfo {
        request_id: u64,
        id: String,
        info: serde_json::Value,
    },

    /// Extension info response
    #[serde(rename = "extension_info_response")]
    ExtensionInfoResponse {
        request_id: u64,
        id: String,
        info: serde_json::Value,
    },

    /// Extension exported response
    #[serde(rename = "extension_exported")]
    ExtensionExported {
        request_id: u64,
        id: String,
        output: String,
    },

    /// Extension bundled response
    #[serde(rename = "extension_bundled")]
    ExtensionBundled {
        request_id: u64,
        name: String,
        count: usize,
    },

    /// System clean response
    #[serde(rename = "system_cleaned")]
    SystemCleaned {
        request_id: u64,
        cleaned: Vec<String>,
        bytes_freed: u64,
    },

    /// Cron job added (simplified)
    #[serde(rename = "cron_added_simple")]
    CronAddedSimple { request_id: u64, job_id: String },

    /// Session branched
    #[serde(rename = "session_branched")]
    SessionBranched {
        request_id: u64,
        new_session_id: String,
        parent_session_id: String,
    },

    /// Session compacted
    #[serde(rename = "session_compacted")]
    SessionCompacted {
        request_id: u64,
        session_id: String,
        messages_compacted: usize,
        tokens_saved: usize,
        tokens_before: usize,
        tokens_after: usize,
    },

    /// Dry-run preview of a compaction (no JSONL mutation).
    ///
    /// Carries the full [`crate::compaction::cli::DryRunReport`] fields
    /// directly so the wire format is not overloaded with the real
    /// `SessionCompacted` response (whose `messages_compacted` means
    /// "messages folded into the summary", not "messages in the
    /// session").
    #[serde(rename = "session_compact_dry_run")]
    SessionCompactDryRun {
        request_id: u64,
        session_id: String,
        estimated_tokens: usize,
        context_window: usize,
        percent: usize,
        message_count: usize,
        messages_to_compact: usize,
    },

    /// Extension installed response
    #[serde(rename = "extension_installed")]
    ExtensionInstalled {
        request_id: u64,
        id: String,
        message: String,
    },

    /// Extension uninstalled response
    #[serde(rename = "extension_uninstalled")]
    ExtensionUninstalled {
        request_id: u64,
        id: String,
        message: String,
    },

    /// Agent exported
    #[serde(rename = "agent_exported")]
    AgentExported {
        request_id: u64,
        name: String,
        output_path: String,
    },

    /// Agent imported
    #[serde(rename = "agent_imported")]
    AgentImported {
        request_id: u64,
        name: String,
        config_path: String,
    },

    /// Team exported
    #[serde(rename = "team_exported")]
    TeamExported {
        request_id: u64,
        name: String,
        output_path: String,
    },

    /// Team imported
    #[serde(rename = "team_imported")]
    TeamImported {
        request_id: u64,
        name: String,
        path: String,
    },

    /// Registry pull completed
    #[serde(rename = "registry_pulled")]
    RegistryPulled {
        request_id: u64,
        name: String,
        version: String,
        digest: String,
    },

    // ── Runtime (ADR-032) ──
    #[serde(rename = "runtime_id")]
    RuntimeId { request_id: u64, did: String },
    #[serde(rename = "runtime_info")]
    RuntimeInfo {
        request_id: u64,
        metadata: RuntimeMetadataResponse,
    },
    #[serde(rename = "runtime_list")]
    RuntimeList {
        request_id: u64,
        runtimes: Vec<KnownRuntimeResponse>,
    },

    // ── Tunnel (ADR-035) ──
    #[serde(rename = "tunnel_status")]
    TunnelStatus {
        request_id: u64,
        configured: bool,
        daemon_running: bool,
        connected: bool,
    },

    /// Comprehensive daemon status payload (issue #8). Includes tunnel
    /// health snapshot suitable for `peko daemon status --json`.
    #[serde(rename = "status")]
    Status {
        request_id: u64,
        uptime_secs: u64,
        version: String,
        tunnel_state: String,
        tunnel_reconnect_attempts: u32,
        tunnel_last_error: Option<String>,
        degraded: bool,
    },

    // ── Auth management (ADR-034) ──
    #[serde(rename = "auth_api_key_created")]
    AuthApiKeyCreated {
        request_id: u64,
        key_id: String,
        full_key: String,
    },
    #[serde(rename = "auth_api_key_list")]
    AuthApiKeyList {
        request_id: u64,
        keys: Vec<ApiKeySummary>,
    },
    #[serde(rename = "auth_api_key_revoked")]
    AuthApiKeyRevoked { request_id: u64, key_id: String },
    #[serde(rename = "auth_status")]
    AuthStatus {
        request_id: u64,
        local_trust_enabled: bool,
        pekohub_jwt_enabled: bool,
        api_key_enabled: bool,
        api_key_count: usize,
    },
}

/// Summary of an extension for IPC responses
/// Provider info for listing available LLM providers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub api_type: String, // "openai" or "anthropic"
    pub default_model: String,
    pub requires_key: bool,
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub ext_type: String,
    pub version: String,
    pub source: String, // "built-in" or "installed"
    pub enabled: bool,
    pub runtime: String, // "running", "stopped", or "n/a"
    pub description: String,
}

/// A single doctor check result
/// Runtime metadata response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeMetadataResponse {
    pub runtime_id: String,
    pub display_name: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub host_info: HostInfoResponse,
}

/// Host information response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfoResponse {
    pub os: String,
    pub arch: String,
    pub hostname: String,
}

/// Known runtime response for IPC (ADR-032)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownRuntimeResponse {
    pub runtime_id: String,
    pub display_name: String,
    pub last_seen: Option<String>,
    pub connection_endpoint: Option<String>,
    pub trust_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub message: String,
    pub suggestion: Option<String>,
}

/// API key summary for IPC responses (ADR-034)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub scopes: Vec<String>,
    pub enabled: bool,
}

impl AuthenticatedRequest {
    /// Deserialize an authenticated request from JSON bytes.
    ///
    /// First tries to parse as `AuthenticatedRequest` (with auth envelope).
    /// If that fails, falls back to plain `RequestPacket` with `AuthCredential::None`.
    ///
    /// # Errors
    /// Returns error if deserialization fails for both formats.
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        // Try the new format first
        if let Ok(envelope) = serde_json::from_slice::<Self>(bytes) {
            return Ok(envelope);
        }
        // Fall back to plain RequestPacket (old clients)
        let packet = serde_json::from_slice::<RequestPacket>(bytes)?;
        Ok(Self {
            auth: AuthHeader::default(),
            packet,
        })
    }

    /// Get the request ID from the inner packet
    #[must_use]
    pub fn request_id(&self) -> u64 {
        self.packet.request_id()
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }
}

impl ResponsePacket {
    /// Get the request ID from any variant
    #[must_use]
    pub fn request_id(&self) -> u64 {
        match self {
            Self::Text { request_id, .. }
            | Self::AsyncReceipt { request_id, .. }
            | Self::Done { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Pong { request_id, .. }
            | Self::Heartbeat { request_id }
            | Self::ShuttingDown { request_id }
            | Self::CronList { request_id, .. }
            | Self::CronAdded { request_id, .. }
            | Self::CronRemoved { request_id, .. }
            | Self::CronRunStarted { request_id, .. }
            | Self::CronHistory { request_id, .. }
            | Self::ExtStarted { request_id, .. }
            | Self::ExtStopped { request_id, .. }
            | Self::ExtRestarted { request_id, .. }
            | Self::ExtStatus { request_id, .. }
            | Self::AgentList { request_id, .. }
            | Self::AgentGet { request_id, .. }
            | Self::AgentCreated { request_id, .. }
            | Self::AgentDeleted { request_id, .. }
            | Self::TeamList { request_id, .. }
            | Self::TeamGet { request_id, .. }
            | Self::TeamCreated { request_id, .. }
            | Self::TeamDeleted { request_id, .. }
            | Self::TeamMoved { request_id, .. }
            | Self::TeamJoined { request_id, .. }
            | Self::TeamLeft { request_id, .. }
            | Self::SessionList { request_id, .. }
            | Self::SessionGet { request_id, .. }
            | Self::SessionShown { request_id, .. }
            | Self::SessionRemoved { request_id, .. }
            | Self::SessionSwitched { request_id, .. }
            | Self::SystemStatus { request_id, .. }
            | Self::SystemDoctor { request_id, .. }
            | Self::ProviderList { request_id, .. }
            | Self::ExtensionList { request_id, .. }
            | Self::ExtensionEnabled { request_id, .. }
            | Self::ExtensionDisabled { request_id, .. }
            | Self::ExtensionValidated { request_id, .. }
            | Self::ExtensionDebugInfo { request_id, .. }
            | Self::ExtensionInfoResponse { request_id, .. }
            | Self::ExtensionExported { request_id, .. }
            | Self::ExtensionBundled { request_id, .. }
            | Self::SystemCleaned { request_id, .. }
            | Self::CronAddedSimple { request_id, .. }
            | Self::SessionBranched { request_id, .. }
            | Self::SessionCompacted { request_id, .. }
            | Self::SessionCompactDryRun { request_id, .. }
            | Self::ExtensionInstalled { request_id, .. }
            | Self::ExtensionUninstalled { request_id, .. }
            | Self::AgentExported { request_id, .. }
            | Self::AgentImported { request_id, .. }
            | Self::AgentMoved { request_id, .. }
            | Self::AgentUpdated { request_id, .. }
            | Self::TeamExported { request_id, .. }
            | Self::TeamImported { request_id, .. }
            | Self::RegistryPulled { request_id, .. }
            | Self::RuntimeId { request_id, .. }
            | Self::RuntimeInfo { request_id, .. }
            | Self::RuntimeList { request_id, .. }
            | Self::AuthApiKeyCreated { request_id, .. }
            | Self::AuthApiKeyList { request_id, .. }
            | Self::AuthApiKeyRevoked { request_id, .. }
            | Self::AuthStatus { request_id, .. }
            | Self::TunnelStatus { request_id, .. }
            | Self::Status { request_id, .. } => *request_id,
        }
    }

    /// Serialize to JSON bytes
    ///
    /// # Errors
    /// Returns error if serialization fails
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let json = serde_json::to_vec(self)?;
        if json.len() > MAX_PACKET_SIZE {
            anyhow::bail!(
                "Packet size {} exceeds maximum {}",
                json.len(),
                MAX_PACKET_SIZE
            );
        }
        Ok(json)
    }

    /// Deserialize from JSON bytes
    ///
    /// # Errors
    /// Returns error if deserialization fails
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization_roundtrip() {
        let req = RequestPacket::Execute {
            request_id: 42,
            agent: "test-agent".to_string(),
            team: "default".to_string(),
            message: "Hello".to_string(),
            session_id: None,
            new_session: false,
            stream: true,
            user: "default".to_string(),
        };

        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();

        match decoded {
            RequestPacket::Execute {
                request_id,
                agent,
                team,
                message,
                stream,
                ..
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, "default");
                assert_eq!(message, "Hello");
                assert!(stream);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_response_serialization_roundtrip() {
        let resp = ResponsePacket::Text {
            request_id: 42,
            seq: 7,
            chunk: "hello world".to_string(),
        };

        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();

        match decoded {
            ResponsePacket::Text {
                request_id,
                seq,
                chunk,
            } => {
                assert_eq!(request_id, 42);
                assert_eq!(seq, 7);
                assert_eq!(chunk, "hello world");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_request_id_extraction() {
        let req = RequestPacket::Ping { request_id: 99 };
        assert_eq!(req.request_id(), 99);

        let resp = ResponsePacket::Pong {
            request_id: 99,
            uptime_secs: 10,
            version: "0.1.0".to_string(),
        };
        assert_eq!(resp.request_id(), 99);
    }

    #[test]
    fn test_packet_size_limit() {
        // Create a packet that exceeds the limit
        let huge_chunk = "x".repeat(MAX_PACKET_SIZE + 1000);
        let resp = ResponsePacket::Text {
            request_id: 1,
            seq: 0,
            chunk: huge_chunk,
        };
        assert!(resp.to_bytes().is_err());
    }

    #[test]
    fn test_cron_list_request_roundtrip() {
        let req = RequestPacket::CronList {
            request_id: 100,
            include_disabled: true,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronList {
                request_id,
                include_disabled,
            } => {
                assert_eq!(request_id, 100);
                assert!(include_disabled);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_add_request_roundtrip() {
        let job = crate::cron::CronJob {
            id: "job-1".to_string(),
            name: "Test Job".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60000 },
            target: crate::cron::ExecutionTarget::Main,
            agent_id: None,
            message: "Hello cron".to_string(),
            delivery: crate::cron::DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        let req = RequestPacket::CronAdd {
            request_id: 101,
            job,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronAdd { request_id, job } => {
                assert_eq!(request_id, 101);
                assert_eq!(job.id, "job-1");
                assert_eq!(job.name, "Test Job");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_remove_request_roundtrip() {
        let req = RequestPacket::CronRemove {
            request_id: 102,
            job_id: "job-1".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronRemove { request_id, job_id } => {
                assert_eq!(request_id, 102);
                assert_eq!(job_id, "job-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_run_request_roundtrip() {
        let req = RequestPacket::CronRun {
            request_id: 103,
            job_id: "job-1".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronRun { request_id, job_id } => {
                assert_eq!(request_id, 103);
                assert_eq!(job_id, "job-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_history_request_roundtrip() {
        let req = RequestPacket::CronHistory {
            request_id: 104,
            job_id: "job-1".to_string(),
            limit: 10,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronHistory {
                request_id,
                job_id,
                limit,
            } => {
                assert_eq!(request_id, 104);
                assert_eq!(job_id, "job-1");
                assert_eq!(limit, 10);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_list_response_roundtrip() {
        let job = crate::cron::CronJob {
            id: "job-1".to_string(),
            name: "Test Job".to_string(),
            schedule: crate::cron::ScheduleKind::Every { every_ms: 60000 },
            target: crate::cron::ExecutionTarget::Main,
            agent_id: None,
            message: "Hello cron".to_string(),
            delivery: crate::cron::DeliveryMode::None,
            delete_after_run: false,
            enabled: true,
            created_at: chrono::Utc::now(),
            next_run: chrono::Utc::now(),
            last_run: None,
            last_status: None,
            run_count: 0,
        };
        let resp = ResponsePacket::CronList {
            request_id: 200,
            jobs: vec![job],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronList { request_id, jobs } => {
                assert_eq!(request_id, 200);
                assert_eq!(jobs.len(), 1);
                assert_eq!(jobs[0].id, "job-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_added_response_roundtrip() {
        let resp = ResponsePacket::CronAdded {
            request_id: 201,
            job_id: "job-1".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronAdded { request_id, job_id } => {
                assert_eq!(request_id, 201);
                assert_eq!(job_id, "job-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_removed_response_roundtrip() {
        let resp = ResponsePacket::CronRemoved {
            request_id: 202,
            job_id: "job-1".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronRemoved { request_id, job_id } => {
                assert_eq!(request_id, 202);
                assert_eq!(job_id, "job-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_run_started_response_roundtrip() {
        let resp = ResponsePacket::CronRunStarted {
            request_id: 203,
            job_id: "job-1".to_string(),
            run_id: "run-1".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronRunStarted {
                request_id,
                job_id,
                run_id,
            } => {
                assert_eq!(request_id, 203);
                assert_eq!(job_id, "job-1");
                assert_eq!(run_id, "run-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_history_response_roundtrip() {
        let run = crate::cron::CronRun {
            id: "run-1".to_string(),
            job_id: "job-1".to_string(),
            started_at: chrono::Utc::now(),
            finished_at: None,
            status: "running".to_string(),
            output: None,
            error: None,
        };
        let resp = ResponsePacket::CronHistory {
            request_id: 204,
            runs: vec![run],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronHistory { request_id, runs } => {
                assert_eq!(request_id, 204);
                assert_eq!(runs.len(), 1);
                assert_eq!(runs[0].id, "run-1");
                assert_eq!(runs[0].status, "running");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_request_ids() {
        let req_list = RequestPacket::CronList {
            request_id: 1,
            include_disabled: false,
        };
        assert_eq!(req_list.request_id(), 1);

        let req_add = RequestPacket::CronAdd {
            request_id: 2,
            job: crate::cron::CronJob {
                id: "j".to_string(),
                name: "n".to_string(),
                schedule: crate::cron::ScheduleKind::Every { every_ms: 1000 },
                target: crate::cron::ExecutionTarget::Main,
                agent_id: None,
                message: "m".to_string(),
                delivery: crate::cron::DeliveryMode::None,
                delete_after_run: false,
                enabled: true,
                created_at: chrono::Utc::now(),
                next_run: chrono::Utc::now(),
                last_run: None,
                last_status: None,
                run_count: 0,
            },
        };
        assert_eq!(req_add.request_id(), 2);

        let req_remove = RequestPacket::CronRemove {
            request_id: 3,
            job_id: "j".to_string(),
        };
        assert_eq!(req_remove.request_id(), 3);

        let req_run = RequestPacket::CronRun {
            request_id: 4,
            job_id: "j".to_string(),
        };
        assert_eq!(req_run.request_id(), 4);

        let req_history = RequestPacket::CronHistory {
            request_id: 5,
            job_id: "j".to_string(),
            limit: 5,
        };
        assert_eq!(req_history.request_id(), 5);
    }

    #[test]
    fn test_cron_response_ids() {
        let resp_list = ResponsePacket::CronList {
            request_id: 10,
            jobs: vec![],
        };
        assert_eq!(resp_list.request_id(), 10);

        let resp_added = ResponsePacket::CronAdded {
            request_id: 11,
            job_id: "j".to_string(),
        };
        assert_eq!(resp_added.request_id(), 11);

        let resp_removed = ResponsePacket::CronRemoved {
            request_id: 12,
            job_id: "j".to_string(),
        };
        assert_eq!(resp_removed.request_id(), 12);

        let resp_run_started = ResponsePacket::CronRunStarted {
            request_id: 13,
            job_id: "j".to_string(),
            run_id: "r".to_string(),
        };
        assert_eq!(resp_run_started.request_id(), 13);

        let resp_history = ResponsePacket::CronHistory {
            request_id: 14,
            runs: vec![],
        };
        assert_eq!(resp_history.request_id(), 14);
    }

    #[test]
    fn test_agent_list_request_roundtrip() {
        let req = RequestPacket::AgentList {
            request_id: 300,
            team_filter: Some("default".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentList {
                request_id,
                team_filter,
            } => {
                assert_eq!(request_id, 300);
                assert_eq!(team_filter, Some("default".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_get_request_roundtrip() {
        let req = RequestPacket::AgentGet {
            request_id: 301,
            name: "test-agent".to_string(),
            team: Some("default".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentGet {
                request_id,
                name,
                team,
            } => {
                assert_eq!(request_id, 301);
                assert_eq!(name, "test-agent");
                assert_eq!(team, Some("default".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_create_request_roundtrip() {
        let request = crate::common::types::agent::AgentCreateRequest {
            name: "new-agent".to_string(),
            provider: "ollama".to_string(),
            model: Some("llama3.2".to_string()),
            description: Some("A test agent".to_string()),
            force: false,
            host_runtime_id: None,
            owner_id: None,
        };
        let req = RequestPacket::AgentCreate {
            request_id: 302,
            request: request.clone(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentCreate {
                request_id,
                request: decoded_request,
            } => {
                assert_eq!(request_id, 302);
                assert_eq!(decoded_request.name, "new-agent");
                assert_eq!(decoded_request.provider, "ollama");
                assert_eq!(decoded_request.model, Some("llama3.2".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_delete_request_roundtrip() {
        let req = RequestPacket::AgentDelete {
            request_id: 303,
            name: "old-agent".to_string(),
            team: Some("default".to_string()),
            force: true,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentDelete {
                request_id,
                name,
                team,
                force,
            } => {
                assert_eq!(request_id, 303);
                assert_eq!(name, "old-agent");
                assert_eq!(team, Some("default".to_string()));
                assert!(force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_move_request_roundtrip() {
        let req = RequestPacket::AgentMove {
            request_id: 304,
            old_name: "old-agent".to_string(),
            new_name: "new-agent".to_string(),
            team: Some("default".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentMove {
                request_id,
                old_name,
                new_name,
                team,
            } => {
                assert_eq!(request_id, 304);
                assert_eq!(old_name, "old-agent");
                assert_eq!(new_name, "new-agent");
                assert_eq!(team, Some("default".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_list_request_roundtrip() {
        let req = RequestPacket::TeamList { request_id: 400 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamList { request_id } => {
                assert_eq!(request_id, 400);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_get_request_roundtrip() {
        let req = RequestPacket::TeamGet {
            request_id: 401,
            name: "default".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamGet { request_id, name } => {
                assert_eq!(request_id, 401);
                assert_eq!(name, "default");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_list_request_roundtrip() {
        let req = RequestPacket::SessionList {
            request_id: 500,
            agent: Some("test-agent".to_string()),
            team: Some("test-team".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionList {
                request_id,
                agent,
                team,
            } => {
                assert_eq!(request_id, 500);
                assert_eq!(agent, Some("test-agent".to_string()));
                assert_eq!(team, Some("test-team".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_get_request_roundtrip() {
        let req = RequestPacket::SessionGet {
            request_id: 501,
            id: "sess-123".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionGet { request_id, id } => {
                assert_eq!(request_id, 501);
                assert_eq!(id, "sess-123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_list_response_roundtrip() {
        let resp = ResponsePacket::AgentList {
            request_id: 600,
            agents: vec![crate::common::types::agent::AgentSummary {
                name: "test-agent".to_string(),
                config: crate::types::agent::AgentConfig {
                    name: "test-agent".to_string(),
                    ..Default::default()
                },
                config_path: std::path::PathBuf::from("/tmp/test-agent/config.toml"),
                memberships: vec![],
            }],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentList { request_id, agents } => {
                assert_eq!(request_id, 600);
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].name, "test-agent");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_get_response_roundtrip() {
        let resp = ResponsePacket::AgentGet {
            request_id: 601,
            agent: Some(crate::common::types::agent::AgentInfo {
                name: "test-agent".to_string(),
                config: crate::types::agent::AgentConfig {
                    name: "test-agent".to_string(),
                    ..Default::default()
                },
                config_path: std::path::PathBuf::from("/tmp/test-agent/config.toml"),
                sessions_dir: std::path::PathBuf::from("/tmp/test-agent/sessions"),
                session_count: 0,
                memberships: vec![],
                system_prompt: None,
            }),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentGet { request_id, agent } => {
                assert_eq!(request_id, 601);
                assert!(agent.is_some());
                assert_eq!(agent.unwrap().name, "test-agent");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_created_response_roundtrip() {
        let resp = ResponsePacket::AgentCreated {
            request_id: 602,
            result: crate::common::types::agent::AgentCreationResult {
                name: "new-agent".to_string(),
                config_path: std::path::PathBuf::from("/tmp/new-agent/config.toml"),
                provider: "ollama".to_string(),
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentCreated { request_id, result } => {
                assert_eq!(request_id, 602);
                assert_eq!(result.name, "new-agent");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_deleted_response_roundtrip() {
        let resp = ResponsePacket::AgentDeleted {
            request_id: 603,
            result: crate::common::types::agent::AgentDeleteResult {
                name: "old-agent".to_string(),
                config_deleted: true,
                sessions_deleted: true,
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentDeleted { request_id, result } => {
                assert_eq!(request_id, 603);
                assert!(result.config_deleted);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_moved_response_roundtrip() {
        let resp = ResponsePacket::AgentMoved {
            request_id: 604,
            result: crate::common::types::agent::AgentRenameResult {
                old_name: "old-agent".to_string(),
                new_name: "new-agent".to_string(),
                new_config_path: std::path::PathBuf::from("/tmp/new-agent/config.toml"),
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentMoved { request_id, result } => {
                assert_eq!(request_id, 604);
                assert_eq!(result.old_name, "old-agent");
                assert_eq!(result.new_name, "new-agent");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_list_response_roundtrip() {
        let resp = ResponsePacket::TeamList {
            request_id: 700,
            teams: vec![crate::common::types::team::TeamInfo {
                name: "default".to_string(),
                metadata: crate::common::types::team::TeamMetadata {
                    name: "default".to_string(),
                    description: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    host_runtime_id: "".to_string(),
                    owner_id: "".to_string(),
                    permissions: Vec::new(),
                },
                agent_count: 0,
                members: vec![],
                path: std::path::PathBuf::from("/tmp/teams/default"),
            }],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamList { request_id, teams } => {
                assert_eq!(request_id, 700);
                assert_eq!(teams.len(), 1);
                assert_eq!(teams[0].name, "default");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_get_response_roundtrip() {
        let resp = ResponsePacket::TeamGet {
            request_id: 701,
            team: Some(crate::common::types::team::TeamInfo {
                name: "default".to_string(),
                metadata: crate::common::types::team::TeamMetadata {
                    name: "default".to_string(),
                    description: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    host_runtime_id: "".to_string(),
                    owner_id: "".to_string(),
                    permissions: Vec::new(),
                },
                agent_count: 0,
                members: vec![],
                path: std::path::PathBuf::from("/tmp/teams/default"),
            }),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamGet { request_id, team } => {
                assert_eq!(request_id, 701);
                assert!(team.is_some());
                assert_eq!(team.unwrap().name, "default");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_list_response_roundtrip() {
        let resp = ResponsePacket::SessionList {
            request_id: 800,
            sessions: vec![crate::common::services::session_service::SessionInfo {
                id: "sess-123".to_string(),
                agent_name: "test-agent".to_string(),
                created_at: 0,
                updated_at: 0,
                turn_count: 0,
                message_count: 0,
                context_window: 0,
                total_input_tokens: 0,
                total_output_tokens: 0,
                parent_session_id: None,
                title: None,
            }],
            active_session: Some("sess-123".to_string()),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionList {
                request_id,
                sessions,
                active_session,
            } => {
                assert_eq!(request_id, 800);
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, "sess-123");
                assert_eq!(active_session, Some("sess-123".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_get_response_roundtrip() {
        let resp = ResponsePacket::SessionGet {
            request_id: 801,
            session: Some(crate::common::services::session_service::SessionDetails {
                info: crate::common::services::session_service::SessionInfo {
                    id: "sess-123".to_string(),
                    agent_name: "test-agent".to_string(),
                    created_at: 0,
                    updated_at: 0,
                    turn_count: 0,
                    message_count: 0,
                    context_window: 0,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    parent_session_id: None,
                    title: None,
                },
                history_summary: crate::common::services::session_service::HistorySummary::default(
                ),
            }),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionGet {
                request_id,
                session,
            } => {
                assert_eq!(request_id, 801);
                assert!(session.is_some());
                assert_eq!(session.unwrap().info.id, "sess-123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_crud_request_ids() {
        let req_agent_list = RequestPacket::AgentList {
            request_id: 1,
            team_filter: None,
        };
        assert_eq!(req_agent_list.request_id(), 1);

        let req_agent_get = RequestPacket::AgentGet {
            request_id: 2,
            name: "a".to_string(),
            team: None,
        };
        assert_eq!(req_agent_get.request_id(), 2);

        let req_agent_create = RequestPacket::AgentCreate {
            request_id: 3,
            request: crate::common::types::agent::AgentCreateRequest::new("a", "ollama"),
        };
        assert_eq!(req_agent_create.request_id(), 3);

        let req_agent_delete = RequestPacket::AgentDelete {
            request_id: 4,
            name: "a".to_string(),
            team: None,
            force: false,
        };
        assert_eq!(req_agent_delete.request_id(), 4);

        let req_agent_move = RequestPacket::AgentMove {
            request_id: 4,
            old_name: "a".to_string(),
            new_name: "b".to_string(),
            team: None,
        };
        assert_eq!(req_agent_move.request_id(), 4);

        let req_team_list = RequestPacket::TeamList { request_id: 5 };
        assert_eq!(req_team_list.request_id(), 5);

        let req_team_get = RequestPacket::TeamGet {
            request_id: 6,
            name: "t".to_string(),
        };
        assert_eq!(req_team_get.request_id(), 6);

        let req_session_list = RequestPacket::SessionList {
            request_id: 7,
            agent: None,
            team: None,
        };
        assert_eq!(req_session_list.request_id(), 7);

        let req_session_get = RequestPacket::SessionGet {
            request_id: 8,
            id: "s".to_string(),
        };
        assert_eq!(req_session_get.request_id(), 8);
    }

    #[test]
    fn test_crud_response_ids() {
        let resp_agent_list = ResponsePacket::AgentList {
            request_id: 10,
            agents: vec![],
        };
        assert_eq!(resp_agent_list.request_id(), 10);

        let resp_agent_get = ResponsePacket::AgentGet {
            request_id: 11,
            agent: None,
        };
        assert_eq!(resp_agent_get.request_id(), 11);

        let resp_agent_created = ResponsePacket::AgentCreated {
            request_id: 12,
            result: crate::common::types::agent::AgentCreationResult {
                name: "a".to_string(),
                config_path: std::path::PathBuf::from("/tmp"),
                provider: "p".to_string(),
            },
        };
        assert_eq!(resp_agent_created.request_id(), 12);

        let resp_agent_deleted = ResponsePacket::AgentDeleted {
            request_id: 13,
            result: crate::common::types::agent::AgentDeleteResult {
                name: "a".to_string(),
                config_deleted: true,
                sessions_deleted: false,
            },
        };
        assert_eq!(resp_agent_deleted.request_id(), 13);

        let resp_agent_moved = ResponsePacket::AgentMoved {
            request_id: 13,
            result: crate::common::types::agent::AgentRenameResult {
                old_name: "a".to_string(),
                new_name: "b".to_string(),
                new_config_path: std::path::PathBuf::from("/tmp"),
            },
        };
        assert_eq!(resp_agent_moved.request_id(), 13);

        let resp_team_list = ResponsePacket::TeamList {
            request_id: 14,
            teams: vec![],
        };
        assert_eq!(resp_team_list.request_id(), 14);

        let resp_team_get = ResponsePacket::TeamGet {
            request_id: 15,
            team: None,
        };
        assert_eq!(resp_team_get.request_id(), 15);

        let resp_session_list = ResponsePacket::SessionList {
            request_id: 16,
            sessions: vec![],
            active_session: None,
        };
        assert_eq!(resp_session_list.request_id(), 16);

        let resp_session_get = ResponsePacket::SessionGet {
            request_id: 17,
            session: None,
        };
        assert_eq!(resp_session_get.request_id(), 17);
    }

    #[test]
    fn test_system_status_request_roundtrip() {
        let req = RequestPacket::SystemStatus { request_id: 900 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SystemStatus { request_id } => {
                assert_eq!(request_id, 900);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_doctor_request_roundtrip() {
        let req = RequestPacket::SystemDoctor { request_id: 901 };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SystemDoctor { request_id } => {
                assert_eq!(request_id, 901);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_status_response_roundtrip() {
        let resp = ResponsePacket::SystemStatus {
            request_id: 902,
            version: "1.0.0".to_string(),
            uptime_secs: 12345,
            degraded: false,
            instance_count: 3,
            team_count: 2,
            ready: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SystemStatus {
                request_id,
                version,
                uptime_secs,
                degraded,
                instance_count,
                team_count,
                ready,
            } => {
                assert_eq!(request_id, 902);
                assert_eq!(version, "1.0.0");
                assert_eq!(uptime_secs, 12345);
                assert!(!degraded);
                assert_eq!(instance_count, 3);
                assert_eq!(team_count, 2);
                assert!(ready);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_doctor_response_roundtrip() {
        let resp = ResponsePacket::SystemDoctor {
            request_id: 903,
            checks: vec![
                DoctorCheck {
                    name: "daemon_ready".to_string(),
                    status: "pass".to_string(),
                    message: "Daemon is ready".to_string(),
                    suggestion: None,
                },
                DoctorCheck {
                    name: "not_degraded".to_string(),
                    status: "warn".to_string(),
                    message: "Daemon is in degraded mode".to_string(),
                    suggestion: Some("Restart daemon".to_string()),
                },
            ],
            passed: 1,
            failed: 0,
            warnings: 1,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SystemDoctor {
                request_id,
                checks,
                passed,
                failed,
                warnings,
            } => {
                assert_eq!(request_id, 903);
                assert_eq!(checks.len(), 2);
                assert_eq!(checks[0].name, "daemon_ready");
                assert_eq!(checks[0].status, "pass");
                assert_eq!(checks[1].name, "not_degraded");
                assert_eq!(checks[1].status, "warn");
                assert_eq!(checks[1].suggestion, Some("Restart daemon".to_string()));
                assert_eq!(passed, 1);
                assert_eq!(failed, 0);
                assert_eq!(warnings, 1);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_request_ids() {
        let req_status = RequestPacket::SystemStatus { request_id: 1 };
        assert_eq!(req_status.request_id(), 1);

        let req_doctor = RequestPacket::SystemDoctor { request_id: 2 };
        assert_eq!(req_doctor.request_id(), 2);
    }

    #[test]
    fn test_system_response_ids() {
        let resp_status = ResponsePacket::SystemStatus {
            request_id: 10,
            version: "0.1.0".to_string(),
            uptime_secs: 0,
            degraded: false,
            instance_count: 0,
            team_count: 0,
            ready: false,
        };
        assert_eq!(resp_status.request_id(), 10);

        let resp_doctor = ResponsePacket::SystemDoctor {
            request_id: 11,
            checks: vec![],
            passed: 0,
            failed: 0,
            warnings: 0,
        };
        assert_eq!(resp_doctor.request_id(), 11);
    }

    #[test]
    fn test_extension_list_request_roundtrip() {
        let req = RequestPacket::ExtensionList {
            request_id: 1000,
            enabled_only: true,
            ext_type: Some("tool".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionList {
                request_id,
                enabled_only,
                ext_type,
            } => {
                assert_eq!(request_id, 1000);
                assert!(enabled_only);
                assert_eq!(ext_type, Some("tool".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_enable_request_roundtrip() {
        let req = RequestPacket::ExtensionEnable {
            request_id: 1001,
            id: "ext-1".to_string(),
            target: Some("all".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionEnable {
                request_id,
                id,
                target,
            } => {
                assert_eq!(request_id, 1001);
                assert_eq!(id, "ext-1");
                assert_eq!(target, Some("all".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_disable_request_roundtrip() {
        let req = RequestPacket::ExtensionDisable {
            request_id: 1002,
            id: "ext-1".to_string(),
            target: None,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionDisable {
                request_id,
                id,
                target,
            } => {
                assert_eq!(request_id, 1002);
                assert_eq!(id, "ext-1");
                assert_eq!(target, None);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_clean_request_roundtrip() {
        let req = RequestPacket::SystemClean {
            request_id: 1003,
            scope: Some("logs".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SystemClean { request_id, scope } => {
                assert_eq!(request_id, 1003);
                assert_eq!(scope, Some("logs".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_list_response_roundtrip() {
        let resp = ResponsePacket::ExtensionList {
            request_id: 2000,
            extensions: vec![ExtensionSummary {
                id: "ext-1".to_string(),
                name: "Test Extension".to_string(),
                ext_type: "tool".to_string(),
                version: "1.0.0".to_string(),
                source: "installed".to_string(),
                enabled: true,
                runtime: "running".to_string(),
                description: "A test extension".to_string(),
            }],
            total: 1,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionList {
                request_id,
                extensions,
                total,
            } => {
                assert_eq!(request_id, 2000);
                assert_eq!(extensions.len(), 1);
                assert_eq!(extensions[0].id, "ext-1");
                assert_eq!(extensions[0].name, "Test Extension");
                assert_eq!(extensions[0].ext_type, "tool");
                assert_eq!(extensions[0].version, "1.0.0");
                assert_eq!(extensions[0].source, "installed");
                assert!(extensions[0].enabled);
                assert_eq!(extensions[0].runtime, "running");
                assert_eq!(extensions[0].description, "A test extension");
                assert_eq!(total, 1);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_enabled_response_roundtrip() {
        let resp = ResponsePacket::ExtensionEnabled {
            request_id: 2001,
            id: "ext-1".to_string(),
            message: "Extension enabled successfully".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionEnabled {
                request_id,
                id,
                message,
            } => {
                assert_eq!(request_id, 2001);
                assert_eq!(id, "ext-1");
                assert_eq!(message, "Extension enabled successfully");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_disabled_response_roundtrip() {
        let resp = ResponsePacket::ExtensionDisabled {
            request_id: 2002,
            id: "ext-1".to_string(),
            message: "Extension disabled successfully".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionDisabled {
                request_id,
                id,
                message,
            } => {
                assert_eq!(request_id, 2002);
                assert_eq!(id, "ext-1");
                assert_eq!(message, "Extension disabled successfully");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_cleaned_response_roundtrip() {
        let resp = ResponsePacket::SystemCleaned {
            request_id: 2003,
            cleaned: vec!["logs".to_string(), "temp".to_string()],
            bytes_freed: 1024,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SystemCleaned {
                request_id,
                cleaned,
                bytes_freed,
            } => {
                assert_eq!(request_id, 2003);
                assert_eq!(cleaned, vec!["logs".to_string(), "temp".to_string()]);
                assert_eq!(bytes_freed, 1024);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_request_ids() {
        let req_list = RequestPacket::ExtensionList {
            request_id: 1,
            enabled_only: false,
            ext_type: None,
        };
        assert_eq!(req_list.request_id(), 1);

        let req_enable = RequestPacket::ExtensionEnable {
            request_id: 2,
            id: "e".to_string(),
            target: None,
        };
        assert_eq!(req_enable.request_id(), 2);

        let req_disable = RequestPacket::ExtensionDisable {
            request_id: 3,
            id: "e".to_string(),
            target: None,
        };
        assert_eq!(req_disable.request_id(), 3);

        let req_clean = RequestPacket::SystemClean {
            request_id: 4,
            scope: None,
        };
        assert_eq!(req_clean.request_id(), 4);
    }

    #[test]
    fn test_extension_response_ids() {
        let resp_list = ResponsePacket::ExtensionList {
            request_id: 10,
            extensions: vec![],
            total: 0,
        };
        assert_eq!(resp_list.request_id(), 10);

        let resp_enabled = ResponsePacket::ExtensionEnabled {
            request_id: 11,
            id: "e".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(resp_enabled.request_id(), 11);

        let resp_disabled = ResponsePacket::ExtensionDisabled {
            request_id: 12,
            id: "e".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(resp_disabled.request_id(), 12);

        let resp_cleaned = ResponsePacket::SystemCleaned {
            request_id: 13,
            cleaned: vec![],
            bytes_freed: 0,
        };
        assert_eq!(resp_cleaned.request_id(), 13);
    }

    #[test]
    fn test_extension_install_request_roundtrip() {
        let req = RequestPacket::ExtensionInstall {
            request_id: 1100,
            path: "/path/to/extension".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionInstall { request_id, path } => {
                assert_eq!(request_id, 1100);
                assert_eq!(path, "/path/to/extension");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_uninstall_request_roundtrip() {
        let req = RequestPacket::ExtensionUninstall {
            request_id: 1101,
            id: "ext-1".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionUninstall { request_id, id } => {
                assert_eq!(request_id, 1101);
                assert_eq!(id, "ext-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_installed_response_roundtrip() {
        let resp = ResponsePacket::ExtensionInstalled {
            request_id: 2100,
            id: "ext-1".to_string(),
            message: "Extension 'ext-1' installed successfully".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionInstalled {
                request_id,
                id,
                message,
            } => {
                assert_eq!(request_id, 2100);
                assert_eq!(id, "ext-1");
                assert_eq!(message, "Extension 'ext-1' installed successfully");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_uninstalled_response_roundtrip() {
        let resp = ResponsePacket::ExtensionUninstalled {
            request_id: 2101,
            id: "ext-1".to_string(),
            message: "Extension 'ext-1' uninstalled".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionUninstalled {
                request_id,
                id,
                message,
            } => {
                assert_eq!(request_id, 2101);
                assert_eq!(id, "ext-1");
                assert_eq!(message, "Extension 'ext-1' uninstalled");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_install_uninstall_request_ids() {
        let req_install = RequestPacket::ExtensionInstall {
            request_id: 1,
            path: "/path/to/ext".to_string(),
        };
        assert_eq!(req_install.request_id(), 1);

        let req_uninstall = RequestPacket::ExtensionUninstall {
            request_id: 2,
            id: "ext-1".to_string(),
        };
        assert_eq!(req_uninstall.request_id(), 2);
    }

    #[test]
    fn test_extension_install_uninstall_response_ids() {
        let resp_installed = ResponsePacket::ExtensionInstalled {
            request_id: 10,
            id: "ext-1".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(resp_installed.request_id(), 10);

        let resp_uninstalled = ResponsePacket::ExtensionUninstalled {
            request_id: 11,
            id: "ext-1".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(resp_uninstalled.request_id(), 11);
    }

    #[test]
    fn test_cron_add_simple_request_roundtrip() {
        let req = RequestPacket::CronAddSimple {
            request_id: 1200,
            name: "Daily Report".to_string(),
            schedule: "0 9 * * *".to_string(),
            message: "Generate daily report".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::CronAddSimple {
                request_id,
                name,
                schedule,
                message,
            } => {
                assert_eq!(request_id, 1200);
                assert_eq!(name, "Daily Report");
                assert_eq!(schedule, "0 9 * * *");
                assert_eq!(message, "Generate daily report");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_branch_request_roundtrip() {
        let req = RequestPacket::SessionBranch {
            request_id: 1201,
            agent: "test-agent".to_string(),
            team: Some("default".to_string()),
            session_id: "sess-123".to_string(),
            label: Some("feature-x".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionBranch {
                request_id,
                agent,
                team,
                session_id,
                label,
            } => {
                assert_eq!(request_id, 1201);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, Some("default".to_string()));
                assert_eq!(session_id, "sess-123");
                assert_eq!(label, Some("feature-x".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_compact_request_roundtrip() {
        let req = RequestPacket::SessionCompact {
            request_id: 1202,
            agent: "test-agent".to_string(),
            team: None,
            session_id: "sess-123".to_string(),
            dry_run: true,
            instruction: Some("Summarize".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionCompact {
                request_id,
                agent,
                team,
                session_id,
                dry_run,
                instruction,
            } => {
                assert_eq!(request_id, 1202);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, None);
                assert_eq!(session_id, "sess-123");
                assert!(dry_run);
                assert_eq!(instruction, Some("Summarize".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_cron_added_simple_response_roundtrip() {
        let resp = ResponsePacket::CronAddedSimple {
            request_id: 2200,
            job_id: "cron_abc123".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::CronAddedSimple { request_id, job_id } => {
                assert_eq!(request_id, 2200);
                assert_eq!(job_id, "cron_abc123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_branched_response_roundtrip() {
        let resp = ResponsePacket::SessionBranched {
            request_id: 2201,
            new_session_id: "sess-new".to_string(),
            parent_session_id: "sess-parent".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionBranched {
                request_id,
                new_session_id,
                parent_session_id,
            } => {
                assert_eq!(request_id, 2201);
                assert_eq!(new_session_id, "sess-new");
                assert_eq!(parent_session_id, "sess-parent");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_compacted_response_roundtrip() {
        let resp = ResponsePacket::SessionCompacted {
            request_id: 2202,
            session_id: "sess-123".to_string(),
            messages_compacted: 10,
            tokens_saved: 500,
            tokens_before: 2000,
            tokens_after: 1500,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionCompacted {
                request_id,
                session_id,
                messages_compacted,
                tokens_saved,
                tokens_before,
                tokens_after,
            } => {
                assert_eq!(request_id, 2202);
                assert_eq!(session_id, "sess-123");
                assert_eq!(messages_compacted, 10);
                assert_eq!(tokens_saved, 500);
                assert_eq!(tokens_before, 2000);
                assert_eq!(tokens_after, 1500);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_compact_dry_run_response_roundtrip() {
        let resp = ResponsePacket::SessionCompactDryRun {
            request_id: 2301,
            session_id: "sess-dry".to_string(),
            estimated_tokens: 622,
            context_window: 128_000,
            percent: 0,
            message_count: 12,
            messages_to_compact: 10,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionCompactDryRun {
                request_id,
                session_id,
                estimated_tokens,
                context_window,
                percent,
                message_count,
                messages_to_compact,
            } => {
                assert_eq!(request_id, 2301);
                assert_eq!(session_id, "sess-dry");
                assert_eq!(estimated_tokens, 622);
                assert_eq!(context_window, 128_000);
                assert_eq!(percent, 0);
                assert_eq!(message_count, 12);
                assert_eq!(messages_to_compact, 10);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_new_request_ids() {
        let req_cron = RequestPacket::CronAddSimple {
            request_id: 1,
            name: "n".to_string(),
            schedule: "* * * * *".to_string(),
            message: "m".to_string(),
        };
        assert_eq!(req_cron.request_id(), 1);

        let req_branch = RequestPacket::SessionBranch {
            request_id: 2,
            agent: "a".to_string(),
            team: None,
            session_id: "s".to_string(),
            label: None,
        };
        assert_eq!(req_branch.request_id(), 2);

        let req_compact = RequestPacket::SessionCompact {
            request_id: 3,
            agent: "a".to_string(),
            team: None,
            session_id: "s".to_string(),
            dry_run: false,
            instruction: None,
        };
        assert_eq!(req_compact.request_id(), 3);
    }

    #[test]
    fn test_new_response_ids() {
        let resp_cron = ResponsePacket::CronAddedSimple {
            request_id: 10,
            job_id: "j".to_string(),
        };
        assert_eq!(resp_cron.request_id(), 10);

        let resp_branch = ResponsePacket::SessionBranched {
            request_id: 11,
            new_session_id: "n".to_string(),
            parent_session_id: "p".to_string(),
        };
        assert_eq!(resp_branch.request_id(), 11);

        let resp_compact = ResponsePacket::SessionCompacted {
            request_id: 12,
            session_id: "s".to_string(),
            messages_compacted: 0,
            tokens_saved: 0,
            tokens_before: 0,
            tokens_after: 0,
        };
        assert_eq!(resp_compact.request_id(), 12);
    }

    #[test]
    fn test_agent_export_request_roundtrip() {
        let req = RequestPacket::AgentExport {
            request_id: 1200,
            name: "test-agent".to_string(),
            team: Some("default".to_string()),
            output: Some("/tmp/export.zip".to_string()),
            include_sessions: true,
            with_extensions: false,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentExport {
                request_id,
                name,
                team,
                output,
                include_sessions,
                with_extensions,
            } => {
                assert_eq!(request_id, 1200);
                assert_eq!(name, "test-agent");
                assert_eq!(team, Some("default".to_string()));
                assert_eq!(output, Some("/tmp/export.zip".to_string()));
                assert!(include_sessions);
                assert!(!with_extensions);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_import_request_roundtrip() {
        let req = RequestPacket::AgentImport {
            request_id: 1201,
            file_path: "/tmp/export.zip".to_string(),
            name: Some("renamed-agent".to_string()),
            team: Some("default".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentImport {
                request_id,
                file_path,
                name,
                team,
            } => {
                assert_eq!(request_id, 1201);
                assert_eq!(file_path, "/tmp/export.zip");
                assert_eq!(name, Some("renamed-agent".to_string()));
                assert_eq!(team, Some("default".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_exported_response_roundtrip() {
        let resp = ResponsePacket::AgentExported {
            request_id: 2200,
            name: "test-agent".to_string(),
            output_path: "/tmp/export.zip".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentExported {
                request_id,
                name,
                output_path,
            } => {
                assert_eq!(request_id, 2200);
                assert_eq!(name, "test-agent");
                assert_eq!(output_path, "/tmp/export.zip");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_imported_response_roundtrip() {
        let resp = ResponsePacket::AgentImported {
            request_id: 2201,
            name: "test-agent".to_string(),
            config_path: "/tmp/agents/test-agent/config.toml".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::AgentImported {
                request_id,
                name,
                config_path,
            } => {
                assert_eq!(request_id, 2201);
                assert_eq!(name, "test-agent");
                assert_eq!(config_path, "/tmp/agents/test-agent/config.toml");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_agent_export_import_request_ids() {
        let req_export = RequestPacket::AgentExport {
            request_id: 1,
            name: "a".to_string(),
            team: None,
            output: None,
            include_sessions: false,
            with_extensions: false,
        };
        assert_eq!(req_export.request_id(), 1);

        let req_import = RequestPacket::AgentImport {
            request_id: 2,
            file_path: "/tmp/x.zip".to_string(),
            name: None,
            team: None,
        };
        assert_eq!(req_import.request_id(), 2);
    }

    #[test]
    fn test_agent_export_import_response_ids() {
        let resp_exported = ResponsePacket::AgentExported {
            request_id: 10,
            name: "a".to_string(),
            output_path: "/tmp/x.zip".to_string(),
        };
        assert_eq!(resp_exported.request_id(), 10);

        let resp_imported = ResponsePacket::AgentImported {
            request_id: 11,
            name: "a".to_string(),
            config_path: "/tmp/a/config.toml".to_string(),
        };
        assert_eq!(resp_imported.request_id(), 11);
    }

    #[test]
    fn test_team_export_request_roundtrip() {
        let req = RequestPacket::TeamExport {
            request_id: 1300,
            name: "my-team".to_string(),
            output: Some("/tmp/export.team".to_string()),
            include_sessions: true,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamExport {
                request_id,
                name,
                output,
                include_sessions,
            } => {
                assert_eq!(request_id, 1300);
                assert_eq!(name, "my-team");
                assert_eq!(output, Some("/tmp/export.team".to_string()));
                assert!(include_sessions);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_import_request_roundtrip() {
        let req = RequestPacket::TeamImport {
            request_id: 1301,
            file_path: "/tmp/import.team".to_string(),
            name: Some("new-team".to_string()),
            force: false,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamImport {
                request_id,
                file_path,
                name,
                force,
            } => {
                assert_eq!(request_id, 1301);
                assert_eq!(file_path, "/tmp/import.team");
                assert_eq!(name, Some("new-team".to_string()));
                assert!(!force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_exported_response_roundtrip() {
        let resp = ResponsePacket::TeamExported {
            request_id: 2300,
            name: "my-team".to_string(),
            output_path: "/tmp/export.team".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamExported {
                request_id,
                name,
                output_path,
            } => {
                assert_eq!(request_id, 2300);
                assert_eq!(name, "my-team");
                assert_eq!(output_path, "/tmp/export.team");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_imported_response_roundtrip() {
        let resp = ResponsePacket::TeamImported {
            request_id: 2301,
            name: "new-team".to_string(),
            path: "/tmp/teams/new-team".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamImported {
                request_id,
                name,
                path,
            } => {
                assert_eq!(request_id, 2301);
                assert_eq!(name, "new-team");
                assert_eq!(path, "/tmp/teams/new-team");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_export_import_request_ids() {
        let req_export = RequestPacket::TeamExport {
            request_id: 1,
            name: "t".to_string(),
            output: None,
            include_sessions: false,
        };
        assert_eq!(req_export.request_id(), 1);

        let req_import = RequestPacket::TeamImport {
            request_id: 2,
            file_path: "/tmp/f.team".to_string(),
            name: None,
            force: false,
        };
        assert_eq!(req_import.request_id(), 2);
    }

    #[test]
    fn test_team_export_import_response_ids() {
        let resp_exported = ResponsePacket::TeamExported {
            request_id: 10,
            name: "t".to_string(),
            output_path: "/tmp/e.team".to_string(),
        };
        assert_eq!(resp_exported.request_id(), 10);

        let resp_imported = ResponsePacket::TeamImported {
            request_id: 11,
            name: "t".to_string(),
            path: "/tmp/t".to_string(),
        };
        assert_eq!(resp_imported.request_id(), 11);
    }

    #[test]
    fn test_registry_pull_request_roundtrip() {
        let req = RequestPacket::RegistryPull {
            request_id: 1400,
            registry_ref: "my-agent:v1.0".to_string(),
            team: Some("default".to_string()),
            force: false,
            registry_token: Some("token123".to_string()),
            registry_host: Some("pekohub.org".to_string()),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::RegistryPull {
                request_id,
                registry_ref,
                team,
                force,
                registry_token,
                registry_host,
            } => {
                assert_eq!(request_id, 1400);
                assert_eq!(registry_ref, "my-agent:v1.0");
                assert_eq!(team, Some("default".to_string()));
                assert!(!force);
                assert_eq!(registry_token, Some("token123".to_string()));
                assert_eq!(registry_host, Some("pekohub.org".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_registry_pulled_response_roundtrip() {
        let resp = ResponsePacket::RegistryPulled {
            request_id: 2400,
            name: "my-agent".to_string(),
            version: "v1.0".to_string(),
            digest: "sha256:abc123".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::RegistryPulled {
                request_id,
                name,
                version,
                digest,
            } => {
                assert_eq!(request_id, 2400);
                assert_eq!(name, "my-agent");
                assert_eq!(version, "v1.0");
                assert_eq!(digest, "sha256:abc123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_registry_pull_request_id() {
        let req = RequestPacket::RegistryPull {
            request_id: 1,
            registry_ref: "a".to_string(),
            team: None,
            force: false,
            registry_token: None,
            registry_host: None,
        };
        assert_eq!(req.request_id(), 1);
    }

    #[test]
    fn test_registry_pulled_response_id() {
        let resp = ResponsePacket::RegistryPulled {
            request_id: 10,
            name: "a".to_string(),
            version: "v1".to_string(),
            digest: "sha256:d".to_string(),
        };
        assert_eq!(resp.request_id(), 10);
    }

    // ─── Team operations tests ──────────────────────────────────────

    #[test]
    fn test_team_create_request_roundtrip() {
        let req = RequestPacket::TeamCreate {
            request_id: 1500,
            name: "new-team".to_string(),
            description: Some("A new team".to_string()),
            members: None,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamCreate {
                request_id,
                name,
                description,
                ..
            } => {
                assert_eq!(request_id, 1500);
                assert_eq!(name, "new-team");
                assert_eq!(description, Some("A new team".to_string()));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_delete_request_roundtrip() {
        let req = RequestPacket::TeamDelete {
            request_id: 1501,
            name: "old-team".to_string(),
            force: true,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamDelete {
                request_id,
                name,
                force,
            } => {
                assert_eq!(request_id, 1501);
                assert_eq!(name, "old-team");
                assert!(force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_move_request_roundtrip() {
        let req = RequestPacket::TeamMove {
            request_id: 1502,
            old_name: "old-team".to_string(),
            new_name: "new-team".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::TeamMove {
                request_id,
                old_name,
                new_name,
            } => {
                assert_eq!(request_id, 1502);
                assert_eq!(old_name, "old-team");
                assert_eq!(new_name, "new-team");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_created_response_roundtrip() {
        let resp = ResponsePacket::TeamCreated {
            request_id: 2500,
            result: crate::common::types::team::TeamCreationResult {
                metadata: crate::common::types::team::TeamMetadata {
                    name: "new-team".to_string(),
                    description: Some("A new team".to_string()),
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    host_runtime_id: "".to_string(),
                    owner_id: "".to_string(),
                    permissions: Vec::new(),
                },
                path: std::path::PathBuf::from("/tmp/teams/new-team"),
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamCreated { request_id, result } => {
                assert_eq!(request_id, 2500);
                assert_eq!(result.metadata.name, "new-team");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_deleted_response_roundtrip() {
        let resp = ResponsePacket::TeamDeleted {
            request_id: 2501,
            result: crate::common::types::team::TeamDeletionResult {
                name: "old-team".to_string(),
                agents_deleted: 3,
            },
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamDeleted { request_id, result } => {
                assert_eq!(request_id, 2501);
                assert_eq!(result.name, "old-team");
                assert_eq!(result.agents_deleted, 3);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_moved_response_roundtrip() {
        let resp = ResponsePacket::TeamMoved {
            request_id: 2502,
            old_name: "old-team".to_string(),
            new_name: "new-team".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::TeamMoved {
                request_id,
                old_name,
                new_name,
            } => {
                assert_eq!(request_id, 2502);
                assert_eq!(old_name, "old-team");
                assert_eq!(new_name, "new-team");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_request_ids() {
        let req_create = RequestPacket::TeamCreate {
            request_id: 1,
            name: "t".to_string(),
            description: None,
            members: None,
        };
        assert_eq!(req_create.request_id(), 1);

        let req_delete = RequestPacket::TeamDelete {
            request_id: 2,
            name: "t".to_string(),
            force: false,
        };
        assert_eq!(req_delete.request_id(), 2);

        let req_move = RequestPacket::TeamMove {
            request_id: 3,
            old_name: "a".to_string(),
            new_name: "b".to_string(),
        };
        assert_eq!(req_move.request_id(), 3);
    }

    #[test]
    fn test_team_response_ids() {
        let resp_created = ResponsePacket::TeamCreated {
            request_id: 10,
            result: crate::common::types::team::TeamCreationResult {
                metadata: crate::common::types::team::TeamMetadata {
                    name: "t".to_string(),
                    description: None,
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    host_runtime_id: "".to_string(),
                    owner_id: "".to_string(),
                    permissions: Vec::new(),
                },
                path: std::path::PathBuf::from("/tmp"),
            },
        };
        assert_eq!(resp_created.request_id(), 10);

        let resp_deleted = ResponsePacket::TeamDeleted {
            request_id: 11,
            result: crate::common::types::team::TeamDeletionResult {
                name: "t".to_string(),
                agents_deleted: 0,
            },
        };
        assert_eq!(resp_deleted.request_id(), 11);

        let resp_moved = ResponsePacket::TeamMoved {
            request_id: 12,
            old_name: "a".to_string(),
            new_name: "b".to_string(),
        };
        assert_eq!(resp_moved.request_id(), 12);
    }

    // ─── Session operations tests ───────────────────────────────────

    #[test]
    fn test_session_show_request_roundtrip() {
        let req = RequestPacket::SessionShow {
            request_id: 1600,
            agent: "test-agent".to_string(),
            team: Some("default".to_string()),
            session_id: "sess-123".to_string(),
            history: true,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionShow {
                request_id,
                agent,
                team,
                session_id,
                history,
            } => {
                assert_eq!(request_id, 1600);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, Some("default".to_string()));
                assert_eq!(session_id, "sess-123");
                assert!(history);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_remove_request_roundtrip() {
        let req = RequestPacket::SessionRemove {
            request_id: 1601,
            agent: "test-agent".to_string(),
            team: None,
            session_id: "sess-123".to_string(),
            force: false,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionRemove {
                request_id,
                agent,
                team,
                session_id,
                force,
            } => {
                assert_eq!(request_id, 1601);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, None);
                assert_eq!(session_id, "sess-123");
                assert!(!force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_switch_request_roundtrip() {
        let req = RequestPacket::SessionSwitch {
            request_id: 1602,
            agent: "test-agent".to_string(),
            team: Some("default".to_string()),
            session_id: "sess-123".to_string(),
            user: "alice".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionSwitch {
                request_id,
                agent,
                team,
                session_id,
                user,
            } => {
                assert_eq!(request_id, 1602);
                assert_eq!(agent, "test-agent");
                assert_eq!(team, Some("default".to_string()));
                assert_eq!(session_id, "sess-123");
                assert_eq!(user, "alice");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_shown_response_roundtrip() {
        let resp = ResponsePacket::SessionShown {
            request_id: 2600,
            session: crate::common::services::session_service::SessionDetails {
                info: crate::common::services::session_service::SessionInfo {
                    id: "sess-123".to_string(),
                    agent_name: "test-agent".to_string(),
                    created_at: 0,
                    updated_at: 0,
                    turn_count: 0,
                    message_count: 0,
                    context_window: 0,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    parent_session_id: None,
                    title: None,
                },
                history_summary: crate::common::services::session_service::HistorySummary::default(
                ),
            },
            history: Some(vec![
                crate::common::services::session_service::HistoryEvent::Message {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                    timestamp: "2024-01-01T00:00:00Z".to_string(),
                },
            ]),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionShown {
                request_id,
                session,
                history,
            } => {
                assert_eq!(request_id, 2600);
                assert_eq!(session.info.id, "sess-123");
                assert!(history.is_some());
                assert_eq!(history.unwrap().len(), 1);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_removed_response_roundtrip() {
        let resp = ResponsePacket::SessionRemoved {
            request_id: 2601,
            session_id: "sess-123".to_string(),
            deleted: true,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionRemoved {
                request_id,
                session_id,
                deleted,
            } => {
                assert_eq!(request_id, 2601);
                assert_eq!(session_id, "sess-123");
                assert!(deleted);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_switched_response_roundtrip() {
        let resp = ResponsePacket::SessionSwitched {
            request_id: 2602,
            session_id: "sess-123".to_string(),
            agent: "test-agent".to_string(),
            team: "default".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionSwitched {
                request_id,
                session_id,
                agent,
                team,
            } => {
                assert_eq!(request_id, 2602);
                assert_eq!(session_id, "sess-123");
                assert_eq!(agent, "test-agent");
                assert_eq!(team, "default");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_session_request_ids() {
        let req_show = RequestPacket::SessionShow {
            request_id: 1,
            agent: "a".to_string(),
            team: None,
            session_id: "s".to_string(),
            history: false,
        };
        assert_eq!(req_show.request_id(), 1);

        let req_remove = RequestPacket::SessionRemove {
            request_id: 2,
            agent: "a".to_string(),
            team: None,
            session_id: "s".to_string(),
            force: false,
        };
        assert_eq!(req_remove.request_id(), 2);

        let req_switch = RequestPacket::SessionSwitch {
            request_id: 3,
            agent: "a".to_string(),
            team: None,
            session_id: "s".to_string(),
            user: "u".to_string(),
        };
        assert_eq!(req_switch.request_id(), 3);
    }

    #[test]
    fn test_session_response_ids() {
        let resp_shown = ResponsePacket::SessionShown {
            request_id: 10,
            session: crate::common::services::session_service::SessionDetails {
                info: crate::common::services::session_service::SessionInfo {
                    id: "s".to_string(),
                    agent_name: "a".to_string(),
                    created_at: 0,
                    updated_at: 0,
                    turn_count: 0,
                    message_count: 0,
                    context_window: 0,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    parent_session_id: None,
                    title: None,
                },
                history_summary: crate::common::services::session_service::HistorySummary::default(
                ),
            },
            history: None,
        };
        assert_eq!(resp_shown.request_id(), 10);

        let resp_removed = ResponsePacket::SessionRemoved {
            request_id: 11,
            session_id: "s".to_string(),
            deleted: true,
        };
        assert_eq!(resp_removed.request_id(), 11);

        let resp_switched = ResponsePacket::SessionSwitched {
            request_id: 12,
            session_id: "s".to_string(),
            agent: "a".to_string(),
            team: "t".to_string(),
        };
        assert_eq!(resp_switched.request_id(), 12);
    }

    // ─── Extension operations tests ─────────────────────────────────

    #[test]
    fn test_extension_validate_request_roundtrip() {
        let req = RequestPacket::ExtensionValidate {
            request_id: 1700,
            path: "/path/to/ext".to_string(),
            verbose: true,
            semantic: false,
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionValidate {
                request_id,
                path,
                verbose,
                semantic,
            } => {
                assert_eq!(request_id, 1700);
                assert_eq!(path, "/path/to/ext");
                assert!(verbose);
                assert!(!semantic);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_debug_request_roundtrip() {
        let req = RequestPacket::ExtensionDebug {
            request_id: 1701,
            id: "ext-1".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionDebug { request_id, id } => {
                assert_eq!(request_id, 1701);
                assert_eq!(id, "ext-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_info_request_roundtrip() {
        let req = RequestPacket::ExtensionInfo {
            request_id: 1702,
            id: "ext-1".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionInfo { request_id, id } => {
                assert_eq!(request_id, 1702);
                assert_eq!(id, "ext-1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_export_request_roundtrip() {
        let req = RequestPacket::ExtensionExport {
            request_id: 1703,
            id: "ext-1".to_string(),
            output: "/tmp/export.ext".to_string(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionExport {
                request_id,
                id,
                output,
            } => {
                assert_eq!(request_id, 1703);
                assert_eq!(id, "ext-1");
                assert_eq!(output, "/tmp/export.ext");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_bundle_request_roundtrip() {
        let req = RequestPacket::ExtensionBundle {
            request_id: 1704,
            name: "my-bundle".to_string(),
            ids: vec!["ext-1".to_string(), "ext-2".to_string()],
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::ExtensionBundle {
                request_id,
                name,
                ids,
            } => {
                assert_eq!(request_id, 1704);
                assert_eq!(name, "my-bundle");
                assert_eq!(ids, vec!["ext-1".to_string(), "ext-2".to_string()]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_validated_response_roundtrip() {
        let resp = ResponsePacket::ExtensionValidated {
            request_id: 2700,
            valid: true,
            errors: vec![],
            warnings: vec!["warning-1".to_string()],
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionValidated {
                request_id,
                valid,
                errors,
                warnings,
            } => {
                assert_eq!(request_id, 2700);
                assert!(valid);
                assert!(errors.is_empty());
                assert_eq!(warnings, vec!["warning-1".to_string()]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_debug_info_response_roundtrip() {
        let resp = ResponsePacket::ExtensionDebugInfo {
            request_id: 2701,
            id: "ext-1".to_string(),
            info: serde_json::json!({"hooks": 5}),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionDebugInfo {
                request_id,
                id,
                info,
            } => {
                assert_eq!(request_id, 2701);
                assert_eq!(id, "ext-1");
                assert_eq!(info["hooks"], 5);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_info_response_roundtrip() {
        let resp = ResponsePacket::ExtensionInfoResponse {
            request_id: 2702,
            id: "ext-1".to_string(),
            info: serde_json::json!({"name": "Test Extension"}),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionInfoResponse {
                request_id,
                id,
                info,
            } => {
                assert_eq!(request_id, 2702);
                assert_eq!(id, "ext-1");
                assert_eq!(info["name"], "Test Extension");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_exported_response_roundtrip() {
        let resp = ResponsePacket::ExtensionExported {
            request_id: 2703,
            id: "ext-1".to_string(),
            output: "/tmp/export.ext".to_string(),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionExported {
                request_id,
                id,
                output,
            } => {
                assert_eq!(request_id, 2703);
                assert_eq!(id, "ext-1");
                assert_eq!(output, "/tmp/export.ext");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_bundled_response_roundtrip() {
        let resp = ResponsePacket::ExtensionBundled {
            request_id: 2704,
            name: "my-bundle".to_string(),
            count: 3,
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::ExtensionBundled {
                request_id,
                name,
                count,
            } => {
                assert_eq!(request_id, 2704);
                assert_eq!(name, "my-bundle");
                assert_eq!(count, 3);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_extension_ops_request_ids() {
        let req_validate = RequestPacket::ExtensionValidate {
            request_id: 1,
            path: "/tmp".to_string(),
            verbose: false,
            semantic: false,
        };
        assert_eq!(req_validate.request_id(), 1);

        let req_debug = RequestPacket::ExtensionDebug {
            request_id: 2,
            id: "e".to_string(),
        };
        assert_eq!(req_debug.request_id(), 2);

        let req_info = RequestPacket::ExtensionInfo {
            request_id: 3,
            id: "e".to_string(),
        };
        assert_eq!(req_info.request_id(), 3);

        let req_export = RequestPacket::ExtensionExport {
            request_id: 4,
            id: "e".to_string(),
            output: "/tmp".to_string(),
        };
        assert_eq!(req_export.request_id(), 4);

        let req_bundle = RequestPacket::ExtensionBundle {
            request_id: 5,
            name: "b".to_string(),
            ids: vec![],
        };
        assert_eq!(req_bundle.request_id(), 5);
    }

    #[test]
    fn test_extension_ops_response_ids() {
        let resp_validated = ResponsePacket::ExtensionValidated {
            request_id: 10,
            valid: true,
            errors: vec![],
            warnings: vec![],
        };
        assert_eq!(resp_validated.request_id(), 10);

        let resp_debug = ResponsePacket::ExtensionDebugInfo {
            request_id: 11,
            id: "e".to_string(),
            info: serde_json::Value::Null,
        };
        assert_eq!(resp_debug.request_id(), 11);

        let resp_info = ResponsePacket::ExtensionInfoResponse {
            request_id: 12,
            id: "e".to_string(),
            info: serde_json::Value::Null,
        };
        assert_eq!(resp_info.request_id(), 12);

        let resp_exported = ResponsePacket::ExtensionExported {
            request_id: 13,
            id: "e".to_string(),
            output: "/tmp".to_string(),
        };
        assert_eq!(resp_exported.request_id(), 13);

        let resp_bundled = ResponsePacket::ExtensionBundled {
            request_id: 14,
            name: "b".to_string(),
            count: 0,
        };
        assert_eq!(resp_bundled.request_id(), 14);
    }

    #[test]
    fn test_authenticated_request_roundtrip() {
        // Critical path: auth envelope + request packet must serialize together
        let envelope = AuthenticatedRequest {
            auth: AuthHeader {
                credential: AuthCredential::ApiKey("pkr_testkey123".to_string()),
            },
            packet: RequestPacket::Ping { request_id: 7 },
        };

        let json = serde_json::to_vec(&envelope).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 7);
        match decoded.auth.credential {
            AuthCredential::ApiKey(key) => assert_eq!(key, "pkr_testkey123"),
            other => panic!("Expected ApiKey, got: {:?}", other),
        }
    }

    #[test]
    fn test_authenticated_request_jwt_roundtrip() {
        let envelope = AuthenticatedRequest {
            auth: AuthHeader {
                credential: AuthCredential::Jwt("eyJhbGciOiJIUzI1NiJ9.test".to_string()),
            },
            packet: RequestPacket::SystemStatus { request_id: 8 },
        };

        let json = serde_json::to_vec(&envelope).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 8);
        match decoded.auth.credential {
            AuthCredential::Jwt(token) => {
                assert_eq!(token, "eyJhbGciOiJIUzI1NiJ9.test")
            }
            other => panic!("Expected Jwt, got: {:?}", other),
        }
    }

    #[test]
    fn test_authenticated_request_none_defaults() {
        // Bare RequestPacket deserialized as AuthenticatedRequest should have None auth
        let packet = RequestPacket::Ping { request_id: 9 };
        let json = serde_json::to_vec(&packet).unwrap();
        let decoded: AuthenticatedRequest = serde_json::from_slice(&json).unwrap();

        assert_eq!(decoded.packet.request_id(), 9);
        match decoded.auth.credential {
            AuthCredential::None => (), // Expected
            other => panic!("Expected None credential for bare packet, got: {:?}", other),
        }
    }
}
