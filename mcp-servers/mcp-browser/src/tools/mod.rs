//! Browser automation tools

pub mod navigate;
pub mod click;
pub mod type_text;
pub mod screenshot;
pub mod get_content;

use std::sync::Mutex;
use once_cell::sync::Lazy;

/// Shared browser instance
static BROWSER: Lazy<Mutex<Option<headless_chrome::Browser>>> = Lazy::new(|| Mutex::new(None));
static TAB: Lazy<Mutex<Option<std::sync::Arc<headless_chrome::Tab>>>> = Lazy::new(|| Mutex::new(None));

/// Initialize browser if not already initialized
fn init_browser() -> Result<(), String> {
    let mut browser = BROWSER.lock().map_err(|e| format!("Lock error: {}", e))?;
    
    if browser.is_none() {
        let options = headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .build()
            .map_err(|e| format!("Failed to build options: {:?}", e))?;
        
        let new_browser = headless_chrome::Browser::new(options)
            .map_err(|e| format!("Failed to launch browser: {:?}", e))?;
        
        *browser = Some(new_browser);
    }
    
    Ok(())
}

/// Get or create a tab
fn get_tab() -> Result<std::sync::Arc<headless_chrome::Tab>, String> {
    init_browser()?;
    
    let mut tab = TAB.lock().map_err(|e| format!("Lock error: {}", e))?;
    
    if tab.is_none() {
        let browser = BROWSER.lock().map_err(|e| format!("Lock error: {}", e))?;
        let new_tab = browser.as_ref()
            .ok_or("Browser not initialized")?
            .new_tab()
            .map_err(|e| format!("Failed to create tab: {:?}", e))?;
        *tab = Some(new_tab);
    }
    
    tab.as_ref()
        .cloned()
        .ok_or_else(|| "Failed to get tab".to_string())
}

/// Wait for element
fn wait_for_element<'a>(tab: &'a std::sync::Arc<headless_chrome::Tab>, selector: &'a str) -> Result<headless_chrome::Element<'a>, String> {
    tab.wait_for_element(selector)
        .map_err(|e| format!("Element '{}' not found: {:?}", selector, e))
}
