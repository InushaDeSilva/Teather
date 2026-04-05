use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::api::auth::ApsAuthClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::api::storage::ApsStorageClient;
use crate::db::database::{FileEntryRow, SyncDatabase};
use crate::sync::conflict::{self, ConflictStrategy, StaleBaseOutcome};
use crate::sync::hasher;
use crate::sync::parity::{prompt_payload_json, PromptKind, PromptPayload};
use crate::sync::queue::SyncQueue;
use crate::sync::task::{SyncOperation, SyncTask};

pub async fn start_workers(
    num_workers: usize,
    queue: Arc<SyncQueue>,
    storage: ApsStorageClient,
    data_mgmt: ApsDataManagementClient,
    auth: ApsAuthClient,
    db: Arc<Mutex<SyncDatabase>>,
    project_id: String,
) {
    let storage = Arc::new(storage);
    let data_mgmt = Arc::new(data_mgmt);
    let auth = Arc::new(auth);
    let project_id = Arc::new(project_id);

    for i in 0..num_workers {
        let q = queue.clone();
        let s = storage.clone();
        let d = data_mgmt.clone();
        let a = auth.clone();
        let db = db.clone();
        let p_id = project_id.clone();

        tokio::spawn(async move {
            debug!("Worker {} started", i);
            loop {
                let mut task = match q.pop().await {
                    Some(t) => t,
                    None => {
                        q.wait_for_work(Duration::from_millis(500)).await;
                        continue;
                    }
                };

                let token = match a.get_access_token() {
                    Ok(t) => t,
                    Err(e) => {
                        warn!("Worker {} task {} auth error: {} — retry later", i, task.id, e);
                        task.not_before = Some(Instant::now() + Duration::from_secs(2));
                        q.finish(&task).await;
                        q.push(task).await;
                        continue;
                    }
                };

                let _upload_permit = if uses_upload_permit(&task.operation) {
                    match q.upload_semaphore().acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!("Upload semaphore closed; re-queue task {}", task.id);
                            task.not_before = Some(Instant::now() + Duration::from_millis(200));
                            q.finish(&task).await;
                            q.push(task).await;
                            continue;
                        }
                    }
                } else {
                    None
                };

                let _download_permit = if uses_download_permit(&task.operation) {
                    match q.download_semaphore().acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!("Download semaphore closed; re-queue task {}", task.id);
                            task.not_before = Some(Instant::now() + Duration::from_millis(200));
                            q.finish(&task).await;
                            q.push(task).await;
                            continue;
                        }
                    }
                } else {
                    None
                };

                match process_task(&task, &token, &s, &d, &db, &p_id).await {
                    Ok(_) => {
                        complete_journal(&db, &task);
                        info!("Task completed: {:?}", task.operation);
                        q.finish(&task).await;
                    }
                    Err(e) => {
                        error!("Task failed: {:?} - {}", task.operation, e);
                        task.retry_count += 1;
                        if task.retry_count < 5 {
                            task.not_before = Some(Instant::now() + task.backoff_duration());
                            q.finish(&task).await;
                            q.push(task).await;
                        } else {
                            error!("Task exceeded max retries: {:?}", task.operation);
                            fail_journal(&db, &task, &e.to_string());
                            q.finish(&task).await;
                        }
                    }
                }
            }
        });
    }
}

fn uses_upload_permit(op: &SyncOperation) -> bool {
    matches!(
        op,
        SyncOperation::Upload
            | SyncOperation::CreateRemoteFile
            | SyncOperation::CreateRemoteFolder
            | SyncOperation::DeleteCloud
    )
}

fn uses_download_permit(op: &SyncOperation) -> bool {
    matches!(op, SyncOperation::Download | SyncOperation::GetLatestVersion)
}

