# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

## Updated implementation task + Desktop Connector behavior parity research

**Updated:** 2026-03-27  
**Goal:** turn the current Tether/Tether-CFAPI implementation into a behaviorally accurate replacement for Autodesk Desktop Connector for Inventor/assembly-heavy workflows, especially Autodesk Drive / Docs style Windows Explorer usage.

---

## 1) Current implementation snapshot

### Already implemented
- Cargo workspace and Tauri app shell
- OAuth PKCE login flow
- APS hub / project / folder browsing
- Aggregated Drive view + hidden folder resolution by URL/URN
- Core sync engine scaffolding
- SQLite state database
- Local watcher via `notify` + debouncer
- Cloud polling at 30 s
- Background download worker
- System tray + sync status UI
- CFAPI registration and session wiring

### Still missing from the original task *(see [task.md](task.md) for live parity backlog)*
- Full **upload** pipeline with stale-base checks on every path
- **Explorer shell** context menus (MSIX manifest stubs exist; full Windows 11 integration ongoing)
- **Lock/unlock** APS workflows + read-only enforcement
- Rich **metadata / Data Panel** (fields + API wiring incremental)
- **Bulk-delete** blocking queue + admin thresholds (DB `pending_jobs` ready)
- **Export…** verb — verify on x64 Desktop Connector before implementing
- **Auto-update** / **Inventor add-in**
- **Webhooks** (optional vs polling)

**Implemented since this snapshot:** stale-base evaluation + keep-both download path, DB hydration/pin/lock columns, `save_patterns` coalescer hook, `reference` + `sync_now`, `urls` (view online / copy link), CFAPI `on_hydration_complete`, dehydration helper, cloud poller skips `online_only` auto-pull, troubleshooter + diagnostics ZIP, IPJ persistence, parity test matrix doc.

---

## 2) Official Desktop Connector behavior model we need to match

This section is the important part. The existing codebase is mostly a sync skeleton. What is still missing is **behavior parity** with how Autodesk Desktop Connector actually behaves in Windows Explorer.

### 2.1 Core product model
Desktop Connector is not a dumb mirror-sync client. It is a **virtual-drive, on-demand, reference-aware cloud file system**.

Behavioral pillars:
1. It exposes Autodesk cloud data inside Windows File Explorer as a local-looking connector.
2. It **does not fully pre-sync everything** by default; it downloads on demand.
3. It has **reference awareness** for CAD/design datasets.
4. It uses local file state to decide whether cloud changes should continue being pulled automatically.
5. It integrates actions directly into Explorer and the Desktop Connector home screen/troubleshooter.

### 2.2 Sync cadence / freshness model
Observed official behavior:
- For active projects, Desktop Connector checks cloud changes every **30 seconds**.
- For inactive projects, it checks every **5 minutes**.
- Once a file has been opened locally, Desktop Connector keeps downloading newer cloud versions of that file until the user explicitly frees up space on that file.
- If a file is marked **Always keep on this device**, updates download automatically even if the file has never been opened locally.

**Implication for Tether:** hydration state is not just a cache flag. It changes ongoing remote refresh behavior.

### 2.3 On-demand local state model
Desktop Connector has multiple file states, not just "exists / not exists":
- **Online only**: visible in Explorer, not downloaded yet
- **Downloaded**: downloaded because it was opened or synced
- **Always keep on this device**: pinned; always retained locally and auto-updated
- **Processing / syncing**: upload/download in progress
- **Locked by you / locked by another member**

Tether needs a real internal state machine for this:
- `online_only`
- `hydrated_ephemeral`
- `hydrated_pinned`
- `uploading`
- `downloading`
- `conflict`
- `error`
- `locked_by_me`
- `locked_by_other`

---

## 3) Exact Windows / Explorer behavior to implement

## 3.1 Open / double-click behavior
### Expected Desktop Connector behavior
When the user opens a placeholder file:
1. Hydrate the file on demand.
2. Preserve Explorer progress / in-progress feedback.
3. Open the file in the associated authoring application.
4. Keep the local file around afterward as a downloaded copy.
5. Continue pulling newer cloud versions of that file later unless the user frees up space.

