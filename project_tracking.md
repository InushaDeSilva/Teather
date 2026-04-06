# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

## 1. Project Overview & Context
Tether is an experimental custom cloud sync engine designed to act as a behaviorally accurate replacement for the official **Autodesk Desktop Connector**. It targets **ARM64 Windows environments** (such as Snapdragon X laptops) where official x64 Autodesk software is completely unsupported. 

Tether acts as a low-level, virtual OS file system seamlessly bridging the user's localized Windows File Explorer with the strict **Autodesk Platform Services (APS - formerly Forge) Data Management API**. 

## 2. Core Operational Systems (For Brand New Models)

### 2.1 Virtual File System & CFAPI (Dynamic Hydration)
The app leverages the **Windows Cloud Files API (CFAPI)** via our custom `tether-cfapi` proxy. This mounts a virtual drive directly into the Windows File Explorer Navigation Pane.
- **`fetch_placeholders` (Wired & Working)**: Syncs lightweight, 0-byte directory structures directly from the Autodesk Cloud. Users see their files natively in Explorer (with cloud icons) but no storage is consumed.
- **Dynamic Hydration (`fetch_data`) (Wired & Working)**: When a user double-clicks an online-only placeholder, Windows intercepts the file launch and signals Tether. Tether holds the application briefly while it hits the Autodesk S3 buckets, downloads the payload bytes, and fulfills the stream transparently.
- **Hydration Tracking (Wired & Working)**: Our SQLite database pairs with CFAPI callbacks (`on_hydration_complete`) to log if a file is `online_only`, `hydrated_ephemeral` (downloaded), or `hydrated_pinned` (Always keep on this device).

### 2.2 Syncing, Indexing & Uploading
- **Local Indexing (Wired & Working)**: We run a `notify` filesystem watcher backed by a `debouncer`. File modifications queue a `SyncOperation::Upload` into background workers (`worker.rs`). 
- **Cloud Polling (Wired & Working)**: The remote APS cloud is polled every 30 seconds for remote changes on active datasets.
- **CAD Save-Patterns (Wired & Working)**: Inventor save patterns (e.g., write `.new`, delete original, rename) are coalesced by `save_patterns.rs` to push a cohesive remote API version update without breaking history.
- **Conflict Management & Gating (Wired & Working)**: Enforced via `conflict.rs`. A local hash and a `base_remote_version_id` are maintained. The upload pipeline (`worker.rs`) refuses to overwrite a remote file if the remote version advanced ahead of the local file's origin copy, invoking a "Keep Both" collision handling logic.

### 2.3 Unified Mount Architecture
- **Single CFAPI Sync Root (Wired & Working)**: All Autodesk Drive projects are mounted under one unified CFAPI sync root at `%LOCALAPPDATA%\Tether\Drive`. Each enabled project appears as a top-level placeholder directory. This mirrors Autodesk Desktop Connector behavior instead of creating a separate mount point per project.
- **Auto-Discovery & Persistence (Wired & Working)**: On first launch, `auto_discover_drive_folders` enumerates all accessible project folders across non-personal hubs and saves them to `settings.json`. Subsequent launches restore from saved settings without re-discovery, preserving manual additions and removals.
- **Auto-Start on Boot (Wired & Working)**: The sync engine starts automatically on app launch if saved settings contain synced folders and a valid token. No user interaction required on subsequent launches.
- **Project Add/Remove (Wired & Working)**: Projects can be added by pasting an Autodesk Drive folder link. Removing a project immediately deletes its placeholder directory from disk. Orphaned directories (from projects removed while offline) are cleaned up on the next startup.
- **Fusion 360 Hub Filtering (Wired & Working)**: Personal hubs (`hubs:autodesk.a360:PersonalHub`) are filtered out during discovery. Only business/team hubs (`hubs:autodesk.core:Hub`) such as ACC and BIM360 are synced.