async fn process_task(
    task: &SyncTask,
    token: &str,
    storage: &ApsStorageClient,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    match &task.operation {
        SyncOperation::Download => process_download(task, token, storage, data_mgmt, db, project_id, false).await,
        SyncOperation::GetLatestVersion => {
            process_download(task, token, storage, data_mgmt, db, project_id, true).await
        }
        SyncOperation::Upload => process_upload_existing(task, token, storage, data_mgmt, db, project_id).await,
        SyncOperation::CreateRemoteFile => {
            process_create_remote_file(task, token, storage, data_mgmt, db, project_id).await
        }
        SyncOperation::CreateRemoteFolder => {
            process_create_remote_folder(task, token, data_mgmt, db, project_id).await
        }
        SyncOperation::DeleteCloud => {
            process_delete_cloud(task, token, data_mgmt, db, project_id).await
        }
        SyncOperation::DeleteLocal => process_delete_local(task, db).await,
        SyncOperation::CreateFolder => {
            if !tether_cfapi::path_exists_no_recall(&task.local_path) {
                std::fs::create_dir_all(&task.local_path)?;
                debug!("Created folder {}", task.local_path.display());
            }
            Ok(())
        }
        _ => {
            warn!("Unhandled sync operation: {:?}", task.operation);
            Ok(())
        }
    }
}

async fn process_download(
    task: &SyncTask,
    token: &str,
    storage: &ApsStorageClient,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
    prompt_on_conflict: bool,
) -> anyhow::Result<()> {
    let item_id = task
        .cloud_item_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Download missing cloud_item_id"))?;

    debug!("Fetching versions for item: {}", item_id);
    let versions = data_mgmt.get_item_versions(token, project_id, item_id).await?;
    let active_version = versions
        .first()
        .ok_or_else(|| anyhow::anyhow!("No versions found for item {}", item_id))?;
    let remote_head_id = active_version.id.clone();
    let cloud_modified = active_version
        .attributes
        .last_modified_time
        .as_deref()
        .and_then(parse_cloud_time);

    let storage_urn = active_version
        .relationships
        .as_ref()
        .and_then(|r| r.storage.as_ref())
        .and_then(|s| s.data.as_ref())
        .map(|d| d.id.clone())
        .ok_or_else(|| anyhow::anyhow!("Item version missing storage URN"))?;

    let (bucket_key, object_key) = parse_storage_urn(&storage_urn)?;

    if let Some(parent) = task.local_path.parent() {
        if !tether_cfapi::path_exists_no_recall(parent) {
            std::fs::create_dir_all(parent)?;
        }
    }

    info!("Downloading {} to {}", object_key, task.local_path.display());
    let data = storage
        .download_to_bytes(token, &bucket_key, &object_key)
        .await?;

    if let Some((sr_id, rel, existing_base)) = stale_conflict_details(task, db, &remote_head_id, cloud_modified)? {
        if prompt_on_conflict {
            stage_conflict_prompt(
                db,
                &sr_id,
                PromptKind::GetLatestConflict,
                &rel,
                Some(item_id.clone()),
                Some(remote_head_id.clone()),
                format!(
                    "Get Latest Version found unsynced local edits for {rel}. Choose how to resolve the local and cloud versions."
                ),
            )?;
            info!("Queued get-latest prompt for {}", rel);
            return Ok(());
        }

        if existing_base.is_some() {
            stage_conflict_prompt(
                db,
                &sr_id,
                PromptKind::ConflictUpload,
                &rel,
                Some(item_id.clone()),
                Some(remote_head_id.clone()),
                format!(
                    "Cloud version changed while local edits exist for {rel}. Resolve before replacing local data."
                ),
            )?;
            info!("Queued download conflict prompt for {}", rel);
            return Ok(());
        }
    }

    let local_newer = match (tether_cfapi::path_exists_no_recall(&task.local_path), cloud_modified) {
        (true, Some(cloud_t)) => std::fs::metadata(&task.local_path)
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|lm| lm > cloud_t)
            .unwrap_or(false),
        _ => false,
    };

    if local_newer && prompt_on_conflict {
        if let (Some(sr_id), Some(root)) = (&task.sync_root_id, &task.sync_root_path) {
            let rel = relative_under_root(root, &task.local_path);
            stage_conflict_prompt(
                db,
                sr_id,
                PromptKind::GetLatestConflict,
                &rel,
                Some(item_id.clone()),
                Some(remote_head_id.clone()),
                format!("Local edits exist for {rel}. Resolve before applying Get Latest Version."),
            )?;
            return Ok(());
        }
    }

    if local_newer {
        conflict::resolve_conflict(&task.local_path, &data, ConflictStrategy::KeepBoth).await?;
    } else {
        tokio::fs::write(&task.local_path, &data).await?;
    }

    if let (Some(sr_id), Some(root)) = (&task.sync_root_id, &task.sync_root_path) {
        let rel = relative_under_root(root, &task.local_path);
        persist_after_download(
            db,
            sr_id,
            &rel,
            item_id,
            &remote_head_id,
            active_version.attributes.last_modified_time.as_deref(),
            &storage_urn,
            &task.local_path,
        )
        .await?;
    }

    try_mark_downloaded_in_sync(&task.local_path);
    info!("Finished downloading item {}", item_id);
    Ok(())
}

