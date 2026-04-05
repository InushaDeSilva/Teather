use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;

use anyhow::Result;
use notify_debouncer_full::DebouncedEvent;
use tokio::runtime::Handle;

use crate::db::database::{FileEntryRow, SyncDatabase};
use crate::sync::change_detector::{should_exclude, ChangeDetector};
use crate::sync::parity::{offline_payload_json, OfflineJournalPayload, ServiceState};
use crate::sync::queue::SyncQueue;
use crate::sync::save_patterns::{is_old_versions_archive_path, SavePatternCoalescer};
use crate::sync::task::{SyncOperation, SyncPriority, SyncTask};
use crate::sync::worker::relative_under_root;

pub fn start(
    runtime: Handle,
    sync_root: PathBuf,
    db: Arc<Mutex<SyncDatabase>>,
    queue: Arc<SyncQueue>,
    sync_root_id: String,
    save_patterns: Arc<Mutex<SavePatternCoalescer>>,
) -> Result<ChangeDetector> {
    let (detector, rx) = ChangeDetector::start(&sync_root)?;
    let root_for_watch = sync_root.clone();
    let db_for_watch = db.clone();
    let queue_for_watch = queue.clone();
    let sync_root_for_watch = sync_root_id.clone();
    let save_patterns_for_watch = save_patterns.clone();

    thread::spawn(move || {
        while let Ok(next) = rx.recv() {
            let Ok(events) = next else {
                continue;
            };
            let root = root_for_watch.clone();
            let db = db_for_watch.clone();
            let queue = queue_for_watch.clone();
            let sync_root_id = sync_root_for_watch.clone();
            let save_patterns = save_patterns_for_watch.clone();
            runtime.block_on(async move {
                let _ = reconcile_event_paths(
                    &root,
                    &db,
                    &queue,
                    &sync_root_id,
                    &save_patterns,
                    &events,
                )
                .await;
            });
        }
    });

    let repair_root = sync_root;
    let save_patterns_for_repair = save_patterns;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let _ = reconcile_local_state(
                &repair_root,
                &db,
                &queue,
                &sync_root_id,
                &save_patterns_for_repair,
            )
            .await;
        }
    });

    Ok(detector)
}

