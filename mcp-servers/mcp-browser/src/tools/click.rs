//! Click tool

use serde_json::Value;
use tracing::info;

/// Click an element
pub async fn execute(args: Value) -> Result<String, String> {
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    info!("Clicking element: {}", selector);

    let tab = super::get_tab()?;
    let element = super::wait_for_element(&tab, selector)?;

    element.click()
        .map_err(|e| format!("Click failed: {:?}", e))?;

    // Small delay after click
    std::thread::sleep(std::time::Duration::from_millis(500));

    Ok(format!("Clicked element: {}", selector))
}
