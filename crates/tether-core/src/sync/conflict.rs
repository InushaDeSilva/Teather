//! Conflict resolution — last-write-wins with safety copy.

use std::path::{Path, PathBuf};
use anyhow::Result;
use chrono::Utc;
use tracing::info;

/// Conflict resolution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStrategy {
    /// Keep both: rename conflicting copy (default).
    KeepBoth,
    /// Always keep local version, overwrite cloud.
    KeepLocal,
    /// Always keep cloud version, overwrite local.
    KeepCloud,
}

/// Generate a conflict-renamed path.
/// Example: `Assembly.iam` → `Assembly (cloud conflict 2026-03-26).iam`
pub fn conflict_path(original: &Path) -> PathBuf {
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = original
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let date = Utc::now().format("%Y-%m-%d");
    let new_name = if ext.is_empty() {
        format!("{stem} (cloud conflict {date})")
    } else {
        format!("{stem} (cloud conflict {date}).{ext}")
    };

    original.with_file_name(new_name)
}

/// Resolve a conflict according to the chosen strategy.
/// Returns the path where the cloud version was saved (if applicable).
pub async fn resolve_conflict(
    local_path: &Path,
    cloud_bytes: &[u8],
    strategy: ConflictStrategy,
) -> Result<Option<PathBuf>> {
    match strategy {
        ConflictStrategy::KeepBoth => {
            // Save cloud version alongside local with a conflict name
            let dest = conflict_path(local_path);
            tokio::fs::write(&dest, cloud_bytes).await?;
            info!(
                "Conflict resolved (keep both): cloud version saved as {}",
                dest.display()
            );
            Ok(Some(dest))
        }
        ConflictStrategy::KeepLocal => {
            // Do nothing — local version stays, we'll upload it as new cloud version
            info!("Conflict resolved (keep local): {}", local_path.display());
            Ok(None)
        }
        ConflictStrategy::KeepCloud => {
            // Overwrite local with cloud version
            tokio::fs::write(local_path, cloud_bytes).await?;
            info!(
                "Conflict resolved (keep cloud): overwritten {}",
                local_path.display()
            );
            Ok(None)
        }
    }
}
