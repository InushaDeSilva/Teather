//! Sync engine — the main orchestrator that ties everything together.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tracing::info;

use crate::api::auth::ApsAuthClient;
use crate::api::data_management::ApsDataManagementClient;
use crate::api::storage::ApsStorageClient;
use crate::config::settings::AppSettings;
use crate::db::database::SyncDatabase;

use super::queue::SyncQueue;

/// Overall sync status for UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub enum SyncStatus {
    Idle,
    Syncing { active_count: usize },
    Error { message: String },
    Offline,
    Paused,
}

/// The main sync engine that coordinates all sync operations.
pub struct SyncEngine {
    pub settings: AppSettings,
    pub auth: ApsAuthClient,
    pub data_mgmt: ApsDataManagementClient,
    pub storage: ApsStorageClient,
    pub db: Arc<Mutex<SyncDatabase>>,
    pub queue: Arc<SyncQueue>,
    pub status: SyncStatus,
    pub sync_root_path: Option<PathBuf>,
    /// Row id in `sync_roots` for the active session.
    pub sync_root_db_id: Option<String>,
    pub current_hub_id: Option<String>,
    pub current_project_id: Option<String>,
    pub current_root_folder_id: Option<String>,
    pub cf_connection: Option<tether_cfapi::Connection<tether_cfapi::TetherSyncFilter>>,
    paused: bool,
}

impl SyncEngine {
    pub fn new(settings: AppSettings) -> Result<Self> {
        let auth = ApsAuthClient::new(
            settings.client_id.clone(),
            settings.redirect_uri.clone(),
        );
        let data_mgmt = ApsDataManagementClient::new();
        let storage = ApsStorageClient::new();
        let db = Arc::new(Mutex::new(SyncDatabase::open_default()?));
        let queue = Arc::new(SyncQueue::new(2, 2));

        info!("Sync engine initialized");

        Ok(Self {
            settings,
            auth,
            data_mgmt,
            storage,
            db,
            queue,
            status: SyncStatus::Idle,
            sync_root_path: None,
            sync_root_db_id: None,
            current_hub_id: None,
            current_project_id: None,
            current_root_folder_id: None,
            cf_connection: None,
            paused: false,
        })
    }

    pub fn current_status(&self) -> SyncStatus {
        self.status.clone()
    }

    pub fn sync_root_path(&self) -> Option<PathBuf> {
        self.sync_root_path.clone()
    }

    pub fn pause(&mut self) {
        self.paused = true;
        self.status = SyncStatus::Paused;
        info!("Sync paused");
    }

    pub fn resume(&mut self) {
        self.paused = false;
        self.status = SyncStatus::Idle;
        info!("Sync resumed");
    }

