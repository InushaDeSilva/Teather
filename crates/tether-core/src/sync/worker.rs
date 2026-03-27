use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::path::Path;

use tracing::{debug, error, info, warn};

use uuid::Uuid;

use crate::api::auth::ApsAuthClient;
use crate::api::storage::ApsStorageClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::db::database::{FileEntryRow, SyncDatabase};
use crate::sync::conflict::{self, ConflictStrategy, StaleBaseOutcome};
use crate::sync::hasher;
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
                        q.push(task).await;
                        continue;
                    }
                };

                let _upload_permit = if matches!(task.operation, SyncOperation::Upload) {
                    match q.upload_semaphore().acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!("Upload semaphore closed; re-queue task {}", task.id);
                            task.not_before = Some(Instant::now() + Duration::from_millis(200));
                            q.push(task).await;
                            continue;
                        }
                    }
                } else {
                    None
                };

                let _download_permit = if matches!(task.operation, SyncOperation::Download) {
                    match q.download_semaphore().acquire_owned().await {
                        Ok(p) => Some(p),
                        Err(_) => {
                            warn!("Download semaphore closed; re-queue task {}", task.id);
                            task.not_before = Some(Instant::now() + Duration::from_millis(200));
                            q.push(task).await;
                            continue;
                        }
                    }
                } else {
                    None
                };

                match process_task(&task, &token, &s, &d, &db, &p_id).await {
                    Ok(_) => {
                        info!("Task completed: {:?}", task.operation);
                    }
                    Err(e) => {
                        error!("Task failed: {:?} - {}", task.operation, e);
                        task.retry_count += 1;
                        if task.retry_count < 5 {
                            task.not_before = Some(Instant::now() + task.backoff_duration());
                            q.push(task).await;
                        } else {
                            error!("Task exceeded max retries: {:?}", task.operation);
                        }
                    }
                }
            }
        });
    }
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
        SyncOperation::Download => {
            if let Some(item_id) = &task.cloud_item_id {
                debug!("Fetching versions for item: {}", item_id);
                let versions = match data_mgmt.get_item_versions(token, project_id, item_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Failed to fetch versions for item {}: {}", item_id, e);
                        return Err(e.into());
                    }
                };

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

                let parts: Vec<&str> = storage_urn.split(':').collect();
                if parts.len() < 4 {
                    anyhow::bail!("Invalid storage URN format: {}", storage_urn);
                }

                let path_parts: Vec<&str> = parts[3].split('/').collect();
                if path_parts.len() < 2 {
                    anyhow::bail!("Invalid storage URN bucket/object path: {}", storage_urn);
                }

                let bucket_key = path_parts[0];
                let object_key = path_parts[1..].join("/");

                if let Some(parent) = task.local_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent)?;
                    }
                }

                info!("Downloading {} to {}", object_key, task.local_path.display());
                let data = storage
                    .download_to_bytes(token, bucket_key, &object_key)
                    .await?;

                // Stale-base check — release DB lock before any await
                let stale_keep_both: Option<(String, String)> = (|| {
                    let sr_id = task.sync_root_id.as_ref()?;
                    let root = task.sync_root_path.as_ref()?;
                    let rel = relative_under_root(root, &task.local_path);
                    let entry = {
                        let g = db.lock().ok()?;
                        g.get_file_entry_by_path(sr_id, &rel).ok()??
                    };
                    let base_vid = entry.base_remote_version_id.as_ref()?;
                    if !matches!(
                        conflict::evaluate_stale_base(Some(base_vid.as_str()), &remote_head_id),
                        StaleBaseOutcome::StaleConflict { .. }
                    ) {
                        return None;
                    }
                    if !task.local_path.exists() {
                        return None;
                    }
                    let local_changed = std::fs::metadata(&task.local_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|lm| cloud_modified.map(|ct| lm > ct).unwrap_or(true))
                        .unwrap_or(false);
                    if !local_changed {
                        return None;
                    }
                    Some((sr_id.clone(), rel))
                })();

                if let Some((sr_id, rel)) = stale_keep_both {
                    conflict::resolve_conflict(
                        &task.local_path,
                        &data,
                        ConflictStrategy::KeepBoth,
                    )
                    .await?;
                    persist_after_download(
                        db,
                        &sr_id,
                        &rel,
                        item_id,
                        &remote_head_id,
                        active_version
                            .attributes
                            .last_modified_time
                            .as_deref(),
                        &storage_urn,
                        &task.local_path,
                    )
                    .await?;
                    try_mark_downloaded_in_sync(&task.local_path);
                    return Ok(());
                }

                let local_newer = match (task.local_path.exists(), cloud_modified) {
                    (true, Some(cloud_t)) => std::fs::metadata(&task.local_path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .map(|lm| lm > cloud_t)
                        .unwrap_or(false),
                    _ => false,
                };

                if local_newer {
                    conflict::resolve_conflict(
                        &task.local_path,
                        &data,
                        ConflictStrategy::KeepBoth,
                    )
                    .await?;
                } else {
                    tokio::fs::write(&task.local_path, &data).await?;
                }

                let hash = hasher::hash_file(&task.local_path).await?;
                debug!("SHA-256 after download: {} ({})", hash, task.local_path.display());

                if let (Some(sr_id), Some(root)) = (&task.sync_root_id, &task.sync_root_path) {
                    let rel = relative_under_root(root, &task.local_path);
                    persist_after_download(
                        db,
                        sr_id,
                        &rel,
                        item_id,
                        &remote_head_id,
                        active_version
                            .attributes
                            .last_modified_time
                            .as_deref(),
                        &storage_urn,
                        &task.local_path,
                    )
                    .await?;
                }

                try_mark_downloaded_in_sync(&task.local_path);
                info!("Finished downloading item {}", item_id);
            }
        }
        SyncOperation::Upload => {
            let item_id = task
                .cloud_item_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Upload missing cloud_item_id"))?;

            let (item, parent_folder_id) = data_mgmt
                .get_item_with_parent_folder(token, project_id, item_id)
                .await?;

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

            let cloud_modified = version.attributes.last_modified_time.as_deref();
            let vid = version.id.clone();

            if let (Some(sr_id), Some(root)) = (&task.sync_root_id, &task.sync_root_path) {
                let rel = relative_under_root(root, &task.local_path);
                persist_after_upload(
                    db,
                    sr_id,
                    &rel,
                    item_id,
                    &vid,
                    cloud_modified,
                    &storage_urn,
                    &task.local_path,
                )
                .await?;
            }

            tether_cfapi::mark_placeholder_in_sync(&task.local_path)
                .map_err(|e| anyhow::anyhow!("mark in sync: {e:#}"))?;
            info!("Upload finished for {}", task.local_path.display());
        }
        SyncOperation::CreateFolder => {
            if !task.local_path.exists() {
                std::fs::create_dir_all(&task.local_path)?;
                debug!("Created folder {}", task.local_path.display());
            }
        }
        _ => {
            warn!("Unhandled sync operation: {:?}", task.operation);
        }
    }
    Ok(())
}

