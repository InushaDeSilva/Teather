# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

## Phase 1: Core Sync Engine (MVP)

### Project Setup
- [x] Create Cargo workspace with `tether-core` (lib) and `src-tauri` (Tauri app)
- [x] Initialize Tauri 2.x project with frontend (`src/`)
- [x] Add all dependencies per handover doc §7
- [x] Create project directory structure per handover doc §6

### APS API Client (`crates/tether-core/src/api/`)
- [x] OAuth 2.0 PKCE auth flow (`auth.rs`)
  - [x] Build auth URL with PKCE challenge
  - [x] Local TCP callback listener (port 8765)
  - [x] Token exchange + refresh
  - [x] Persist tokens to local JSON file
- [x] Auto-Login Flow (UI + Backend)
  - [x] Persist tokens via secure_storage
  - [x] Tauri command to check validity + auto-refresh
  - [x] Auto-bypass login screen if valid
- [x] Data Management API (`data_management.rs`)
  - [x] `get_hubs` — list hubs with pagination
  - [x] `get_projects` — list projects per hub with pagination
  - [x] `get_top_folders` — list top folders per project
  - [x] `get_folder_contents` — list items/subfolders with pagination
  - [x] `get_item_versions` — fetch item versions
  - [x] `create_item` — create new item in folder
  - [x] `create_version` — create new version of existing item
  - [x] `get_drive_view` — aggregated view: top folders + immediate subfolder children across all hubs/projects
  - [x] `resolve_folder_urn` — resolve any folder URN by searching across all hubs/projects (for hidden Drive folders)
- [x] Storage client (`storage.rs`) — S3 signed URL upload/download workflow
- [x] API models (`models.rs`) — serde DTOs for Hub, Project, Folder, Item, Version, TokenResponse, DriveItem

### State Database (`crates/tether-core/src/db/`)
- [x] SQLite schema via `rusqlite` (sync_roots, file_entries, auth_state, activity_log)
- [x] `database.rs` — CRUD operations
- [x] `migrations.rs` — schema creation

### Sync Engine (`crates/tether-core/src/sync/`)
- [x] `engine.rs` — main orchestrator (start sync, pause/resume, status, default folder selection)
- [x] `queue.rs` — priority queue + scheduler (max 4 concurrent: 2 up / 2 down)
- [x] `task.rs` — SyncTask, SyncOperation, SyncPriority, SyncTaskStatus
- [x] `change_detector.rs` — `notify` crate + `notify-debouncer-full` (3s debounce, exclusion rules)
- [x] `cloud_poller.rs` — periodic polling (30s) via `tokio::time::interval`
- [x] `worker.rs` — S3 download background loops
- [ ] `conflict.rs` — last-write-wins with safety copy
- [ ] `hasher.rs` — SHA-256 via `sha2` crate

### Configuration (`crates/tether-core/src/config/`)
- [x] `settings.rs` — settings model (AppSettings, load from defaults)
- [x] `secure_storage.rs` — local token file storage (`%LOCALAPPDATA%\Tether\tokens.json`)

### Tauri App (`src-tauri/`)
- [x] `lib.rs` — app entry, tray setup, plugin registration (shell, notification, positioner, single-instance, autostart)
- [x] `commands.rs` — `#[tauri::command]` handlers:
  - [x] `check_auth_status` — validate + refresh stored token
  - [x] `start_login` — launch OAuth flow in system browser
  - [x] `get_hubs` — list hubs with extension type info
  - [x] `get_projects` — list projects for a hub
  - [x] `get_folders` — list top folders for a project
  - [x] `get_drive_view` — aggregated Drive folder view with depth
  - [x] `get_subfolders` — browse subfolder contents of any folder
  - [x] `resolve_drive_folder` — resolve an Autodesk Drive URL folder URN
  - [x] `start_sync` — start sync for a hub/project/folder
  - [x] `get_sync_status` — poll current sync status + queue count
  - [x] `pause_sync` / `resume_sync` — toggle sync state
  - [x] `open_sync_folder` — open local sync folder in explorer
- [x] `tray.rs` — system tray icon setup
- [x] `state.rs` — shared app state (`Arc<Mutex<SyncEngine>>`)

### Frontend UI (`src/`)
- [x] `index.html` — single-page app with three views:
  - [x] Login view — "Sign in with Autodesk" button + status text
  - [x] Project picker view:
    - [x] Autodesk Drive section — flat view of all top folders + subfolders (depth-based grouping)
    - [x] Expandable folder tree — click ▶ to browse subfolders recursively, click folder name to sync
    - [x] Paste Drive URL — input field to add hidden Autodesk Drive folders by URL
    - [x] All Projects (Advanced) — hub → project tree with click-to-expand
  - [x] Sync status view — live polling (5s), activity list, open folder + pause/resume buttons
- [x] `styles/main.css` — dark theme styling

### Hub & Folder Discovery
- [x] Hub filtering — identify and prioritize Autodesk Drive (`hubs:autodesk.core:Hub`)
- [x] Paginated hub/project/folder enumeration — follows `links.next` for all list endpoints
- [x] Error logging in `get_drive_view` — warns on failed hub/project/folder fetches instead of silent swallowing
- [x] Hidden Drive folder support — `resolve_folder_urn` finds folders in hidden "Drive Project" roots not returned by `topFolders`

## Phase 2: CFAPI Placeholder Integration (Post-MVP)
- [x] Windows CFAPI Integration (`tether-cfapi` crate)
  - [x] Native Sync Root Registration (`registry.rs`)
  - [x] Wire up `cloud-filter` Session
- [/] CFAPI Callbacks
  - [ ] `fetch_placeholders` — sync directory tree
  - [ ] `fetch_data` — hydrate files on-demand
  - [ ] `notify_delete` / `notify_rename` — propagate local changes
- [ ] Placeholder creation, status overlays
- [ ] MSIX packaging via `winapp` CLI

## Phase 3: Polish (Future)
- [ ] Selective sync, bandwidth throttling, conflict UI
- [ ] Context menu integration, Inventor add-in, webhooks, auto-update