### Tether gap
- CFAPI `fetch_data` is not implemented.
- There is no full file hydration pipeline yet.
- No post-open state transition to “downloaded and keep refreshing”.

### Required work
- Implement `fetch_data`.
- Record hydration reason (`opened`, `manual_sync`, `pinned`).
- Persist hydrated/pinned state in SQLite.
- After successful hydration, mark file as downloaded and subscribe it to background refresh behavior.

---

## 3.2 Sync command behavior
### Expected Desktop Connector behavior
`Sync` is not just “download this single file blob.” In reference-aware workflows, Autodesk documents that right-clicking the **host design file** and using **Sync** or **Always keep on this device** can identify and fetch the files needed for references to resolve.

For assemblies / design hosts, Sync should behave more like:
1. Resolve dataset root / host identity.
2. Analyze references.
3. Pull host + needed referenced children.
4. Leave dataset coherent enough to open / inspect correctly.

### Tether gap
- Current polling/downloader is file-centric, not dataset-centric.
- No dependency graph prefetch.
- No explicit Sync verb.

### Required work
- Add a real `Sync Now` operation for file/folder.
- For supported CAD hosts, expand task from one file to a **reference closure** / dependency set.
- Expose manual Sync from Explorer context menu and app UI.

---

## 3.3 Always keep on this device
### Expected Desktop Connector behavior
This is a **pin** semantic, not merely “download once.”
- File/folder becomes always available locally.
- Future remote updates auto-download.
- Storage Sense / free-up automation must not dehydrate pinned files.
- For folders, child files must inherit effective pinned availability semantics.

### Tether gap
- No pinned state model.
- No recursive pin support.
- No dehydrate guard.

### Required work
- Add persistent `pin_state` in DB.
- Add recursive pin for folders.
- Block auto-dehydration for pinned files.
- Ensure remote updates to pinned files always hydrate/refresh locally.

---

## 3.4 Free up space
### Expected Desktop Connector behavior
`Free up space` removes the **local** copy only, while preserving the cloud item.
- File remains visible in Explorer.
- Placeholder remains.
- Next open rehydrates on demand.
- Folder-level free-up must work recursively.
- Pinned files must be excluded.

### Tether gap
- No dehydrate pipeline.
- No folder-level recursive dehydrate.
- No pinned protection.

### Required work
- Implement placeholder dehydration.
- Add recursive `Free Up Space` for folders.
- Respect open-file constraints and pending transfer constraints.
- Preserve metadata/identity while discarding local payload.

---

## 3.5 Delete behavior
### Expected Desktop Connector behavior
Delete is dangerous and is not the same as Free up space.
- Online mode: Delete removes the file from both local connector and cloud source.
- Desktop Connector shows delete notifications / confirmation behavior.
- Some delete flows can halt syncing until the delete prompt is addressed.
- Offline mode uses a different semantic (`Delete Local`) that removes only the local copy.

### Tether gap
- No robust delete policy or confirmation workflow.
- No “online delete vs offline local-only delete” split.
- No bulk delete guardrail.

### Required work
- Implement 3 delete modes:
  1. `Delete` = cloud + local
  2. `Delete Local` = local-only while offline / forced-local mode
  3. `Free Up Space` = dehydrate but keep placeholder
- Add bulk-delete confirmation and threshold logic.
- Add sync suspension / explicit user acknowledgment flow for destructive actions.
- Add recovery-safe local backup of unsynced changes before destructive reconciliations.

---

## 3.6 Rename / move behavior
### Expected Desktop Connector behavior
Rename and move should behave like normal Explorer operations, but propagate to cloud and preserve project structure semantics.

Important nuances:
- Project-level folder renames often must be done in the cloud product, not via local Explorer.
- Shared folder permissions may restrict rename/move.
- Some long-path and duplicate-name cases are explicitly unsupported / error-prone.

### Tether gap
- `notify_rename` not yet implemented.
- No cloud move/rename propagation pipeline.
- No project-level restriction handling.

### Required work
- Implement CFAPI/local rename ack + queued cloud rename.
- Distinguish rename vs move.
- Block prohibited renames/moves with clear message.
- Detect duplicate name cases before upload/commit where possible.

