//! ApsCloudProvider — implements tether_cfapi::CloudProvider using real APS API clients.
//!
//! CFAPI callbacks run on arbitrary Windows threads (synchronous), but the APS
//! clients are async.  We hold a tokio `Handle` and `block_on` inside each method
//! to bridge the gap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc::UnboundedSender;

use anyhow::{Context, Result};
use crate::api::models::latest_version;
use tether_cfapi::{CloudFileInfo, CloudProvider};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::api::auth::ApsAuthClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::api::storage::ApsStorageClient;
use crate::db::database::{FileEntryRow, SyncDatabase};
use crate::sync::parity::{prompt_payload_json, PromptKind, PromptPayload};
use crate::sync::save_patterns::SavePatternCoalescer;

fn normalize_rel(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Concrete CloudProvider backed by APS REST APIs.
pub struct ApsCloudProvider {
    runtime: Handle,
    auth: ApsAuthClient,
    data_mgmt: ApsDataManagementClient,
    storage: ApsStorageClient,
    synced_folders: Vec<crate::config::settings::SyncedFolderConfig>,
    /// Maps local relative paths (from sync root) → cloud folder IDs.
    /// The root folder ("") is inserted at construction time.
    folder_map: Mutex<HashMap<PathBuf, String>>,
    state_db: Option<Arc<Mutex<SyncDatabase>>>,
    sync_root_id: Option<String>,
    upload_tx: UnboundedSender<(PathBuf, String)>,
    save_patterns: Arc<Mutex<SavePatternCoalescer>>,
    /// Collapse duplicate `closed` callbacks for the same path (same tick).
    upload_dedup: Mutex<HashMap<PathBuf, Instant>>,
}

impl ApsCloudProvider {
    pub fn new(
        runtime: Handle,
        auth: ApsAuthClient,
        data_mgmt: ApsDataManagementClient,
        storage: ApsStorageClient,
        synced_folders: Vec<crate::config::settings::SyncedFolderConfig>,
        state_db: Option<Arc<Mutex<SyncDatabase>>>,
        sync_root_id: Option<String>,
        upload_tx: UnboundedSender<(PathBuf, String)>,
        save_patterns: Arc<Mutex<SavePatternCoalescer>>,
    ) -> Self {
        let mut map = HashMap::new();
        // Empty path = the sync root itself
        map.insert(PathBuf::new(), "".to_string());
        for folder in &synced_folders {
            if folder.enabled {
                let cloud_folder_id = format!("{}|{}", folder.project_id, folder.folder_id);
                map.insert(PathBuf::from(&folder.display_name), cloud_folder_id);
            }
        }

        Self {
            runtime,
            auth,
            data_mgmt,
            storage,
            synced_folders,
            folder_map: Mutex::new(map),
            state_db,
            sync_root_id,
            upload_tx,
            save_patterns,
            upload_dedup: Mutex::new(HashMap::new()),
        }
    }
}

impl CloudProvider for ApsCloudProvider {
    fn list_folder_contents(&self, cloud_folder_id: &str) -> Result<Vec<CloudFileInfo>> {
        if cloud_folder_id.is_empty() {
            let mut out = Vec::new();
            for folder in &self.synced_folders {
                if folder.enabled {
                    out.push(CloudFileInfo {
                        name: folder.display_name.clone(),
                        is_directory: true,
                        size: 0,
                        cloud_id: format!("{}|{}", folder.project_id, folder.folder_id),
                        last_modified: None,
                        created: None,
                    });
                }
            }
            // Persist synced folder root mappings to the DB so that
            // ensure_remote_folder can resolve the first path segment (e.g.
            // "ProjectA") to its real project_id|folder_id without falling back
            // to the APS API with the dummy "unified" identifiers.
            if let (Some(db), Some(sync_root_id)) = (&self.state_db, &self.sync_root_id) {
                if let Ok(db) = db.lock() {
                    for folder in &self.synced_folders {
                        if !folder.enabled {
                            continue;
                        }
                        let rel = folder.display_name.clone();
                        let cloud_id = format!("{}|{}", folder.project_id, folder.folder_id);
                        let existing = db.get_file_entry_by_path(sync_root_id, &rel).ok().flatten();
                        let mut row = existing.unwrap_or_else(|| {
                            let mut r = FileEntryRow::default();
                            r.id = Uuid::new_v4().to_string();
                            r
                        });
                        row.sync_root_id = sync_root_id.clone();
                        row.local_relative_path = rel;
                        row.cloud_item_id = Some(cloud_id);
                        row.is_directory = true;
                        row.is_placeholder = true;
                        row.sync_state = "in_sync".into();
                        row.hydration_state = "directory".into();
                        let _ = db.upsert_file_entry(&row);
                    }
                }
            }
            return Ok(out);
        }

        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder listing")?;

        let (project_id, real_folder_id) = cloud_folder_id.split_once('|').unwrap_or(("", cloud_folder_id));

        let items = self.runtime.block_on(
            self.data_mgmt
                .get_folder_contents(&token, project_id, real_folder_id),
        )?;

        let mut out = Vec::with_capacity(items.len());
        let mut persisted_rows: Vec<FileEntryRow> = Vec::new();
        let rel_prefix = self
            .folder_map
            .lock()
            .unwrap()
            .iter()
            .find_map(|(rel, id)| {
                if id == cloud_folder_id {
                    Some(rel.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        for item in items {
            let is_directory = item.item_type == "folders";
            let mut size = if is_directory {
                0
            } else {
                item.attributes.storage_size.unwrap_or(0)
            };
            if !is_directory && size == 0 {
                if let Ok(versions) = self.runtime.block_on(
                    self.data_mgmt
                        .get_item_versions(&token, project_id, &item.id),
                ) {
                    if let Some(v) = latest_version(&versions) {
                        size = v.attributes.storage_size.unwrap_or(0);
                    }
                }
            }
            out.push(CloudFileInfo {
                name: item.attributes.display_name.clone(),
                is_directory,
                size,
                cloud_id: format!("{}|{}", project_id, item.id),
                last_modified: item.attributes.last_modified_time.clone(),
                created: item.attributes.create_time.clone(),
            });

            if let (Some(db), Some(sync_root_id)) = (&self.state_db, &self.sync_root_id) {
                let rel_path = if rel_prefix.as_os_str().is_empty() {
                    PathBuf::from(&item.attributes.display_name)
                } else {
                    rel_prefix.join(&item.attributes.display_name)
                };
                let rel = normalize_rel(&rel_path);
                let mut row = {
                    let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
                    db.get_file_entry_by_path(sync_root_id, &rel)?
                        .or_else(|| db.get_file_entry_by_cloud_item(sync_root_id, &item.id).ok().flatten())
                        .unwrap_or_else(|| {
                            let mut r = FileEntryRow::default();
                            r.id = Uuid::new_v4().to_string();
                            r
                        })
                };
                row.sync_root_id = sync_root_id.clone();
                row.local_relative_path = rel;
                let full_item_id = format!("{}|{}", project_id, item.id);
                row.cloud_item_id = Some(full_item_id);
                row.file_size = Some(size as i64);
                row.last_cloud_modified = item.attributes.last_modified_time.clone();
                row.sync_state = "in_sync".into();
                row.is_placeholder = true;
                row.is_directory = is_directory;
                row.hydration_state = if is_directory {
                    "directory".into()
                } else {
                    "online_only".into()
                };
                persisted_rows.push(row);
            }
        }

        if let (Some(db), Some(_)) = (&self.state_db, &self.sync_root_id) {
            let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            for row in &persisted_rows {
                db.upsert_file_entry(row)?;
            }
        }
        Ok(out)
    }

    fn download_file_content(&self, cloud_item_id: &str) -> Result<Vec<u8>> {
        let (project_id, real_item_id) = cloud_item_id.split_once('|').unwrap_or(("", cloud_item_id));
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for download")?;

        // 1. Get item versions → extract storage URN from the latest version
        let versions = self.runtime.block_on(
            self.data_mgmt
                .get_item_versions(&token, project_id, real_item_id),
        )?;

        let active_version = latest_version(&versions).context("No versions found for item")?;

        let storage_urn = active_version
            .relationships
            .as_ref()
            .and_then(|r| r.storage.as_ref())
            .and_then(|s| s.data.as_ref())
            .map(|d| d.id.clone())
            .context("Item version is missing storage URN")?;

        // 2. Parse URN → bucket_key / object_key
        let prefix = "urn:adsk.objects:os.object:";
        let rest = storage_urn
            .strip_prefix(prefix)
            .context("Invalid storage URN format")?;
        let (bucket_key, object_key) = rest
            .split_once('/')
            .context("Invalid storage URN: no '/' separator")?;

        // 3. Download via S3 signed URL → bytes
        let bytes = self.runtime.block_on(
            self.storage
                .download_to_bytes(&token, bucket_key, object_key),
        )?;

        Ok(bytes)
    }

    fn resolve_folder_id(&self, relative_path: &Path) -> Result<Option<String>> {
        {
            let map = self.folder_map.lock().unwrap();
            if let Some(id) = map.get(relative_path).cloned() {
                return Ok(Some(id));
            }
        }
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(None),
        };
        let rel = normalize_rel(relative_path);
        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let entry = db.get_file_entry_by_path(root_id, &rel)?;
        Ok(entry.and_then(|row| if row.is_directory { row.cloud_item_id } else { None }))
    }

    fn register_folder_mapping(
        &self,
        relative_path: &Path,
        cloud_folder_id: &str,
    ) -> Result<()> {
        let mut map = self.folder_map.lock().unwrap();
        map.insert(relative_path.to_path_buf(), cloud_folder_id.to_string());
        tracing::debug!(
            "Registered folder mapping: {:?} → {}",
            relative_path,
            cloud_folder_id
        );
        Ok(())
    }

    fn delete_cloud_item(&self, cloud_item_id: &str, item_display_name: &str) -> Result<()> {
        let (project_id, real_item_id) = cloud_item_id.split_once('|').unwrap_or(("", cloud_item_id));
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for delete")?;
        self.runtime.block_on(
            self.data_mgmt.delete_item_as_deleted_version(
                &token,
                project_id,
                real_item_id,
                item_display_name,
            ),
        )?;
        Ok(())
    }

    fn delete_cloud_folder_recursive(&self, cloud_folder_id: &str) -> Result<()> {
        let (project_id, real_folder_id) = cloud_folder_id.split_once('|').unwrap_or(("", cloud_folder_id));
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder delete")?;
        let dm = self.data_mgmt.clone();
        self.runtime
            .block_on(delete_folder_recursive(&dm, &token, project_id, real_folder_id))
    }

    fn rename_cloud_item(&self, cloud_item_id: &str, new_name: &str) -> Result<()> {
        let (project_id, real_item_id) = cloud_item_id.split_once('|').unwrap_or(("", cloud_item_id));
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for rename")?;
        let versions = self.runtime.block_on(
            self.data_mgmt
                .get_item_versions(&token, project_id, real_item_id),
        )?;
        let vid = latest_version(&versions)
            .map(|v| v.id.as_str())
            .context("No versions for item rename")?;
        self.runtime.block_on(self.data_mgmt.patch_version_name(
            &token,
            project_id,
            vid,
            new_name,
        ))?;
        Ok(())
    }

    fn rename_cloud_folder(&self, cloud_folder_id: &str, new_name: &str) -> Result<()> {
        let (project_id, real_folder_id) = cloud_folder_id.split_once('|').unwrap_or(("", cloud_folder_id));
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder rename")?;
        self.runtime.block_on(self.data_mgmt.patch_folder_display_name(
            &token,
            project_id,
            real_folder_id,
            new_name,
        ))?;
        Ok(())
    }

    fn rename_folder_mapping(&self, old_relative: &Path, new_relative: &Path) -> Result<()> {
        let mut map = self.folder_map.lock().unwrap();
        if let Some(id) = map.remove(&old_relative.to_path_buf()) {
            map.insert(new_relative.to_path_buf(), id);
            tracing::debug!(
                "Renamed folder mapping {:?} → {:?}",
                old_relative,
                new_relative
            );
        }
        Ok(())
    }

    fn rename_file_mapping(
        &self,
        old_relative: &Path,
        new_relative: &Path,
        cloud_item_id: &str,
    ) -> Result<()> {
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(()),
        };
        let old_rel = normalize_rel(old_relative);
        let new_rel = normalize_rel(new_relative);
        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;

        let mapped = db.get_file_entry_by_path(root_id, &old_rel)?;
        let fallback = if mapped.is_none() {
            db.get_file_entry_by_cloud_item(root_id, cloud_item_id)?
        } else {
            None
        };

        let candidate = mapped.or(fallback);
        if let Some(row) = candidate {
            if row.cloud_item_id.as_deref() == Some(cloud_item_id) {
                db.move_file_entry(root_id, &row.local_relative_path, &new_rel)?;
                tracing::info!(
                    "Updated file mapping after rename: {} -> {} ({})",
                    row.local_relative_path,
                    new_rel,
                    cloud_item_id
                );
            }
        }

        Ok(())
    }

    fn note_archive_move(&self, old_relative: &Path, new_relative: &Path) -> Result<()> {
        let old_rel = normalize_rel(old_relative);
        let new_rel = normalize_rel(new_relative);
        let mut save_patterns = self
            .save_patterns
            .lock()
            .map_err(|e| anyhow::anyhow!("save_patterns lock: {e}"))?;
        save_patterns.note_archive_move(old_rel.clone(), new_rel.clone());
        tracing::info!(
            "Recorded archive move for save coalescing: {} -> {}",
            old_rel,
            new_rel
        );
        Ok(())
    }

    fn queue_upload_if_dirty(&self, local_full_path: PathBuf, cloud_item_id: &str) -> Result<()> {
        const DEBOUNCE: Duration = Duration::from_millis(400);
        let now = Instant::now();
        let mut map = self.upload_dedup.lock().unwrap();
        map.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
        if let Some(prev) = map.get(&local_full_path) {
            if now.duration_since(*prev) < DEBOUNCE {
                tracing::debug!(
                    "queue_upload_if_dirty: debounce {:?}",
                    local_full_path
                );
                return Ok(());
            }
        }
        map.insert(local_full_path.clone(), now);
        drop(map);
        self.upload_tx
            .send((local_full_path, cloud_item_id.to_string()))
            .map_err(|e| anyhow::anyhow!("upload channel closed: {e}"))
    }

    fn resolve_cloud_item_id_by_path(&self, relative_path: &Path) -> Result<Option<String>> {
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(None),
        };
        let rel = normalize_rel(relative_path);
        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let entry = db.get_file_entry_by_path(root_id, &rel)?;
        Ok(entry.and_then(|e| e.cloud_item_id))
    }

    fn on_hydration_complete(&self, cloud_item_id: &str, relative_path: &Path) -> Result<()> {
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(()),
        };
        let rel = normalize_rel(relative_path);
        let full_path = {
            let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            let root = db
                .get_sync_root(root_id)?
                .ok_or_else(|| anyhow::anyhow!("Missing sync root {}", root_id))?;
            PathBuf::from(root.local_path).join(relative_path)
        };
        let observed = std::fs::metadata(&full_path).ok().map(|meta| {
            (
                meta.modified().ok().map(system_time_to_rfc3339),
                meta.len() as i64,
            )
        });

        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        if let Some((modified, size)) = observed {
            // Hydration writes local bytes and updates the file mtime. Persist that observed
            // state immediately so the local watcher does not mistake a read/open hydrate for a
            // user edit and queue an upload.
            db.update_local_observed_state(
                root_id,
                &rel,
                modified.as_deref(),
                Some(size),
                "hydrated_ephemeral",
                false,
                "in_sync",
                Some("opened"),
            )?;
        } else {
            db.update_hydration_state(
                root_id,
                &rel,
                "hydrated_ephemeral",
                false,
                Some("opened"),
            )?;
        }
        db.log_activity(
            "hydrate",
            Some(&rel),
            Some(cloud_item_id),
            "success",
            None,
            None,
        )?;
        Ok(())
    }

    fn queue_delete_prompt(
        &self,
        relative_path: &Path,
        cloud_id: &str,
        is_directory: bool,
    ) -> Result<()> {
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(()),
        };
        let rel = normalize_rel(relative_path);
        let payload = PromptPayload {
            kind: PromptKind::DeleteConfirm,
            relative_path: rel.clone(),
            cloud_item_id: Some(cloud_id.to_string()),
            remote_head_version_id: None,
            message: format!(
                "Delete requested for {rel}. Choose whether to remove only the local copy or both local and cloud."
            ),
            is_directory,
        };
        let payload_json = prompt_payload_json(&payload)?;
        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        let pending_id = db.insert_pending_job(root_id, "prompt", Some(&payload_json), None)?;
        db.update_pending_job(&pending_id, "action_required", None, None)?;
        db.log_activity(
            "delete_prompt",
            Some(&rel),
            Some(cloud_id),
            "queued",
            Some("delete confirmation required"),
            None,
        )?;
        Ok(())
    }
}

fn system_time_to_rfc3339(ts: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Utc> = ts.into();
    dt.to_rfc3339()
}

/// Recursively delete folder contents (files as Deleted versions; nested folders first).
async fn delete_folder_recursive(
    data_mgmt: &ApsDataManagementClient,
    token: &str,
    project_id: &str,
    folder_id: &str,
) -> Result<()> {
    let contents = data_mgmt
        .get_folder_contents(token, project_id, folder_id)
        .await?;
    for item in contents {
        if item.item_type == "folders" {
            Box::pin(delete_folder_recursive(data_mgmt, token, project_id, &item.id)).await?;
        } else if item.item_type == "items" {
            data_mgmt
                .delete_item_as_deleted_version(
                    token,
                    project_id,
                    &item.id,
                    &item.attributes.display_name,
                )
                .await?;
        }
    }
    Ok(())
}