async fn process_upload_existing(
    task: &SyncTask,
    token: &str,
    storage: &ApsStorageClient,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    if tether_cfapi::is_cloud_only_attr(&task.local_path) {
        info!(
            "Skipping upload for cloud-only placeholder {}",
            task.local_path.display()
        );
        return Ok(());
    }

    let item_id = task
        .cloud_item_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Upload missing cloud_item_id"))?;
    let (item, parent_folder_id) = data_mgmt
        .get_item_with_parent_folder(token, project_id, item_id)
        .await?;
    let versions = data_mgmt.get_item_versions(token, project_id, item_id).await?;
    let remote_head_id = versions
        .first()
        .map(|v| v.id.clone())
        .ok_or_else(|| anyhow::anyhow!("No remote versions for item {}", item_id))?;

    if let Some((sr_id, rel, _)) = stale_conflict_details(task, db, &remote_head_id, None)? {
        stage_conflict_prompt(
            db,
            &sr_id,
            PromptKind::ConflictUpload,
            &rel,
            Some(item_id.clone()),
            Some(remote_head_id.clone()),
            format!(
                "Local changes for {rel} were based on an older cloud version. Resolve before upload."
            ),
        )?;
        return Ok(());
    }

    let file_name = item.attributes.display_name.clone();
    let loc = storage
        .create_storage_location(token, project_id, &parent_folder_id, &file_name)
        .await?;
    storage
        .upload_file(token, &loc.bucket_key, &loc.object_key, &task.local_path)
        .await?;

    let version = data_mgmt
        .create_version(token, project_id, item_id, &file_name, &loc.id)
        .await?;
    let storage_urn = version
        .relationships
        .as_ref()
        .and_then(|r| r.storage.as_ref())
        .and_then(|s| s.data.as_ref())
        .map(|d| d.id.clone())
        .unwrap_or_else(|| loc.id.clone());

    if let (Some(sr_id), Some(root)) = (&task.sync_root_id, &task.sync_root_path) {
        let rel = relative_under_root(root, &task.local_path);
        persist_after_upload(
            db,
            sr_id,
            &rel,
            item_id,
            &version.id,
            version.attributes.last_modified_time.as_deref(),
            &storage_urn,
            &task.local_path,
        )
        .await?;
    }

    try_mark_downloaded_in_sync(&task.local_path);
    info!("Upload finished for {}", task.local_path.display());
    Ok(())
}

