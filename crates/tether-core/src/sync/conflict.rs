//! Conflict resolution — keep-both default; **stale-base** guard for uploads.

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

/// Result of comparing local edits to the last known remote head (stale-base detection).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleBaseOutcome {
    /// Remote head matches what we based local work on — safe to upload.
    SafeToUpload,
    /// Remote has moved on and local may have diverged — do not overwrite silently.
    StaleConflict {
        local_base_version_id: Option<String>,
        remote_head_version_id: String,
    },
}

/// Before uploading, compare `remote_head_version_id` to the version the user last synced from.
pub fn evaluate_stale_base(
    base_remote_version_id: Option<&str>,
    remote_head_version_id: &str,
) -> StaleBaseOutcome {
    match base_remote_version_id {
        None => StaleBaseOutcome::SafeToUpload,
        Some(base) if base == remote_head_version_id => StaleBaseOutcome::SafeToUpload,
        Some(base) => StaleBaseOutcome::StaleConflict {
            local_base_version_id: Some(base.to_string()),
            remote_head_version_id: remote_head_version_id.to_string(),
        },
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_base_safe_when_heads_match() {
        let o = evaluate_stale_base(Some("v1"), "v1");
        assert_eq!(o, StaleBaseOutcome::SafeToUpload);
    }

    #[test]
    fn stale_base_conflict_when_remote_moved() {
        let o = evaluate_stale_base(Some("v1"), "v2");
        match o {
            StaleBaseOutcome::StaleConflict { .. } => {}
            _ => panic!("expected stale conflict"),
        }
    }
}
