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
    AgentList { request_id: u64, team_filter: Option<String> },

    #[serde(rename = "agent_get")]
    AgentGet { request_id: u64, name: String, team: Option<String> },

    #[serde(rename = "agent_create")]
    AgentCreate { request_id: u64, request: crate::common::types::agent::AgentCreateRequest },

    #[serde(rename = "agent_delete")]
    AgentDelete { request_id: u64, name: String, team: Option<String>, force: bool },

    // ─── Team CRUD ──────────────────────────────────────────────────
    #[serde(rename = "team_list")]
    TeamList { request_id: u64 },

    #[serde(rename = "team_get")]
    TeamGet { request_id: u64, name: String },

    // ─── Session CRUD ───────────────────────────────────────────────
    #[serde(rename = "session_list")]
    SessionList { request_id: u64, agent: Option<String> },

    #[serde(rename = "session_get")]
    SessionGet { request_id: u64, id: String },

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

    #[serde(rename = "system_clean")]
    SystemClean {
        request_id: u64,
        scope: Option<String>,
    },

    /// Install an extension from a path
    #[serde(rename = "extension_install")]
    ExtensionInstall {
        request_id: u64,
        path: String,
    },

    /// Uninstall an extension by ID
    #[serde(rename = "extension_uninstall")]
    ExtensionUninstall {
        request_id: u64,
        id: String,
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
            | Self::SessionList { request_id, .. }
            | Self::SessionGet { request_id, .. }
            | Self::SystemStatus { request_id }
            | Self::SystemDoctor { request_id }
            | Self::ExtensionList { request_id, .. }
            | Self::ExtensionEnable { request_id, .. }
            | Self::ExtensionDisable { request_id, .. }
            | Self::SystemClean { request_id, .. }
            | Self::ExtensionInstall { request_id, .. }
            | Self::ExtensionUninstall { request_id, .. } => *request_id,
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
    AgentList { request_id: u64, agents: Vec<crate::common::types::agent::AgentSummary> },

    /// Agent detail response
    #[serde(rename = "agent_get")]
    AgentGet { request_id: u64, agent: Option<crate::common::types::agent::AgentInfo> },

    /// Agent created response
    #[serde(rename = "agent_created")]
    AgentCreated { request_id: u64, result: crate::common::types::agent::AgentCreationResult },

    /// Agent deleted response
    #[serde(rename = "agent_deleted")]
    AgentDeleted { request_id: u64, result: crate::common::types::agent::AgentDeleteResult },

    /// Team list response
    #[serde(rename = "team_list")]
    TeamList { request_id: u64, teams: Vec<crate::common::types::team::TeamInfo> },

    /// Team detail response
    #[serde(rename = "team_get")]
    TeamGet { request_id: u64, team: Option<crate::common::types::team::TeamInfo> },

    /// Session list response
    #[serde(rename = "session_list")]
    SessionList { request_id: u64, sessions: Vec<crate::common::services::session_service::SessionInfo> },

    /// Session detail response
    #[serde(rename = "session_get")]
    SessionGet { request_id: u64, session: Option<crate::common::services::session_service::SessionDetails> },

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

    /// System clean response
    #[serde(rename = "system_cleaned")]
    SystemCleaned {
        request_id: u64,
        cleaned: Vec<String>,
        bytes_freed: u64,
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
}

/// Summary of an extension for IPC responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionSummary {
    pub id: String,
    pub name: String,
    pub ext_type: String,
    pub version: String,
    pub source: String,      // "built-in" or "installed"
    pub enabled: bool,
    pub runtime: String,     // "running", "stopped", or "n/a"
    pub description: String,
}