### 2.4 UI & Routing
- **Tauri Application Interface (Wired & Working)**: The frontend compiles into a lightweight system tray providing real-time Sync Status, OAuth PKCE login pipelines (`auth.rs`), an interior Database trouble-shooter panel, and bundled diagnostic `.zip` export generators (`diagnostics.rs`).
- **Explorer Shell Context Menus (Stubs Only - NOT WIRED NATIVELY)**: Natively right-clicking inside Windows Explorer to `Sync Now`, `Free Up Space`, or `View Online` is fundamentally **missing** for end-users. The API logic streams exist programmatically, but MSIX manifest stubs and deep Windows 11 shell integrations are pending.

## 3. Physical Limitations & Constraints Encountered
* **A360 Personal Hub (Autodesk Drive) File Deletion Restrictions:** 
   Autodesk APS rigidly prohibits third-party app access tokens from executing data destruction on Personal Hub domains to eliminate catastrophic data loss.
   - Using the `versions:autodesk.core:Deleted` payload fails with `400 BAD_INPUT`.
   - Applying `PATCH {"hidden": true}` artificially tricks the app with a `200 OK` return but server-side, it ignores the parameter and just updates the server timestamp (deceptive "Updated 1 minute ago" UI).
   - **Tether Action:** When a user deletes a file locally, the CFAPI placeholder deletes flawlessly alongside the local DB entry. However, Tether will actively swallow the deletion API failure. The file will permanently reside on Autodesk Drive until removed via the official Web Application.

---

## 4. Parity Backlog: Missing Desktop Connector Behaviors (TODO)
*(This formal checklist maps critical missing pieces required to mimic x64 official behaviors)*

### 4.0 Known Limitations (Current Build)
- **File deletion from Autodesk Cloud only via Web UI**: Autodesk APS blocks deletion on personal hubs (see §3). Business hub deletion works via `versions:autodesk.core:Deleted` payload.
- **Explorer shell context menus require MSIX packaging**: Right-click `Sync Now`, `Free Up Space`, `View Online` are in the backend but not wired to Windows shell — requires sparse manifest registration (Windows 11 only, separate workstream).
- **No native conflict/delete toast notifications yet**: Conflict resolution and delete confirmations are handled inside the Tether UI panel. Native Windows toast notifications with action buttons are designed but not yet wired.

### 4.1 Missing Explorer Semantics & Locking
- **Full Shell Context Pipeline**: Currently, tray/menu routing is decoupled. Wire the Context menu stubs fully natively via Windows 11 shell implementation standards (to expose `Free Up Space`, `Copy Link`, etc., on right click).
- **File Locks (DB and Enforcement)**: Hook APS Data Management locking endpoints. If a remote user is flagged as `"Locked by another member"`, set the CFAPI file blob strictly to Read-Only locally.

### 4.2 Missing Deep Reference Closure (Inventor CAD Parity)
- **Assembly Pre-Fetching**: `reference.rs` contains heuristic regex logic (`.iam`/`.ipt`), but `fetch_data` **does not** aggressively recurse down fetching requirements before unlocking the handle. When opening an assembly right now, children may be missed, causing Inventor to crash.
- **IPJ Bindings**: Assembly rules follow `.ipj` config references. File structures must bind to the `inventor_project_context` to resolve library pathways accurately.

### 4.3 Missing UX Polish & Edge Metadata
- **Metadata Viewer Panels**: Autodesk possesses a specific "Data Panel" that pulls extended attributes (Review Status, Modifiers, Standard flags). Needs a floating Tauri dialog pulling these corresponding APS API fields.
- **Bulk-Delete Guardrails**: If a user selects 5,000 files and hits delete natively, Tether must stop the queue and serve an "Are You Sure?" notification via Tauri.
- **Process for Viewing**: Add standalone API verbs to auto-generate Web Viewables for ACC since recent APIs uncoupled visual processing from generic file loads.

---
## Test Matrix (For QA)
1. Open online-only `.iam` file -> hydrate -> close -> reopen.
2. Mark file 'always keep' -> verify remote updates forcefully auto-download.
3. Remote modifies a file while the user's CFAPI blob indicates an open local handle.
