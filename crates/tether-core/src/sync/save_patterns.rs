//! Coalesce authoring-app save patterns (temp + rename, delete + recreate) into logical updates.
//!
//! Wire [`SavePatternCoalescer`] into the debounced watcher loop when uploads are implemented.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Tracks a recent rename pair so a subsequent delete of the *new* path can be ignored
/// when it is part of a save-by-replace pattern.
#[derive(Debug, Default)]
pub struct SavePatternCoalescer {
    last_rename: Option<(PathBuf, PathBuf, Instant)>,
    window: Duration,
}

impl SavePatternCoalescer {
    pub fn new() -> Self {
        Self {
            last_rename: None,
            window: Duration::from_secs(5),
        }
    }

    pub fn on_rename(&mut self, from: PathBuf, to: PathBuf) {
        self.last_rename = Some((from, to, Instant::now()));
    }

    /// True if `path` was the target of a very recent rename from another file (same-folder save).
    pub fn recent_rename_target(&self, path: &Path) -> bool {
        if let Some((_, to, t)) = &self.last_rename {
            path == to.as_path() && Instant::now().duration_since(*t) < self.window
        } else {
            false
        }
    }

    pub fn clear_stale(&mut self) {
        if let Some((_, _, t)) = &self.last_rename {
            if Instant::now().duration_since(*t) >= self.window {
                self.last_rename = None;
            }
        }
    }
}

/// Same parent + same stem suggests replace-save (e.g. delete+create during save).
pub fn is_likely_replace_save(removed: &Path, added: &Path) -> bool {
    removed.file_stem() == added.file_stem()
        && removed.parent() == added.parent()
        && removed != added
}
