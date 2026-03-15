//! Navigate tool

use serde_json::Value;
use tracing::info;

/// Navigate to a URL
pub async fn execute(args: Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;

    let wait_for = args
        .get("wait_for")
        .and_then(|v| v.as_str());

    info!("Navigating to: {}", url);

    let tab = super::get_tab()?;

    tab.navigate_to(url)
        .map_err(|e| format!("Navigation failed: {:?}", e))?;

    // Wait for specific element if requested
    if let Some(selector) = wait_for {
        tab.wait_for_element(selector)
            .map_err(|e| format!("Failed to wait for element '{}': {:?}", selector, e))?;
    }

    let title = tab.get_title()
        .map_err(|e| format!("Failed to get title: {:?}", e))?;

    Ok(format!("Navigated to: {} (Title: {})", url, title))
}
