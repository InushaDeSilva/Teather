use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncedFolderConfig {
    pub hub_id: String,
    pub project_id: String,
    pub folder_id: String,
    pub display_name: String,
    pub enabled: bool,
}

/// Application settings, persisted to disk as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Autodesk APS client ID.
    pub client_id: String,

    /// OAuth redirect URI (default: http://localhost:8765/callback).
    pub redirect_uri: String,

    /// How often to poll for cloud changes (seconds).
    pub sync_interval_secs: u64,

    /// Maximum concurrent sync operations.
    pub max_concurrent_ops: usize,

    /// Max upload bandwidth in KB/s (0 = unlimited).
    pub bandwidth_limit_up_kbps: u64,

    /// Max download bandwidth in KB/s (0 = unlimited).
    pub bandwidth_limit_down_kbps: u64,

    /// Start Tether with Windows login.
    pub start_with_windows: bool,

    /// Log level filter (e.g. "info", "debug", "trace").
    pub log_level: String,

    /// Root mount path for the unified Autodesk Drive.
    pub drive_mount_path: Option<String>,
    
    /// Cached list of synced folder URNs (auto-discovered from home).
    pub synced_folders: Vec<SyncedFolderConfig>,
    
    /// Whether to filter out Fusion 360 hubs.
    pub filter_fusion_hubs: bool,
    
    /// Last successful authentication state.
    pub last_auth_state: Option<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            redirect_uri: "http://localhost:8765/callback".into(),
            sync_interval_secs: 30,
            max_concurrent_ops: 4,
            bandwidth_limit_up_kbps: 0,
            bandwidth_limit_down_kbps: 0,
            start_with_windows: false,
            log_level: "info".into(),
            drive_mount_path: None,
            synced_folders: Vec::new(),
            filter_fusion_hubs: true,
            last_auth_state: None,
        }
    }
}

impl AppSettings {
    /// Load settings from the standard config path, or create defaults.
    pub fn load() -> anyhow::Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let data = std::fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&data)?)
        } else {
            let settings = Self::default();
            settings.save()?;
            Ok(settings)
        }
    }

    /// Save settings to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)?;
        Ok(())
    }

    fn config_path() -> std::path::PathBuf {
        let local_app_data = std::env::var("LOCALAPPDATA")
            .unwrap_or_else(|_| ".".into());
        std::path::PathBuf::from(local_app_data)
            .join("Tether")
            .join("settings.json")
    }
}
