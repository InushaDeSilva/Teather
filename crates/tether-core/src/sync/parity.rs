use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Offline,
    Disabled,
    Reconnecting,
    Error,
}

impl ServiceState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Running => "running",
            ServiceState::Offline => "offline",
            ServiceState::Disabled => "disabled",
            ServiceState::Reconnecting => "reconnecting",
            ServiceState::Error => "error",
        }
    }

    pub fn from_db(value: &str) -> Self {
        match value {
            "running" => ServiceState::Running,
            "offline" => ServiceState::Offline,
            "reconnecting" => ServiceState::Reconnecting,
            "error" => ServiceState::Error,
            _ => ServiceState::Disabled,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    ConflictUpload,
    GetLatestConflict,
    DeleteConfirm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptResolution {
    KeepBoth,
    KeepLocal,
    KeepCloud,
    DeleteLocalOnly,
    DeleteLocalAndCloud,
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPayload {
    pub kind: PromptKind,
    pub relative_path: String,
    pub cloud_item_id: Option<String>,
    pub remote_head_version_id: Option<String>,
    pub message: String,
    pub is_directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineJournalPayload {
    pub operation: String,
    pub relative_path: String,
    pub cloud_item_id: Option<String>,
    pub destination_relative_path: Option<String>,
}

pub fn prompt_payload_json(payload: &PromptPayload) -> Result<String> {
    Ok(serde_json::to_string(payload)?)
}

pub fn offline_payload_json(payload: &OfflineJournalPayload) -> Result<String> {
    Ok(serde_json::to_string(payload)?)
}

pub fn recovery_root() -> PathBuf {
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(local_app_data).join("Tether").join("Recovery")
}

pub fn recovery_path_for(original: &Path) -> PathBuf {
    let stem = original
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("item");
    let ext = original
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    let file_name = if ext.is_empty() {
        format!("{stem}-{stamp}")
    } else {
        format!("{stem}-{stamp}.{ext}")
    };
    recovery_root().join(file_name)
}
