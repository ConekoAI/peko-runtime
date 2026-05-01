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
    CronList { request_id: u64, include_disabled: bool },

    /// Add a cron job
    #[serde(rename = "cron_add")]
    CronAdd { request_id: u64, job: crate::cron::CronJob },

    /// Remove a cron job
    #[serde(rename = "cron_remove")]
    CronRemove { request_id: u64, job_id: String },

    /// Run a cron job immediately
    #[serde(rename = "cron_run")]
    CronRun { request_id: u64, job_id: String },

    /// Get cron job history
    #[serde(rename = "cron_history")]
    CronHistory { request_id: u64, job_id: String, limit: usize },
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
            | Self::CronHistory { request_id, .. } => *request_id,
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
        receipt: crate::tools::AsyncTaskReceipt,
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
    CronList { request_id: u64, jobs: Vec<crate::cron::CronJob> },

    /// Cron job added response
    #[serde(rename = "cron_added")]
    CronAdded { request_id: u64, job_id: String },

    /// Cron job removed response
    #[serde(rename = "cron_removed")]
    CronRemoved { request_id: u64, job_id: String },

    /// Cron job run started response
    #[serde(rename = "cron_run_started")]
    CronRunStarted { request_id: u64, job_id: String, run_id: String },

    /// Cron job history response
    #[serde(rename = "cron_history")]
    CronHistory { request_id: u64, runs: Vec<crate::cron::CronRun> },
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
            | Self::CronHistory { request_id, .. } => *request_id,
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
}
