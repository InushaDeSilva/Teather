use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, Row};
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

        conn.pragma_update(None, "journal_mode", "WAL")?;

        migrations::run_migrations(&conn)?;

        info!("Database opened at {}", path.display());
        Ok(Self { conn })
    }

    fn default_path() -> PathBuf {
        let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
        PathBuf::from(local_app_data).join("Tether").join("tether.db")
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

    pub fn find_sync_root(
        &self,
        hub_id: &str,
        project_id: &str,
        folder_id: &str,
        local_path: &str,
    ) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT id
             FROM sync_roots
             WHERE hub_id = ?1 AND project_id = ?2 AND folder_id = ?3 AND local_path = ?4
             ORDER BY (
                SELECT COUNT(*)
                FROM file_entries
                WHERE file_entries.sync_root_id = sync_roots.id
             ) DESC, rowid DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(
            rusqlite::params![hub_id, project_id, folder_id, local_path],
            |row| row.get::<_, String>(0),
        )?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_sync_root(&self, id: &str) -> Result<Option<SyncRootRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hub_id, project_id, folder_id, local_path, display_name, last_full_sync,
                    COALESCE(delete_prompt_pending, 0), last_poll_at, COALESCE(service_state, 'disabled')
             FROM sync_roots WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| Self::map_sync_root(row))?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_active_sync_roots(&self) -> Result<Vec<SyncRootRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, hub_id, project_id, folder_id, local_path, display_name, last_full_sync,
                    COALESCE(delete_prompt_pending, 0), last_poll_at, COALESCE(service_state, 'disabled')
             FROM sync_roots WHERE is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| Self::map_sync_root(row))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn map_sync_root(row: &Row<'_>) -> rusqlite::Result<SyncRootRow> {
        Ok(SyncRootRow {
            id: row.get(0)?,
            hub_id: row.get(1)?,
            project_id: row.get(2)?,
            folder_id: row.get(3)?,
            local_path: row.get(4)?,
            display_name: row.get(5)?,
            last_full_sync: row.get(6)?,
            delete_prompt_pending: row.get::<_, i64>(7)? != 0,
            last_poll_at: row.get(8)?,
            service_state: row.get(9)?,
        })
    }

    pub fn update_sync_root_service_state(&self, id: &str, service_state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sync_roots SET service_state = ?1 WHERE id = ?2",
            rusqlite::params![service_state, id],
        )?;
        Ok(())
    }

    // ── File entries ──

    pub fn upsert_file_entry(&self, entry: &FileEntryRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_entries
                (id, sync_root_id, local_relative_path, cloud_item_id, cloud_version_id,
                 cloud_storage_urn, local_hash, cloud_hash, file_size,
                 last_local_modified, last_cloud_modified, sync_state, is_placeholder, is_directory,
                 hydration_state, pin_state, lock_state, base_remote_version_id, base_remote_modified, hydration_reason)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
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
                is_placeholder = excluded.is_placeholder,
                hydration_state = excluded.hydration_state,
                pin_state = excluded.pin_state,
                lock_state = excluded.lock_state,
                base_remote_version_id = excluded.base_remote_version_id,
                base_remote_modified = excluded.base_remote_modified,
                hydration_reason = excluded.hydration_reason",
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
                entry.hydration_state,
                entry.pin_state,
                entry.lock_state,
                entry.base_remote_version_id,
                entry.base_remote_modified,
                entry.hydration_reason,
            ],
        )?;
        Ok(())
    }

    fn map_file_entry(row: &Row<'_>) -> rusqlite::Result<FileEntryRow> {
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
            hydration_state: row.get(14)?,
            pin_state: row.get(15)?,
            lock_state: row.get(16)?,
            base_remote_version_id: row.get(17)?,
            base_remote_modified: row.get(18)?,
            hydration_reason: row.get(19)?,
        })
    }

    /// Full column list for file_entries (post-migration).
    const FILE_ENTRY_SELECT: &'static str = "
        SELECT id, sync_root_id, local_relative_path, cloud_item_id, cloud_version_id,
               cloud_storage_urn, local_hash, cloud_hash, file_size,
               last_local_modified, last_cloud_modified, sync_state, is_placeholder, is_directory,
               hydration_state, pin_state, lock_state, base_remote_version_id, base_remote_modified, hydration_reason";

    pub fn get_file_entry_by_path(
        &self,
        sync_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<FileEntryRow>> {
        let sql = format!(
            "{} FROM file_entries WHERE sync_root_id = ?1 AND local_relative_path = ?2",
            Self::FILE_ENTRY_SELECT
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![sync_root_id, relative_path], |row| {
            Self::map_file_entry(row)
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_file_entry_by_cloud_item(
        &self,
        sync_root_id: &str,
        cloud_item_id: &str,
    ) -> Result<Option<FileEntryRow>> {
        let sql = format!(
            "{} FROM file_entries WHERE sync_root_id = ?1 AND cloud_item_id = ?2",
            Self::FILE_ENTRY_SELECT
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(rusqlite::params![sync_root_id, cloud_item_id], |row| {
            Self::map_file_entry(row)
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_all_file_entries(&self, sync_root_id: &str) -> Result<Vec<FileEntryRow>> {
        let sql = format!(
            "{} FROM file_entries WHERE sync_root_id = ?1",
            Self::FILE_ENTRY_SELECT
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params![sync_root_id], |row| Self::map_file_entry(row))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_hydration_state(
        &self,
        sync_root_id: &str,
        relative_path: &str,
        hydration_state: &str,
        is_placeholder: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE file_entries SET hydration_state = ?1, is_placeholder = ?2, hydration_reason = ?3
             WHERE sync_root_id = ?4 AND local_relative_path = ?5",
            rusqlite::params![
                hydration_state,
                if is_placeholder { 1 } else { 0 },
                reason,
                sync_root_id,
                relative_path
            ],
        )?;
        Ok(())
    }

    pub fn update_base_remote_version(
        &self,
        sync_root_id: &str,
        relative_path: &str,
        version_id: &str,
        cloud_modified: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE file_entries SET base_remote_version_id = ?1, base_remote_modified = ?2, cloud_version_id = ?1
             WHERE sync_root_id = ?3 AND local_relative_path = ?4",
            rusqlite::params![version_id, cloud_modified, sync_root_id, relative_path],
        )?;
        Ok(())
    }

    pub fn set_pin_state(&self, sync_root_id: &str, relative_path: &str, pinned: bool) -> Result<()> {
        let pin = if pinned { 1 } else { 0 };
        self.conn.execute(
            "UPDATE file_entries SET pin_state = ?1,
                 hydration_state = CASE WHEN ?1 = 1 THEN 'hydrated_pinned' ELSE hydration_state END
             WHERE sync_root_id = ?2 AND local_relative_path = ?3",
            rusqlite::params![pin, sync_root_id, relative_path],
        )?;
        Ok(())
    }

    pub fn remove_file_entry(&self, sync_root_id: &str, relative_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM file_entries WHERE sync_root_id = ?1 AND local_relative_path = ?2",
            rusqlite::params![sync_root_id, relative_path],
        )?;
        Ok(())
    }

    pub fn move_file_entry(
        &self,
        sync_root_id: &str,
        old_relative_path: &str,
        new_relative_path: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE file_entries
             SET local_relative_path = ?1
             WHERE sync_root_id = ?2 AND local_relative_path = ?3",
            rusqlite::params![new_relative_path, sync_root_id, old_relative_path],
        )?;
        Ok(())
    }

    // ── Inventor IPJ ──

    pub fn set_inventor_ipj(&self, sync_root_id: &str, ipj_path: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inventor_project_context (sync_root_id, ipj_path, updated_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(sync_root_id) DO UPDATE SET ipj_path = excluded.ipj_path, updated_at = datetime('now')",
            rusqlite::params![sync_root_id, ipj_path],
        )?;
        Ok(())
    }

    pub fn get_inventor_ipj(&self, sync_root_id: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ipj_path FROM inventor_project_context WHERE sync_root_id = ?1")?;
        let mut rows = stmt.query_map(rusqlite::params![sync_root_id], |row| row.get(0))?;
        Ok(rows.next().transpose()?)
    }

    // ── Pending jobs (troubleshooter / bulk delete) ──

    pub fn insert_pending_job(
        &self,
        sync_root_id: &str,
        job_type: &str,
        payload_json: Option<&str>,
        recovery_path: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO pending_jobs (id, sync_root_id, job_type, payload_json, status, recovery_path)
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
            rusqlite::params![id, sync_root_id, job_type, payload_json, recovery_path],
        )?;
        Ok(id)
    }

    pub fn list_pending_jobs(&self, status: &str, limit: usize) -> Result<Vec<PendingJobRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sync_root_id, job_type, payload_json, status, details, recovery_path, created_at
             FROM pending_jobs WHERE status = ?1 ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![status, limit], |row| {
            Ok(PendingJobRow {
                id: row.get(0)?,
                sync_root_id: row.get(1)?,
                job_type: row.get(2)?,
                payload_json: row.get(3)?,
                status: row.get(4)?,
                details: row.get(5)?,
                recovery_path: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn get_pending_job(&self, id: &str) -> Result<Option<PendingJobRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sync_root_id, job_type, payload_json, status, details, recovery_path, created_at
             FROM pending_jobs WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(PendingJobRow {
                id: row.get(0)?,
                sync_root_id: row.get(1)?,
                job_type: row.get(2)?,
                payload_json: row.get(3)?,
                status: row.get(4)?,
                details: row.get(5)?,
                recovery_path: row.get(6)?,
                created_at: row.get(7)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    pub fn update_pending_job(
        &self,
        id: &str,
        status: &str,
        details: Option<&str>,
        recovery_path: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE pending_jobs
             SET status = ?1,
                 details = COALESCE(?2, details),
                 recovery_path = COALESCE(?3, recovery_path)
             WHERE id = ?4",
            rusqlite::params![status, details, recovery_path, id],
        )?;
        Ok(())
    }

    pub fn insert_operation_journal(
        &self,
        sync_root_id: &str,
        operation_type: &str,
        relative_path: Option<&str>,
        payload_json: Option<&str>,
        recovery_path: Option<&str>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO operation_journal
                (id, sync_root_id, operation_type, relative_path, payload_json, status, recovery_path)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)",
            rusqlite::params![
                id,
                sync_root_id,
                operation_type,
                relative_path,
                payload_json,
                recovery_path
            ],
        )?;
        Ok(id)
    }

    pub fn list_operation_journal(
        &self,
        sync_root_id: &str,
        status: &str,
        limit: usize,
    ) -> Result<Vec<OperationJournalRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, sync_root_id, operation_type, relative_path, payload_json, status,
                    recovery_path, created_at, updated_at
             FROM operation_journal
             WHERE sync_root_id = ?1 AND status = ?2
             ORDER BY created_at ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(rusqlite::params![sync_root_id, status, limit], |row| {
            Ok(OperationJournalRow {
                id: row.get(0)?,
                sync_root_id: row.get(1)?,
                operation_type: row.get(2)?,
                relative_path: row.get(3)?,
                payload_json: row.get(4)?,
                status: row.get(5)?,
                recovery_path: row.get(6)?,
                created_at: row.get(7)?,
                updated_at: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn update_operation_journal_status(
        &self,
        id: &str,
        status: &str,
        payload_json: Option<&str>,
        recovery_path: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE operation_journal
             SET status = ?1,
                 payload_json = COALESCE(?2, payload_json),
                 recovery_path = COALESCE(?3, recovery_path),
                 updated_at = datetime('now')
             WHERE id = ?4",
            rusqlite::params![status, payload_json, recovery_path, id],
        )?;
        Ok(())
    }

    pub fn delete_operation_journal(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM operation_journal WHERE id = ?1",
            rusqlite::params![id],
        )?;
        Ok(())
    }

    pub fn set_app_setting_json(&self, key: &str, value_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_settings (key, value_json, updated_at)
             VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = datetime('now')",
            rusqlite::params![key, value_json],
        )?;
        Ok(())
    }

    pub fn get_app_setting_json(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value_json FROM app_settings WHERE key = ?1")?;
        let mut rows = stmt.query_map(rusqlite::params![key], |row| row.get(0))?;
        Ok(rows.next().transpose()?)
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
    pub delete_prompt_pending: bool,
    pub last_poll_at: Option<String>,
    pub service_state: String,
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
    pub hydration_state: String,
    pub pin_state: i64,
    pub lock_state: String,
    pub base_remote_version_id: Option<String>,
    pub base_remote_modified: Option<String>,
    pub hydration_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PendingJobRow {
    pub id: String,
    pub sync_root_id: String,
    pub job_type: String,
    pub payload_json: Option<String>,
    pub status: String,
    pub details: Option<String>,
    pub recovery_path: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct OperationJournalRow {
    pub id: String,
    pub sync_root_id: String,
    pub operation_type: String,
    pub relative_path: Option<String>,
    pub payload_json: Option<String>,
    pub status: String,
    pub recovery_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
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

impl Default for FileEntryRow {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sync_root_id: String::new(),
            local_relative_path: String::new(),
            cloud_item_id: None,
            cloud_version_id: None,
            cloud_storage_urn: None,
            local_hash: None,
            cloud_hash: None,
            file_size: None,
            last_local_modified: None,
            last_cloud_modified: None,
            sync_state: "unknown".into(),
            is_placeholder: true,
            is_directory: false,
            hydration_state: "online_only".into(),
            pin_state: 0,
            lock_state: "none".into(),
            base_remote_version_id: None,
            base_remote_modified: None,
            hydration_reason: None,
        }
    }
}
