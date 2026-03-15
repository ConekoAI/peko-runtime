//! Type text tool

use serde_json::Value;
use tracing::info;

/// Type text into an input
pub async fn execute(args: Value) -> Result<String, String> {
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'text' parameter")?;

    info!("Typing into element: {}", selector);

    let tab = super::get_tab()?;
    let element = super::wait_for_element(&tab, selector)?;

    element.click()
        .map_err(|e| format!("Click failed: {:?}", e))?;

    element.type_into(text)
        .map_err(|e| format!("Type failed: {:?}", e))?;

    Ok(format!("Typed '{}' into element: {}", text, selector))
}