---

## 3.7 View Online / Copy Link
### Expected Desktop Connector behavior
- **View Online** should navigate directly to the item’s location in the cloud product.
- **Copy Link** should copy a shareable web/cloud link where supported.
- Permission-scoped behaviors differ by connector and share type.

### Tether gap
- No native context verbs yet.
- No cloud-web deep-link generation layer.

### Required work
- Add canonical web URL generator for Docs / Drive / Fusion item contexts.
- Add `View Online` and `Copy Link` verbs.
- Respect connector type + permission type.

---

## 3.8 Lock / unlock behavior
### Expected Desktop Connector behavior
For Autodesk Docs workflows:
- It attempts **automatic locking on open** and **automatic unlock on close**.
- Manual Lock/Unlock is available from Explorer.
- Automatic locking typically applies to the file opened directly, **not referenced files**.
- If lock by another user exists, local file becomes effectively read-only.

### Tether gap
- No lock state model.
- No lock API flow.
- No read-only enforcement from remote lock state.

### Required work
- Add lock state to DB and status pipeline.
- Add auto-lock on open for supported connector types.
- Add auto-unlock on close.
- Add manual Lock/Unlock verbs.
- Apply read-only local semantics when file is locked by another user.

---

## 3.9 Process for viewing
### Expected Desktop Connector behavior
As of the 2025-09-01 behavior change, files uploaded through Desktop Connector to Docs generally **do not auto-generate viewables**. Instead the user must explicitly trigger **Process for viewing** (older versions: Create Viewable).

### Tether gap
- No equivalent verb.
- No distinction between “file uploaded successfully” vs “file processed for web view.”

### Required work
- Add optional `Process for Viewing` verb for Docs-type connectors.
- Treat this as a separate post-upload cloud action, not part of sync completion.
- Surface “upload complete but not processed” clearly in UI.

---

## 3.10 Export
### Observed state
Your screenshot shows an **Export...** verb under the Desktop Connector flyout. Public Autodesk documentation accessible during this research did **not** clearly document the exact semantics of Export across all connector types.

### Decision
Treat Export as **connector-specific / item-type-specific and currently unverified**.

### Required work
- Do not guess behavior.
- Add a live validation task against real Desktop Connector on x64:
  - Docs connector
  - Drive connector
  - Fusion connector
  - file vs folder
  - assembly vs generic document
- Only implement after the behavior is captured exactly.

---

## 4) Inventor / assembly / dependency behavior

This is the main missing piece for your use case.

## 4.1 Reference-aware behavior
Official Autodesk docs and release notes show that Desktop Connector is **dependency aware** for CAD files and assemblies.

Important consequences:
- Upload workflows interrogate files for references.
- Sync / Always Keep can be used to gather files needed for references.
- Reference Explorer expects the host and dependencies to be present/synced to display relationships.
- Broken or missing dependency discovery results in incomplete uploads / incomplete opens.

### Tether gap
- Current implementation is transport-oriented, not dependency-graph-oriented.
- No CAD reference graph service.

### Required work
- Add a **reference resolver service** abstraction:
  - `get_dependency_graph(host_file)`
  - `prefetch_dependency_closure(host_file)`
  - `validate_missing_dependencies(host_file)`
- Drive this from:
  - manual Sync
  - open-host workflow
  - upload workflow
  - status/troubleshooter

---

## 4.2 Inventor `.ipj` behavior
Official Autodesk behavior around Inventor is more specific than generic CAD:
- Inventor workflows may require selecting an **Inventor Project File (`.ipj`)**.
- Upload with References prompts for IPJ selection when Inventor files are detected.
- Changing IPJ reloads dataset/reference resolution.
- Desktop Connector-created project files may need library-path customization.
- For Fusion workflows, opening Inventor files uses the default project `.ipj` for the project.

### Tether gap
- No IPJ-aware resolution logic.
- No project-context binding for assembly opens.
- No library-path handling.