/// A single doctor check result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub message: String,
    pub suggestion: Option<String>,
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
            | Self::SessionList { request_id, .. }
            | Self::SessionGet { request_id, .. }
            | Self::SystemStatus { request_id, .. }
            | Self::SystemDoctor { request_id, .. }
            | Self::ExtensionList { request_id, .. }
            | Self::ExtensionEnabled { request_id, .. }
            | Self::ExtensionDisabled { request_id, .. }
            | Self::SystemCleaned { request_id, .. }
            | Self::ExtensionInstalled { request_id, .. }
            | Self::ExtensionUninstalled { request_id, .. } => *request_id,
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
            RequestPacket::AgentList { request_id, team_filter } => {
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
            RequestPacket::AgentGet { request_id, name, team } => {
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
            team: Some("default".to_string()),
            provider: "ollama".to_string(),
            model: Some("llama3.2".to_string()),
            description: Some("A test agent".to_string()),
            auto_create_team: true,
            force: false,
        };
        let req = RequestPacket::AgentCreate {
            request_id: 302,
            request: request.clone(),
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::AgentCreate { request_id, request: decoded_request } => {
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
            RequestPacket::AgentDelete { request_id, name, team, force } => {
                assert_eq!(request_id, 303);
                assert_eq!(name, "old-agent");
                assert_eq!(team, Some("default".to_string()));
                assert!(force);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_team_list_request_roundtrip() {
        let req = RequestPacket::TeamList {
            request_id: 400,
        };
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
        };
        let bytes = req.to_bytes().unwrap();
        let decoded = RequestPacket::from_bytes(&bytes).unwrap();
        match decoded {
            RequestPacket::SessionList { request_id, agent } => {
                assert_eq!(request_id, 500);
                assert_eq!(agent, Some("test-agent".to_string()));
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
                team: "default".to_string(),
                config: crate::types::agent::AgentConfig {
                    name: "test-agent".to_string(),
                    ..Default::default()
                },
                config_path: std::path::PathBuf::from("/tmp/test-agent/config.toml"),
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
                team: "default".to_string(),
                config: crate::types::agent::AgentConfig {
                    name: "test-agent".to_string(),
                    ..Default::default()
                },
                config_path: std::path::PathBuf::from("/tmp/test-agent/config.toml"),
                sessions_dir: std::path::PathBuf::from("/tmp/test-agent/sessions"),
                session_count: 0,
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
                team: "default".to_string(),
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
                team: "default".to_string(),
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
    fn test_team_list_response_roundtrip() {
        let resp = ResponsePacket::TeamList {
            request_id: 700,
            teams: vec![crate::common::types::team::TeamInfo {
                name: "default".to_string(),
                metadata: None,
                agent_count: 0,
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
                metadata: None,
                agent_count: 0,
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
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionList { request_id, sessions } => {
                assert_eq!(request_id, 800);
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].id, "sess-123");
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
                history_summary: crate::common::services::session_service::HistorySummary::default(),
            }),
        };
        let bytes = resp.to_bytes().unwrap();
        let decoded = ResponsePacket::from_bytes(&bytes).unwrap();
        match decoded {
            ResponsePacket::SessionGet { request_id, session } => {
                assert_eq!(request_id, 801);
                assert!(session.is_some());
                assert_eq!(session.unwrap().info.id, "sess-123");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_crud_request_ids() {
        let req_agent_list = RequestPacket::AgentList { request_id: 1, team_filter: None };
        assert_eq!(req_agent_list.request_id(), 1);

        let req_agent_get = RequestPacket::AgentGet { request_id: 2, name: "a".to_string(), team: None };
        assert_eq!(req_agent_get.request_id(), 2);

        let req_agent_create = RequestPacket::AgentCreate {
            request_id: 3,
            request: crate::common::types::agent::AgentCreateRequest::new("a", "ollama"),
        };
        assert_eq!(req_agent_create.request_id(), 3);

        let req_agent_delete = RequestPacket::AgentDelete { request_id: 4, name: "a".to_string(), team: None, force: false };
        assert_eq!(req_agent_delete.request_id(), 4);

        let req_team_list = RequestPacket::TeamList { request_id: 5 };
        assert_eq!(req_team_list.request_id(), 5);

        let req_team_get = RequestPacket::TeamGet { request_id: 6, name: "t".to_string() };
        assert_eq!(req_team_get.request_id(), 6);

        let req_session_list = RequestPacket::SessionList { request_id: 7, agent: None };
        assert_eq!(req_session_list.request_id(), 7);

        let req_session_get = RequestPacket::SessionGet { request_id: 8, id: "s".to_string() };
        assert_eq!(req_session_get.request_id(), 8);
    }

    #[test]
    fn test_crud_response_ids() {
        let resp_agent_list = ResponsePacket::AgentList { request_id: 10, agents: vec![] };
        assert_eq!(resp_agent_list.request_id(), 10);

        let resp_agent_get = ResponsePacket::AgentGet { request_id: 11, agent: None };
        assert_eq!(resp_agent_get.request_id(), 11);

        let resp_agent_created = ResponsePacket::AgentCreated {
            request_id: 12,
            result: crate::common::types::agent::AgentCreationResult {
                name: "a".to_string(),
                team: "t".to_string(),
                config_path: std::path::PathBuf::from("/tmp"),
                provider: "p".to_string(),
            },
        };
        assert_eq!(resp_agent_created.request_id(), 12);

        let resp_agent_deleted = ResponsePacket::AgentDeleted {
            request_id: 13,
            result: crate::common::types::agent::AgentDeleteResult {
                name: "a".to_string(),
                team: "t".to_string(),
                config_deleted: true,
                sessions_deleted: false,
            },
        };
        assert_eq!(resp_agent_deleted.request_id(), 13);

        let resp_team_list = ResponsePacket::TeamList { request_id: 14, teams: vec![] };
        assert_eq!(resp_team_list.request_id(), 14);

        let resp_team_get = ResponsePacket::TeamGet { request_id: 15, team: None };
        assert_eq!(resp_team_get.request_id(), 15);

        let resp_session_list = ResponsePacket::SessionList { request_id: 16, sessions: vec![] };
        assert_eq!(resp_session_list.request_id(), 16);

        let resp_session_get = ResponsePacket::SessionGet { request_id: 17, session: None };
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
            ResponsePacket::SystemStatus { request_id, version, uptime_secs, degraded, instance_count, team_count, ready } => {
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
            ResponsePacket::SystemDoctor { request_id, checks, passed, failed, warnings } => {
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
            RequestPacket::SystemClean {
                request_id,
                scope,
            } => {
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
            extensions: vec![
                ExtensionSummary {
                    id: "ext-1".to_string(),
                    name: "Test Extension".to_string(),
                    ext_type: "tool".to_string(),
                    version: "1.0.0".to_string(),
                    source: "installed".to_string(),
                    enabled: true,
                    runtime: "running".to_string(),
                    description: "A test extension".to_string(),
                },
            ],
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
            ResponsePacket::ExtensionInstalled { request_id, id, message } => {
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
            ResponsePacket::ExtensionUninstalled { request_id, id, message } => {
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
}
