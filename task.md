# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

## Phase 1: Core Sync Engine (MVP)

### Project Setup
- [x] Create Cargo workspace with `tether-core` (lib) and `src-tauri` (Tauri app)
- [x] Initialize Tauri 2.x project with frontend (`src/`)
- [x] Add all dependencies per handover doc §7
- [x] Create project directory structure per handover doc §6

### APS API Client (`crates/tether-core/src/api/`)
- [ ] OAuth### Phase 2: On-Demand Virtual File System (CFAPI)
- [x] Auto-Login Flow (UI + Backend)
  - [x] Persist tokens
  - [x] Tauri command to check validity
  - [x] Auto-bypass login screen if valid
- [x] Hub Filtering
  - [x] Identify Autodesk Drive
  - [x] Prioritize Autodesk Drive
- [x] Windows CFAPI Integration (`tether-cfapi`)
  - [x] Native Sync Root Registration
  - [x] Wire up `cloud-filter` Session
- [/] CFAPI Callbacks
  - [ ] `fetch_placeholders` - sync directory tree
  - [ ] `fetch_data` - hydrate files on-demandte
- [ ] Data Management API (`data_management.rs`) — hubs, projects, folders, items, pagination
- [ ] Storage client (`storage.rs`) — S3 signed URL upload/download workflow
- [ ] API models (`models.rs`) — serde DTOs for Hub, Project, Folder, Item, Version, TokenResponse

### State Database (`crates/tether-core/src/db/`)
- [ ] SQLite schema via `rusqlite` (sync_roots, file_entries, auth_state, activity_log)
- [ ] `database.rs` — CRUD operations
- [ ] `migrations.rs` — schema creation

### Sync Engine (`crates/tether-core/src/sync/`)
- [x] `engine.rs` — main orchestrator
- [x] `queue.rs` — priority queue + scheduler (max 4 concurrent: 2 up / 2 down)
- [x] `task.rs` — SyncTask, SyncOperation, SyncPriority, SyncTaskStatus
- [x] `change_detector.rs` — `notify` crate + `notify-debouncer-full` (3s debounce, exclusion rules)
- [x] `cloud_poller.rs` — periodic polling (30s) via `tokio::time::interval`
- [x] `worker.rs` - S3 download background loops
- [ ] `conflict.rs` — last-write-wins with safety copy
- [ ] `hasher.rs` — SHA-256 via `sha2` crate

### Configuration (`crates/tether-core/src/config/`)
- [ ] `settings.rs` — settings model
- [ ] `secure_storage.rs` — `keyring` crate wrapper

### Tauri App (`src-tauri/`)
- [ ] `main.rs` — app entry, tray setup, plugin registration
- [ ] `commands.rs` — `#[tauri::command]` handlers (get_sync_status, login, pause, etc.)
- [ ] `tray.rs` — system tray icon states + click handling
- [ ] `state.rs` — shared app state (`Arc<Mutex<SyncEngine>>`)
- [ ] `tauri.conf.json` — windows, tray, permissions

### Frontend UI (`src/`)
- [ ] `login.html` + `login.js` — OAuth login window
- [ ] `tray-popup.html` + `tray-popup.js` — status popup panel
- [ ] `settings.html` + `settings.js` — settings window
- [ ] `styles/main.css` — styling
- [ ] `index.html` — project picker / main window

## Phase 2: cfapi Placeholder Integration (Post-MVP)
- [ ] `tether-cfapi` crate — `cloud-filter` `SyncFilter` trait impl
- [ ] Sync root registration/unregistration
- [ ] Callback handlers (FETCH_PLACEHOLDERS, FETCH_DATA, NOTIFY_DELETE, NOTIFY_RENAME)
- [ ] Placeholder creation, status overlays
- [ ] MSIX packaging via `winapp` CLI

## Phase 3: Polish (Future)
- [ ] Selective sync, bandwidth throttling, conflict UI
- [ ] Context menu integration, Inventor add-in, webhooks, auto-update
