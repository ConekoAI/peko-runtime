//! Screenshot tool

use serde_json::Value;
use tracing::info;

/// Take a screenshot
pub async fn execute(args: Value) -> Result<String, String> {
    let selector = args
        .get("selector")
        .and_then(|v| v.as_str());

    info!("Taking screenshot");

    let tab = super::get_tab()?;

    let png_data = if let Some(sel) = selector {
        // Screenshot specific element
        let element = super::wait_for_element(&tab, sel)?;
        element.capture_screenshot(headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png)
            .map_err(|e| format!("Screenshot failed: {:?}", e))?
    } else {
        // Screenshot full page
        tab.capture_screenshot(headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| format!("Screenshot failed: {:?}", e))?
    };

    // Encode to base64
    use base64::Engine;
    let base64_png = base64::engine::general_purpose::STANDARD.encode(&png_data);
    
    Ok(format!(
        "Screenshot captured ({} bytes)\ndata:image/png;base64,{}...",
        png_data.len(),
        &base64_png[..100.min(base64_png.len())]
    ))
}
