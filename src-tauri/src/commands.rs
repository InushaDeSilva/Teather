use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use tether_core::sync::conflict::{self, ConflictStrategy};
use tether_core::sync::engine::SyncStatus;
use tether_core::sync::parity::{
    prompt_payload_json, recovery_path_for, PromptKind, PromptPayload, PromptResolution,
    ServiceState,
};
use tether_core::sync::reference;
use tether_core::sync::task::{QueueJobView, SyncOperation, SyncPriority, SyncTask};
use tether_core::sync::urls;
use tether_core::sync::worker::{parse_storage_urn, persist_after_download};

use crate::state::AppState;

#[derive(Serialize)]
pub struct SyncStatusResponse {
    pub status: SyncStatus,
    pub service_state: ServiceState,
    pub queued_count: usize,
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
    pub service_state: ServiceState,
}

#[derive(Serialize)]
pub struct TroubleshooterReport {
    pub activity_lines: Vec<String>,
    pub pending_job_summaries: Vec<String>,
}

#[derive(Serialize)]
pub struct PendingPromptView {
    pub id: String,
    pub kind: PromptKind,
    pub relative_path: String,
    pub message: String,
    pub is_directory: bool,
}

#[derive(Serialize)]
pub struct OperationJournalView {
    pub id: String,
    pub operation_type: String,
    pub relative_path: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[tauri::command]
pub async fn get_sync_status(state: State<'_, AppState>) -> Result<SyncStatusResponse, String> {
    let engine = state.engine.lock().await;
    let queued = engine.queue.len().await;
    let queue_jobs = engine.queue.snapshot_queue_views().await;
    Ok(SyncStatusResponse {
        status: engine.current_status(),
        service_state: engine.current_service_state(),
        queued_count: queued,
        queue_jobs,
    })
}

#[tauri::command]
pub async fn check_auth_status(state: State<'_, AppState>) -> Result<bool, String> {
    let engine = state.engine.lock().await;
    match engine.auth.get_access_token() {
        Ok(_) => match engine.auth.refresh_token().await {
            Ok(_) => Ok(true),
            Err(e) => {
                println!("Token refresh failed (will require re-login): {e:#}");
                Ok(false)
            }
        },
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

    opener::open(&url).map_err(|e| format!("Failed to open browser: {}", e))?;
    let code = auth
        .listen_for_callback(&csrf)
        .await
        .map_err(|e| format!("Login failed: {}", e))?;
    let _token = auth
        .exchange_code(&code, &verifier)
        .await
        .map_err(|e| format!("Token exchange failed: {}", e))?;

    Ok("success".to_string())
}

#[tauri::command]
pub async fn get_hubs(state: State<'_, AppState>) -> Result<Vec<HubInfo>, String> {
    let engine = state.engine.lock().await;
    let mut token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let hubs = match engine.data_mgmt.get_hubs(&token).await {
        Ok(h) => h,
        Err(e) => {
            println!("get_hubs failed ({}), attempting token refresh...", e);
            let refreshed = engine
                .auth
                .refresh_token()
                .await
                .map_err(|e| format!("Token refresh failed: {e:#}"))?;
            token = refreshed.access_token;
            engine
                .data_mgmt
                .get_hubs(&token)
                .await
                .map_err(|e| format!("{e:#}"))?
        }
    };
    Ok(hubs
        .into_iter()
        .map(|h| HubInfo {
            id: h.id,
            name: h.attributes.name,
            extension_type: h.attributes.extension.map(|e| e.type_code).unwrap_or_default(),
        })
        .collect())
}

#[tauri::command]
pub async fn get_projects(
    state: State<'_, AppState>,
    hub_id: String,
) -> Result<Vec<ProjectInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let projects = engine
        .data_mgmt
        .get_projects(&token, &hub_id)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(projects
        .into_iter()
        .map(|p| ProjectInfo {
            id: p.id,
            name: p.attributes.name,
        })
        .collect())
}

#[tauri::command]
pub async fn get_folders(
    state: State<'_, AppState>,
    hub_id: String,
    project_id: String,
) -> Result<Vec<FolderInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let folders = engine
        .data_mgmt
        .get_top_folders(&token, &hub_id, &project_id)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(folders
        .into_iter()
        .map(|f| FolderInfo {
            id: f.id,
            name: f.attributes.display_name,
        })
        .collect())
}

