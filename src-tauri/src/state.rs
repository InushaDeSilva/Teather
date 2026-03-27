use std::sync::Arc;
use tokio::sync::Mutex;
use tether_core::sync::engine::SyncEngine;

/// Shared app state managed by Tauri.
pub struct AppState {
    pub engine: Arc<Mutex<SyncEngine>>,
}
