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

/// If an existing placeholder is hydrated (has local bytes), dehydrate it so the
/// next user-initiated open triggers a fresh `fetch_data` download.  Returns `true`
/// if the file was dehydrated, `false` if it was already cloud-only or not a
/// placeholder.
pub fn dehydrate_if_hydrated(path: &Path) -> anyhow::Result<bool> {
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