#[tauri::command]
pub async fn get_drive_view(state: State<'_, AppState>) -> Result<Vec<DriveItemInfo>, String> {
    let engine = state.engine.lock().await;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let items = engine
        .data_mgmt
        .get_drive_view(&token)
        .await
        .map_err(|e| format!("{e:#}"))?;

    Ok(items
        .into_iter()
        .map(|i| DriveItemInfo {
            name: i.name,
            hub_id: i.hub_id,
            project_id: i.project_id,
            folder_id: i.folder_id,
            depth: i.depth,
        })
        .collect())
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
    state.engine.lock().await.resume().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_service_state(
    state: State<'_, AppState>,
    service_state: String,
) -> Result<(), String> {
    let mut engine = state.engine.lock().await;
    let next = match service_state.as_str() {
        "running" => ServiceState::Running,
        "offline" => ServiceState::Offline,
        "disabled" => ServiceState::Disabled,
        "reconnecting" => ServiceState::Reconnecting,
        "error" => ServiceState::Error,
        _ => return Err(format!("Unknown service state: {service_state}")),
    };
    engine.set_service_state(next).await.map_err(|e| e.to_string())
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
    folder_id: Option<String>,
) -> Result<(), String> {
    let mut engine = state.engine.lock().await;
    engine
        .start(&hub_id, &project_id, &project_name, folder_id)
        .await
        .map_err(|e| format!("{e:#}"))?;
    Ok(())
}

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
        service_state: engine.current_service_state(),
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
pub async fn get_copy_link(state: State<'_, AppState>, item_id: String) -> Result<String, String> {
    get_view_online_url(state, item_id).await
}

#[tauri::command]
pub async fn sync_now(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<String, String> {
    let (root, sr_id, queue, jobs) = {
        let engine = state.engine.lock().await;
        let root = engine
            .sync_root_path
            .clone()
            .ok_or_else(|| "No sync root".to_string())?;
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
pub async fn get_latest_version(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<String, String> {
    let (root, sr_id, queue, cloud_item_id) =
        lookup_entry_for_relative_path(&state, &relative_path).await?;
    let mut task = SyncTask::new(
        SyncOperation::GetLatestVersion,
        SyncPriority::High,
        root.join(relative_path.replace('/', "\\")),
    );
    task.cloud_item_id = Some(cloud_item_id);
    task.sync_root_id = Some(sr_id);
    task.sync_root_path = Some(root);
    queue.push(task).await;
    Ok("Queued Get Latest Version".into())
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
pub async fn delete_local_item(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<String, String> {
    let (root, sync_root_id, rel, is_cloud_backed) = {
        let engine = state.engine.lock().await;
        let root = engine
            .sync_root_path
            .clone()
            .ok_or_else(|| "No active sync".to_string())?;
        let sync_root_id = engine
            .sync_root_db_id
            .clone()
            .ok_or_else(|| "No active sync".to_string())?;
        let rel = relative_path.replace('\\', "/");
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        let entry = db
            .get_file_entry_by_path(&sync_root_id, &rel)
            .map_err(|e| e.to_string())?;
        let is_cloud_backed = entry.as_ref().and_then(|e| e.cloud_item_id.as_ref()).is_some();
        (root, sync_root_id, rel, is_cloud_backed)
    };

    let full = root.join(relative_path.replace('/', "\\"));
    if is_cloud_backed {
        if full.exists() {
            tether_cfapi::dehydrate_placeholder_file(&full).map_err(|e| e.to_string())?;
        }
        let engine = state.engine.lock().await;
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        db.update_hydration_state(&sync_root_id, &rel, "online_only", true, Some("delete_local"))
            .map_err(|e| e.to_string())?;
        return Ok("Local cache removed; cloud item kept.".into());
    }

    let recovery = recovery_path_for(&full);
    if let Some(parent) = recovery.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if full.exists() {
        std::fs::rename(&full, &recovery).map_err(|e| e.to_string())?;
    }
    let engine = state.engine.lock().await;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.remove_file_entry(&sync_root_id, &rel)
        .map_err(|e| e.to_string())?;
    Ok(format!("Moved local-only file to recovery: {}", recovery.display()))
}

#[tauri::command]
pub async fn request_delete_prompt(
    state: State<'_, AppState>,
    relative_path: String,
) -> Result<String, String> {
    let (sync_root_id, payload_json) = {
        let engine = state.engine.lock().await;
        let sync_root_id = engine
            .sync_root_db_id
            .clone()
            .ok_or_else(|| "No active sync".to_string())?;
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        let rel = relative_path.replace('\\', "/");
        let entry = db
            .get_file_entry_by_path(&sync_root_id, &rel)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Item not found".to_string())?;
        let payload = PromptPayload {
            kind: PromptKind::DeleteConfirm,
            relative_path: rel.clone(),
            cloud_item_id: entry.cloud_item_id.clone(),
            remote_head_version_id: None,
            message: format!(
                "Delete requested for {rel}. Choose whether to remove only the local copy or both local and cloud."
            ),
            is_directory: entry.is_directory,
        };
        (sync_root_id, prompt_payload_json(&payload).map_err(|e| e.to_string())?)
    };

    let engine = state.engine.lock().await;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    let id = db
        .insert_pending_job(&sync_root_id, "prompt", Some(&payload_json), None)
        .map_err(|e| e.to_string())?;
    db.update_pending_job(&id, "action_required", None, None)
        .map_err(|e| e.to_string())?;
    Ok(id)
}

#[tauri::command]
pub async fn list_pending_prompts(
    state: State<'_, AppState>,
) -> Result<Vec<PendingPromptView>, String> {
    let engine = state.engine.lock().await;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    let rows = db
        .list_pending_jobs("action_required", 20)
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        if row.job_type != "prompt" {
            continue;
        }
        let Some(payload_json) = row.payload_json.as_deref() else {
            continue;
        };
        let payload: PromptPayload = serde_json::from_str(payload_json).map_err(|e| e.to_string())?;
        out.push(PendingPromptView {
            id: row.id,
            kind: payload.kind,
            relative_path: payload.relative_path,
            message: payload.message,
            is_directory: payload.is_directory,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn list_operation_journal(
    state: State<'_, AppState>,
) -> Result<Vec<OperationJournalView>, String> {
    let engine = state.engine.lock().await;
    let sync_root_id = engine
        .sync_root_db_id
        .clone()
        .ok_or_else(|| "No active sync".to_string())?;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    let mut rows = Vec::new();
    rows.extend(db.list_operation_journal(&sync_root_id, "queued", 20).map_err(|e| e.to_string())?);
    rows.extend(
        db.list_operation_journal(&sync_root_id, "applying", 20)
            .map_err(|e| e.to_string())?,
    );
    Ok(rows
        .into_iter()
        .map(|row| OperationJournalView {
            id: row.id,
            operation_type: row.operation_type,
            relative_path: row.relative_path,
            status: row.status,
            created_at: row.created_at,
        })
        .collect())
}

#[tauri::command]
pub async fn resolve_pending_prompt(
    state: State<'_, AppState>,
    prompt_id: String,
    resolution: String,
) -> Result<String, String> {
    let resolution = match resolution.as_str() {
        "keep_both" => PromptResolution::KeepBoth,
        "keep_local" => PromptResolution::KeepLocal,
        "keep_cloud" => PromptResolution::KeepCloud,
        "delete_local_only" => PromptResolution::DeleteLocalOnly,
        "delete_local_and_cloud" => PromptResolution::DeleteLocalAndCloud,
        "cancel" => PromptResolution::Cancel,
        _ => return Err(format!("Unknown resolution: {resolution}")),
    };

    let (payload, sync_root_id, root_path) = {
        let engine = state.engine.lock().await;
        let sync_root_id = engine
            .sync_root_db_id
            .clone()
            .ok_or_else(|| "No active sync".to_string())?;
        let root_path = engine
            .sync_root_path
            .clone()
            .ok_or_else(|| "No active sync".to_string())?;
        let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
        let row = db
            .get_pending_job(&prompt_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "Prompt not found".to_string())?;
        let payload_json = row.payload_json.ok_or_else(|| "Prompt payload missing".to_string())?;
        let payload: PromptPayload =
            serde_json::from_str(&payload_json).map_err(|e| e.to_string())?;
        (payload, sync_root_id, root_path)
    };

    let outcome = match payload.kind {
        PromptKind::DeleteConfirm => {
            resolve_delete_prompt(&state, &sync_root_id, &root_path, &payload, resolution).await?
        }
        PromptKind::ConflictUpload | PromptKind::GetLatestConflict => {
            resolve_conflict_prompt(&state, &sync_root_id, &root_path, &payload, resolution).await?
        }
    };

    let engine = state.engine.lock().await;
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    db.update_pending_job(&prompt_id, "resolved", Some(&outcome), None)
        .map_err(|e| e.to_string())?;
    Ok(outcome)
}

#[tauri::command]
pub async fn shell_dispatch(
    state: State<'_, AppState>,
    verb: String,
    relative_path: String,
) -> Result<String, String> {
    match verb.as_str() {
        "sync_now" => sync_now(state, relative_path).await,
        "get_latest_version" => get_latest_version(state, relative_path).await,
        "delete_local" => delete_local_item(state, relative_path).await,
        "delete_prompt" => request_delete_prompt(state, relative_path).await,
        "free_up_space" => {
            free_up_space(state, relative_path).await?;
            Ok("Freed local space".into())
        }
        _ => Err(format!("Unknown shell verb: {verb}")),
    }
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
    let activity = db.get_recent_activity(40).map_err(|e| e.to_string())?;
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
    let mut pending = db
        .list_pending_jobs("queued", 20)
        .map_err(|e| e.to_string())?;
    pending.extend(
        db.list_pending_jobs("action_required", 20)
            .map_err(|e| e.to_string())?,
    );
    let pending_job_summaries: Vec<String> = pending
        .iter()
        .map(|j| format!("{}: {} ({})", j.id, j.job_type, j.status))
        .collect();
    Ok(TroubleshooterReport {
        activity_lines,
        pending_job_summaries,
    })
}

async fn lookup_entry_for_relative_path(
    state: &State<'_, AppState>,
    relative_path: &str,
) -> Result<(PathBuf, String, std::sync::Arc<tether_core::sync::queue::SyncQueue>, String), String>
{
    let engine = state.engine.lock().await;
    let root = engine
        .sync_root_path
        .clone()
        .ok_or_else(|| "No sync root".to_string())?;
    let sr_id = engine
        .sync_root_db_id
        .clone()
        .ok_or_else(|| "No sync root id".to_string())?;
    let queue = engine.queue.clone();
    let db = engine.db.lock().map_err(|e| format!("{e:?}"))?;
    let rel = relative_path.replace('\\', "/");
    let entry = db
        .get_file_entry_by_path(&sr_id, &rel)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("No file entry found for {rel}"))?;
    let cloud_item_id = entry
        .cloud_item_id
        .ok_or_else(|| format!("No cloud item id for {rel}"))?;
    Ok((root, sr_id, queue, cloud_item_id))
}

async fn resolve_delete_prompt(
    state: &State<'_, AppState>,
    sync_root_id: &str,
    root_path: &Path,
    payload: &PromptPayload,
    resolution: PromptResolution,
) -> Result<String, String> {
    match resolution {
        PromptResolution::DeleteLocalOnly => {
            delete_local_item(state.clone(), payload.relative_path.clone()).await
        }
        PromptResolution::DeleteLocalAndCloud => {
            let engine = state.engine.lock().await;
            let queue = engine.queue.clone();
            let mut task = SyncTask::new(
                SyncOperation::DeleteCloud,
                SyncPriority::Critical,
                root_path.join(payload.relative_path.replace('/', "\\")),
            );
            task.cloud_item_id = payload.cloud_item_id.clone();
            task.sync_root_id = Some(sync_root_id.to_string());
            task.sync_root_path = Some(root_path.to_path_buf());
            queue.push(task).await;
            drop(engine);
            let _ = delete_local_item(state.clone(), payload.relative_path.clone()).await;
            Ok("Queued local+cloud delete".into())
        }
        PromptResolution::Cancel => Ok("Delete cancelled".into()),
        _ => Err("Unsupported delete resolution".into()),
    }
}

async fn resolve_conflict_prompt(
    state: &State<'_, AppState>,
    sync_root_id: &str,
    root_path: &Path,
    payload: &PromptPayload,
    resolution: PromptResolution,
) -> Result<String, String> {
    let local_path = root_path.join(payload.relative_path.replace('/', "\\"));
    let engine = state.engine.lock().await;
    let project_id = engine
        .current_project_id
        .clone()
        .ok_or_else(|| "No active project".to_string())?;
    let item_id = payload
        .cloud_item_id
        .clone()
        .ok_or_else(|| "Prompt missing cloud item id".to_string())?;
    let token = engine.auth.get_access_token().map_err(|e| format!("{e:#}"))?;
    let versions = engine
        .data_mgmt
        .get_item_versions(&token, &project_id, &item_id)
        .await
        .map_err(|e| e.to_string())?;
    let active = versions.first().ok_or_else(|| "No remote version found".to_string())?;
    let storage_urn = active
        .relationships
        .as_ref()
        .and_then(|r| r.storage.as_ref())
        .and_then(|s| s.data.as_ref())
        .map(|d| d.id.clone())
        .ok_or_else(|| "Remote storage URN missing".to_string())?;
    let (bucket_key, object_key) = parse_storage_urn(&storage_urn).map_err(|e| e.to_string())?;
    let bytes = engine
        .storage
        .download_to_bytes(&token, &bucket_key, &object_key)
        .await
        .map_err(|e| e.to_string())?;
    let queue = engine.queue.clone();
    let db = engine.db.clone();
    drop(engine);

    match resolution {
        PromptResolution::KeepBoth => {
            conflict::resolve_conflict(&local_path, &bytes, ConflictStrategy::KeepBoth)
                .await
                .map_err(|e| e.to_string())?;
            if matches!(payload.kind, PromptKind::ConflictUpload) {
                let mut task = SyncTask::new(SyncOperation::Upload, SyncPriority::High, local_path);
                task.cloud_item_id = Some(item_id);
                task.sync_root_id = Some(sync_root_id.to_string());
                task.sync_root_path = Some(root_path.to_path_buf());
                queue.push(task).await;
            }
            Ok("Kept both versions".into())
        }
        PromptResolution::KeepLocal => {
            if matches!(payload.kind, PromptKind::ConflictUpload) {
                let mut task = SyncTask::new(SyncOperation::Upload, SyncPriority::High, local_path);
                task.cloud_item_id = Some(item_id);
                task.sync_root_id = Some(sync_root_id.to_string());
                task.sync_root_path = Some(root_path.to_path_buf());
                queue.push(task).await;
            }
            Ok("Kept local version".into())
        }
        PromptResolution::KeepCloud => {
            tokio::fs::write(&local_path, &bytes)
                .await
                .map_err(|e| e.to_string())?;
            persist_after_download(
                &db,
                sync_root_id,
                &payload.relative_path,
                &item_id,
                &active.id,
                active.attributes.last_modified_time.as_deref(),
                &storage_urn,
                &local_path,
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok("Applied cloud version".into())
        }
        PromptResolution::Cancel => Ok("Conflict resolution cancelled".into()),
        _ => Err("Unsupported conflict resolution".into()),
    }
}
