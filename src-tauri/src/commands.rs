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
    pub extension_type: String,
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

#[derive(Serialize)]
pub struct DriveItemInfo {
    pub name: String,
    pub hub_id: String,
    pub project_id: String,
    pub folder_id: String,
    pub depth: u32,
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
pub async fn check_auth_status(state: State<'_, AppState>) -> Result<bool, String> {
    let engine = state.engine.lock().await;
    match engine.auth.get_access_token() {
        Ok(_) => {
            // Token exists — proactively refresh it so it's always fresh on startup
            match engine.auth.refresh_token().await {
                Ok(_) => Ok(true),
                Err(e) => {
                    println!("Token refresh failed (will require re-login): {e:#}");
                    Ok(false)
                }
            }
        }
        Err(_) => Ok(false),
    }
}

#[tauri::command]
pub async fn start_login(state: State<'_, AppState>) -> Result<String, String> {
    let (url, csrf, verifier, auth) = {
        let engine = state.engine.lock().await;
        let (u, c, v) = engine.auth.build_auth_url();
        (u, c, v, engine.auth.clone())
    };

    // Open the URL in the system browser
    opener::open(&url).map_err(|e| format!("Failed to open browser: {}", e))?;

    // Wait for the callback
    let code = auth.listen_for_callback(&csrf).await.map_err(|e| format!("Login failed: {}", e))?;
    
    // Exchange code for tokens
    let _token = auth.exchange_code(&code, &verifier).await.map_err(|e| format!("Token exchange failed: {}", e))?;

    Ok("success".to_string())
}

#[tauri::command]
pub async fn get_hubs(state: State<'_, AppState>) -> Result<Vec<HubInfo>, String> {
    let engine = state.engine.lock().await;
    let mut token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    
    // Try fetching hubs; if auth fails, refresh and retry once
    let hubs = match engine.data_mgmt.get_hubs(&token).await {
        Ok(h) => h,
        Err(e) => {
            println!("get_hubs failed ({}), attempting token refresh...", e);
            let refreshed = engine.auth.refresh_token().await.map_err(|e| format!("Token refresh failed: {e:#}"))?;
            token = refreshed.access_token;
            engine.data_mgmt.get_hubs(&token).await.map_err(|e| format!("{e:#}"))?
        }
    };
    
    for h in &hubs {
        println!("Loaded Hub: {} (ID: {})", h.attributes.name, h.id);
        println!("   -> Extension: {:?}", h.attributes.extension);
    }
    
    Ok(hubs.into_iter().map(|h| HubInfo {
        id: h.id,
        name: h.attributes.name,
        extension_type: h.attributes.extension.map(|e| e.type_code).unwrap_or_default(),
    }).collect())
}

#[tauri::command]
pub async fn get_projects(state: State<'_, AppState>, hub_id: String) -> Result<Vec<ProjectInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let projects = engine.data_mgmt.get_projects(&token, &hub_id).await.map_err(|e| format!("{e:#}"))?;
    Ok(projects.into_iter().map(|p| ProjectInfo {
        id: p.id,
        name: p.attributes.name,
    }).collect())
}

#[tauri::command]
pub async fn get_folders(state: State<'_, AppState>, hub_id: String, project_id: String) -> Result<Vec<FolderInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let folders = engine.data_mgmt.get_top_folders(&token, &hub_id, &project_id).await
        .map_err(|e| format!("{e:#}"))?;
    Ok(folders.into_iter().map(|f| FolderInfo {
        id: f.id,
        name: f.attributes.display_name,
    }).collect())
}

#[tauri::command]
pub async fn get_drive_view(state: State<'_, AppState>) -> Result<Vec<DriveItemInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let items = engine.data_mgmt.get_drive_view(&token).await.map_err(|e| format!("{e:#}"))?;
    
    Ok(items.into_iter().map(|i| DriveItemInfo {
        name: i.name,
        hub_id: i.hub_id,
        project_id: i.project_id,
        folder_id: i.folder_id,
        depth: i.depth,
    }).collect())
}


#[tauri::command]
pub async fn resolve_drive_folder(
    state: State<'_, AppState>,
    folder_urn: String,
) -> Result<DriveItemInfo, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let item = engine
        .data_mgmt
        .resolve_folder_urn(&token, &folder_urn)
        .await
        .map_err(|e| format!("{e:#}"))?;

    Ok(DriveItemInfo {
        name: item.name,
        hub_id: item.hub_id,
        project_id: item.project_id,
        folder_id: item.folder_id,
        depth: 0,
    })
}

#[tauri::command]
pub async fn get_subfolders(
    state: State<'_, AppState>,
    project_id: String,
    folder_id: String,
) -> Result<Vec<FolderInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let items = engine
        .data_mgmt
        .get_folder_contents(&token, &project_id, &folder_id)
        .await
        .map_err(|e| format!("{e:#}"))?;

    Ok(items
        .into_iter()
        .filter(|i| i.item_type == "folders")
        .map(|i| FolderInfo {
            id: i.id,
            name: i.attributes.display_name,
        })
        .collect())
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
        opener::open(path).map_err(|e| format!("{e:#}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn start_sync(
    state: State<'_, AppState>, 
    hub_id: String, 
    project_id: String, 
    project_name: String,
    folder_id: Option<String>
) -> Result<(), String> {
    let mut engine = state.engine.lock().await;
    engine.start(&hub_id, &project_id, &project_name, folder_id).await.map_err(|e| format!("{e:#}"))?;
    Ok(())
}

