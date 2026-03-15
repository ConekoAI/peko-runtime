//! Get content tool

use serde_json::Value;
use tracing::info;

/// Get page content
pub async fn execute(args: Value) -> Result<String, String> {
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("text");

    info!("Getting page content in '{}' format", format);

    let tab = super::get_tab()?;

    match format {
        "html" => {
            let content = tab.get_content()
                .map_err(|e| format!("Failed to get content: {:?}", e))?;
            Ok(content)
        }
        _ => {
            // Get text content using JavaScript
            let text: String = tab.evaluate("document.body.innerText", false)
                .map_err(|e| format!("Failed to get text: {:?}", e))?
                .value
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();
            
            Ok(text)
        }
    }
}
