//! Periodic cloud change polling — compare folder contents with local state (recursive).

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::api::data_management::ApsDataManagementClient;
use crate::db::database::SyncDatabase;

/// Max subfolder depth from the synced root (prevents runaway traversal).
const MAX_FOLDER_DEPTH: usize = 64;

/// A detected cloud change.
#[derive(Debug)]
pub enum CloudChange {
    /// A cloud file is newer than the local version — dehydrate so next open gets fresh bytes.
    Updated {
        cloud_item_id: String,
        /// Path relative to sync root, `/` separators (e.g. `Rocket Stand/foo.ipt`).
        local_relative_path: String,
        file_size: u64,
    },
    /// A new file appeared in the cloud — create a placeholder (no download).
    Added {
        cloud_item_id: String,
        local_relative_path: String,
        file_size: u64,
    },
    /// A file was removed from the cloud.
    Removed {
        cloud_item_id: String,
        local_relative_path: String,
    },
}

fn join_rel(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", prefix, name)
    }
}

/// Resolve `rel` (`/` separators) under `sync_root` for existence checks (Windows-safe).
fn resolve_sync_path(sync_root: &Path, rel: &str) -> PathBuf {
    let mut p = sync_root.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    p
}

fn local_file_already_present(sync_root: &Path, rel_path: &str) -> bool {
    resolve_sync_path(sync_root, rel_path).exists()
}

/// Poll cloud folders for changes at a regular interval (recursive).
pub async fn start_polling(
    interval_secs: u64,
    api: ApsDataManagementClient,
    db: std::sync::Arc<std::sync::Mutex<SyncDatabase>>,
    token_getter: impl Fn() -> Result<String> + Send + 'static,
    project_id: String,
    root_folder_id: String,
    sync_root_id: String,
    sync_root_path: PathBuf,
    tx: mpsc::Sender<CloudChange>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;
        debug!("Polling cloud for changes (recursive)...");

        let token = match token_getter() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to get token for polling: {e}");
                continue;
            }
        };

        if let Err(e) = poll_once(
            &api,
            &token,
            &project_id,
            &root_folder_id,
            &db,
            &sync_root_id,
            &sync_root_path,
            &tx,
        )
        .await
        {
            warn!("Cloud polling failed: {e}");
        }
    }
}

async fn poll_once(
    api: &ApsDataManagementClient,
    token: &str,
    project_id: &str,
    root_folder_id: &str,
    db: &std::sync::Arc<std::sync::Mutex<SyncDatabase>>,
    sync_root_id: &str,
    sync_root_path: &Path,
    tx: &mpsc::Sender<CloudChange>,
) -> Result<()> {
    let local_entries = {
        let db = db.lock().unwrap();
        db.get_all_file_entries(sync_root_id)?
    };

    let mut stack: VecDeque<(String, String, usize)> = VecDeque::new();
    stack.push_back((root_folder_id.to_string(), String::new(), 0));

    let mut seen_cloud_item_ids: HashSet<String> = HashSet::new();

    while let Some((folder_id, rel_prefix, depth)) = stack.pop_front() {
        if depth > MAX_FOLDER_DEPTH {
            warn!(
                "Skipping folder at depth {} (max {}) under {:?}",
                depth, MAX_FOLDER_DEPTH, rel_prefix
            );
            continue;
        }

        let cloud_items = api
            .get_folder_contents(token, project_id, &folder_id)
            .await?;

        for item in &cloud_items {
            let name = &item.attributes.display_name;
            let rel_path = join_rel(&rel_prefix, name);

            if item.item_type == "folders" {
                stack.push_back((item.id.clone(), rel_path, depth + 1));
                continue;
            }

            if item.item_type != "items" {
                continue;
            }

            seen_cloud_item_ids.insert(item.id.clone());

            let existing = local_entries.iter().find(|e| {
                e.cloud_item_id.as_deref() == Some(item.id.as_str())
            });

            match existing {
                Some(entry) => {
                    if entry.hydration_state == "online_only" && entry.pin_state == 0 {
                        continue;
                    }
                    if let Some(ref cloud_time) = item.attributes.last_modified_time {
                        if entry.last_cloud_modified.as_deref() != Some(cloud_time.as_str()) {
                            send_change(
                                tx,
                                CloudChange::Updated {
                                    cloud_item_id: item.id.clone(),
                                    local_relative_path: rel_path.clone(),
                                    file_size: item.attributes.storage_size.unwrap_or(0),
                                },
                            )
                            .await;
                        }
                    }
                }
                None => {
                    // Placeholders often exist on disk before `file_entries` is populated; do not
                    // treat every recursive cloud item as a missing file (queue explosion).
                    if local_file_already_present(sync_root_path, &rel_path) {
                        debug!(
                            "Skipping Added for {:?}: already present on disk (no DB row yet)",
                            rel_path
                        );
                        continue;
                    }
                    send_change(
                        tx,
                        CloudChange::Added {
                            cloud_item_id: item.id.clone(),
                            local_relative_path: rel_path.clone(),
                            file_size: item.attributes.storage_size.unwrap_or(0),
                        },
                    )
                    .await;
                }
            }
        }
    }

    for entry in &local_entries {
        if let Some(ref cloud_id) = entry.cloud_item_id {
            if !seen_cloud_item_ids.contains(cloud_id) {
                send_change(
                    tx,
                    CloudChange::Removed {
                        cloud_item_id: cloud_id.clone(),
                        local_relative_path: entry.local_relative_path.clone(),
                    },
                )
                .await;
            }
        }
    }

    Ok(())
}

async fn send_change(tx: &mpsc::Sender<CloudChange>, change: CloudChange) {
    if let Err(e) = tx.send(change).await {
        warn!("Cloud change channel closed (stopping poller consumer path): {e}");
    }
}
