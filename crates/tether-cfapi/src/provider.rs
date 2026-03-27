//! CloudProvider trait — abstracts cloud API calls so tether-cfapi doesn't depend on tether-core.
//!
//! tether-core implements this trait with real APS API clients;
//! tether-cfapi consumes it inside SyncFilter callbacks.

use std::path::Path;

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
}
