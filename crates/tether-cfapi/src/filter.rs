//! CFAPI SyncFilter implementation — handles Windows Cloud Files callbacks.

use cloud_filter::error::CloudErrorKind;
use cloud_filter::filter::{info, ticket, Request, SyncFilter};
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder_file::PlaceholderFile;
use cloud_filter::utility::WriteAt;
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::CloudProvider;

pub struct TetherSyncFilter {
    pub root_path: PathBuf,
    pub provider: Arc<dyn CloudProvider>,
}

impl TetherSyncFilter {
    pub fn new(root_path: PathBuf, provider: Arc<dyn CloudProvider>) -> Self {
        Self {
            root_path,
            provider,
        }
    }
}

impl SyncFilter for TetherSyncFilter {
    /// Called when a user navigates to a folder in Explorer — populate it with placeholders.
    fn fetch_placeholders(
        &self,
        request: Request,
        ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> Result<(), CloudErrorKind> {
        let full_path = request.path();
        let relative = full_path
            .strip_prefix(&self.root_path)
            .unwrap_or(&full_path);

        tracing::info!(
            "fetch_placeholders: path={}, relative={}",
            full_path.display(),
            relative.display()
        );

        // Resolve which cloud folder this local path maps to
        let folder_id = match self.provider.resolve_folder_id(relative) {
            Ok(Some(id)) => id,
            Ok(None) => {
                tracing::warn!(
                    "No cloud folder mapping for {:?} — cannot populate",
                    relative
                );
                return Err(CloudErrorKind::NotSupported);
            }
            Err(e) => {
                tracing::error!("Failed to resolve folder ID for {:?}: {}", relative, e);
                return Err(CloudErrorKind::NetworkUnavailable);
            }
        };

        // Fetch cloud folder contents via the provider (blocks on async internally)
        let items = match self.provider.list_folder_contents(&folder_id) {
            Ok(items) => items,
            Err(e) => {
                tracing::error!("Failed to list cloud folder {}: {}", folder_id, e);
                return Err(CloudErrorKind::NetworkUnavailable);
            }
        };

        tracing::info!(
            "fetch_placeholders: got {} items for folder {}",
            items.len(),
            folder_id
        );

        // Build PlaceholderFile entries
        let mut placeholders: Vec<PlaceholderFile> = items
            .iter()
            .map(|item| {
                let metadata = if item.is_directory {
                    Metadata::directory()
                } else {
                    Metadata::file().size(item.size)
                };

                // Store the cloud ID as the file identity blob (max 4KB).
                // This lets us retrieve it in fetch_data without a DB lookup.
                let blob = item.cloud_id.as_bytes().to_vec();

                PlaceholderFile::new(&item.name)
                    .metadata(metadata)
                    .blob(blob)
                    .mark_in_sync()
            })
            .collect();

        // Pass placeholders to Windows
        if !placeholders.is_empty() {
            ticket
                .pass_with_placeholder(&mut placeholders)
                .map_err(|e| {
                    tracing::error!("pass_with_placeholder failed: {}", e);
                    CloudErrorKind::InvalidRequest
                })?;
        }

        // Register sub-folder mappings so future navigations can resolve them
        for item in &items {
            if item.is_directory {
                let sub_path = if relative.as_os_str().is_empty() {
                    PathBuf::from(&item.name)
                } else {
                    relative.join(&item.name)
                };
                if let Err(e) =
                    self.provider.register_folder_mapping(&sub_path, &item.cloud_id)
                {
                    tracing::warn!(
                        "Failed to register folder mapping for {:?}: {}",
                        sub_path,
                        e
                    );
                }
            }
        }

        Ok(())
    }

    /// Called when a user opens/reads a placeholder file — download and hydrate it.
    fn fetch_data(
        &self,
        request: Request,
        ticket: ticket::FetchData,
        info: info::FetchData,
    ) -> Result<(), CloudErrorKind> {
        // The cloud item ID was stored as the file identity blob when the placeholder was created
        let blob = request.file_blob();
        let cloud_item_id = std::str::from_utf8(blob).unwrap_or("").to_string();

        if cloud_item_id.is_empty() {
            tracing::error!(
                "fetch_data: no cloud item ID in blob for {}",
                request.path().display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        let file_size = request.file_size();
        let required_range = info.required_file_range();

        tracing::info!(
            "fetch_data: item={}, size={}, range={}..{}",
            cloud_item_id,
            file_size,
            required_range.start,
            required_range.end
        );

        // Download the file content from cloud storage
        let data = match self.provider.download_file_content(&cloud_item_id) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(
                    "Failed to download file content for {}: {}",
                    cloud_item_id,
                    e
                );
                return Err(CloudErrorKind::NetworkUnavailable);
            }
        };

        // Write the full file to the placeholder.
        // write_at requires 4KB alignment OR ending at the logical file size.
        // Writing the entire file from offset 0 satisfies this (ends at file size).
        ticket.write_at(&data, 0).map_err(|e| {
            tracing::error!("ticket.write_at failed: {}", e);
            CloudErrorKind::InvalidRequest
        })?;

        // Report completion progress
        let _ = ticket.report_progress(file_size, data.len() as u64);

        tracing::info!(
            "fetch_data: hydrated {} ({} bytes)",
            cloud_item_id,
            data.len()
        );

        Ok(())
    }
}
