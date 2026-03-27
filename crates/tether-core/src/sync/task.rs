use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

// ── Sync operations ──

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOperation {
    Download,
    Upload,
    Delete,
    Rename { new_name: String },
    CreateFolder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SyncPriority {
    Background = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    Critical = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncTaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Serializable view of a queued job for UI / diagnostics.
#[derive(Debug, Clone, Serialize)]
pub struct QueueJobView {
    pub id: String,
    pub operation: String,
    pub local_path: String,
    pub cloud_item_id: Option<String>,
    pub queued_at_rfc3339: String,
}

#[derive(Debug, Clone)]
pub struct SyncTask {
    pub id: String,
    pub operation: SyncOperation,
    pub priority: SyncPriority,
    pub local_path: PathBuf,
    pub cloud_item_id: Option<String>,
    pub cloud_folder_id: Option<String>,
    /// SQLite sync_roots.id for this job (parity / DB updates).
    pub sync_root_id: Option<String>,
    pub sync_root_path: Option<PathBuf>,
    pub retry_count: u32,
    /// When set, the worker must not run this task before this instant (retry / auth backoff).
    pub not_before: Option<Instant>,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub status: SyncTaskStatus,
}

impl SyncTask {
    pub fn new(
        operation: SyncOperation,
        priority: SyncPriority,
        local_path: PathBuf,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            operation,
            priority,
            local_path,
            cloud_item_id: None,
            cloud_folder_id: None,
            sync_root_id: None,
            sync_root_path: None,
            retry_count: 0,
            not_before: None,
            queued_at: Utc::now(),
            started_at: None,
            status: SyncTaskStatus::Queued,
        }
    }

    /// Maximum backoff duration for this task's retry count.
    pub fn backoff_duration(&self) -> Duration {
        let secs = 2u64.pow(self.retry_count.min(5)); // 1, 2, 4, 8, 16, 32 max
        Duration::from_secs(secs)
    }

    pub fn to_queue_view(&self) -> QueueJobView {
        QueueJobView {
            id: self.id.clone(),
            operation: format!("{:?}", self.operation),
            local_path: self.local_path.display().to_string(),
            cloud_item_id: self.cloud_item_id.clone(),
            queued_at_rfc3339: self.queued_at.to_rfc3339(),
        }
    }
}
