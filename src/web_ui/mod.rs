//! Web UI Module
//!
//! This module provides the embedded web UI for Pekobot.
//! The UI is served as a single HTML file at `/ui`.

use crate::api::state::AppState;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};

/// The embedded HTML content
const WEB_UI_HTML: &str = include_str!("index.html");

/// Create the web UI router
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ui", get(serve_web_ui))
        .route("/ui/", get(serve_web_ui))
}

/// Serve the web UI HTML
async fn serve_web_ui() -> impl IntoResponse {
    Html(WEB_UI_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_web_ui_html_embedded() {
        // Verify the HTML is embedded correctly
        assert!(!WEB_UI_HTML.is_empty());
        assert!(WEB_UI_HTML.contains("<!DOCTYPE html>"));
        assert!(WEB_UI_HTML.contains("Pekobot"));
    }
}
