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

use super::change_detector::ChangeDetector;
use super::local_indexer;
use super::parity::ServiceState;
use super::queue::SyncQueue;
use super::save_patterns::SavePatternCoalescer;

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
    pub service_state: ServiceState,
    pub sync_root_path: Option<PathBuf>,
    /// Row id in `sync_roots` for the active session.
    pub sync_root_db_id: Option<String>,
    pub current_hub_id: Option<String>,
    pub current_project_id: Option<String>,
    pub current_root_folder_id: Option<String>,
    pub cf_connection: Option<tether_cfapi::Connection<tether_cfapi::TetherSyncFilter>>,
    local_change_detector: Option<ChangeDetector>,
    paused: bool,
}

impl SyncEngine {
    pub fn new(settings: AppSettings) -> Result<Self> {
        let auth = ApsAuthClient::new(settings.client_id.clone(), settings.redirect_uri.clone());
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
            service_state: ServiceState::Disabled,
            sync_root_path: None,
            sync_root_db_id: None,
            current_hub_id: None,
            current_project_id: None,
            current_root_folder_id: None,
            cf_connection: None,
            local_change_detector: None,
            paused: false,
        })
    }

    pub fn current_status(&self) -> SyncStatus {
        self.status.clone()
    }

    pub fn current_service_state(&self) -> ServiceState {
        self.service_state.clone()
    }

    pub fn sync_root_path(&self) -> Option<PathBuf> {
        self.sync_root_path.clone()
    }

    pub fn pause(&mut self) {
        self.paused = true;
        self.status = SyncStatus::Paused;
        info!("Sync paused");
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.paused = false;
        self.status = SyncStatus::Idle;
        self.set_service_state(ServiceState::Running).await?;
        info!("Sync resumed");
        Ok(())
    }

    pub async fn set_service_state(&mut self, service_state: ServiceState) -> Result<()> {
        self.service_state = service_state.clone();
        self.status = match service_state {
            ServiceState::Offline => SyncStatus::Offline,
            ServiceState::Disabled => SyncStatus::Paused,
            ServiceState::Error => SyncStatus::Error {
                message: "Service error".into(),
            },
            _ => SyncStatus::Idle,
        };
        if let Some(sync_root_id) = &self.sync_root_db_id {
            let db = self.db.lock().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            db.update_sync_root_service_state(sync_root_id, service_state.as_str())?;
        }
        if matches!(service_state, ServiceState::Running | ServiceState::Reconnecting) {
            if let (Some(sync_root), Some(sync_root_id)) =
                (&self.sync_root_path, &self.sync_root_db_id)
            {
                let replayed = local_indexer::replay_operation_journal(
                    sync_root,
                    &self.db,
                    &self.queue,
                    sync_root_id,
                )
                .await?;
                if replayed > 0 {
                    info!("Replayed {} offline journal operation(s)", replayed);
                }
            }
        }
        Ok(())
    }

    pub async fn start_unified(&mut self) -> Result<()> {
        // Idempotent — if CFAPI is already connected, skip re-initialisation.
        if self.sync_root_path.is_some() {
            info!("Unified sync already running — skipping re-init");
            return Ok(());
        }

        info!("Starting unified sync for Autodesk Drive");

        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        let default_mount = std::path::PathBuf::from(local_app_data)
            .join("Tether")
            .join("Drive");
            
        let sync_root = match &self.settings.drive_mount_path {
            Some(path) if !path.is_empty() => std::path::PathBuf::from(path),
            _ => default_mount,
        };

        std::fs::create_dir_all(&sync_root)?;
        self.sync_root_path = Some(sync_root.clone());
        self.status = SyncStatus::Idle;

        let synced_folders = self.settings.synced_folders.clone();

        let sync_root_db_id = {
            let db = self.db.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let local_path = sync_root.to_str().unwrap_or(".");
            if let Some(existing) = db.find_sync_root("unified", "unified", "unified", local_path)? {
                existing
            } else {
                db.insert_sync_root("unified", "unified", "unified", local_path, "Tether Drive")?
            }
        };
        self.sync_root_db_id = Some(sync_root_db_id.clone());
        self.current_hub_id = Some("unified".to_string());
        self.current_project_id = Some("unified".to_string());
        self.current_root_folder_id = Some("unified".to_string());

        let (upload_tx, mut upload_rx) = tokio::sync::mpsc::unbounded_channel();
        let save_patterns = Arc::new(Mutex::new(SavePatternCoalescer::new()));
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

        let provider = std::sync::Arc::new(super::cfapi_provider::ApsCloudProvider::new(
            tokio::runtime::Handle::current(),
            self.auth.clone(),
            self.data_mgmt.clone(),
            self.storage.clone(),
            synced_folders.clone(),
            Some(self.db.clone()),
            Some(sync_root_db_id.clone()),
            upload_tx,
            save_patterns.clone(),
        ));

        let provider_name = "Tether Drive".to_string();
        tether_cfapi::register_sync_root(&provider_name, "1.0.0", &sync_root)?;
        let connection = tether_cfapi::connect_sync_root(&sync_root, provider)?;
        self.cf_connection = Some(connection);
        info!("CFAPI virtual drive connected at {:?}", sync_root);

        super::worker::start_workers(
            2,
            self.queue.clone(),
            self.storage.clone(),
            self.data_mgmt.clone(),
            self.auth.clone(),
            self.db.clone(),
            "unified".to_string(),
        )
        .await;

        self.queue.clear_downloads().await;

        {
            let (tx, mut rx) = tokio::sync::mpsc::channel(100);

            let auth_clone = self.auth.clone();
            let data_clone = self.data_mgmt.clone();
            let db_clone = self.db.clone();
            let poll_root_id = sync_root_db_id.clone();
            let poll_sync_root = sync_root.clone();
            let interval_secs = self.settings.sync_interval_secs;

            tokio::spawn(async move {
                super::cloud_poller::start_polling(
                    interval_secs,
                    data_clone,
                    db_clone,
                    move || auth_clone.get_access_token().map_err(|e| anyhow::anyhow!("{e}")),
                    synced_folders,
                    poll_root_id,
                    poll_sync_root,
                    tx,
                )
                .await;
            });

            let root_clone = sync_root.clone();
            tokio::spawn(async move {
                while let Some(change) = rx.recv().await {
                    match change {
                        super::cloud_poller::CloudChange::Added {
                            cloud_item_id,
                            local_relative_path,
                            file_size,
                        } => {
                            let full = root_clone.join(local_relative_path.replace('/', "\\"));
                            if tether_cfapi::path_exists_no_recall(&full) {
                                continue;
                            }
                            if let Some(parent) = full.parent() {
                                if !tether_cfapi::path_exists_no_recall(parent) {
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
                            if !tether_cfapi::path_exists_no_recall(&full) {
                                continue;
                            }
                            // Skip dehydration for cloud-only files — File::open()
                            // would trigger hydration.
                            if tether_cfapi::is_cloud_only_attr(&full) {
                                continue;
                            }
                            match tether_cfapi::dehydrate_if_hydrated(&full) {
                                Ok(true) => {
                                    tracing::info!(
                                        "Dehydrated stale local copy: {}",
                                        local_relative_path
                                    );
                                }
                                Ok(false) => {}
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
                            if tether_cfapi::path_exists_no_recall(&full) {
                                let _ = std::fs::remove_file(&full);
                            }
                        }
                    }
                }
            });
        }

        self.local_change_detector = Some(local_indexer::start(
            tokio::runtime::Handle::current(),
            sync_root.clone(),
            self.db.clone(),
            self.queue.clone(),
            sync_root_db_id.clone(),
            save_patterns.clone(),
        )?);

        // Pre-create each enabled synced folder as a proper CFAPI placeholder
        // directory so it is visible in Explorer immediately on every launch —
        // even before CFAPI lazily calls fetch_placeholders for the root.
        // This MUST use the CFAPI placeholder API (not std::fs::create_dir_all)
        // so Windows knows to call fetch_placeholders when the user navigates in.
        for folder in &self.settings.synced_folders {
            if folder.enabled {
                let cloud_id = format!("{}|{}", folder.project_id, folder.folder_id);
                if let Err(e) = tether_cfapi::create_placeholder_dir(
                    &sync_root,
                    &folder.display_name,
                    &cloud_id,
                ) {
                    tracing::warn!(
                        "Could not create placeholder dir for '{}': {e}",
                        folder.display_name
                    );
                }
            }
        }

        self.set_service_state(ServiceState::Running).await?;
        local_indexer::reconcile_local_state(
            &sync_root,
            &self.db,
            &self.queue,
            &sync_root_db_id,
            &save_patterns,
        )
        .await?;
        Ok(())
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }
}