### Required work
- Add an **Inventor project context** object in DB and runtime.
- On first assembly open / sync, determine active IPJ.
- Persist `(sync_root, folder, dataset_root) -> ipj path / ipj rules`.
- Add missing-library diagnostics and recommended fix paths.
- For shared folders / assembly datasets, ensure references do not silently span unsupported roots.

---

## 4.3 Open assembly from web / fetch related files
This exact end-user experience is not documented as a single Autodesk help article, but the documented pieces imply the behavior target:
- opening / syncing a host design file should produce a **coherent local dataset**, not a single isolated blob
- reference-aware commands need to find and fetch required related files
- Inventor/IPJ context affects whether references resolve correctly

### Tether target behavior
When the user invokes an assembly open from a web link / Drive URL / Explorer placeholder:
1. Resolve the host item.
2. Resolve dataset context (folder root + IPJ if applicable).
3. Discover dependency closure.
4. Prefetch required files before launch.
5. Only launch Inventor when minimum-open dependency set is present.
6. Continue background-prefetch for deeper dependencies after launch if needed.

### Required work
- Add an explicit **open-with-prefetch pipeline**.
- Return structured status to UI:
  - `resolving references`
  - `fetching 7/24 files`
  - `launching Inventor`
  - `background fetching remaining dependencies`

---

## 5) Freshness, overwrite prevention, and conflict handling

This is the second biggest parity gap.

## 5.1 Stale-base edit behavior
Official Autodesk support documents explicitly describe “you have made edits to an old version...” conflict cases.

### Required Tether rule
Never silently upload over a newer remote version when the local file was based on an older cloud revision.

Tether must store for each local working file:
- `base_remote_version_id`
- `base_remote_modified_at`
- `last_local_seen_hash`
- `last_local_modified_at`

Before upload, compare current remote head to `base_remote_version_id`.

If remote head changed and local changed too:
- trigger conflict
- default to **keep both**
- never overwrite silently

---

## 5.2 Keep-both default
Recommended default parity behavior:
- Preserve local modified file
- Download remote latest as sibling file or safety copy
- Upload local as separate/safe resolution path, or keep local as conflict copy pending user resolution
- Notify user clearly

### Tether gap
- `conflict.rs` not implemented.

### Required work
- Implement `conflict.rs` now, not later.
- Add deterministic conflict naming.
- Preserve recoverability.
- Add local recycle/recovery handling for failed conflict resolutions.

---

## 5.3 Delete-and-recreate authoring app patterns
Autodesk explicitly calls out that some authoring apps save by deleting/recreating files, and Desktop Connector has special handling for this because naive handling can:
- create false delete warnings
- reset cloud versioning
- break history

### Tether gap
- current `notify`-based watcher will misinterpret some save patterns unless special-cased

### Required work
- Implement save-pattern normalization for major authoring workflows:
  - temp file + rename replace
  - delete old + create new same name
  - write hidden temp + swap
- Coalesce into one logical `update existing item` operation instead of delete+create.

---

## 6) Metadata, columns, and Explorer information model

## 6.1 Default Explorer columns / properties
Desktop Connector exposes file/folder information in Explorer and supports customized columns.

Base/default file attributes include:
- Name
- Status
- Date modified
- Type
- Size
- Date created

Docs-specific docs also note that File Size in Desktop Connector can reflect the server-side file size.

### Tether target
At minimum, support:
- Name
- Status
- Date modified
- Type
- Size
- Date created
- cloud item ID (internal)
- cloud version ID (internal)
- local hydration state (internal)
- lock state

---

## 6.2 Data Panel behavior
Recent Desktop Connector releases introduced a **Data Panel** showing cloud-side file metadata directly from Explorer selection.

Available / described properties include:
- Description
- Version
- Date Created in Autodesk Docs
- Date Modified in Autodesk Docs
- Last Modified By
- Lock Status
- Locked By
- Naming Standard
- Revit Cloud Model flag
- Thumbnail

Limitations called out by Autodesk:
- only individual files, not folders/projects
- only connector-backed files
- tied to Desktop Connector running properly

### Tether gap
- no metadata side panel
- no author / modifier metadata plumbing
- no selection-linked explorer-side metadata UI