async fn process_create_remote_file(
    task: &SyncTask,
    token: &str,
    storage: &ApsStorageClient,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    if tether_cfapi::is_cloud_only_attr(&task.local_path) {
        info!(
            "Skipping remote create for cloud-only placeholder {}",
            task.local_path.display()
        );
        return Ok(());
    }

    let sync_root_id = task
        .sync_root_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("CreateRemoteFile missing sync_root_id"))?;
    let sync_root = task
        .sync_root_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("CreateRemoteFile missing sync_root_path"))?;
    let rel = relative_under_root(sync_root, &task.local_path);
    let parent_rel = parent_relative_path(&rel);
    let parent_folder_id =
        ensure_remote_folder(token, data_mgmt, db, project_id, sync_root_id, &parent_rel).await?;
    let file_name = task
        .local_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("CreateRemoteFile missing file name"))?;

    let existing = data_mgmt
        .find_folder_entry_by_name(token, project_id, &parent_folder_id, file_name)
        .await?;

    let loc = storage
        .create_storage_location(token, project_id, &parent_folder_id, file_name)
        .await?;
    storage
        .upload_file(token, &loc.bucket_key, &loc.object_key, &task.local_path)
        .await?;

    let (item_id, version_id, cloud_modified, storage_urn) = if let Some(existing_item) = existing {
        let version = data_mgmt
            .create_version(token, project_id, &existing_item.id, file_name, &loc.id)
            .await?;
        (
            existing_item.id,
            version.id.clone(),
            version.attributes.last_modified_time,
            version
                .relationships
                .as_ref()
                .and_then(|r| r.storage.as_ref())
                .and_then(|s| s.data.as_ref())
                .map(|d| d.id.clone())
                .unwrap_or_else(|| loc.id.clone()),
        )
    } else {
        let item = data_mgmt
            .create_item(token, project_id, &parent_folder_id, file_name, &loc.id)
            .await?;
        let versions = data_mgmt.get_item_versions(token, project_id, &item.id).await?;
        let active = versions
            .first()
            .ok_or_else(|| anyhow::anyhow!("Created item missing version"))?;
        (
            item.id,
            active.id.clone(),
            active.attributes.last_modified_time.clone(),
            active
                .relationships
                .as_ref()
                .and_then(|r| r.storage.as_ref())
                .and_then(|s| s.data.as_ref())
                .map(|d| d.id.clone())
                .unwrap_or_else(|| loc.id.clone()),
        )
    };

    persist_after_upload(
        db,
        sync_root_id,
        &rel,
        &item_id,
        &version_id,
        cloud_modified.as_deref(),
        &storage_urn,
        &task.local_path,
    )
    .await?;
    try_mark_downloaded_in_sync(&task.local_path);
    Ok(())
}

async fn process_create_remote_folder(
    task: &SyncTask,
    token: &str,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    let sync_root_id = task
        .sync_root_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("CreateRemoteFolder missing sync_root_id"))?;
    let sync_root = task
        .sync_root_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("CreateRemoteFolder missing sync_root_path"))?;
    let rel = relative_under_root(sync_root, &task.local_path);
    ensure_remote_folder(token, data_mgmt, db, project_id, sync_root_id, &rel).await?;
    Ok(())
}

async fn process_delete_cloud(
    task: &SyncTask,
    token: &str,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    let item_id = task
        .cloud_item_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("DeleteCloud missing cloud_item_id"))?;

    let is_directory = if let (Some(sync_root_id), Some(sync_root_path)) =
        (&task.sync_root_id, &task.sync_root_path)
    {
        let rel = relative_under_root(sync_root_path, &task.local_path);
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard
            .get_file_entry_by_path(sync_root_id, &rel)?
            .map(|e| e.is_directory)
            .unwrap_or_else(|| tether_cfapi::is_dir_no_recall(&task.local_path))
    } else {
        tether_cfapi::is_dir_no_recall(&task.local_path)
    };

    if is_directory {
        let contents = data_mgmt
            .get_folder_contents(token, project_id, item_id)
            .await
            .unwrap_or_default();
        for child in contents {
            if child.item_type == "items" {
                data_mgmt
                    .delete_item_as_deleted_version(
                        token,
                        project_id,
                        &child.id,
                        &child.attributes.display_name,
                    )
                    .await?;
            }
        }
    } else {
        let display_name = task
            .local_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        data_mgmt
            .delete_item_as_deleted_version(token, project_id, item_id, display_name)
            .await?;
    }

    Ok(())
}

