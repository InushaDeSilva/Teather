//! Windows CFAPI in-sync state — clears Explorer "Sync pending" after cloud upload.

use std::path::Path;

use cloud_filter::placeholder::Placeholder;

/// Mark a hydrated placeholder file as in sync with the cloud (Explorer badge clears).
pub fn mark_placeholder_in_sync(path: &Path) -> anyhow::Result<()> {
    let mut ph = Placeholder::open(path).map_err(|e| anyhow::anyhow!("open placeholder: {e:?}"))?;
    ph.mark_in_sync(true, None)
        .map_err(|e| anyhow::anyhow!("CfSetInSyncState: {e:?}"))?;
    Ok(())
}
