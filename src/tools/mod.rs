//! Tools for agents
//! 
//! Core tools are always included. On-demand tools can be enabled via features.

// Core tools (always included)
pub mod browser;
pub mod filesystem;
pub mod http;
pub mod memory_tool;
pub mod process;
pub mod session_messaging;
pub mod traits;

// On-demand tools (optional, via feature flags)
#[cfg(feature = "calendar")]
pub mod calendar;
#[cfg(feature = "document")]
pub mod document;
#[cfg(feature = "email")]
pub mod email;
#[cfg(feature = "expense")]
pub mod expense;
#[cfg(feature = "inventory")]
pub mod inventory;
#[cfg(feature = "research")]
pub mod research;
#[cfg(feature = "social_media")]
pub mod social_media;

pub use browser::BrowserTool;
pub use filesystem::FileSystemTool;
pub use http::{HttpMethod, HttpTool};
pub use memory_tool::{MemoryTool, MemoryToolFactory};
pub use process::ProcessTool;
pub use session_messaging::{SessionMessagingTool, SessionRegistry};
pub use traits::Tool;

// Re-export on-demand tools when features are enabled
#[cfg(feature = "calendar")]
pub use calendar::{CalendarCredentials, CalendarProvider, CalendarTool};
#[cfg(feature = "document")]
pub use document::DocumentTool;
#[cfg(feature = "email")]
pub use email::{EmailConfig, EmailProvider, EmailTool, ReplyTone};
#[cfg(feature = "expense")]
pub use expense::{ExpenseConfig, ExpenseTool};
#[cfg(feature = "inventory")]
pub use inventory::{EcommercePlatform, InventoryConfig, InventoryTool, PlatformCredentials};
#[cfg(feature = "research")]
pub use research::{CitationStyle, OutputFormat, ResearchConfig, ResearchTool, SearchProvider};
#[cfg(feature = "social_media")]
pub use social_media::SocialMediaTool;
