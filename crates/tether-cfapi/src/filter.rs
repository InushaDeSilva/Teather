//! CFAPI SyncFilter implementation — handles Windows Cloud Files callbacks.

use cloud_filter::error::CloudErrorKind;
use cloud_filter::filter::{info, ticket, Request, SyncFilter};
use cloud_filter::metadata::{Metadata, MetadataExt};
use cloud_filter::placeholder::Placeholder;
use cloud_filter::placeholder_file::PlaceholderFile;
use cloud_filter::utility::WriteAt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;

use crate::provider::{CloudFileInfo, CloudProvider};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HydrationCallerPolicy {
    Allow,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HydrationDecision {
    policy: HydrationCallerPolicy,
    process_name: String,
    process_path: Option<String>,
    managed_extension: Option<String>,
}

fn is_old_versions_archive_path(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|s| s.eq_ignore_ascii_case("OldVersions"))
            .unwrap_or(false)
    })
}

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

fn parse_rfc3339_to_filetime(value: &str) -> Option<i64> {
    const WINDOWS_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
    const TICKS_PER_SEC: i128 = 10_000_000;

    let dt = chrono::DateTime::parse_from_rfc3339(value)
        .ok()?
        .with_timezone(&chrono::Utc);
    let secs = dt.timestamp().checked_add(WINDOWS_EPOCH_OFFSET_SECS)?;
    let ticks = i128::from(secs)
        .checked_mul(TICKS_PER_SEC)?
        .checked_add(i128::from(dt.timestamp_subsec_nanos() / 100))?;
    i64::try_from(ticks).ok()
}

fn placeholder_metadata(item: &CloudFileInfo) -> Metadata {
    let mut metadata = if item.is_directory {
        Metadata::directory()
    } else {
        Metadata::file().size(item.size)
    }
    .attributes(FILE_ATTRIBUTE_NOT_CONTENT_INDEXED.0);

    if let Some(created) = item
        .created
        .as_deref()
        .and_then(parse_rfc3339_to_filetime)
    {
        metadata = metadata.creation_time(created).last_access_time(created);
    }
    if let Some(modified) = item
        .last_modified
        .as_deref()
        .and_then(parse_rfc3339_to_filetime)
    {
        metadata = metadata.last_write_time(modified).change_time(modified);
    }

    metadata
}

fn managed_cad_extension(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "iam" | "ipt" | "ipn" | "idw" | "ipj" | "dwg" | "dxf" | "step" | "stp"
    )
    .then_some(ext)
}

fn normalize_process_name_from_path(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
}

fn normalize_process_name(request: &Request) -> (String, Option<String>) {
    let process = request.process();
    let process_path = process.path().map(|p| p.to_string_lossy().into_owned());
    let process_name = process
        .path()
        .as_deref()
        .and_then(normalize_process_name_from_path)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            let name = process.name().to_string_lossy().trim().to_ascii_lowercase();
            (!name.is_empty()).then_some(name)
        })
        .unwrap_or_else(|| "unknown".into());

    (process_name, process_path)
}

fn classify_hydration_request_for(
    process_name: &str,
    process_path: Option<String>,
    full_path: &Path,
) -> HydrationDecision {
    let managed_extension = managed_cad_extension(full_path);
    let policy = match managed_extension.as_deref() {
        None => HydrationCallerPolicy::Allow,
        Some(_) => match process_name {
            "inventor.exe" | "inventorcoreconsole.exe" => HydrationCallerPolicy::Allow,
            "explorer.exe"
            | "dllhost.exe"
            | "prevhost.exe"
            | "searchindexer.exe"
            | "searchprotocolhost.exe" => HydrationCallerPolicy::Block,
            _ => HydrationCallerPolicy::Block,
        },
    };

    HydrationDecision {
        policy,
        process_name: process_name.to_string(),
        process_path,
        managed_extension,
    }
}

