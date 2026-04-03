//! Windows CFAPI in-sync state helpers.

use std::path::Path;

use cloud_filter::placeholder::Placeholder;

/// Mark a hydrated placeholder file as in sync with the cloud (Explorer badge clears).
pub fn mark_placeholder_in_sync(path: &Path) -> anyhow::Result<()> {
    let mut ph = Placeholder::open(path).map_err(|e| anyhow::anyhow!("open placeholder: {e:?}"))?;
    ph.mark_in_sync(true, None)
        .map_err(|e| anyhow::anyhow!("CfSetInSyncState: {e:?}"))?;
    Ok(())
}

/// Returns `true` if the file has local bytes and is NOT in sync (i.e. Explorer shows
/// "Sync pending"). For non-placeholder files (worker-downloaded), returns `true` if
/// the file exists on disk.
pub fn is_sync_pending(path: &Path) -> bool {
    match Placeholder::open(path) {
        Ok(ph) => match ph.info() {
            Ok(Some(pi)) => !pi.is_in_sync() && pi.on_disk_data_size() > 0,
            _ => false,
        },
        Err(_) => path.is_file(),
    }
}

/// Returns true when the path is backed by a CFAPI placeholder, regardless of hydration.
pub fn is_placeholder(path: &Path) -> bool {
    Placeholder::open(path)
        .ok()
        .and_then(|ph| ph.info().ok().flatten())
        .is_some()
}

/// Returns true when the path is a cloud-only placeholder and has no local payload yet.
pub fn is_cloud_only_placeholder(path: &Path) -> bool {
    match Placeholder::open(path) {
        Ok(ph) => match ph.info() {
            Ok(Some(pi)) => pi.on_disk_data_size() == 0,
            _ => false,
        },
        Err(_) => false,
    }
}
