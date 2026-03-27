//! Sync queue — priority-ordered task queue with concurrency control.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, Notify, Semaphore};
use tracing::debug;

use super::task::SyncTask;

/// Wrapper to make SyncTask orderable by priority (highest first).
pub(crate) struct PrioritizedTask(pub(crate) SyncTask);

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
    notify: Arc<Notify>,
}

impl SyncQueue {
    pub fn new(max_uploads: usize, max_downloads: usize) -> Self {
        Self {
            queue: Mutex::new(BinaryHeap::new()),
            upload_semaphore: Arc::new(Semaphore::new(max_uploads)),
            download_semaphore: Arc::new(Semaphore::new(max_downloads)),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Wait until [`push`] notifies or `timeout` elapses (polling fallback).
    pub async fn wait_for_work(&self, timeout: std::time::Duration) {
        tokio::select! {
            _ = self.notify.notified() => {}
            _ = tokio::time::sleep(timeout) => {}
        }
    }

    /// Enqueue a new sync task.
    pub async fn push(&self, task: SyncTask) {
        debug!("Queued {:?} for {}", task.operation, task.local_path.display());
        self.queue.lock().await.push(PrioritizedTask(task));
        self.notify.notify_one();
    }

    /// Pop the highest-priority task that is ready (`not_before` elapsed).
    pub async fn pop(&self) -> Option<SyncTask> {
        let now = Instant::now();
        let mut q = self.queue.lock().await;
        let mut deferred: Vec<SyncTask> = Vec::new();
        let mut result = None;
        while let Some(p) = q.pop() {
            let t = p.0;
            if t
                .not_before
                .map(|nb| nb > now)
                .unwrap_or(false)
            {
                deferred.push(t);
            } else {
                result = Some(t);
                break;
            }
        }
        for t in deferred {
            q.push(PrioritizedTask(t));
        }
        result
    }

    /// Number of tasks currently queued.
    pub async fn len(&self) -> usize {
        self.queue.lock().await.len()
    }

    /// Snapshot queued tasks without removing them (order is oldest-first in the returned vec).
    pub async fn snapshot_queue_views(&self) -> Vec<super::task::QueueJobView> {
        let heap = self.queue.lock().await;
        let mut views: Vec<_> = heap
            .iter()
            .map(|p| p.0.to_queue_view())
            .collect();
        views.sort_by(|a, b| a.queued_at_rfc3339.cmp(&b.queued_at_rfc3339));
        views
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
