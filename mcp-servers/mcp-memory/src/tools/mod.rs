//! Memory storage tools

pub mod store;
pub mod retrieve;
pub mod search;
pub mod delete;
pub mod list;

use std::sync::Mutex;
use once_cell::sync::Lazy;


/// Shared database instance
static DB: Lazy<Mutex<Option<sled::Db>>> = Lazy::new(|| Mutex::new(None));

/// Get or initialize the database
fn get_db() -> Result<sled::Db, String> {
    let mut db = DB.lock().map_err(|e| format!("Lock error: {}", e))?;
    
    if db.is_none() {
        let data_dir = dirs::data_dir()
            .map(|d| d.join("mcp-memory"))
            .unwrap_or_else(|| std::path::PathBuf::from("./mcp-memory-data"));
        
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| format!("Failed to create data dir: {}", e))?;
        
        let new_db = sled::open(&data_dir)
            .map_err(|e| format!("Failed to open database: {}", e))?;
        
        *db = Some(new_db);
    }
    
    db.as_ref()
        .cloned()
        .ok_or_else(|| "Failed to get database".to_string())
}

/// Make a namespaced key
fn namespaced_key(namespace: &str, key: &str) -> String {
    format!("{}:{}", namespace, key)
}
