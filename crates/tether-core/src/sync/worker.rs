use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::api::auth::ApsAuthClient;
use crate::api::storage::ApsStorageClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::db::database::SyncDatabase;
use crate::sync::conflict::{self, ConflictStrategy};
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
                if let Some(mut task) = q.pop().await {
                    let token = match a.get_access_token() {
                        Ok(t) => t,
                        Err(e) => {
                            warn!("Worker {} failing task {} due to auth error: {}", i, task.id, e);
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            q.push(task).await;
                            continue;
                        }
                    };

                    match process_task(&task, &token, &s, &d, &db, &p_id).await {
                        Ok(_) => {
                            info!("Task completed: {:?}", task.operation);
                        }
                        Err(e) => {
                            error!("Task failed: {:?} - {}", task.operation, e);
                            task.retry_count += 1;
                            if task.retry_count < 5 {
                                let backoff = task.backoff_duration();
                                tokio::time::sleep(backoff).await;
                                q.push(task).await;
                            } else {
                                error!("Task exceeded max retries: {:?}", task.operation);
                            }
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(500)).await;
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
    _db: &Arc<Mutex<SyncDatabase>>,
    project_id: &str,
) -> anyhow::Result<()> {
    match &task.operation {
        SyncOperation::Download => {
            if let Some(item_id) = &task.cloud_item_id {
                debug!("Fetching versions for item: {}", item_id);
                // 1. Fetch item versions to get the active storage URN
                let versions = match data_mgmt.get_item_versions(token, project_id, item_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Failed to fetch versions for item {}: {}", item_id, e);
                        return Err(e.into());
                    }
                };

                let active_version = versions.first().ok_or_else(|| anyhow::anyhow!("No versions found for item {}", item_id))?;

                let cloud_modified = active_version
                    .attributes
                    .last_modified_time
                    .as_deref()
                    .and_then(parse_cloud_time);

                // 2. Extract storage urn
                let storage_urn = active_version
                    .relationships
                    .as_ref()
                    .and_then(|r| r.storage.as_ref())
                    .and_then(|s| s.data.as_ref())
                    .map(|d| d.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("Item version missing storage URN"))?;

                debug!("Derived storage URN: {}", storage_urn);

                // 3. Extract urn bucket_key and object_key
                // urn format is usually: urn:adsk.objects:os.object:bucket/object
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

                // 4. Create local directory if missing
                if let Some(parent) = task.local_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent)?;
                    }
                }

                // 5. Download bytes, resolve conflicts, write, hash
                info!("Downloading {} to {}", object_key, task.local_path.display());
                let data = storage
                    .download_to_bytes(token, bucket_key, &object_key)
                    .await?;

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

                info!("Finished downloading item {}", item_id);
            }
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

/// Parse APS ISO-8601 timestamps into [`std::time::SystemTime`].
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
