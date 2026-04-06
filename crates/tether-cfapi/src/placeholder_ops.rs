//! Placeholder operations — create, dehydrate, and update cloud file placeholders.

use std::fs::File;
use std::path::Path;

use anyhow::Context;
use cloud_filter::ext::FileExt;
use cloud_filter::metadata::Metadata;
use cloud_filter::placeholder::Placeholder;
use cloud_filter::placeholder_file::PlaceholderFile;
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED;

/// Remove local file bytes while keeping the placeholder (same as Explorer *Free up space*).
pub fn dehydrate_placeholder_file(path: &Path) -> anyhow::Result<()> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    f.dehydrate(..)
        .with_context(|| format!("CfDehydratePlaceholder for {}", path.display()))?;
    Ok(())
}

/// Create a new cloud-only placeholder file on disk (no bytes downloaded).
///
/// `parent_dir` must already exist (e.g. the user navigated to that folder so its
/// placeholders were populated). `file_name` is just the leaf name. The blob stores
/// the cloud item ID so `fetch_data` can retrieve it later.
pub fn create_placeholder_file(
    parent_dir: &Path,
    file_name: &str,
    file_size: u64,
    cloud_item_id: &str,
) -> anyhow::Result<()> {
    PlaceholderFile::new(file_name)
        .metadata(
            Metadata::file()
                .size(file_size)
                .attributes(FILE_ATTRIBUTE_NOT_CONTENT_INDEXED.0),
        )
        .blob(cloud_item_id.as_bytes().to_vec())
        .mark_in_sync()
        .create::<&Path>(parent_dir)
        .with_context(|| {
            format!(
                "create placeholder {:?} in {:?}",
                file_name,
                parent_dir.display()
            )
        })?;
    Ok(())
}

/// Create a new cloud-only placeholder directory on disk.
///
/// Unlike `std::fs::create_dir_all`, this registers the directory with Windows
/// CFAPI so the OS knows to call `fetch_placeholders` when the user navigates
/// into it.  No-op if a CFAPI placeholder already exists at that path.
/// Returns an error only for unexpected failures (not for ALREADY_EXISTS when
/// the path is already a placeholder).
pub fn create_placeholder_dir(parent_dir: &Path, dir_name: &str, cloud_id: &str) -> anyhow::Result<()> {
    let full = parent_dir.join(dir_name);
    // Already a CFAPI placeholder directory — nothing to do.
    if crate::sync_state::is_placeholder(&full) {
        return Ok(());
    }
    // Plain directory left over from a non-CFAPI creation (e.g. create_dir_all).
    // Remove it only if empty; if it has content, leave it for the user.
    if crate::sync_state::is_dir_no_recall(&full) {
        let is_empty = std::fs::read_dir(&full)
            .map(|mut e| e.next().is_none())
            .unwrap_or(false);
        if is_empty {
            std::fs::remove_dir(&full)
                .with_context(|| format!("remove plain dir {:?}", full))?;
        } else {
            // Non-empty plain dir — cannot convert safely; leave as-is.
            tracing::warn!(
                "Synced folder {:?} is a non-empty plain directory — skipping placeholder creation",
                full
            );
            return Ok(());
        }
    }
    PlaceholderFile::new(dir_name)
        .metadata(
            Metadata::directory()
                .attributes(FILE_ATTRIBUTE_NOT_CONTENT_INDEXED.0),
        )
        .blob(cloud_id.as_bytes().to_vec())
        .mark_in_sync()
        .create::<&Path>(parent_dir)
        .with_context(|| format!("create placeholder dir {:?} in {:?}", dir_name, parent_dir.display()))?;
    Ok(())
}

/// If an existing placeholder is hydrated (has local bytes), dehydrate it so the
/// next user-initiated open triggers a fresh `fetch_data` download.  Returns `true`
/// if the file was dehydrated, `false` if it was already cloud-only or not a
/// placeholder.
pub fn dehydrate_if_hydrated(path: &Path) -> anyhow::Result<bool> {
    // Quick attribute check — if the file is cloud-only, there's nothing to
    // dehydrate and we must NOT call File::open() below (it would hydrate it).
    if crate::sync_state::is_cloud_only_attr(path) {
        return Ok(false);
    }
    let ph = match Placeholder::open(path) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    let info = match ph.info() {
        Ok(Some(i)) => i,
        _ => return Ok(false),
    };
    if info.on_disk_data_size() == 0 {
        return Ok(false);
    }
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    f.dehydrate(..)
        .with_context(|| format!("CfDehydratePlaceholder for {}", path.display()))?;
    Ok(true)
}
