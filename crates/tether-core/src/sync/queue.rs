//! Sync queue — priority-ordered task queue with concurrency control.

use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, info};

use super::task::{SyncTask, SyncPriority, SyncTaskStatus};

/// Wrapper to make SyncTask orderable by priority (highest first).
struct PrioritizedTask(SyncTask);

impl PartialEq for PrioritizedTask {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority == other.0.priority
    }
}

impl Eq for PrioritizedTask {}

impl PartialOrd for PrioritizedTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedTask {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first, then earlier queued time
        self.0
            .priority
            .cmp(&other.0.priority)
            .then_with(|| other.0.queued_at.cmp(&self.0.queued_at))
    }
}

/// Thread-safe sync queue with concurrency limits.
pub struct SyncQueue {
    queue: Mutex<BinaryHeap<PrioritizedTask>>,
    upload_semaphore: Arc<Semaphore>,
    download_semaphore: Arc<Semaphore>,
}

impl SyncQueue {
    pub fn new(max_uploads: usize, max_downloads: usize) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            upload_semaphore: Arc::new(Semaphore::new(max_uploads)),
            download_semaphore: Arc::new(Semaphore::new(max_downloads)),
        }
    }

    /// Enqueue a new sync task.
    pub async fn push(&self, task: SyncTask) {
        debug!("Queued {:?} for {}", task.operation, task.local_path.display());
        self.queue.lock().await.push(PrioritizedTask(task));
    }

    /// Pop the highest-priority task.
    pub async fn pop(&self) -> Option<SyncTask> {
        self.queue.lock().await.pop().map(|p| p.0)
    }

    /// Number of tasks currently queued.
    pub async fn len(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// Get an upload semaphore permit (blocks until a slot is free).
    pub fn upload_semaphore(&self) -> Arc<Semaphore> {
        self.upload_semaphore.clone()
    }

    /// Get a download semaphore permit (blocks until a slot is free).
    pub fn download_semaphore(&self) -> Arc<Semaphore> {
        self.download_semaphore.clone()
    }
}