/// Clear Explorer "Sync pending" after we wrote cloud bytes (best-effort).
fn try_mark_downloaded_in_sync(path: &Path) {
    if let Err(e) = tether_cfapi::mark_placeholder_in_sync(path) {
        debug!(
            "mark_placeholder_in_sync after download (non-fatal) {}: {e:#}",
            path.display()
        );
    }
}

fn relative_under_root(root: &std::path::Path, full: &std::path::Path) -> String {
    full.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            full.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

async fn persist_after_download(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    rel: &str,
    item_id: &str,
    version_id: &str,
    cloud_modified: Option<&str>,
    storage_urn: &str,
    local_path: &std::path::Path,
) -> anyhow::Result<()> {
    let hash = hasher::hash_file(local_path).await?;
    let meta = tokio::fs::metadata(local_path).await?;
    let size = meta.len() as i64;

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
    row.last_cloud_modified = cloud_modified.map(String::from);
    row.sync_state = "in_sync".into();
    row.is_placeholder = false;
    row.base_remote_version_id = Some(version_id.to_string());
    row.base_remote_modified = cloud_modified.map(String::from);
    row.hydration_state = "hydrated_ephemeral".into();
    guard.upsert_file_entry(&row)?;
    guard.log_activity(
        "download",
        Some(rel),
        Some(item_id),
        "success",
        None,
        Some(size),
    )?;
    Ok(())
}

async fn persist_after_upload(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
    rel: &str,
    item_id: &str,
    version_id: &str,
    cloud_modified: Option<&str>,
    storage_urn: &str,
    local_path: &std::path::Path,
) -> anyhow::Result<()> {
    let hash = hasher::hash_file(local_path).await?;
    let meta = tokio::fs::metadata(local_path).await?;
    let size = meta.len() as i64;

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
    row.last_cloud_modified = cloud_modified.map(String::from);
    row.sync_state = "in_sync".into();
    row.is_placeholder = false;
    row.base_remote_version_id = Some(version_id.to_string());
    row.base_remote_modified = cloud_modified.map(String::from);
    row.hydration_state = "hydrated_ephemeral".into();
    guard.upsert_file_entry(&row)?;
    guard.log_activity(
        "upload",
        Some(rel),
        Some(item_id),
        "success",
        None,
        Some(size),
    )?;
    Ok(())
}

fn parse_cloud_time(s: &str) -> Option<std::time::SystemTime> {
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
    use super::relative_under_root;
    use std::path::Path;

    #[test]
    fn relative_under_root_normalizes() {
        let root = Path::new(r"C:\Tether\Sync\P");
        let full = Path::new(r"C:\Tether\Sync\P\sub\a.ipt");
        assert_eq!(relative_under_root(root, full), "sub/a.ipt");
    }
}
