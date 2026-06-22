//! A2A message request/response types — compatibility re-export.
//!
//! The canonical definitions now live in
//! [`crate::common::types::a2a`](crate::common::types::a2a). This module
//! re-exports them under their original names so existing callers keep
//! compiling without churn.

pub use crate::common::types::a2a::{
    A2aMessageRequest, A2aMessageResponse, MessageRequest, MessageResult, ToolCallInfo,
};
