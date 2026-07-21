//! Local re-export of `peko_extension_api` types for ergonomic
//! `peko_extension_host::types::*` paths. The host depends on
//! `peko-extension-api` for the contract types it consumes; this
//! module just gives them a stable home that mirrors the old
//! `crate::extensions::framework::types` layout.

pub use peko_extension_api::*;

pub mod async_types {
    pub use peko_extension_api::async_types::*;
}

pub mod capabilities {
    pub use peko_extension_api::capabilities::*;
}

pub mod hook_io {
    pub use peko_extension_api::hook_io::*;
}

pub mod manifest {
    pub use peko_extension_api::manifest::*;
}

pub mod session {
    pub use peko_extension_api::session::*;
}

pub mod tool {
    pub use peko_extension_api::tool::*;
}