async fn reconcile_event_paths(
    sync_root: &Path,
    db: &Arc<Mutex<SyncDatabase>>,
    queue: &Arc<SyncQueue>,
    sync_root_id: &str,
    save_patterns: &Arc<Mutex<SavePatternCoalescer>>,
    events: &[DebouncedEvent],
) -> Result<()> {
    let mut changed_paths = HashSet::new();
    for event in events {
        for path in &event.paths {
            if !path.starts_with(sync_root) {
                continue;
            }
            changed_paths.insert(path.clone());
        }
    }
    if changed_paths.is_empty() {
        return Ok(());
    }

    let service_state = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let root = guard
            .get_sync_root(sync_root_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing sync root {}", sync_root_id))?;
        ServiceState::from_db(&root.service_state)
    };
    let online = matches!(service_state, ServiceState::Running | ServiceState::Reconnecting);
    let db_rows = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard.get_all_file_entries(sync_root_id)?
    };
    let known_by_rel: HashMap<String, FileEntryRow> = db_rows
        .into_iter()
        .map(|row| (row.local_relative_path.clone(), row))
        .collect();
    let pending_keys = pending_journal_keys(db, sync_root_id)?;
    let deferred_archives = {
        let mut guard = save_patterns
            .lock()
            .map_err(|e| anyhow::anyhow!("save_patterns lock: {e}"))?;
        guard.clear_stale();
        changed_paths
            .iter()
            .filter_map(|path| {
                let rel = relative_under_root(sync_root, path);
                if !is_old_versions_archive_path(Path::new(&rel)) || !guard.should_defer_archive(&rel) {
                    return None;
                }
                let live_rel = guard.live_path_for_archive(&rel)?.to_string();
                Some((rel, live_rel))
            })
            .collect::<Vec<_>>()
    };
    let prioritized_live_paths: HashSet<String> = deferred_archives
        .iter()
        .map(|(_, live_rel)| live_rel.clone())
        .collect();

    for path in changed_paths {
        if should_exclude(&path) {
            continue;
        }
        let rel = relative_under_root(sync_root, &path);
        let known_row = known_by_rel.get(&rel);

        if let Some(row) = known_row {
            if row.is_directory {
                continue;
            }

            if row.cloud_item_id.is_none() {
                if tether_cfapi::is_placeholder(&path) && !is_old_versions_archive_path(Path::new(&rel)) {
                    continue;
                }
                tracing::info!("Detected new local file for remote create: {}", rel);
                queue_or_journal(
                    sync_root,
                    db,
                    queue,
                    sync_root_id,
                    &rel,
                    SyncOperation::CreateRemoteFile,
                    None,
                    online,
                    &pending_keys,
                )
                .await?;
                continue;
            }

            let sync_pending = tether_cfapi::is_sync_pending(&path);
            if row.is_placeholder
                && row.hydration_state == "online_only"
                && !sync_pending
            {
                tracing::debug!(
                    "Ignoring placeholder watcher event for {} while it remains online-only",
                    rel
                );
                continue;
            }

            if let Some((_, live_rel)) = deferred_archives.iter().find(|(archive_rel, _)| archive_rel == &rel) {
                tracing::info!(
                    "Deferring archive sync for {} until live file {} settles",
                    rel,
                    live_rel
                );
                continue;
            }

            // Only probe filesystem metadata for files that are NOT online-only.
            // For online-only entries the DB mtime is authoritative — reading
            // std::fs::metadata() would trigger hydration via CreateFileW.
            let metadata_result = if row.hydration_state == "online_only" {
                Ok(None) // We don't probe online-only, so assume unchanged metadata
            } else {
                std::fs::metadata(&path).map(Some)
            };

            match metadata_result {
                Err(_) => {
                    // File no longer exists, enqueue a delete cloud
                    tracing::info!("Detected local file deletion for {}", rel);
                    queue_or_journal(
                        sync_root,
                        db,
                        queue,
                        sync_root_id,
                        &rel,
                        SyncOperation::DeleteCloud,
                        row.cloud_item_id.clone(),
                        online,
                        &pending_keys,
                    )
                    .await?;
                }
                Ok(fs_metadata) => {
                    let local_modified = if row.hydration_state == "online_only" {
                        row.last_local_modified.clone()
                    } else {
                        fs_metadata
                            .and_then(|m| m.modified().ok())
                            .map(system_time_to_rfc3339)
                    };
                    
                    let changed_since_last =
                        local_modified.as_deref() != row.last_local_modified.as_deref();
                    
                    if prioritized_live_paths.contains(&rel) || changed_since_last || sync_pending {
                        tracing::info!(
                            "Detected local change for {} (changed_since_last={}, sync_pending={})",
                            rel,
                            changed_since_last,
                            sync_pending
                        );
                        queue_or_journal(
                            sync_root,
                            db,
                            queue,
                            sync_root_id,
                            &rel,
                            SyncOperation::Upload,
                            row.cloud_item_id.clone(),
                            online,
                            &pending_keys,
                        )
                        .await?;
                    }
                }
            }
            continue;
        }

        // Use attribute-based check — no handle open, no hydration.
        if tether_cfapi::is_cloud_only_attr(&path) {
            tracing::debug!(
                "Ignoring watcher event for cloud-only placeholder {}",
                rel
            );
            continue;
        }
        // Use no-recall dir check — avoids opening a handle.
        if tether_cfapi::is_dir_no_recall(&path) {
            if rel.is_empty() {
                continue;
            }
            queue_or_journal(
                sync_root,
                db,
                queue,
                sync_root_id,
                &rel,
                SyncOperation::CreateRemoteFolder,
                None,
                online,
                &pending_keys,
            )
            .await?;
            continue;
        }
        // Use no-recall file check — avoids opening a handle.
        if !tether_cfapi::is_file_no_recall(&path) {
            continue;
        }
        if let Some((_, live_rel)) = deferred_archives.iter().find(|(archive_rel, _)| archive_rel == &rel) {
            tracing::info!(
                "Deferring archive sync for {} until live file {} settles",
                rel,
                live_rel
            );
            continue;
        }

        if tether_cfapi::is_placeholder(&path) && !is_old_versions_archive_path(Path::new(&rel)) {
            continue;
        }
        tracing::info!("Detected new local file for remote create: {}", rel);
        queue_or_journal(
            sync_root,
            db,
            queue,
            sync_root_id,
            &rel,
            SyncOperation::CreateRemoteFile,
            None,
            online,
            &pending_keys,
        )
        .await?;
    }

    Ok(())
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
    save_patterns: &Arc<Mutex<SavePatternCoalescer>>,
) -> Result<()> {
    let service_state = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let root = guard
            .get_sync_root(sync_root_id)?
            .ok_or_else(|| anyhow::anyhow!("Missing sync root {}", sync_root_id))?;
        ServiceState::from_db(&root.service_state)
    };

    let online = matches!(service_state, ServiceState::Running | ServiceState::Reconnecting);

    // --- DB-first approach: load entries and skip online-only files ---
    let db_rows = {
        let guard = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        guard.get_all_file_entries(sync_root_id)?
    };

    let known_by_rel: HashMap<String, FileEntryRow> = db_rows
        .into_iter()
        .map(|row| (row.local_relative_path.clone(), row))
        .collect();

    // Only walk the filesystem for files that might need action.
    // collect_paths now uses is_cloud_only_attr (GetFileAttributesW)
    // instead of is_cloud_only_placeholder (CfOpenFileWithOplock).
    let mut local_files = Vec::new();
    let mut local_dirs = Vec::new();
    collect_paths(sync_root, sync_root, &mut local_dirs, &mut local_files)?;
    let pending_keys = pending_journal_keys(db, sync_root_id)?;
    let deferred_archives = {
        let mut guard = save_patterns
            .lock()
            .map_err(|e| anyhow::anyhow!("save_patterns lock: {e}"))?;
        guard.clear_stale();
        local_files
            .iter()
            .filter_map(|rel| {
                if !is_old_versions_archive_path(Path::new(rel)) || !guard.should_defer_archive(rel) {
                    return None;
                }
                let live_rel = guard.live_path_for_archive(rel)?.to_string();
                Some((rel.clone(), live_rel))
            })
            .collect::<Vec<_>>()
    };
    let prioritized_live_paths: HashSet<String> = deferred_archives
        .iter()
        .map(|(_, live_rel)| live_rel.clone())
        .collect();

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
        if let Some((_, live_rel)) = deferred_archives
            .iter()
            .find(|(archive_rel, _)| archive_rel == file_rel)
        {
            tracing::info!(
                "Deferring archive sync for {} until live file {} settles",
                file_rel,
                live_rel
            );
            continue;
        }
        let full = sync_root.join(file_rel.replace('/', "\\"));
        // Use attribute-based check — no handle open, no hydration.
        if tether_cfapi::is_cloud_only_attr(&full) {
            continue;
        }
        // Skip online-only OldVersions entries proactively — these should
        // only sync if locally created by Inventor's save flow.
        if let Some(row) = known_by_rel.get(file_rel) {
            if row.hydration_state == "online_only"
                && is_old_versions_archive_path(Path::new(file_rel))
            {
                continue;
            }
        }
        match known_by_rel.get(file_rel) {
            None => {
                let full_path = sync_root.join(file_rel.replace('/', "\\"));
                if tether_cfapi::is_placeholder(&full_path) && !is_old_versions_archive_path(Path::new(file_rel)) {
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
                if prioritized_live_paths.contains(file_rel) {
                    tracing::info!(
                        "Prioritizing live file upload after archive move: {}",
                        file_rel
                    );
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
                    continue;
                }
                let sync_pending = tether_cfapi::is_sync_pending(&full);
                if sync_pending {
                    tracing::info!(
                        "Detected local change for {} (changed_since_last={}, sync_pending={})",
                        file_rel,
                        false,
                        sync_pending
                    );
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
    if matches!(operation, SyncOperation::Upload | SyncOperation::CreateRemoteFile)
        && tether_cfapi::is_cloud_only_attr(&local_path)
    {
        tracing::debug!(
            "Skipping {} for cloud-only placeholder {}",
            op_key,
            local_path.display()
        );
        return Ok(());
    }

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
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let rel = relative_under_root(root, &path);
        if file_type.is_dir() {
            dirs.push(rel.clone());
            collect_paths(root, &path, dirs, files)?;
        } else if file_type.is_file() {
            // Use attribute-based check — no handle open, no hydration.
            if tether_cfapi::is_cloud_only_attr(&path) {
                continue;
            }
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
