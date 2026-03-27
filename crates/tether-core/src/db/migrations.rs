use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

/// Initialises the database schema: creates tables and indices if they don't exist.
pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
        .context("Failed to run database migrations")?;
    info!("Database schema is up to date");
    Ok(())
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sync_roots (
    id TEXT PRIMARY KEY,
    hub_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    folder_id TEXT NOT NULL,
    local_path TEXT NOT NULL,
    display_name TEXT NOT NULL,
    last_full_sync TEXT,
    is_active INTEGER DEFAULT 1
);

CREATE TABLE IF NOT EXISTS file_entries (
    id TEXT PRIMARY KEY,
    sync_root_id TEXT NOT NULL REFERENCES sync_roots(id),
    local_relative_path TEXT NOT NULL,
    cloud_item_id TEXT,
    cloud_version_id TEXT,
    cloud_storage_urn TEXT,
    local_hash TEXT,
    cloud_hash TEXT,
    file_size INTEGER,
    last_local_modified TEXT,
    last_cloud_modified TEXT,
    sync_state TEXT NOT NULL DEFAULT 'unknown',
    is_placeholder INTEGER DEFAULT 1,
    is_directory INTEGER DEFAULT 0,
    last_sync_attempt TEXT,
    last_sync_error TEXT,
    UNIQUE(sync_root_id, local_relative_path)
);

CREATE TABLE IF NOT EXISTS auth_state (
    id INTEGER PRIMARY KEY DEFAULT 1,
    client_id TEXT NOT NULL,
    access_token_encrypted BLOB,
    refresh_token_encrypted BLOB,
    token_expiry TEXT,
    user_id TEXT,
    user_email TEXT,
    user_name TEXT
);

CREATE TABLE IF NOT EXISTS activity_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp TEXT NOT NULL DEFAULT (datetime('now')),
    operation TEXT NOT NULL,
    file_path TEXT,
    cloud_item_id TEXT,
    status TEXT NOT NULL,
    details TEXT,
    bytes_transferred INTEGER
);

CREATE INDEX IF NOT EXISTS idx_file_entries_path
    ON file_entries(sync_root_id, local_relative_path);
CREATE INDEX IF NOT EXISTS idx_file_entries_cloud_id
    ON file_entries(cloud_item_id);
CREATE INDEX IF NOT EXISTS idx_file_entries_state
    ON file_entries(sync_state);
CREATE INDEX IF NOT EXISTS idx_activity_log_timestamp
    ON activity_log(timestamp DESC);
"#;
