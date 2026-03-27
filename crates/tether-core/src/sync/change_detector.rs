//! Local filesystem change detection using the `notify` crate with debouncing.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent, Debouncer, FileIdMap};
use tracing::info;

/// Patterns to exclude from file watching.
const EXCLUDED_PATTERNS: &[&str] = &[
    ".tmp", "~$", ".bak", ".lck",
    "Thumbs.db", "desktop.ini", ".DS_Store",
    ".cloudforge", ".tether",
];

pub struct ChangeDetector {
    _debouncer: Debouncer<notify::RecommendedWatcher, FileIdMap>,
}

impl ChangeDetector {
    /// Start watching a directory. Changed paths are sent through the returned receiver.
    pub fn start(
        watch_path: &Path,
    ) -> Result<(Self, mpsc::Receiver<std::result::Result<Vec<DebouncedEvent>, Vec<notify::Error>>>)> {
        let (tx, rx) = mpsc::channel();

        let mut debouncer = new_debouncer(
            Duration::from_secs(3), // Wait 3s after last change
            None,                    // No tick rate
            tx,
        )?;

        debouncer.watch(watch_path, RecursiveMode::Recursive)?;

        info!("Watching for changes in {}", watch_path.display());
        Ok((Self { _debouncer: debouncer }, rx))
    }
}

/// Check if a path should be excluded from sync.
pub fn should_exclude(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    for pattern in EXCLUDED_PATTERNS {
        if name.starts_with(pattern) || name.ends_with(pattern) {
            return true;
        }
    }

    // Exclude hidden files (starting with .)
    if name.starts_with('.') {
        return true;
    }

    false
}
