//! ApsCloudProvider — implements tether_cfapi::CloudProvider using real APS API clients.
//!
//! CFAPI callbacks run on arbitrary Windows threads (synchronous), but the APS
//! clients are async.  We hold a tokio `Handle` and `block_on` inside each method
//! to bridge the gap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;

use anyhow::{Context, Result};
use tether_cfapi::{CloudFileInfo, CloudProvider};
use tokio::runtime::Handle;

use crate::api::auth::ApsAuthClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::api::storage::ApsStorageClient;
use crate::db::database::SyncDatabase;

fn normalize_rel(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Concrete CloudProvider backed by APS REST APIs.
pub struct ApsCloudProvider {
    runtime: Handle,
    auth: ApsAuthClient,
    data_mgmt: ApsDataManagementClient,
    storage: ApsStorageClient,
    project_id: String,
    /// Maps local relative paths (from sync root) → cloud folder IDs.
    /// The root folder ("") is inserted at construction time.
    folder_map: Mutex<HashMap<PathBuf, String>>,
    state_db: Option<Arc<Mutex<SyncDatabase>>>,
    sync_root_id: Option<String>,
    upload_tx: UnboundedSender<(PathBuf, String)>,
}

impl ApsCloudProvider {
    pub fn new(
        runtime: Handle,
        auth: ApsAuthClient,
        data_mgmt: ApsDataManagementClient,
        storage: ApsStorageClient,
        project_id: String,
        root_folder_id: String,
        state_db: Option<Arc<Mutex<SyncDatabase>>>,
        sync_root_id: Option<String>,
        upload_tx: UnboundedSender<(PathBuf, String)>,
    ) -> Self {
        let mut map = HashMap::new();
        // Empty path = the sync root itself
        map.insert(PathBuf::new(), root_folder_id);

        Self {
            runtime,
            auth,
            data_mgmt,
            storage,
            project_id,
            folder_map: Mutex::new(map),
            state_db,
            sync_root_id,
            upload_tx,
        }
    }
}

impl CloudProvider for ApsCloudProvider {
    fn list_folder_contents(&self, cloud_folder_id: &str) -> Result<Vec<CloudFileInfo>> {
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder listing")?;

        let items = self.runtime.block_on(
            self.data_mgmt
                .get_folder_contents(&token, &self.project_id, cloud_folder_id),
        )?;

        let mut out = Vec::with_capacity(items.len());
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
                        .get_item_versions(&token, &self.project_id, &item.id),
                ) {
                    if let Some(v) = versions.first() {
                        size = v.attributes.storage_size.unwrap_or(0);
                    }
                }
            }
            out.push(CloudFileInfo {
                name: item.attributes.display_name,
                is_directory,
                size,
                cloud_id: item.id,
                last_modified: item.attributes.last_modified_time,
                created: item.attributes.create_time,
            });
        }
        Ok(out)
    }

    fn download_file_content(&self, cloud_item_id: &str) -> Result<Vec<u8>> {
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for download")?;

        // 1. Get item versions → extract storage URN from the latest version
        let versions = self.runtime.block_on(
            self.data_mgmt
                .get_item_versions(&token, &self.project_id, cloud_item_id),
        )?;

        let active_version = versions
            .first()
            .context("No versions found for item")?;

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
        let map = self.folder_map.lock().unwrap();
        Ok(map.get(relative_path).cloned())
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
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for delete")?;
        self.runtime.block_on(
            self.data_mgmt.delete_item_as_deleted_version(
                &token,
                &self.project_id,
                cloud_item_id,
                item_display_name,
            ),
        )?;
        Ok(())
    }

    fn delete_cloud_folder_recursive(&self, cloud_folder_id: &str) -> Result<()> {
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder delete")?;
        let project_id = self.project_id.clone();
        let dm = self.data_mgmt.clone();
        self.runtime
            .block_on(delete_folder_recursive(&dm, &token, &project_id, cloud_folder_id))
    }

    fn rename_cloud_item(&self, cloud_item_id: &str, new_name: &str) -> Result<()> {
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for rename")?;
        let versions = self.runtime.block_on(
            self.data_mgmt
                .get_item_versions(&token, &self.project_id, cloud_item_id),
        )?;
        let vid = versions
            .first()
            .map(|v| v.id.as_str())
            .context("No versions for item rename")?;
        self.runtime.block_on(self.data_mgmt.patch_version_name(
            &token,
            &self.project_id,
            vid,
            new_name,
        ))?;
        Ok(())
    }

    fn rename_cloud_folder(&self, cloud_folder_id: &str, new_name: &str) -> Result<()> {
        let token = self
            .auth
            .get_access_token()
            .context("Failed to get access token for folder rename")?;
        self.runtime.block_on(self.data_mgmt.patch_folder_display_name(
            &token,
            &self.project_id,
            cloud_folder_id,
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

    fn queue_upload_if_dirty(&self, local_full_path: PathBuf, cloud_item_id: &str) -> Result<()> {
        self.upload_tx
            .send((local_full_path, cloud_item_id.to_string()))
            .map_err(|e| anyhow::anyhow!("upload channel closed: {e}"))
    }

    fn on_hydration_complete(&self, cloud_item_id: &str, relative_path: &Path) -> Result<()> {
        let (db, root_id) = match (&self.state_db, &self.sync_root_id) {
            (Some(db), Some(rid)) => (db, rid.as_str()),
            _ => return Ok(()),
        };
        let rel = normalize_rel(relative_path);
        let db = db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
        db.update_hydration_state(
            root_id,
            &rel,
            "hydrated_ephemeral",
            false,
            Some("opened"),
        )?;
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