### Required work
- Add metadata fetch pipeline keyed by selected item.
- Expose at least:
  - version
  - last modified by
  - cloud created/modified timestamps
  - lock state
  - thumbnail if available
- Add a Tauri side panel or details popout first; Explorer shell property integration can come later.

---

## 7) Error handling and troubleshooting parity

## 7.1 Home screen / pending actions model
Desktop Connector surfaces failures through:
- Home Screen / tray UI
- Pending Actions / Failed - action required
- Troubleshooting tool
- delete notifications that can block syncing until resolved

### Tether target
Need a first-class **operation journal + repair UI**, not just logs.

Minimum structure:
- queued jobs
- active jobs
- completed jobs
- failed jobs
- blocked jobs requiring user action

---

## 7.2 Troubleshooter behavior
Official docs show Desktop Connector has a built-in troubleshooting flow and diagnostics/log export.

### Required Tether behavior
Add a troubleshooting screen that can explicitly surface:
- unsynced local files
- failed uploads/downloads
- duplicate filename/path problems
- missing references
- long-path issues
- lock issues
- stale placeholder/db mismatch
- permission-denied cloud actions

Should support:
- `Retry`
- `Retry all`
- `Open containing folder`
- `View online`
- `Collect logs`

---

## 7.3 Reset / unhealthy workspace recovery
Autodesk documents:
- unhealthy/bad environments
- cleanup utilities
- reset utilities
- workspace auto-rename on reinstall / upgrade
- duplicate connector-drive artifacts after crashes

### Tether target
Need explicit recovery features:
- soft rescan
- rebuild placeholder tree from DB/cloud
- reset local cache for a sync root
- quarantine/rename old workspace instead of deleting blindly
- collect zipped diagnostics

---

## 8) Known issues / edge cases to explicitly design for

These need to be in the task because Autodesk docs and release notes repeatedly mention them.

### 8.1 Long path issues
- some lock workflows unsupported
- upload/reference gathering can fail
- move/delete edge cases exist

**Task:** add path-length validation everywhere before queue commit.

### 8.2 Duplicate names / 8.3 alias collisions
- can create sync failures and duplicate logical identity issues

**Task:** normalize and detect duplicate logical file identities before processing.

### 8.3 Large CAD datasets
- Desktop Connector is not intended as a bulk migration tool
- reference gathering on large datasets is expensive

**Task:** add “migration / large-batch mode” separately from normal interactive sync.

### 8.4 Project selection / subscription limits
- Desktop Connector caps selected projects (80 combined Docs+Fusion)

**Task:** if Tether later exposes project subscriptions, preserve comparable resource limits and user warnings.

### 8.5 Shared folders / permissions
- shared folders can allow open/sync/free up space/view online/copy link but may restrict rename/delete/move depending on permissions

**Task:** permission-aware verb enable/disable logic in context menus.

### 8.6 Elevated process caveat
- Autodesk docs note some right-click commands do not appear if Desktop Connector runs elevated

**Task:** ensure shell extension / context menu strategy is not dependent on unsupported elevation assumptions.

---

## 9) Gap analysis against current Tether codebase

## 9.1 High-risk missing parity items
These are the biggest blockers for “feels like Desktop Connector”:
- [ ] CFAPI hydration / dehydration end-to-end
- [ ] pinned vs downloaded vs online-only state model
- [ ] Explorer context menu verbs
- [ ] stale-base conflict detection and keep-both flow
- [ ] delete notification / destructive action guardrails
- [ ] reference-aware assembly sync/open
- [ ] Inventor IPJ-aware dependency resolution
- [ ] lock/unlock behavior
- [ ] metadata / data panel behavior
- [ ] troubleshooting / recovery tooling

## 9.2 Medium-risk missing items
- [ ] folder-level Free Up Space semantics
- [ ] folder-level Sync semantics
- [ ] copy link / view online
- [ ] process for viewing
- [ ] local save-pattern normalization
- [ ] read-only enforcement for locked-by-other
- [ ] diagnostics bundle export

## 9.3 Lower-priority / verify-first items
- [ ] exact Export behavior by connector
- [ ] shell-level rich property columns beyond our own panel
- [ ] admin registry controls similar to Desktop Connector

