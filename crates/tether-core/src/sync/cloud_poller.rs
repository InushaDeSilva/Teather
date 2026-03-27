//! Periodic cloud change polling — compare folder contents with local state.

use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::api::data_management::ApsDataManagementClient;
use crate::db::database::SyncDatabase;

/// A detected cloud change.
#[derive(Debug)]
pub enum CloudChange {
    /// A cloud file is newer than the local version.
    Updated {
        cloud_item_id: String,
        display_name: String,
    },
    /// A new file appeared in the cloud that we don't have locally.
    Added {
        cloud_item_id: String,
        display_name: String,
    },
    /// A file was removed from the cloud.
    Removed {
        cloud_item_id: String,
        local_relative_path: String,
    },
}

/// Poll cloud folders for changes at a regular interval.
pub async fn start_polling(
    interval_secs: u64,
    api: ApsDataManagementClient,
    db: std::sync::Arc<std::sync::Mutex<SyncDatabase>>,
    token_getter: impl Fn() -> Result<String> + Send + 'static,
    project_id: String,
    folder_id: String,
    sync_root_id: String,
    tx: mpsc::Sender<CloudChange>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

    loop {
        interval.tick().await;
        debug!("Polling cloud for changes...");

        let token = match token_getter() {
            Ok(t) => t,
            Err(e) => {
                warn!("Failed to get token for polling: {e}");
                continue;
            }
        };

        match api.get_folder_contents(&token, &project_id, &folder_id).await {
            Ok(cloud_items) => {
                let db = db.lock().unwrap();
                let local_entries = match db.get_all_file_entries(&sync_root_id) {
                    Ok(entries) => entries,
                    Err(e) => {
                        warn!("Failed to read local entries: {e}");
                        continue;
                    }
                };

                // Check for new/updated cloud items
                for item in &cloud_items {
                    let display_name = &item.attributes.display_name;
                    let existing = local_entries.iter().find(|e| {
                        e.cloud_item_id.as_deref() == Some(&item.id)
                    });

                    match existing {
                        Some(entry) => {
                            // Compare timestamps
                            if let Some(ref cloud_time) = item.attributes.last_modified_time {
                                if entry.last_cloud_modified.as_deref() != Some(cloud_time.as_str()) {
                                    let _ = tx.try_send(CloudChange::Updated {
                                        cloud_item_id: item.id.clone(),
                                        display_name: display_name.clone(),
                                    });
                                }
                            }
                        }
                        None => {
                            let _ = tx.try_send(CloudChange::Added {
                                cloud_item_id: item.id.clone(),
                                display_name: display_name.clone(),
                            });
                        }
                    }
                }

                // Check for removed cloud items
                for entry in &local_entries {
                    if let Some(ref cloud_id) = entry.cloud_item_id {
                        let still_exists = cloud_items.iter().any(|i| &i.id == cloud_id);
                        if !still_exists {
                            let _ = tx.try_send(CloudChange::Removed {
                                cloud_item_id: cloud_id.clone(),
                                local_relative_path: entry.local_relative_path.clone(),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Cloud polling failed: {e}");
            }
        }
    }
}
