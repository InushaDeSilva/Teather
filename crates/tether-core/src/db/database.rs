use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;
use uuid::Uuid;

use super::migrations;

/// Wrapper around a SQLite connection for Tether's state database.
pub struct SyncDatabase {
    conn: Connection,
}

impl SyncDatabase {
    /// Open (or create) the database at the standard location.
    pub fn open_default() -> Result<Self> {
        let path = Self::default_path();
        Self::open(&path)
    }

    /// Open (or create) the database at a specific path.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {}", path.display()))?;

        // Enable WAL mode for better concurrent read performance.
        conn.pragma_update(None, "journal_mode", "WAL")?;

        migrations::run_migrations(&conn)?;

        info!("Database opened at {}", path.display());
        Ok(Self { conn })
    }

    fn default_path() -> PathBuf {
        let local_app_data =
            std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(local_app_data)
            .join("Tether")
            .join("tether.db")
    }

    // ── Sync roots ──

    pub fn insert_sync_root(
        &self,
        hub_id: &str,
        project_id: &str,
        folder_id: &str,
        local_path: &str,
        display_name: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO sync_roots (id, hub_id, project_id, folder_id, local_path, display_name)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, hub_id, project_id, folder_id, local_path, display_name],
        )?;
        Ok(id)
    }

    pub fn get_active_sync_roots(&self) -> Result<Vec<SyncRootRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hub_id, project_id, folder_id, local_path, display_name, last_full_sync
             FROM sync_roots WHERE is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SyncRootRow {
                id: row.get(0)?,
                hub_id: row.get(1)?,
                project_id: row.get(2)?,
                folder_id: row.get(3)?,
                local_path: row.get(4)?,
                display_name: row.get(5)?,
                last_full_sync: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ── File entries ──

    pub fn upsert_file_entry(&self, entry: &FileEntryRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_entries
                (id, sync_root_id, local_relative_path, cloud_item_id, cloud_version_id,
                 cloud_storage_urn, local_hash, cloud_hash, file_size,
                 last_local_modified, last_cloud_modified, sync_state, is_placeholder, is_directory)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(sync_root_id, local_relative_path) DO UPDATE SET
                cloud_item_id = excluded.cloud_item_id,
                cloud_version_id = excluded.cloud_version_id,
                cloud_storage_urn = excluded.cloud_storage_urn,
                local_hash = excluded.local_hash,
                cloud_hash = excluded.cloud_hash,
                file_size = excluded.file_size,
                last_local_modified = excluded.last_local_modified,
                last_cloud_modified = excluded.last_cloud_modified,
                sync_state = excluded.sync_state,
                is_placeholder = excluded.is_placeholder",
            rusqlite::params![
                entry.id,
                entry.sync_root_id,
                entry.local_relative_path,
                entry.cloud_item_id,
                entry.cloud_version_id,
                entry.cloud_storage_urn,
                entry.local_hash,
                entry.cloud_hash,
                entry.file_size,
                entry.last_local_modified,
                entry.last_cloud_modified,
                entry.sync_state,
                entry.is_placeholder,
                entry.is_directory,
            ],
        )?;
        Ok(())
    }

    pub fn get_file_entry_by_path(
        &self,
        sync_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<FileEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sync_root_id, local_relative_path, cloud_item_id, cloud_version_id,
                    cloud_storage_urn, local_hash, cloud_hash, file_size,
                    last_local_modified, last_cloud_modified, sync_state, is_placeholder, is_directory
             FROM file_entries
             WHERE sync_root_id = ?1 AND local_relative_path = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![sync_root_id, relative_path], |row| {
            Ok(FileEntryRow {
                id: row.get(0)?,
                sync_root_id: row.get(1)?,
                local_relative_path: row.get(2)?,
                cloud_item_id: row.get(3)?,
                cloud_version_id: row.get(4)?,
                cloud_storage_urn: row.get(5)?,
                local_hash: row.get(6)?,
                cloud_hash: row.get(7)?,
                file_size: row.get(8)?,
                last_local_modified: row.get(9)?,
                last_cloud_modified: row.get(10)?,
                sync_state: row.get(11)?,
                is_placeholder: row.get(12)?,
                is_directory: row.get(13)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_all_file_entries(&self, sync_root_id: &str) -> Result<Vec<FileEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sync_root_id, local_relative_path, cloud_item_id, cloud_version_id,
                    cloud_storage_urn, local_hash, cloud_hash, file_size,
                    last_local_modified, last_cloud_modified, sync_state, is_placeholder, is_directory
             FROM file_entries WHERE sync_root_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![sync_root_id], |row| {
            Ok(FileEntryRow {
                id: row.get(0)?,
                sync_root_id: row.get(1)?,
                local_relative_path: row.get(2)?,
                cloud_item_id: row.get(3)?,
                cloud_version_id: row.get(4)?,
                cloud_storage_urn: row.get(5)?,
                local_hash: row.get(6)?,
                cloud_hash: row.get(7)?,
                file_size: row.get(8)?,
                last_local_modified: row.get(9)?,
                last_cloud_modified: row.get(10)?,
                sync_state: row.get(11)?,
                is_placeholder: row.get(12)?,
                is_directory: row.get(13)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ── Activity log ──

    pub fn log_activity(
        &self,
        operation: &str,
        file_path: Option<&str>,
        cloud_item_id: Option<&str>,
        status: &str,
        details: Option<&str>,
        bytes_transferred: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO activity_log (operation, file_path, cloud_item_id, status, details, bytes_transferred)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![operation, file_path, cloud_item_id, status, details, bytes_transferred],
        )?;
        Ok(())
    }

    pub fn get_recent_activity(&self, limit: usize) -> Result<Vec<ActivityLogRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, operation, file_path, cloud_item_id, status, details, bytes_transferred
             FROM activity_log ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit], |row| {
            Ok(ActivityLogRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                operation: row.get(2)?,
                file_path: row.get(3)?,
                cloud_item_id: row.get(4)?,
                status: row.get(5)?,
                details: row.get(6)?,
                bytes_transferred: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}

// ── Row types ──

#[derive(Debug, Clone)]
pub struct SyncRootRow {
    pub id: String,
    pub hub_id: String,
    pub project_id: String,
    pub folder_id: String,
    pub local_path: String,
    pub display_name: String,
    pub last_full_sync: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileEntryRow {
    pub id: String,
    pub sync_root_id: String,
    pub local_relative_path: String,
    pub cloud_item_id: Option<String>,
    pub cloud_version_id: Option<String>,
    pub cloud_storage_urn: Option<String>,
    pub local_hash: Option<String>,
    pub cloud_hash: Option<String>,
    pub file_size: Option<i64>,
    pub last_local_modified: Option<String>,
    pub last_cloud_modified: Option<String>,
    pub sync_state: String,
    pub is_placeholder: bool,
    pub is_directory: bool,
}

#[derive(Debug, Clone)]
pub struct ActivityLogRow {
    pub id: i64,
    pub timestamp: String,
    pub operation: String,
    pub file_path: Option<String>,
    pub cloud_item_id: Option<String>,
    pub status: String,
    pub details: Option<String>,
    pub bytes_transferred: Option<i64>,
}