async fn process_delete_local(task: &SyncTask, db: &Arc<Mutex<SyncDatabase>>) -> anyhow::Result<()> {
    if tether_cfapi::is_dir_no_recall(&task.local_path) {
        return Ok(());
    }
    if tether_cfapi::path_exists_no_recall(&task.local_path) {
        tokio::fs::remove_file(&task.local_path).await?;
    }
    if let (Some(sync_root_id), Some(sync_root_path)) = (&task.sync_root_id, &task.sync_root_path) {
        let rel = relative_under_root(sync_root_path, &task.local_path);
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard.update_hydration_state(sync_root_id, &rel, "online_only", true, Some("delete_local"))?;
    }
    Ok(())
}

fn stale_conflict_details(
    task: &SyncTask,
    db: &Arc<Mutex<SyncDatabase>>,
    remote_head_id: &str,
    cloud_modified: Option<SystemTime>,
) -> anyhow::Result<Option<(String, String, Option<String>)>> {
    let sync_root_id = match &task.sync_root_id {
        Some(v) => v,
        None => return Ok(None),
    };
    let sync_root_path = match &task.sync_root_path {
        Some(v) => v,
        None => return Ok(None),
    };
    let rel = relative_under_root(sync_root_path, &task.local_path);
    let entry = {
        let g = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        g.get_file_entry_by_path(sync_root_id, &rel)?
    };
    let Some(entry) = entry else {
        return Ok(None);
    };
    let Some(base_vid) = entry.base_remote_version_id.as_ref() else {
        return Ok(None);
    };
    if !matches!(
        conflict::evaluate_stale_base(Some(base_vid.as_str()), remote_head_id),
        StaleBaseOutcome::StaleConflict { .. }
    ) {
        return Ok(None);
    }
    if !tether_cfapi::path_exists_no_recall(&task.local_path) {
        return Ok(None);
    }
    let local_changed = std::fs::metadata(&task.local_path)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|lm| cloud_modified.map(|ct| lm > ct).unwrap_or(true))
        .unwrap_or(true);
    if !local_changed {
        return Ok(None);
    }
    Ok(Some((
        sync_root_id.clone(),
        rel,
        entry.base_remote_version_id.clone(),
    )))
}

fn stage_conflict_prompt(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    kind: PromptKind,
    relative_path: &str,
    cloud_item_id: Option<String>,
    remote_head_version_id: Option<String>,
    message: String,
) -> anyhow::Result<()> {
    let payload = PromptPayload {
        kind,
        relative_path: relative_path.to_string(),
        cloud_item_id,
        remote_head_version_id,
        message,
        is_directory: false,
    };
    let payload_json = prompt_payload_json(&payload)?;
    let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let pending_id = guard.insert_pending_job(sync_root_id, "prompt", Some(&payload_json), None)?;
    guard.update_pending_job(&pending_id, "action_required", None, None)?;
    Ok(())
}

