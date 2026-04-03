use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use anyhow::Result;
use tokio::runtime::Handle;

use crate::db::database::{FileEntryRow, SyncDatabase};
use crate::sync::change_detector::{should_exclude, ChangeDetector};
use crate::sync::parity::{offline_payload_json, OfflineJournalPayload, ServiceState};
use crate::sync::queue::SyncQueue;
use crate::sync::task::{SyncOperation, SyncPriority, SyncTask};
use crate::sync::worker::relative_under_root;

pub fn start(
    runtime: Handle,
    sync_root: PathBuf,
    db: Arc<Mutex<SyncDatabase>>,
    queue: Arc<SyncQueue>,
    sync_root_id: String,
) -> Result<ChangeDetector> {
    let (detector, rx) = ChangeDetector::start(&sync_root)?;
    let root_for_watch = sync_root.clone();
    let db_for_watch = db.clone();
    let queue_for_watch = queue.clone();
    let sync_root_for_watch = sync_root_id.clone();

    thread::spawn(move || {
        while let Ok(next) = rx.recv() {
            if next.is_err() {
                continue;
            }
            let root = root_for_watch.clone();
            let db = db_for_watch.clone();
            let queue = queue_for_watch.clone();
            let sync_root_id = sync_root_for_watch.clone();
            runtime.block_on(async move {
                let _ = reconcile_local_state(&root, &db, &queue, &sync_root_id).await;
            });
        }
    });

    let repair_root = sync_root;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
        loop {
            interval.tick().await;
            let _ = reconcile_local_state(&repair_root, &db, &queue, &sync_root_id).await;
        }
    });

    Ok(detector)
}

pub async fn replay_operation_journal(
    sync_root: &Path,
    db: &Arc<Mutex<SyncDatabase>>,
    queue: &Arc<SyncQueue>,
    sync_root_id: &str,
) -> Result<usize> {
    let queued = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard.list_operation_journal(sync_root_id, "queued", 256)?
    };

    let mut replayed = 0usize;
    for row in queued {
        let Some(payload_json) = row.payload_json.as_deref() else {
            continue;
        };
        let payload: OfflineJournalPayload = serde_json::from_str(payload_json)?;
        let local_path = sync_root.join(payload.relative_path.replace('/', "\\"));
        let mut task = SyncTask::new(
            match payload.operation.as_str() {
                "create_folder" => SyncOperation::CreateRemoteFolder,
                "create_file" => SyncOperation::CreateRemoteFile,
                "upload" => SyncOperation::Upload,
                "delete_cloud" => SyncOperation::DeleteCloud,
                _ => continue,
            },
            SyncPriority::High,
            local_path,
        );
        task.cloud_item_id = payload.cloud_item_id.clone();
        task.sync_root_id = Some(sync_root_id.to_string());
        task.sync_root_path = Some(sync_root.to_path_buf());
        task.journal_id = Some(row.id.clone());
        if let Some(dest) = payload.destination_relative_path {
            task.destination_path = Some(sync_root.join(dest.replace('/', "\\")));
        }
        queue.push(task).await;
        {
            let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            let _ = guard.update_operation_journal_status(&row.id, "applying", None, None);
        }
        replayed += 1;
    }

    Ok(replayed)
}

