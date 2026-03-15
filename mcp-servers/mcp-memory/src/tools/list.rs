//! List tool

use serde_json::Value;
use tracing::info;

/// List all memories in a namespace
pub async fn execute(args: Value) -> Result<String, String> {
    let namespace = args
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .map(|l| l.max(1).min(1000) as usize)
        .unwrap_or(100);

    info!("Listing memories in '{}' (limit: {})", namespace, limit);

    let db = super::get_db()?;
    let ns_prefix = format!("{}:", namespace);

    let mut results = Vec::new();

    for item in db.scan_prefix(ns_prefix.as_bytes()) {
        if results.len() >= limit {
            break;
        }

        let (key, value) = item.map_err(|e| format!("Scan error: {}", e))?;
        
        let key_str = String::from_utf8_lossy(&key);
        let value_str = String::from_utf8_lossy(&value);
        
        // Extract just the key part (without namespace)
        let short_key = key_str.split(':').nth(1).unwrap_or(&key_str);
        
        if let Ok(json) = serde_json::from_str::<Value>(&value_str) {
            let val = json.get("value").and_then(|v| v.as_str()).unwrap_or("[invalid]");
            let preview: String = val.chars().take(80).collect();
            results.push(format!("- {}: {}", short_key, preview));
        }
    }

    if results.is_empty() {
        Ok(format!("No memories in namespace '{}'", namespace))
    } else {
        Ok(format!(
            "Memories in '{}' (showing {}):\n{}",
            namespace,
            results.len(),
            results.join("\n")
        ))
    }
}
