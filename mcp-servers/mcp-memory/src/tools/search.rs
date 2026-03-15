//! Search tool

use serde_json::Value;
use tracing::info;

/// Search memories by prefix
pub async fn execute(args: Value) -> Result<String, String> {
    let prefix = args
        .get("prefix")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'prefix' parameter")?;

    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    info!("Searching memories in '{}' with prefix: {}", namespace, prefix);

    let db = super::get_db()?;
    let ns_prefix = format!("{}:{}", namespace, prefix);

    let mut results = Vec::new();

    for item in db.scan_prefix(ns_prefix.as_bytes()) {
        let (key, value) = item.map_err(|e| format!("Scan error: {}", e))?;
        
        let key_str = String::from_utf8_lossy(&key);
        let value_str = String::from_utf8_lossy(&value);
        
        // Extract just the key part (without namespace)
        let short_key = key_str.split(':').nth(1).unwrap_or(&key_str);
        
        if let Ok(json) = serde_json::from_str::<Value>(&value_str) {
            let val = json.get("value").and_then(|v| v.as_str()).unwrap_or("[invalid]");
            let preview: String = val.chars().take(100).collect();
            results.push(format!("- {}: {}", short_key, preview));
        }
    }

    if results.is_empty() {
        Ok(format!("No memories found in '{}' with prefix '{}'", namespace, prefix))
    } else {
        Ok(format!(
            "Found {} memory/memories in '{}':\n{}",
            results.len(),
            namespace,
            results.join("\n")
        ))
    }
}
