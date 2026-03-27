use serde::Serialize;
use tauri::State;

use tether_core::sync::engine::SyncStatus;
use crate::state::AppState;

// ── Response types ──

#[derive(Serialize)]
pub struct SyncStatusResponse {
    pub status: SyncStatus,
    pub queued_count: usize,
}

#[derive(Serialize)]
pub struct HubInfo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct FolderInfo {
    pub id: String,
    pub name: String,
}

// ── Commands ──

#[tauri::command]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatusResponse, String> {
    let engine = state.engine.lock().await;
    let queued = engine.queue.len().await;
    Ok(SyncStatusResponse {
        status: engine.current_status(),
        queued_count: queued,
    })
}

#[tauri::command]
pub async fn start_login(state: State<'_, AppState>) -> Result<String, String> {
    let engine = state.engine.lock().await;
    let (url, _csrf, _verifier) = engine.auth.build_auth_url();
    // Open the URL in the system browser
    opener::open(&url).map_err(|e| e.to_string())?;
    Ok(url)
}

#[tauri::command]
pub async fn get_hubs(state: State<'_, AppState>) -> Result<Vec<HubInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| e.to_string())?;
    let hubs = engine.data_mgmt.get_hubs(&token).await.map_err(|e| e.to_string())?;
    Ok(hubs.into_iter().map(|h| HubInfo {
        id: h.id,
        name: h.attributes.name,
    }).collect())
}

#[tauri::command]
pub async fn get_projects(state: State<'_, AppState>, hub_id: String) -> Result<Vec<ProjectInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| e.to_string())?;
    let projects = engine.data_mgmt.get_projects(&token, &hub_id).await.map_err(|e| e.to_string())?;
    Ok(projects.into_iter().map(|p| ProjectInfo {
        id: p.id,
        name: p.attributes.name,
    }).collect())
}

#[tauri::command]
pub async fn get_folders(state: State<'_, AppState>, project_id: String) -> Result<Vec<FolderInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| e.to_string())?;
    let folders = engine.data_mgmt.get_top_folders(&token, &project_id).await.map_err(|e| e.to_string())?;
    Ok(folders.into_iter().map(|f| FolderInfo {
        id: f.id,
        name: f.attributes.display_name,
    }).collect())
}

#[tauri::command]
pub async fn pause_sync(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.lock().await.pause();
    Ok(())
}

#[tauri::command]
pub async fn resume_sync(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.lock().await.resume();
    Ok(())
}

#[tauri::command]
pub async fn open_sync_folder(state: State<'_, AppState>) -> Result<(), String> {
    let engine = state.engine.lock().await;
    if let Some(ref path) = engine.sync_root_path() {
        opener::open(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
