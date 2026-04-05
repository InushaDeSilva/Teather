//! Windows CFAPI in-sync state helpers.
//!
//! Functions in this module come in two flavours:
//!
//! * **Handle-based** (`Placeholder::open` / `CfOpenFileWithOplock`) — safe for
//!   querying full placeholder info (on_disk_data_size, in-sync state, blob).
//!   `CfOpenFileWithOplock` does **not** trigger `fetch_data`.
//!
//! * **Attribute-based** (`GetFileAttributesW`) — the cheapest possible check.
//!   Returns file attribute flags without ever opening a handle or touching
//!   file data.  Use for read-only "is this cloud-only?" queries in hot loops
//!   (reconcile, poller, watcher).

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use cloud_filter::placeholder::Placeholder;

// ── Attribute-based (no handle, no recall) ──────────────────────────────

// Raw Win32 attribute bits (avoids newtype ambiguity across `windows` versions).
const ATTR_DIRECTORY: u32 = 0x10;                // FILE_ATTRIBUTE_DIRECTORY
const ATTR_OFFLINE: u32 = 0x1000;                // FILE_ATTRIBUTE_OFFLINE
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x00400000; // FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
const ATTR_RECALL_ON_OPEN: u32 = 0x00040000;     // FILE_ATTRIBUTE_RECALL_ON_OPEN
const ATTR_INVALID: u32 = 0xFFFF_FFFF;           // INVALID_FILE_ATTRIBUTES

/// Lightweight check: returns `true` when the path has the
/// `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` or `FILE_ATTRIBUTE_OFFLINE` attribute,
/// meaning the file data lives in the cloud and any standard `CreateFileW` /
/// `ReadFile` call would trigger `fetch_data`.
///
/// This never opens a handle, never triggers hydration, and never blocks on
/// network I/O.  Returns `false` for non-existent paths or non-placeholder
/// files.
pub fn is_cloud_only_attr(path: &Path) -> bool {
    let attrs = get_file_attributes(path);
    if attrs == ATTR_INVALID {
        return false;
    }
    // RECALL_ON_DATA_ACCESS is the canonical bit for "dehydrated CFAPI
    // placeholder".  OFFLINE is sometimes also set by the OS.
    (attrs & ATTR_RECALL_ON_DATA_ACCESS != 0)
        || (attrs & ATTR_OFFLINE != 0 && attrs & ATTR_RECALL_ON_OPEN != 0)
}

/// Returns `true` if the path exists on the filesystem (placeholder or
/// regular file).  Unlike `path.exists()`, this never opens a handle and
/// never triggers hydration.
pub fn path_exists_no_recall(path: &Path) -> bool {
    get_file_attributes(path) != ATTR_INVALID
}

/// Returns `true` if the path is a directory.  Unlike `path.is_dir()`,
/// this never opens a handle and never triggers hydration.
pub fn is_dir_no_recall(path: &Path) -> bool {
    let attrs = get_file_attributes(path);
    if attrs == ATTR_INVALID {
        return false;
    }
    attrs & ATTR_DIRECTORY != 0
}

/// Returns `true` if the path is a file (not a directory).  Unlike
/// `path.is_file()`, this never opens a handle and never triggers hydration.
pub fn is_file_no_recall(path: &Path) -> bool {
    let attrs = get_file_attributes(path);
    if attrs == ATTR_INVALID {
        return false;
    }
    attrs & ATTR_DIRECTORY == 0
}

/// Raw wrapper around `GetFileAttributesW`.  Returns `ATTR_INVALID`
/// on failure (file not found, access denied, etc.).
fn get_file_attributes(path: &Path) -> u32 {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // In windows 0.48 the return is u32 directly.
    unsafe {
        windows::Win32::Storage::FileSystem::GetFileAttributesW(
            windows::core::PCWSTR(wide.as_ptr()),
        )
    }
}


// ── Handle-based (CfOpenFileWithOplock — does NOT trigger fetch_data) ────

/// Mark a populated file as in-sync (and convert it from a regular file to a placeholder if necessary).
pub fn mark_placeholder_in_sync(path: &Path, cloud_item_id: &str) -> anyhow::Result<()> {
    let mut is_cloud_file_error = false;
    
    if let Ok(mut ph) = Placeholder::open(path) {
        if let Err(e) = ph.mark_in_sync(true, None) {
            let err_str = format!("{:?}", e);
            if err_str.contains("0x80070178") || err_str.contains("not a cloud file") {
                is_cloud_file_error = true;
            } else {
                return Err(anyhow::anyhow!("CfSetInSyncState: {e:?}"));
            }
        } else {
            return Ok(());
        }
    } else {
        is_cloud_file_error = true;
    }

    if is_cloud_file_error {
        // If it's not a cloud file, we can convert it into a hydrated placeholder
        // using CfConvertToPlaceholder so the sync engine tracks it and it gets
        // the green check badge.
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| anyhow::anyhow!("open for convert: {e}"))?;
        
        use std::os::windows::io::AsRawHandle;
        let handle = windows::Win32::Foundation::HANDLE(f.as_raw_handle() as isize);
        let blob = cloud_item_id.as_bytes();
        
        unsafe {
            windows::Win32::Storage::CloudFilters::CfConvertToPlaceholder(
                handle,
                Some(blob.as_ptr() as *const std::ffi::c_void),
                blob.len() as u32,
                windows::Win32::Storage::CloudFilters::CF_CONVERT_FLAG_MARK_IN_SYNC,
                None,
                None,
            )
            .map_err(|e| anyhow::anyhow!("CfConvertToPlaceholder: {e}"))?;
        }
    }
    
    Ok(())
}

/// Returns `true` if the file has local bytes and is NOT in sync (i.e. Explorer shows
/// "Sync pending"). For non-placeholder files (worker-downloaded), returns `true` if
/// the file exists on disk (checked via attributes, no recall).
pub fn is_sync_pending(path: &Path) -> bool {
    match Placeholder::open(path) {
        Ok(ph) => match ph.info() {
            Ok(Some(pi)) => !pi.is_in_sync() && pi.on_disk_data_size() > 0,
            _ => false,
        },
        Err(_) => is_file_no_recall(path),
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
///
/// Prefer [`is_cloud_only_attr`] when you only need a yes/no answer in a hot
/// loop — it avoids the `CfOpenFileWithOplock` overhead entirely.
pub fn is_cloud_only_placeholder(path: &Path) -> bool {
    match Placeholder::open(path) {
        Ok(ph) => match ph.info() {
            Ok(Some(pi)) => pi.on_disk_data_size() == 0,
            _ => false,
        },
        Err(_) => false,
    }
}

