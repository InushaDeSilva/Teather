//! CloudProvider trait — abstracts cloud API calls so tether-cfapi doesn't depend on tether-core.
//!
//! tether-core implements this trait with real APS API clients;
//! tether-cfapi consumes it inside SyncFilter callbacks.

use std::path::{Path, PathBuf};

/// Metadata for a single cloud file or folder returned by directory listing.
#[derive(Debug, Clone)]
pub struct CloudFileInfo {
    /// Display name (e.g. "Assembly.iam")
    pub name: String,
    /// True if this entry is a folder, false if it's a file
    pub is_directory: bool,
    /// File size in bytes (0 for directories)
    pub size: u64,
    /// Cloud item/folder ID (stored as blob in the placeholder for later callbacks)
    pub cloud_id: String,
    /// ISO-8601 last modified time, if available
    pub last_modified: Option<String>,
    /// ISO-8601 creation time, if available
    pub created: Option<String>,
}

/// Trait that the CFAPI filter uses to talk to the cloud without knowing about
/// APS clients, tokio, or any tether-core internals.
///
/// Implementations must be `Send + Sync` because CFAPI callbacks fire on
/// arbitrary OS threads.
pub trait CloudProvider: Send + Sync {
    /// List the contents of a cloud folder.
    fn list_folder_contents(&self, cloud_folder_id: &str) -> anyhow::Result<Vec<CloudFileInfo>>;

    /// Download the latest version of a file, returning raw bytes.
    fn download_file_content(&self, cloud_item_id: &str) -> anyhow::Result<Vec<u8>>;

    /// Resolve a local relative path (from the sync root) to a cloud folder ID.
    /// Returns `None` if no mapping is known (e.g. the folder hasn't been visited yet).
    fn resolve_folder_id(&self, relative_path: &Path) -> anyhow::Result<Option<String>>;

    /// Register a mapping from a local relative path to a cloud folder ID.
    /// Called when `fetch_placeholders` discovers sub-folders so future navigations
    /// can resolve them.
    fn register_folder_mapping(
        &self,
        relative_path: &Path,
        cloud_folder_id: &str,
    ) -> anyhow::Result<()>;

    /// Delete a cloud file (item) — posts a Deleted version.
    fn delete_cloud_item(&self, cloud_item_id: &str, item_display_name: &str) -> anyhow::Result<()>;

    /// Recursively delete all files (and nested folders) under a cloud folder.
    fn delete_cloud_folder_recursive(&self, cloud_folder_id: &str) -> anyhow::Result<()>;

    /// Rename a file item (latest version name).
    fn rename_cloud_item(&self, cloud_item_id: &str, new_name: &str) -> anyhow::Result<()>;

    /// Rename a folder (display name).
    fn rename_cloud_folder(&self, cloud_folder_id: &str, new_name: &str) -> anyhow::Result<()>;

    /// Update local relative path → folder id map after Explorer renames a folder.
    fn rename_folder_mapping(
        &self,
        old_relative: &Path,
        new_relative: &Path,
    ) -> anyhow::Result<()>;

    /// After a placeholder is fully hydrated in [`crate::filter::TetherSyncFilter::fetch_data`].
    fn on_hydration_complete(
        &self,
        _cloud_item_id: &str,
        _relative_path: &Path,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called when a file handle closes and the placeholder is not in sync (local edits).
    /// Default: no-op (tests / stubs).
    fn queue_upload_if_dirty(
        &self,
        _local_full_path: PathBuf,
        _cloud_item_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Fallback for files whose placeholder blob is empty (e.g. downloaded by the old
    /// worker code which used `fs::write` instead of CFAPI hydration).
    /// Returns the cloud item ID if one can be found for the given relative path.
    fn resolve_cloud_item_id_by_path(
        &self,
        _relative_path: &Path,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// Stage a delete confirmation prompt instead of deleting immediately.
    fn queue_delete_prompt(
        &self,
        _relative_path: &Path,
        _cloud_id: &str,
        _is_directory: bool,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
