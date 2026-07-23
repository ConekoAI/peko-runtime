//! Transport layer for HTTP communication with LLM providers
//!
//! This module provides shared HTTP client and SSE parsing functionality
//! used by all provider adapters.

pub mod client;
pub mod retry;
pub mod sse;

pub use client::{AuthConfig, HttpClient};
pub use retry::{RetryExecutor, RetryPolicy, RetryableError};
pub use sse::SseParser;
