use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

/// Initialises the database schema: creates tables and indices if they don't exist.
pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL)
        .context("Failed to run database migrations")?;
    migrate_file_entries_v2(conn)?;
    migrate_sync_roots_v2(conn)?;
    migrate_aux_tables(conn)?;
    info!("Database schema is up to date");
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, name: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    while let Some(r) = rows.next().transpose()? {
        if r == name {
            return Ok(true);
        }
    }
    Ok(false)
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    name: &str,
    ddl: &str,
) -> Result<()> {
    if !column_exists(conn, table, name)? {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {ddl};"))
            .with_context(|| format!("ALTER TABLE {table} ADD {name}"))?;
        info!("Migration: added column {table}.{name}");
    }
    Ok(())
}

/// Add Desktop Connector parity columns to existing databases.
fn migrate_file_entries_v2(conn: &Connection) -> Result<()> {
    add_column_if_missing(
        conn,
        "file_entries",
        "hydration_state",
        "hydration_state TEXT NOT NULL DEFAULT 'online_only'",
    )?;
    add_column_if_missing(conn, "file_entries", "pin_state", "pin_state INTEGER NOT NULL DEFAULT 0")?;
    add_column_if_missing(
        conn,
        "file_entries",
        "lock_state",
        "lock_state TEXT NOT NULL DEFAULT 'none'",
    )?;
    add_column_if_missing(
        conn,
        "file_entries",
        "base_remote_version_id",
        "base_remote_version_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "file_entries",
        "base_remote_modified",
        "base_remote_modified TEXT",
    )?;
    add_column_if_missing(
        conn,
        "file_entries",
        "hydration_reason",
        "hydration_reason TEXT",
    )?;
    Ok(())
}

fn migrate_sync_roots_v2(conn: &Connection) -> Result<()> {
    add_column_if_missing(
        conn,
        "sync_roots",
        "delete_prompt_pending",
        "delete_prompt_pending INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "sync_roots",
        "last_poll_at",
        "last_poll_at TEXT",
    )?;
    Ok(())
}

fn migrate_aux_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS pending_jobs (
    id TEXT PRIMARY KEY,
    sync_root_id TEXT NOT NULL REFERENCES sync_roots(id),
    job_type TEXT NOT NULL,
    payload_json TEXT,
    status TEXT NOT NULL DEFAULT 'queued',
    details TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_pending_jobs_status ON pending_jobs(status);

CREATE TABLE IF NOT EXISTS inventor_project_context (
    sync_root_id TEXT PRIMARY KEY REFERENCES sync_roots(id),
    ipj_path TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
"#,
    )?;
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
