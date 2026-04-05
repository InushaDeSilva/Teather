# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

## 1. Project Overview & Context
Tether is an experimental custom cloud sync engine designed to act as a behaviorally accurate replacement for the official **Autodesk Desktop Connector**. It targets **ARM64 Windows environments** (such as Snapdragon X laptops) where official x64 Autodesk software is completely unsupported. 

Tether acts as a low-level, virtual OS file system seamlessly bridging the user's localized Windows File Explorer with the strict **Autodesk Platform Services (APS - formerly Forge) Data Management API**. 

It handles everything from deep file tree virtualizations for CAD assemblies (`.iam`/`.ipt`) to API caching and reference linking, making cloud data feel totally native.

## 2. Core Operational Systems (For Brand New Models)

### 2.1 Virtual File System & CFAPI (Dynamic Hydration)
The app leverages the **Windows Cloud Files API (CFAPI)** via our custom `tether-cfapi` proxy. This inserts our drive directly into the Windows File Explorer Navigation Pane.
- **`fetch_placeholders`**: Syncs lightweight, 0-byte directory structures directly from the Autodesk Cloud. Users see their files natively in Explorer (with cloud icons) but no storage is consumed.
- **Dynamic Hydration (`fetch_data`)**: When a user double-clicks an online-only placeholder, Windows intercepts the file launch and signals Tether. Tether holds the application briefly while it simultaneously hits the Autodesk S3 buckets, downloads the payload bytes, and fulfills the stream transparently.
- **Hydration Tracking**: Our SQLite database actively pairs with CFAPI callbacks (`on_hydration_complete`) to log if a file is `online_only`, `hydrated_ephemeral` (downloaded), or `hydrated_pinned` (Always keep on this device), dictating future sync behaviors.

### 2.2 Syncing, Indexing & Uploading
Bidirectional syncing dictates that changes locally are sent to the cloud, and cloud updates are fetched locally.
- **Local Indexing**: We run a `notify` filesystem watcher backed by a `debouncer`. Any changes (Create, Modify) trigger a local event that queues a `SyncOperation::Upload` into standard background workers. 
- **Wait/Polling**: The cloud is polled every 30 seconds for remote changes on active datasets.
- **CAD Save-Patterns**: CAD platforms like Inventor use esoteric save patterns. Instead of overwriting a file directly, Inventor might write `.new`, delete the original, and rename `.new` -> `.iam`. Tether (`save_patterns.rs`) catches and groups these discrete IO events back into a single cohesive remote API version push.
- **Conflict Management (Stale-Base Gating)**: We enforce extreme safety. A local hash and a remote version-pointer (`base_remote_version_id`) are kept. The upload pipeline (`conflict.rs`) refuses to overwrite a remote file if the remote version advanced ahead of the local file's origin copy. It instead invokes a "Keep Both" collision mechanism.

### 2.3 UI: Context Menus, Routing & Notifications
- **Explorer Shell Additions**: Users can right-click any mapped placeholder file in Windows File Explorer via shell extensions. Supported routing covers:
    - **Sync Now**
    - **Free up space** (Dehydrates the file, reclaiming disk space without hurting the cloud)
    - **Always keep on this device**
    - **View Online** (Resolves internal Autodesk URI identifiers and opens the exact browser pathway)
    - **Copy Link**
- **Tauri Application Interface**: The frontend compiles into a lightweight system tray. It tracks real-time Sync Status, manages the multi-legged OAuth PKCE browser tokens (`auth.rs`), serves custom desktop notifications, exposes an internal Database/Sync Trouble-shooter, and creates bundled diagnostic `.zip` logs.

---

## 3. Currently Implemented & Stabilized Architecture

