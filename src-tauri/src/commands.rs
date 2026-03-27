use std::path::Path;

use serde::Serialize;
use tauri::State;

use tether_core::sync::engine::SyncStatus;
use tether_core::sync::reference;
use tether_core::sync::task::{QueueJobView, SyncOperation, SyncPriority, SyncTask};
use tether_core::sync::urls;

use crate::state::AppState;

// ── Response types ──

#[derive(Serialize)]
pub struct SyncStatusResponse {
    pub status: SyncStatus,
    pub queued_count: usize,
    /// In-memory worker queue (upload/download jobs not yet finished).
    pub queue_jobs: Vec<QueueJobView>,
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

#[derive(Serialize)]
pub struct SyncSessionInfo {
    pub hub_id: Option<String>,
    pub project_id: Option<String>,
    pub folder_id: Option<String>,
    pub sync_root_path: Option<String>,
    pub sync_root_db_id: Option<String>,
}

#[derive(Serialize)]
pub struct TroubleshooterReport {
    pub activity_lines: Vec<String>,
    pub pending_job_summaries: Vec<String>,
}


// ── Commands ──

#[tauri::command]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatusResponse, String> {
    let engine = state.engine.lock().await;
    let queued = engine.queue.len().await;
    let queue_jobs = engine.queue.snapshot_queue_views().await;
    Ok(SyncStatusResponse {
        status: engine.current_status(),
        queued_count: queued,
        queue_jobs,
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

// ── Desktop Connector parity helpers ──

#[tauri::command]
pub async fn get_sync_session(state: State<'_, AppState>) -> Result<SyncSessionInfo, String> {
    let engine = state.engine.lock().await;
    Ok(SyncSessionInfo {
        hub_id: engine.current_hub_id.clone(),
        project_id: engine.current_project_id.clone(),
        folder_id: engine.current_root_folder_id.clone(),
        sync_root_path: engine
            .sync_root_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
        sync_root_db_id: engine.sync_root_db_id.clone(),
    })
}

#[tauri::command]
pub async fn get_view_online_url(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<String, String> {
    let engine = state.engine.lock().await;
    let pid = engine
        .current_project_id
        .as_ref()
        .ok_or_else(|| "No active project".to_string())?;
    let fid = engine
        .current_root_folder_id
        .as_ref()
        .ok_or_else(|| "No active folder".to_string())?;
    Ok(urls::acc_view_item_url(pid, fid, &item_id))
}

#[tauri::command]
pub async fn get_copy_link(
    state: State<'_, AppState>,
    item_id: String,
) -> Result<String, String> {
    get_view_online_url(state, item_id).await
}

#[tauri::command]
pub async fn sync_now(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<String, String> {
    let (root, sr_id, queue, jobs) = {
        let engine = state.engine.lock().await;
        let root = engine.sync_root_path.clone().ok_or_else(|| "No sync root".to_string())?;
        let sr_id = engine
            .sync_root_db_id
            .clone()
            .ok_or_else(|| "No sync root id".to_string())?;
        let queue = engine.queue.clone();
        let host_full = root.join(&relative_path);
        let data = tokio::fs::read(&host_full).await.map_err(|e| e.to_string())?;
        let refs = reference::parse_inventor_references(&data);
        let rel_path = Path::new(&relative_path);
        let mut paths = vec![host_full];
        paths.extend(reference::prefetch_closure_paths(rel_path, &root, &refs));
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        let mut jobs = Vec::new();
        for p in paths {
            let rel = p
                .strip_prefix(&root)
                .map(|x| x.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if let Ok(Some(entry)) = db.get_file_entry_by_path(&sr_id, &rel) {
                if let Some(cid) = entry.cloud_item_id {
                    jobs.push((p, cid));
                }
            }
        }
        (root, sr_id, queue, jobs)
    };

    let mut n = 0usize;
    for (local_path, cid) in jobs {
        let mut task = SyncTask::new(
            SyncOperation::Download,
            SyncPriority::Critical,
            local_path,
        );
        task.cloud_item_id = Some(cid);
        task.sync_root_id = Some(sr_id.clone());
        task.sync_root_path = Some(root.clone());
        queue.push(task).await;
        n += 1;
    }
    Ok(format!("Queued {n} download task(s) for reference closure"))
}

#[tauri::command]
pub async fn set_always_keep_on_device(
    state: State<'_, AppState>,
    relative_path: String,
    pinned: bool,
) -> Result<(), String> {
    let engine = state.engine.lock().await;
    let sr = engine
        .sync_root_db_id
        .as_ref()
        .ok_or_else(|| "No active sync".to_string())?;
    let rel = relative_path.replace('\\', "/");
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.set_pin_state(sr, &rel, pinned)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn free_up_space(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<(), String> {
    let engine = state.engine.lock().await;
    let root = engine
        .sync_root_path
        .as_ref()
        .ok_or_else(|| "No active sync".to_string())?;
    let sr = engine
        .sync_root_db_id
        .as_ref()
        .ok_or_else(|| "No active sync".to_string())?;
    let rel = relative_path.replace('\\', "/");
    {
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        if let Ok(Some(entry)) = db.get_file_entry_by_path(sr, &rel) {
            if entry.pin_state != 0 {
                return Err("Item is pinned (Always keep on this device); cannot free space.".into());
            }
        }
    }
    let full = root.join(&relative_path);
    tether_cfapi::dehydrate_placeholder_file(&full).map_err(|e| e.to_string())?;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.update_hydration_state(sr, &rel, "online_only", true, None)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_inventor_ipj(
    state: State<'_, AppState>,
    ipj_path: Option<String>,
) -> Result<(), String> {
    let engine = state.engine.lock().await;
    let sr = engine
        .sync_root_db_id
        .as_ref()
        .ok_or_else(|| "No active sync".to_string())?;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.set_inventor_ipj(sr, ipj_path.as_deref())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_inventor_ipj(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let engine = state.engine.lock().await;
    let sr = engine
        .sync_root_db_id
        .as_ref()
        .ok_or_else(|| "No active sync".to_string())?;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.get_inventor_ipj(sr).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn collect_diagnostics_bundle(
    state: State<'_, AppState>,
) -> Result<String, String> {
    let _ = state;
    let out = std::env::temp_dir().join(format!(
        "tether-diagnostics-{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    tether_core::sync::diagnostics::collect_diagnostics_bundle(&out).map_err(|e| e.to_string())?;
    Ok(out.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn get_troubleshooter_report(
    state: State<'_, AppState>,
) -> Result<TroubleshooterReport, String> {
    let engine = state.engine.lock().await;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    let activity = db
        .get_recent_activity(40)
        .map_err(|e| e.to_string())?;
    let activity_lines: Vec<String> = activity
        .iter()
        .map(|a| {
            format!(
                "{}: {} [{}] {} — {}",
                a.timestamp,
                a.operation,
                a.status,
                a.file_path.as_deref().unwrap_or("-"),
                a.details.as_deref().unwrap_or("")
            )
        })
        .collect();
    let pending = db
        .list_pending_jobs("queued", 20)
        .map_err(|e| e.to_string())?;
    let pending_job_summaries: Vec<String> = pending
        .iter()
        .map(|j| format!("{}: {} ({})", j.id, j.job_type, j.status))
        .collect();
    Ok(TroubleshooterReport {
        activity_lines,
        pending_job_summaries,
    })
}