fn classify_hydration_request(request: &Request, full_path: &Path) -> HydrationDecision {
    let (process_name, process_path) = normalize_process_name(request);
    classify_hydration_request_for(&process_name, process_path, full_path)
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
                let metadata = placeholder_metadata(item);

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
        let required_range = info.required_file_range();
        let decision = classify_hydration_request(&request, &full_path);

        if matches!(decision.policy, HydrationCallerPolicy::Block) {
            tracing::info!(
                "fetch_data policy: action=would_block process={} process_path={} target={} range={}..{} extension={}",
                decision.process_name,
                decision.process_path.as_deref().unwrap_or("unknown"),
                full_path.display(),
                required_range.start,
                required_range.end,
                decision.managed_extension.as_deref().unwrap_or("n/a"),
            );
            // The current `cloud-filter` crate panics when the fetch-data callback fails certain
            // transfers. Keep this policy as audit-only until we can block safely without taking
            // down the provider process.
        }

        if cloud_item_id.is_empty() {
            tracing::error!(
                "fetch_data: no cloud item ID in blob for {}",
                full_path.display()
            );
            return Err(CloudErrorKind::InvalidRequest);
        }

        let file_size = request.file_size();

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

        // We already downloaded the full file from OSS, so materialize the whole payload in one
        // transfer. This is more reliable for CAD/assembly formats than leaving the file partially
        // hydrated across multiple callback rounds.
        let write_res = ticket.write_at(&data, 0);

        write_res.map_err(|e| {
            tracing::error!("ticket.write_at failed: {}", e);
            CloudErrorKind::InvalidRequest
        })?;

        let _ = ticket.report_progress(file_size, file_size);

        tracing::info!(
            "fetch_data: wrote full file after request range {}..{} of {} ({} bytes logical) for {}",
            start,
            end,
            cloud_item_id,
            data.len(),
            full_path.display()
        );

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

        let path = request.path();
        tracing::info!("delete: allowing local delete for {}", path.display());
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

        if !info.is_directory() && is_old_versions_archive_path(&relative_new) {
            if let Err(e) = self.provider.note_archive_move(&relative_old, &relative_new) {
                tracing::warn!("note_archive_move failed: {}", e);
            }
            tracing::info!(
                "rename: treating archive move as local-only {} -> {}",
                relative_old.display(),
                relative_new.display()
            );
            return ticket.pass().map_err(|e| {
                tracing::error!("ticket.pass rename archive move: {:?}", e);
                CloudErrorKind::InvalidRequest
            });
        }

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
            self.provider
                .rename_file_mapping(&relative_old, &relative_new, cloud_id)
                .map_err(|e| {
                    tracing::error!("rename_file_mapping: {}", e);
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

        // Prefer the current relative-path mapping from the DB. Placeholder blobs can be stale
        // after save-as/copy/versioned-save flows, and trusting them can upload a new local file
        // into the wrong cloud lineage.
        let relative = path
            .strip_prefix(&self.root_path)
            .unwrap_or(path.as_path());

        let blob = request.file_blob();
        let blob_cloud_id = std::str::from_utf8(blob).unwrap_or("").trim().to_string();
        let mapped_cloud_id = self
            .provider
            .resolve_cloud_item_id_by_path(relative)
            .ok()
            .flatten();

        let cloud_id = match (mapped_cloud_id, blob_cloud_id.is_empty()) {
            (Some(mapped), true) => mapped,
            (Some(mapped), false) if mapped == blob_cloud_id => mapped,
            (Some(mapped), false) => {
                tracing::warn!(
                    "closed: ignoring stale placeholder blob for {} (blob={}, mapped={})",
                    path.display(),
                    blob_cloud_id,
                    mapped
                );
                mapped
            }
            (None, false) => {
                tracing::info!(
                    "closed: skipping upload for {} because no DB mapping exists for current path; treating it as a local create/copy",
                    path.display()
                );
                return;
            }
            (None, true) => {
                return;
            }
        };

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

#[cfg(test)]
mod tests {
    use super::{
        classify_hydration_request_for, managed_cad_extension, HydrationCallerPolicy,
    };
    use std::path::Path;

    #[test]
    fn managed_extensions_are_detected_case_insensitively() {
        assert_eq!(
            managed_cad_extension(Path::new("Setup3.IAM")).as_deref(),
            Some("iam")
        );
        assert_eq!(
            managed_cad_extension(Path::new("part.step")).as_deref(),
            Some("step")
        );
        assert_eq!(managed_cad_extension(Path::new("notes.txt")), None);
    }

    #[test]
    fn explorer_is_blocked_for_managed_files() {
        let decision = classify_hydration_request_for(
            "explorer.exe",
            Some(String::from(r"C:\Windows\explorer.exe")),
            Path::new(r"C:\Sync\CAE\Setup3.iam"),
        );
        assert_eq!(decision.policy, HydrationCallerPolicy::Block);
        assert_eq!(decision.managed_extension.as_deref(), Some("iam"));
    }

    #[test]
    fn inventor_is_allowed_for_managed_files() {
        let decision = classify_hydration_request_for(
            "inventor.exe",
            Some(String::from(
                r"C:\Program Files\Autodesk\Inventor\Bin\Inventor.exe",
            )),
            Path::new(r"C:\Sync\CAE\Setup3.iam"),
        );
        assert_eq!(decision.policy, HydrationCallerPolicy::Allow);
        assert_eq!(decision.managed_extension.as_deref(), Some("iam"));
    }

    #[test]
    fn non_managed_files_remain_allowed() {
        let decision = classify_hydration_request_for(
            "explorer.exe",
            Some(String::from(r"C:\Windows\explorer.exe")),
            Path::new(r"C:\Sync\CAE\readme.txt"),
        );
        assert_eq!(decision.policy, HydrationCallerPolicy::Allow);
        assert_eq!(decision.managed_extension, None);
    }
}
