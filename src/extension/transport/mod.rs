//! Async transport and execution infrastructure
//!
//! This module provides the low-level async task execution infrastructure:
//! - [`AsyncTaskTransport`] trait and implementations
//! - [`AsyncExecutionRouter`] for routing tool execution based on reserved parameters

pub mod async_router;
pub mod async_transport;

// Re-export main types for convenience
pub use async_router::{AsyncExecutionRouter, AsyncReservedParams, ToolExecutionContext};
pub use async_transport::{
    create_local_transport, create_transport, AsyncTaskTransport, DaemonIpcTransport,
    LocalAsyncTransport, UnavailableAsyncTransport,
};