async fn ensure_remote_folder(
    token: &str,
    data_mgmt: &ApsDataManagementClient,
    db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
    sync_root_id: &str,
    relative_dir: &str,
) -> anyhow::Result<String> {
    let root_folder_id = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let root = guard
            .get_sync_root(sync_root_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing sync root {}", sync_root_id))?;
        root.folder_id
    };

    if relative_dir.is_empty() {
        return Ok(root_folder_id);
    }

    let mut current_folder_id = root_folder_id;
    let mut built_rel = String::new();
    for segment in relative_dir.split('/').filter(|s| !s.is_empty()) {
        if !built_rel.is_empty() {
            built_rel.push('/');
        }
        built_rel.push_str(segment);

        if let Some(existing_id) = {
            let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            guard
                .get_file_entry_by_path(sync_root_id, &built_rel)?
                .and_then(|row| if row.is_directory { row.cloud_item_id } else { None })
        } {
            current_folder_id = existing_id;
            continue;
        }

        let folder_id = match data_mgmt
            .find_folder_entry_by_name(token, project_id, &current_folder_id, segment)
            .await?
        {
            Some(entry) if entry.item_type == "folders" => entry.id,
            Some(_) => anyhow::bail!("A file already exists where folder {} should be", built_rel),
            None => data_mgmt
                .create_folder(token, project_id, &current_folder_id, segment)
                .await?
                .id,
        };

        upsert_directory_entry(db, sync_root_id, &built_rel, &folder_id)?;
        current_folder_id = folder_id;
    }

    Ok(current_folder_id)
}

fn upsert_directory_entry(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    relative_dir: &str,
    cloud_folder_id: &str,
) -> anyhow::Result<()> {
    let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let existing = guard.get_file_entry_by_path(sync_root_id, relative_dir)?;
    let mut row = existing.unwrap_or_else(|| {
        let mut r = FileEntryRow::default();
        r.id = Uuid::new_v4().to_string();
        r
    });
    row.sync_root_id = sync_root_id.to_string();
    row.local_relative_path = relative_dir.to_string();
    row.cloud_item_id = Some(cloud_folder_id.to_string());
    row.is_directory = true;
    row.is_placeholder = false;
    row.sync_state = "in_sync".into();
    row.hydration_state = "hydrated_ephemeral".into();
    guard.upsert_file_entry(&row)?;
    guard.log_activity(
        "create_folder",
        Some(relative_dir),
        Some(cloud_folder_id),
        "success",
        None,
        None,
    )?;
    Ok(())
}

pub fn parse_storage_urn(storage_urn: &str) -> anyhow::Result<(String, String)> {
    let parts: Vec<&str> = storage_urn.split(':').collect();
    if parts.len() < 4 {
        anyhow::bail!("Invalid storage URN format: {}", storage_urn);
    }
    let path_parts: Vec<&str> = parts[3].split('/').collect();
    if path_parts.len() < 2 {
        anyhow::bail!("Invalid storage URN bucket/object path: {}", storage_urn);
    }
    Ok((path_parts[0].to_string(), path_parts[1..].join("/")))
}

fn complete_journal(db: &Arc<Mutex<SyncDatabase>>, task: &SyncTask) {
    let Some(journal_id) = task.journal_id.as_ref() else {
        return;
    };
    if let Ok(guard) = db.lock() {
        let _ = guard.update_operation_journal_status(journal_id, "applied", None, None);
    }
}

fn fail_journal(db: &Arc<Mutex<SyncDatabase>>, task: &SyncTask, error_text: &str) {
    let Some(journal_id) = task.journal_id.as_ref() else {
        return;
    };
    if let Ok(guard) = db.lock() {
        let _ = guard.update_operation_journal_status(journal_id, "failed", Some(error_text), None);
    }
}

/// Clear Explorer "Sync pending" after we wrote cloud bytes (best-effort).
fn try_mark_downloaded_in_sync(path: &Path) {
    if let Err(e) = tether_cfapi::mark_placeholder_in_sync(path) {
        warn!(
            "mark_placeholder_in_sync failed for {}: {e:#}",
            path.display()
        );
    } else {
        info!("Marked in-sync: {}", path.display());
    }
}