---

## 10) Updated implementation tasks (actionable)

## P0 — do next
- [ ] Implement `hasher.rs`
- [ ] Implement `conflict.rs` with stale-base detection + keep-both default
- [ ] Add persistent `hydration_state`, `pin_state`, `base_remote_version_id`, `lock_state` to DB
- [ ] Implement CFAPI `fetch_data`
- [ ] Implement placeholder hydration completion -> state transitions
- [ ] Implement `notify_delete` / `notify_rename`
- [ ] Normalize save patterns so replace-save is treated as version update, not delete+create

## P1 — required for Desktop Connector parity
- [ ] Implement CFAPI `fetch_placeholders`
- [ ] Implement placeholder creation and recursive folder population
- [ ] Implement `Free Up Space` dehydrate flow for file and folder
- [ ] Implement `Always keep on this device` pin flow for file and folder
- [ ] Implement manual `Sync Now`
- [ ] Implement `View Online`
- [ ] Implement `Copy Link`
- [ ] Implement delete workflows:
  - [ ] Delete cloud+local
  - [ ] Delete Local
  - [ ] Free Up Space
- [ ] Add bulk delete confirmation thresholds

## P2 — Inventor/reference correctness
- [ ] Build reference graph abstraction for supported design files
- [ ] Add dataset-coherent sync for host assembly + dependencies
- [ ] Add Inventor IPJ context selection/persistence
- [ ] Add open-with-prefetch for assemblies
- [ ] Add missing-reference diagnostics
- [ ] Add shared-folder reference boundary checks

## P3 — metadata / UX / troubleshooting
- [ ] Add lock/unlock pipeline
- [ ] Add auto-lock on open / auto-unlock on close where supported
- [ ] Add read-only behavior for locked-by-other
- [ ] Add metadata side panel / Data Panel equivalent
- [ ] Add version / last-modified-by / lock-status / thumbnail display
- [ ] Add Pending Actions / Failed - action required UI
- [ ] Add Troubleshooter page with retry / retry-all / view-online / logs
- [ ] Add reset/rebuild/quarantine workspace tools

## P4 — verify-first items
- [ ] Capture exact Export behavior on x64 Desktop Connector for Docs / Drive / Fusion
- [ ] Confirm connector-specific differences in verbs by permission level
- [ ] Confirm folder-level menu availability differences across versions/connectors

---

## 11) Test matrix we should run against real Desktop Connector

## 11.1 Basic local file-state tests
- [ ] open online-only file -> hydrate -> close -> reopen
- [ ] mark file always keep -> verify remote update auto-downloads
- [ ] free up space on file -> verify placeholder remains
- [ ] free up space on folder -> verify recursive dehydrate
- [ ] delete file -> confirm cloud deletion prompt/workflow

## 11.2 Inventor dataset tests
- [ ] open simple `.ipt`
- [ ] open `.iam` with same-folder children
- [ ] open `.iam` with nested subfolder refs
- [ ] open `.iam` requiring `.ipj`
- [ ] move/rename referenced child
- [ ] upload same assembly with refs twice
- [ ] missing reference and circular reference handling

## 11.3 Conflict / concurrency tests
- [ ] remote modifies file while local is open
- [ ] local modifies stale base
- [ ] lock by another user then attempt local edit
- [ ] authoring app save-by-replace pattern
- [ ] interrupted upload + retry

## 11.4 Shell / UX tests
- [ ] Windows 11 context menu placement
- [ ] verbs visible when app not elevated
- [ ] icon/status transitions
- [ ] tray/home-screen pending actions
- [ ] diagnostics bundle generation

---

## 12) Notes on uncertain / not fully documented behavior

These are things we should not fake:
- Exact semantics of **Export...** in every connector context
- Exact web-initiated “open in connector” prefetch path for Inventor assemblies
- Some connector-specific differences between Docs / Drive / Fusion on the same verb

If behavior is not explicitly documented, capture it from:
1. live x64 Desktop Connector testing  
2. Explorer screenshots / ProcMon traces / UI recording  
3. only then implement parity

---

## 13) Key references used for this update

