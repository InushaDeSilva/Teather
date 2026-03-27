use anyhow::{Context, Result};
use std::collections::HashMap;
use tracing::info;

fn token_file_path() -> std::path::PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(local_app_data)
        .join("Tether")
        .join("tokens.json")
}

fn load_tokens() -> HashMap<String, String> {
    let path = token_file_path();
    if let Ok(data) = std::fs::read_to_string(&path) {
        if let Ok(map) = serde_json::from_str(&data) {
            return map;
        }
    }
    HashMap::new()
}

fn save_tokens(map: &HashMap<String, String>) -> Result<()> {
    let path = token_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(map)?;
    std::fs::write(&path, data).context("Failed to write tokens file")?;
    Ok(())
}

/// Store a credential to the local token file.
pub fn store_credential(key: &str, value: &str) -> Result<()> {
    let mut map = load_tokens();
    map.insert(key.to_string(), value.to_string());
    save_tokens(&map)?;
    info!("Stored credential: {key}");
    Ok(())
}

/// Retrieve a credential from the local token file.
pub fn get_credential(key: &str) -> Result<String> {
    let map = load_tokens();
    map.get(key).cloned().context("Credential not found or inaccessible")
}

/// Delete a credential from the local token file.
pub fn delete_credential(key: &str) -> Result<()> {
    let mut map = load_tokens();
    if map.remove(key).is_some() {
        save_tokens(&map)?;
        info!("Deleted credential: {key}");
    }
    Ok(())
}