pub fn relative_under_root(root: &Path, full: &Path) -> String {
    full.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            full.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

pub fn parent_relative_path(rel: &str) -> String {
    let mut parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let _ = parts.pop();
    parts.join("/")
}

pub async fn persist_after_download(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    rel: &str,
    item_id: &str,
    version_id: &str,
    cloud_modified: Option<&str>,
    storage_urn: &str,
    local_path: &Path,
) -> anyhow::Result<()> {
    let hash = hasher::hash_file(local_path).await?;
    let meta = tokio::fs::metadata(local_path).await?;
    let size = meta.len() as i64;
    let modified = meta.modified().ok().map(system_time_to_rfc3339);

    let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let existing = guard.get_file_entry_by_path(sync_root_id, rel)?;
    let mut row = existing.unwrap_or_else(|| {
        let mut r = FileEntryRow::default();
        r.id = Uuid::new_v4().to_string();
        r
    });
    row.sync_root_id = sync_root_id.to_string();
    row.local_relative_path = rel.to_string();
    row.cloud_item_id = Some(item_id.to_string());
    row.cloud_version_id = Some(version_id.to_string());
    row.cloud_storage_urn = Some(storage_urn.to_string());
    row.local_hash = Some(hash);
    row.file_size = Some(size);
    row.last_local_modified = modified;
    row.last_cloud_modified = cloud_modified.map(String::from);
    row.sync_state = "in_sync".into();
    row.is_placeholder = false;
    row.base_remote_version_id = Some(version_id.to_string());
    row.base_remote_modified = cloud_modified.map(String::from);
    row.hydration_state = "hydrated_ephemeral".into();
    guard.upsert_file_entry(&row)?;
    guard.log_activity("download", Some(rel), Some(item_id), "success", None, Some(size))?;
    Ok(())
}

pub async fn persist_after_upload(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    rel: &str,
    item_id: &str,
    version_id: &str,
    cloud_modified: Option<&str>,
    storage_urn: &str,
    local_path: &Path,
) -> anyhow::Result<()> {
    let hash = hasher::hash_file(local_path).await?;
    let meta = tokio::fs::metadata(local_path).await?;
    let size = meta.len() as i64;
    let modified = meta.modified().ok().map(system_time_to_rfc3339);

    let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let existing = guard.get_file_entry_by_path(sync_root_id, rel)?;
    let mut row = existing.unwrap_or_else(|| {
        let mut r = FileEntryRow::default();
        r.id = Uuid::new_v4().to_string();
        r
    });
    row.sync_root_id = sync_root_id.to_string();
    row.local_relative_path = rel.to_string();
    row.cloud_item_id = Some(item_id.to_string());
    row.cloud_version_id = Some(version_id.to_string());
    row.cloud_storage_urn = Some(storage_urn.to_string());
    row.local_hash = Some(hash);
    row.file_size = Some(size);
    row.last_local_modified = modified;
    row.last_cloud_modified = cloud_modified.map(String::from);
    row.sync_state = "in_sync".into();
    row.is_placeholder = false;
    row.base_remote_version_id = Some(version_id.to_string());
    row.base_remote_modified = cloud_modified.map(String::from);
    row.hydration_state = "hydrated_ephemeral".into();
    guard.upsert_file_entry(&row)?;
    guard.log_activity("upload", Some(rel), Some(item_id), "success", None, Some(size))?;
    Ok(())
}

fn system_time_to_rfc3339(ts: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    dt.to_rfc3339()
}

fn parse_cloud_time(s: &str) -> Option<SystemTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.into())
        .ok()
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(&format!("{s}Z"))
                .map(|dt| dt.into())
                .ok()
        })
}

#[cfg(test)]
mod tests {
    use super::{parent_relative_path, relative_under_root};
    use std::path::Path;

    #[test]
    fn relative_under_root_normalizes() {
        let root = Path::new(r"C:\Tether\Sync\P");
        let full = Path::new(r"C:\Tether\Sync\P\sub\a.ipt");
        assert_eq!(relative_under_root(root, full), "sub/a.ipt");
    }

    #[test]
    fn parent_relative_path_handles_root() {
        assert_eq!(parent_relative_path("file.ipt"), "");
        assert_eq!(parent_relative_path("a/b/file.ipt"), "a/b");
    }
}