We mapped a vast amount of underlying capabilities:
- **Rust Tauri & Cargo Pipeline**: Live and active in `.src-tauri` and `/crates/`.
- **Database (`crates/tether-core/src/db/`)**: Fully transactional SQLite schema actively persisting everything (`pending_jobs`, `activity_logs`, `inventor_project_contexts`).
- **Hub Navigation**: Can crawl Autodesk Hubs -> Projects -> Folders -> Items via `resolve_folder_urn`. Includes custom aggregation logic capable of combining disjointed A360 Personal Hub structures (Autodesk Drive) directly into a flat root.
- **CFAPI Defenses Corrected**:
    - Gracefully handles `0x8007018E` (User Canceled/Handle Stolen) events mid-hydration locally, eliminating `STATUS_STACK_BUFFER_OVERRUN` panics.
    - Prevents `UNIQUE constraint failed` SQLite crashes against dense metadata arrays (like Inventor `OldVersions` histories) by adopting a specialized, targeted DB `upsert` mechanism.
    - Suppressed background "Ghost Downloads" ensuring metadata search bots (e.g., Windows Search Indexer) cannot unilaterally trip non-CAD placeholder hydration locks.

---

## 4. Fundamental API Physical Constraints Enforced by Autodesk
* **A360 Personal Hub (Autodesk Drive) File Deletion Restrictions:** 
   According to APS Data Management APIs constraints, Autodesk has rigidly banned any third-party app access tokens from executing data destruction routines on Personal Hub domains to eliminate catastrophic data loss.
   - Attempting `POST /versions` with the standardized payload `versions:autodesk.core:Deleted` fails immediately with a `400 BAD_INPUT`.
   - Applying `PATCH {"hidden": true}` successfully returns `200 OK`, but server-side, it ignores the parameter completely and just updates the server timestamp, leading to deceptive "Updated 1 minute ago" ghosting.
   - **Tether Action:** When a user removes a file natively, the CFAPI placeholder deletes flawlessly alongside the local DB entry. However, the system bypasses infinite cloud requests. It will permanently reside on Autodesk Drive until removed via the Web Application.

---

## 5. Parity Backlog: Missing Desktop Connector Behaviors (TODO)
*(This is a formal checklist mapping what still needs to be built to mimic x64 official equivalents accurately)*

### 5.1 Deep Reference Closure (Inventor CAD Parity)
- **Problem**: When a user double-clicks an assembly (`.iam`), CFAPI fetches that 1 single blob. Autodesk Inventor will try to launch, crash instantly, or show missing parts (`.ipt`) because CFAPI missed the children.
- **Task**: The CFAPI `fetch_data` wrapper needs to halt the thread, perform an active Autodesk Graph dependency pre-fetch `get_dependency_graph(host_file)`, and aggressively recurse down fetching requirements before unlocking the handle.
- **IPJ Bindings**: Assembly rules follow `.ipj` config references. Open files must bind to the project context dynamically logic (`inventor_project_context`).

### 5.2 Deep Explorer Semantics & Locking
- **Full Shell Context Pipeline**: Currently, tray/menu routing is decoupled. Wire the Context menu stubs fully into the DB commands natively via Windows 11 shell implementation standards.
- **File Locks**: Hook APS Data Management locking endpoints. If a remote user is flagged as `"Locked by another member"`, flag the Windows file blob strictly Read-Only immediately. Allow users to auto-lock on open.

### 5.3 UX Polish & Edge Metadata
- **Metadata Viewer Panels**: Autodesk possesses a specific "Data Panel" that pulls extended attributes (Review Status, Modifiers, Standard flags). Route the APS extensions into a floating Tauri dialog when requested.
- **Bulk-Delete Guardrails**: If a user selects 5,000 files and hits delete, stop the job processor and serve an "Are You Sure?" notification via Tauri.
- **Process for Viewing**: Create standalone API verbs to auto-generate Web Viewables for ACC since recent APIs uncoupled visual processing from generic file loads.

### 5.4 Test Matrix
Before declaring a version viable, ensure behavior models match Desktop Connector through live comparisons:
- Open online-only file -> hydrate -> close -> reopen.
- Mark file 'always keep' -> verify remote updates forcefully auto-download.
- Remote modifies a file while the user's CFAPI blob indicates an open local handle.
