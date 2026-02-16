//! Coneko network integration (optional)

pub mod client;
pub mod registry;

/// Coneko adapter
pub struct ConekoAdapter {
    enabled: bool,
    endpoint: String,
}

impl ConekoAdapter {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            endpoint: "".to_string(),
        }
    }

    pub fn enabled(endpoint: &str) -> Self {
        Self {
            enabled: true,
            endpoint: endpoint.to_string(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}