pub async fn reconcile_local_state(
    sync_root: &Path,
    db: &Arc<Mutex<SyncDatabase>>,
    queue: &Arc<SyncQueue>,
    sync_root_id: &str,
) -> Result<()> {
    let service_state = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let root = guard
            .get_sync_root(sync_root_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing sync root {}", sync_root_id))?;
        ServiceState::from_db(&root.service_state)
    };

    let online = matches!(service_state, ServiceState::Running | ServiceState::Reconnecting);

    let mut local_files = Vec::new();
    let mut local_dirs = Vec::new();
    collect_paths(sync_root, sync_root, &mut local_dirs, &mut local_files)?;

    let db_rows = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard.get_all_file_entries(sync_root_id)?
    };

    let known_by_rel: HashMap<String, FileEntryRow> = db_rows
        .into_iter()
        .map(|row| (row.local_relative_path.clone(), row))
        .collect();
    let pending_keys = pending_journal_keys(db, sync_root_id)?;

    for dir_rel in &local_dirs {
        if dir_rel.is_empty() {
            continue;
        }
        if !known_by_rel.contains_key(dir_rel) {
            queue_or_journal(
                sync_root,
                db,
                queue,
                sync_root_id,
                dir_rel,
                SyncOperation::CreateRemoteFolder,
                None,
                online,
                &pending_keys,
            )
            .await?;
        }
    }

    for file_rel in &local_files {
        let full = sync_root.join(file_rel.replace('/', "\\"));
        if tether_cfapi::is_cloud_only_placeholder(&full) {
            continue;
        }
        match known_by_rel.get(file_rel) {
            None => {
                if tether_cfapi::is_placeholder(&full) {
                    continue;
                }
                queue_or_journal(
                    sync_root,
                    db,
                    queue,
                    sync_root_id,
                    file_rel,
                    SyncOperation::CreateRemoteFile,
                    None,
                    online,
                    &pending_keys,
                )
                .await?;
            }
            Some(row) if row.cloud_item_id.is_some() => {
                let local_modified = std::fs::metadata(&full)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(system_time_to_rfc3339);
                let changed_since_last = local_modified.as_deref() != row.last_local_modified.as_deref();
                if changed_since_last || tether_cfapi::is_sync_pending(&full) {
                    queue_or_journal(
                        sync_root,
                        db,
                        queue,
                        sync_root_id,
                        file_rel,
                        SyncOperation::Upload,
                        row.cloud_item_id.clone(),
                        online,
                        &pending_keys,
                    )
                    .await?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn pending_journal_keys(
    db: &Arc<Mutex<SyncDatabase>>,
    sync_root_id: &str,
) -> Result<HashSet<String>> {
    let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
    let queued = guard.list_operation_journal(sync_root_id, "queued", 512)?;
    let applying = guard.list_operation_journal(sync_root_id, "applying", 512)?;
    let mut keys = HashSet::new();
    for row in queued.into_iter().chain(applying.into_iter()) {
        if let Some(rel) = row.relative_path {
            keys.insert(format!("{}:{}", row.operation_type, rel));
        }
    }
    Ok(keys)
}

async fn queue_or_journal(
    sync_root: &Path,
    db: &Arc<Mutex<SyncDatabase>>,
    queue: &Arc<SyncQueue>,
    sync_root_id: &str,
    relative_path: &str,
    operation: SyncOperation,
    cloud_item_id: Option<String>,
    online: bool,
    pending_keys: &HashSet<String>,
) -> Result<()> {
    let op_key = operation_name(&operation);
    let dedupe_key = format!("{op_key}:{relative_path}");
    if pending_keys.contains(&dedupe_key) {
        return Ok(());
    }

    let local_path = sync_root.join(relative_path.replace('/', "\\"));
    if online {
        let mut task = SyncTask::new(operation, SyncPriority::High, local_path);
        task.cloud_item_id = cloud_item_id;
        task.sync_root_id = Some(sync_root_id.to_string());
        task.sync_root_path = Some(sync_root.to_path_buf());
        queue.push(task).await;
    } else {
        let payload = OfflineJournalPayload {
            operation: op_key.to_string(),
            relative_path: relative_path.to_string(),
            cloud_item_id,
            destination_relative_path: None,
        };
        let payload_json = offline_payload_json(&payload)?;
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let _ = guard.insert_operation_journal(
            sync_root_id,
            op_key,
            Some(relative_path),
            Some(&payload_json),
            None,
        )?;
    }
    Ok(())
}

fn collect_paths(
    root: &Path,
    current: &Path,
    dirs: &mut Vec<String>,
    files: &mut Vec<String>,
) -> Result<()> {
    let entries = match std::fs::read_dir(current) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if should_exclude(&path) {
            continue;
        }
        if tether_cfapi::is_cloud_only_placeholder(&path) {
            continue;
        }
        let rel = relative_under_root(root, &path);
        if path.is_dir() {
            if tether_cfapi::is_placeholder(&path) {
                continue;
            }
            dirs.push(rel.clone());
            collect_paths(root, &path, dirs, files)?;
        } else if path.is_file() {
            files.push(rel);
        }
    }
    Ok(())
}

fn operation_name(op: &SyncOperation) -> &'static str {
    match op {
        SyncOperation::CreateRemoteFolder => "create_folder",
        SyncOperation::CreateRemoteFile => "create_file",
        SyncOperation::Upload => "upload",
        SyncOperation::DeleteCloud => "delete_cloud",
        _ => "unknown",
    }
}

fn system_time_to_rfc3339(ts: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    dt.to_rfc3339()
}