### Official Autodesk documentation / release notes
1. What is Desktop Connector  
   https://help.autodesk.com/view/CONNECT/ENU/?guid=What_Is_Desktop-Connector
2. Basic Troubleshooting  
   https://help.autodesk.com/view/CONNECT/ENU/?guid=Trouble_Shooting_Desktop_Connector
3. Syncing Troubleshooting  
   https://help.autodesk.com/cloudhelp/ENU/CONNECT-Troubleshooting/files/Troubleshooting_Sync.htm
4. Docs - Free Up Space  
   https://help.autodesk.com/view/CONNECT/ENU/?guid=Free_Up_Space_Autodesk_Docs_Connector
5. Docs - Manage Files and Folders  
   https://help.autodesk.com/cloudhelp/ENU/CONNECT-User-Guide/files/Files_Folders_Docs.htm
6. Docs - Attributes and Status Icons  
   https://help.autodesk.com/cloudhelp/ENU/CONNECT-User-Guide/files/Status-Icons_Properties_Docs.htm
7. About Reference Explorer  
   https://help.autodesk.com/view/CONNECT/ENU/?guid=About_Reference_Explorer
8. Upload with References  
   https://help.autodesk.com/view/CONNECT/ENU/?guid=Upload_With_References
9. Upload with References (workflow / IPJ selection)  
   https://help.autodesk.com/view/CONNECT/ENU/?contextId=Upload_With_References
10. Docs - File Locking  
    https://help.autodesk.com/cloudhelp/ENU/CONNECT-User-Guide/files/File_Locking_Docs_Connector.htm
11. Latest Release and Notes (v17.1.0.16, 2026-02-02)  
    https://help.autodesk.com/view/BUILD/ENU/?guid=GUID-03D59AAD-65B0-45E3-84F2-A12AAA5BB267&p=CONNECT
12. Historical Releases  
    https://help.autodesk.com/view/CONNECT/ENU/?guid=Historical_Releases_Desktop_Connector
13. On-Demand Viewable Creation / Process for Viewing  
    https://help.autodesk.com/view/DOCS/ENU/?caas=caas%2Fsfdcarticles%2Fsfdcarticles%2FWhat-is-On-Demand-Viewable-Creation-in-BIM-360-ACC-and-Its-Impact-on-Docs-and-Desktop-Connector.html
14. Process for Viewing  
    https://help.autodesk.com/view/DOCS/ENU/?guid=Process_For_Viewing
15. System Requirements (includes Windows on Arm unsupported note)  
    https://help.autodesk.com/view/CONNECT/ENU/?guid=System_Requirements_Desktop_Connector
16. Manage Projects  
    https://help.autodesk.com/cloudhelp/ENU/CONNECT-User-Guide/files/Add_Remove_Projects_Desktop_Connector.htm
17. Drive permissions / shared folders  
    https://help.autodesk.com/cloudhelp/ENU/CONNECT-User-Guide/files/Permissions_Drive.htm
18. Configure Inventor Project File Library Paths  
    https://help.autodesk.com/cloudhelp/NLD/CONNECT/files/GUID-66B1C43F-9BEE-4DB8-BB02-8EB7B43A6907.htm
19. Fusion / Inventor troubleshooting notes relevant to IPJ and upload-with-references  
    https://help.autodesk.com/view/fusion360/ENU/?guid=Troubleshoot_Inventor&p=CONNECT

### Internal project docs used for the current implementation snapshot
- `task_original.md`
- `Teather_Handover_Document_v2_Rust.md`

---

## 14) Bottom line

Right now Tether is best described as:
- a **working Autodesk cloud sync prototype**,
- with the basic API / auth / queue / watcher / tray foundations in place,
- but **without the key Explorer + assembly + conflict semantics** that make Desktop Connector behave correctly for real Inventor workflows.

The next milestone should **not** be “more generic sync.”  
It should be:

**“Make Tether behave like Desktop Connector for one real Inventor assembly workflow end-to-end.”**

That means the next implementation slice should include:
1. placeholder hydration  
2. pin/free-up-space semantics  
3. stale-base conflict handling  
4. reference-aware assembly prefetch  
5. IPJ-aware resolution
