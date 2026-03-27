//! ApsCloudProvider — implements tether_cfapi::CloudProvider using real APS API clients.
//!
//! CFAPI callbacks run on arbitrary Windows threads (synchronous), but the APS
//! clients are async.  We hold a tokio `Handle` and `block_on` inside each method
//! to bridge the gap.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use tether_cfapi::{CloudFileInfo, CloudProvider};
use tokio::runtime::Handle;

use crate::api::auth::ApsAuthClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::api::storage::ApsStorageClient;

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
}

impl ApsCloudProvider {
    pub fn new(
        runtime: Handle,
        auth: ApsAuthClient,
        data_mgmt: ApsDataManagementClient,
        storage: ApsStorageClient,
        project_id: String,
        root_folder_id: String,
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

        Ok(items
            .into_iter()
            .map(|item| {
                let is_directory = item.item_type == "folders";
                CloudFileInfo {
                    name: item.attributes.display_name,
                    is_directory,
                    size: if is_directory {
                        0
                    } else {
                        item.attributes.storage_size.unwrap_or(0)
                    },
                    cloud_id: item.id,
                    last_modified: item.attributes.last_modified_time,
                    created: item.attributes.create_time,
                }
            })
            .collect())
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
}
