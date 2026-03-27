//! CFAPI SyncFilter implementation — handles Windows Cloud Files callbacks.

use cloud_filter::error::CloudErrorKind;
use cloud_filter::filter::{info, ticket, Request, SyncFilter};
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder::Placeholder;
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

        let full_path = request.path();

        if cloud_item_id.is_empty() {
            tracing::error!(
                "fetch_data: no cloud item ID in blob for {}",
                full_path.display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        let file_size = request.file_size();
        let required_range = info.required_file_range();

        tracing::info!(
            "fetch_data: item={}, logical_size={}, required_range={}..{}",
            cloud_item_id,
            file_size,
            required_range.start,
            required_range.end
        );

        // Download the file content from cloud storage
        let mut data = match self.provider.download_file_content(&cloud_item_id) {
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

        let fs = file_size as usize;
        if fs == 0 {
            if data.is_empty() {
                let _ = ticket.report_progress(0, 0);
                let rel = match full_path.strip_prefix(&self.root_path) {
                    Ok(p) => p,
                    Err(_) => full_path.as_path(),
                };
                let _ = self.provider.on_hydration_complete(&cloud_item_id, rel);
                return Ok(());
            }
            tracing::error!(
                "fetch_data: placeholder size is 0 but download is {} bytes — fix folder listing metadata for {}",
                data.len(),
                full_path.display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        // Placeholder logical size must match bytes we materialize. If APS metadata was smaller
        // than the real OSS object, truncate; if larger, fail (would yield corrupt CAD files and
        // errors like Inventor "The database in … could not be opened").
        if data.len() > fs {
            tracing::warn!(
                "fetch_data: download {} bytes > placeholder logical {} — truncating for {}",
                data.len(),
                fs,
                full_path.display()
            );
            data.truncate(fs);
        } else if data.len() < fs {
            tracing::error!(
                "fetch_data: download {} bytes < placeholder logical {} — refusing corrupt hydration for {}",
                data.len(),
                fs,
                full_path.display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        // Write only the range Windows requested (may be full file or a chunk).
        let start = required_range.start as usize;
        let end = (required_range.end as usize).min(data.len());
        if start >= end {
            tracing::error!(
                "fetch_data: invalid required range {}..{} for {}",
                start,
                end,
                full_path.display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        let write_res = if start == 0 && end == data.len() {
            ticket.write_at(&data, 0)
        } else {
            ticket.write_at(&data[start..end], required_range.start)
        };

        write_res.map_err(|e| {
            tracing::error!("ticket.write_at failed: {}", e);
            CloudErrorKind::InvalidRequest
        })?;

        let _ = ticket.report_progress(file_size, end as u64);

        tracing::info!(
            "fetch_data: wrote range {}..{} of {} ({} bytes logical) for {}",
            start,
            end,
            cloud_item_id,
            data.len(),
            full_path.display()
        );

        // Only notify after the last chunk — otherwise DB/state thinks the file is complete early.
        if end == fs {
            let rel = match full_path.strip_prefix(&self.root_path) {
                Ok(p) => p,
                Err(_) => full_path.as_path(),
            };
            if let Err(e) = self
                .provider
                .on_hydration_complete(&cloud_item_id, rel)
            {
                tracing::warn!("on_hydration_complete failed: {}", e);
            }
        }

        Ok(())
    }

    /// User deleted a placeholder — propagate to cloud, then acknowledge.
    fn delete(
        &self,
        request: Request,
        ticket: ticket::Delete,
        info: info::Delete,
    ) -> Result<(), CloudErrorKind> {
        if info.is_undelete() {
            return ticket.pass().map_err(|e| {
                tracing::error!("ticket.pass delete (undelete): {:?}", e);
                CloudErrorKind::InvalidRequest
            });
        }

        let blob = request.file_blob();
        let cloud_id = std::str::from_utf8(blob).unwrap_or("").trim();
        if cloud_id.is_empty() {
            tracing::error!("delete: empty file identity blob");
            return Err(CloudErrorKind::InvalidRequest);
        }

        let path = request.path();
        let display_name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        // Best-effort cloud delete, then always ACK the local delete. Returning `Err` here causes
        // cloud-filter to call `Delete::fail(..).unwrap()`, which can panic with HRESULT 0x8007018E
        // when Windows rejects the chosen completion status.
        if info.is_directory() {
            if let Err(e) = self.provider.delete_cloud_folder_recursive(cloud_id) {
                tracing::warn!(
                    "delete_cloud_folder_recursive failed (local delete still applied): {e:#}"
                );
            }
        } else if let Err(e) = self.provider.delete_cloud_item(cloud_id, display_name) {
            tracing::warn!("delete_cloud_item failed (local delete still applied): {e:#}");
        }

        ticket.pass().map_err(|e| {
            tracing::error!("ticket.pass delete: {:?}", e);
            CloudErrorKind::InvalidRequest
        })
    }

    /// User renamed/moved a placeholder — propagate to cloud, then acknowledge.
    fn rename(
        &self,
        request: Request,
        ticket: ticket::Rename,
        info: info::Rename,
    ) -> Result<(), CloudErrorKind> {
        let blob = request.file_blob();
        let cloud_id = std::str::from_utf8(blob).unwrap_or("").trim();
        if cloud_id.is_empty() {
            tracing::error!("rename: empty file identity blob");
            return Err(CloudErrorKind::InvalidRequest);
        }

        let target = info.target_path();
        let new_name = target
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(CloudErrorKind::InvalidRequest)?;

        let source_path = request.path();
        let relative_old = source_path
            .strip_prefix(&self.root_path)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::new());
        let relative_new = target
            .strip_prefix(&self.root_path)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| PathBuf::new());

        if info.is_directory() {
            self.provider
                .rename_cloud_folder(cloud_id, new_name)
                .map_err(|e| {
                    tracing::error!("rename_cloud_folder: {}", e);
                    CloudErrorKind::NetworkUnavailable
                })?;
            self.provider
                .rename_folder_mapping(&relative_old, &relative_new)
                .map_err(|e| {
                    tracing::error!("rename_folder_mapping: {}", e);
                    CloudErrorKind::NetworkUnavailable
                })?;
        } else {
            self.provider
                .rename_cloud_item(cloud_id, new_name)
                .map_err(|e| {
                    tracing::error!("rename_cloud_item: {}", e);
                    CloudErrorKind::NetworkUnavailable
                })?;
        }

        ticket.pass().map_err(|e| {
            tracing::error!("ticket.pass rename: {:?}", e);
            CloudErrorKind::InvalidRequest
        })
    }

    /// Last handle closed — if sync is pending, upload.
    fn closed(&self, request: Request, info: info::Closed) {
        if info.deleted() {
            return;
        }
        let path = request.path();
        if path.is_dir() {
            return;
        }

        // ── Resolve cloud item ID: blob first, DB fallback ──
        let blob = request.file_blob();
        let mut cloud_id = std::str::from_utf8(blob).unwrap_or("").trim().to_string();

        if cloud_id.is_empty() {
            let relative = path
                .strip_prefix(&self.root_path)
                .unwrap_or(path.as_path());
            cloud_id = self
                .provider
                .resolve_cloud_item_id_by_path(relative)
                .ok()
                .flatten()
                .unwrap_or_default();
        }

        if cloud_id.is_empty() {
            return;
        }

        // ── If sync is pending, upload. That's it. ──
        let in_sync = Placeholder::open(&path)
            .ok()
            .and_then(|ph| ph.info().ok().flatten())
            .map(|pi| pi.is_in_sync())
            .unwrap_or(false);

        if in_sync {
            return;
        }

        tracing::info!(
            "closed: sync pending → upload {} (item={})",
            path.display(),
            cloud_id
        );
        if let Err(e) = self
            .provider
            .queue_upload_if_dirty(path.to_path_buf(), &cloud_id)
        {
            tracing::warn!("closed: upload queue failed: {e:#}");
        }
    }
}
