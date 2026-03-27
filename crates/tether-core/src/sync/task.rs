use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
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

#[derive(Debug, Clone)]
pub struct SyncTask {
    pub id: String,
    pub operation: SyncOperation,
    pub priority: SyncPriority,
    pub local_path: PathBuf,
    pub cloud_item_id: Option<String>,
    pub cloud_folder_id: Option<String>,
    pub retry_count: u32,
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
            retry_count: 0,
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
}