    pub async fn start(
        &mut self, 
        hub_id: &str, 
        project_id: &str, 
        project_name: &str,
        folder_id: Option<String>
    ) -> Result<()> {
        info!("Starting sync for project: {} ({})", project_name, project_id);
        if let Some(ref fid) = folder_id {
            info!("  -> Specific folder: {}", fid);
        }

        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        let sync_root = std::path::PathBuf::from(local_app_data)
            .join("Tether")
            .join("Sync")
            .join(project_name.replace(|c: char| !c.is_alphanumeric() && c != ' ' && c != '-' && c != '_', ""));

        std::fs::create_dir_all(&sync_root)?;
        self.sync_root_path = Some(sync_root.clone());
        self.status = SyncStatus::Idle;

        // Use provided folder_id, or find "Project Files", or fallback to first
        let root_folder_id = if let Some(fid) = folder_id {
            fid
        } else {
            // Discover top-level folders to find a default
            let token = self.auth.get_access_token().map_err(|e| anyhow::anyhow!("Auth failed: {e}"))?;
            let folders = self.data_mgmt.get_top_folders(&token, hub_id, project_id).await?;

            info!("Found {} top-level folders for project.", folders.len());
            for f in &folders {
                info!(" - Folder ID: {}, Name: {}", f.id, f.attributes.display_name);
            }

            let project_files_folder = folders.iter().find(|f| {
                f.attributes.display_name.contains("Project Files")
            }).or_else(|| folders.first()).cloned();

            let root_folder = project_files_folder
                .ok_or_else(|| anyhow::anyhow!("No 'Project Files' folder found in project!"))?;
            
            root_folder.id.clone()
        };

        let sync_root_db_id = {
            let db = self.db.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            db.insert_sync_root(
                hub_id,
                project_id,
                &root_folder_id,
                sync_root.to_str().unwrap_or("."),
                project_name,
            )?
        };
        self.sync_root_db_id = Some(sync_root_db_id.clone());
        self.current_hub_id = Some(hub_id.to_string());
        self.current_project_id = Some(project_id.to_string());
        self.current_root_folder_id = Some(root_folder_id.clone());

        let (upload_tx, mut upload_rx) = tokio::sync::mpsc::unbounded_channel();
        let queue_upload = self.queue.clone();
        let root_upload = sync_root.clone();
        let sr_upload = sync_root_db_id.clone();
        tokio::spawn(async move {
            while let Some((path, item_id)) = upload_rx.recv().await {
                let mut task = super::task::SyncTask::new(
                    super::task::SyncOperation::Upload,
                    super::task::SyncPriority::High,
                    path,
                );
                task.cloud_item_id = Some(item_id);
                task.sync_root_id = Some(sr_upload.clone());
                task.sync_root_path = Some(root_upload.clone());
                queue_upload.push(task).await;
            }
        });

        // ── Build the CloudProvider for CFAPI callbacks ──
        let provider = std::sync::Arc::new(super::cfapi_provider::ApsCloudProvider::new(
            tokio::runtime::Handle::current(),
            self.auth.clone(),
            self.data_mgmt.clone(),
            self.storage.clone(),
            project_id.to_string(),
            root_folder_id.clone(),
            Some(self.db.clone()),
            Some(sync_root_db_id.clone()),
            upload_tx,
        ));

        // ── Register and connect the Windows Cloud Files virtual drive ──
        let provider_name = format!("Tether - {}", project_name);
        tether_cfapi::register_sync_root(&provider_name, "1.0.0", &sync_root)?;

        let connection = tether_cfapi::connect_sync_root(&sync_root, provider)?;
        self.cf_connection = Some(connection);
        info!("CFAPI virtual drive connected at {:?}", sync_root);

        // ── Start background workers for queue-based tasks ──
        super::worker::start_workers(
            2,
            self.queue.clone(),
            self.storage.clone(),
            self.data_mgmt.clone(),
            self.auth.clone(),
            self.db.clone(),
            project_id.to_string(),
        ).await;

        // Purge any stale download tasks from previous sessions / old poller logic.
        self.queue.clear_downloads().await;

        // ── Cloud poller — detects remote changes for proactive placeholder updates ──
        {
            let (tx, mut rx) = tokio::sync::mpsc::channel(100);

            let auth_clone = self.auth.clone();
            let data_clone = self.data_mgmt.clone();
            let db_clone = self.db.clone();
            let pid = project_id.to_string();
            let fid = root_folder_id;
            let poll_root_id = sync_root_db_id.clone();

            let poll_sync_root = sync_root.clone();
            tokio::spawn(async move {
                super::cloud_poller::start_polling(
                    30,
                    data_clone,
                    db_clone,
                    move || auth_clone.get_access_token().map_err(|e| anyhow::anyhow!("{e}")),
                    pid,
                    fid,
                    poll_root_id,
                    poll_sync_root,
                    tx,
                ).await;
            });

            let root_clone = sync_root;
            tokio::spawn(async move {
                while let Some(change) = rx.recv().await {
                    match change {
                        super::cloud_poller::CloudChange::Added {
                            cloud_item_id,
                            local_relative_path,
                            file_size,
                        } => {
                            let full = root_clone.join(local_relative_path.replace('/', "\\"));
                            if full.exists() {
                                continue;
                            }
                            if let Some(parent) = full.parent() {
                                if !parent.exists() {
                                    // Parent folder not visited yet — placeholder will be
                                    // created lazily by fetch_placeholders when user navigates.
                                    continue;
                                }
                                let file_name = full
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or_default();
                                if let Err(e) = tether_cfapi::create_placeholder_file(
                                    parent,
                                    file_name,
                                    file_size,
                                    &cloud_item_id,
                                ) {
                                    tracing::warn!(
                                        "Failed to create placeholder for {}: {e:#}",
                                        local_relative_path
                                    );
                                }
                            }
                        }
                        super::cloud_poller::CloudChange::Updated {
                            local_relative_path,
                            ..
                        } => {
                            let full = root_clone.join(local_relative_path.replace('/', "\\"));
                            if !full.exists() {
                                continue;
                            }
                            match tether_cfapi::dehydrate_if_hydrated(&full) {
                                Ok(true) => {
                                    tracing::info!(
                                        "Dehydrated stale local copy: {}",
                                        local_relative_path
                                    );
                                }
                                Ok(false) => {} // already cloud-only
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to dehydrate {}: {e:#}",
                                        local_relative_path
                                    );
                                }
                            }
                        }
                        super::cloud_poller::CloudChange::Removed {
                            local_relative_path,
                            ..
                        } => {
                            let full = root_clone.join(local_relative_path.replace('/', "\\"));
                            if full.exists() {
                                if let Err(e) = std::fs::remove_file(&full) {
                                    tracing::warn!(
                                        "Failed to remove placeholder {}: {e:#}",
                                        local_relative_path
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        // ── Periodic local scanner — picks up "sync pending" files and queues uploads ──
        {
            let scan_root = self.sync_root_path.clone().unwrap();
            let scan_db = self.db.clone();
            let scan_queue = self.queue.clone();
            let scan_sr_id = sync_root_db_id.clone();

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
                loop {
                    interval.tick().await;
                    scan_pending_uploads(&scan_root, &scan_db, &scan_sr_id, &scan_queue).await;
                }
            });
        }

        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

/// Walk the sync root, find files Explorer marks "Sync pending", queue uploads.
async fn scan_pending_uploads(
    sync_root: &std::path::Path,
    db: &std::sync::Arc<std::sync::Mutex<crate::db::database::SyncDatabase>>,
    sync_root_id: &str,
    queue: &std::sync::Arc<super::queue::SyncQueue>,
) {
    let mut stack = vec![sync_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }

            if !tether_cfapi::is_sync_pending(&path) {
                continue;
            }

            let rel = path
                .strip_prefix(sync_root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();

            let cloud_item_id = {
                let db = match db.lock() {
                    Ok(g) => g,
                    Err(_) => continue,
                };
                db.get_file_entry_by_path(sync_root_id, &rel)
                    .ok()
                    .flatten()
                    .and_then(|e| e.cloud_item_id)
            };

            let item_id = match cloud_item_id {
                Some(id) => id,
                None => continue,
            };

            let mut task = super::task::SyncTask::new(
                super::task::SyncOperation::Upload,
                super::task::SyncPriority::Normal,
                path.clone(),
            );
            task.cloud_item_id = Some(item_id);
            task.sync_root_id = Some(sync_root_id.to_string());
            task.sync_root_path = Some(sync_root.to_path_buf());
            queue.push(task).await;
        }
    }
}
