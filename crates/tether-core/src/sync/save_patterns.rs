//! Coalesce authoring-app save patterns (temp + rename, delete + recreate) into logical updates.

use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ArchiveMove {
    pub live_relative_path: String,
    pub archive_relative_path: String,
    observed_at: Instant,
}

/// Tracks short-lived archive-save churn so the recreated live file gets priority and old-version
/// backups are synced as separate files only after the save sequence settles.
#[derive(Debug, Default)]
pub struct SavePatternCoalescer {
    recent_archive_moves: Vec<ArchiveMove>,
    window: Duration,
}

impl SavePatternCoalescer {
    pub fn new() -> Self {
        Self {
            recent_archive_moves: Vec::new(),
            window: Duration::from_secs(5),
        }
    }

    pub fn note_archive_move(&mut self, live_relative_path: String, archive_relative_path: String) {
        self.clear_stale();
        self.recent_archive_moves.retain(|move_info| {
            move_info.archive_relative_path != archive_relative_path
                && move_info.live_relative_path != live_relative_path
        });
        self.recent_archive_moves.push(ArchiveMove {
            live_relative_path,
            archive_relative_path,
            observed_at: Instant::now(),
        });
    }

    pub fn should_defer_archive(&self, archive_relative_path: &str) -> bool {
        self.recent_archive_moves.iter().any(|move_info| {
            move_info.archive_relative_path == archive_relative_path
                && Instant::now().duration_since(move_info.observed_at) < self.window
        })
    }

    pub fn live_path_for_archive(&self, archive_relative_path: &str) -> Option<&str> {
        self.recent_archive_moves
            .iter()
            .find(|move_info| {
                move_info.archive_relative_path == archive_relative_path
                    && Instant::now().duration_since(move_info.observed_at) < self.window
            })
            .map(|move_info| move_info.live_relative_path.as_str())
    }

    pub fn clear_stale(&mut self) {
        let now = Instant::now();
        self.recent_archive_moves
            .retain(|move_info| now.duration_since(move_info.observed_at) < self.window);
    }
}

pub fn is_old_versions_archive_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .map(|s| s.eq_ignore_ascii_case("OldVersions"))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::{is_old_versions_archive_path, SavePatternCoalescer};
    use std::path::Path;

    #[test]
    fn detects_old_versions_segment() {
        assert!(is_old_versions_archive_path(Path::new(
            "CAE - Project 3/OldVersions/Setup4.0002.iam"
        )));
        assert!(!is_old_versions_archive_path(Path::new(
            "CAE - Project 3/Setup4.iam"
        )));
    }

    #[test]
    fn defers_recent_archive_move() {
        let mut coalescer = SavePatternCoalescer::new();
        coalescer.note_archive_move(
            "CAE - Project 3/Setup4.iam".into(),
            "CAE - Project 3/OldVersions/Setup4.0002.iam".into(),
        );
        assert!(coalescer.should_defer_archive(
            "CAE - Project 3/OldVersions/Setup4.0002.iam"
        ));
        assert_eq!(
            coalescer.live_path_for_archive("CAE - Project 3/OldVersions/Setup4.0002.iam"),
            Some("CAE - Project 3/Setup4.iam")
        );
    }
}
