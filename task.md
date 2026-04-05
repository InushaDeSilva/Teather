# Tether: Autodesk Cloud Sync for ARM64 (Rust + Tauri)

**Execution source of truth** — last aligned with `todo.md` (Desktop Connector parity) on 2026-03-27.

## Completed — Phase 1: Core sync engine (MVP)

### Project setup
- [x] Cargo workspace (`tether-core`, `src-tauri`)
- [x] Tauri 2.x + frontend
- [x] Dependencies per handover §7
- [x] Directory structure per handover §6

### APS API (`crates/tether-core/src/api/`)
- [x] OAuth 2.0 PKCE (`auth.rs`), secure storage, auto-login UI
- [x] Data Management: hubs, projects, folders, items, versions, Drive view, `resolve_folder_urn`
- [x] Storage (`storage.rs`), models (`models.rs`)

### Database (`crates/tether-core/src/db/`)
- [x] SQLite: `sync_roots`, `file_entries`, `auth_state`, `activity_log`
- [x] Extended schema: hydration/pin/lock/base-version fields, `pending_jobs`, `inventor_project_context` (see migrations)

### Sync engine (`crates/tether-core/src/sync/`)
- [x] `engine`, `queue`, `task`, `change_detector`, `cloud_poller`, `worker`
- [x] `conflict.rs` — keep-both naming + **stale-base upload gate** (no silent overwrite)
- [x] `hasher.rs` — SHA-256
- [x] `save_patterns` — coalesce delete+recreate / temp-rename save patterns
- [x] `reference` — Inventor-oriented reference discovery + prefetch closure (heuristic)
- [x] `urls` — View Online / Copy Link URLs (ACC-oriented)
- [x] `diagnostics` — log bundle export

### Tauri (`src-tauri/`)
- [x] Tray, commands, state
- [x] Parity-oriented commands: session info, sync-now, view-online URL, copy-link, pin/free-up-space, IPJ, troubleshooter, diagnostics

### Frontend (`src/`)
- [x] Login, picker, sync status
- [x] Troubleshooter / parity tools section (session, diagnostics, IPJ)

### Hub & folder discovery
- [x] Drive hub priority, pagination, hidden folder resolution, error logging

## Completed — Phase 2: CFAPI

- [x] Sync root registration, `cloud-filter` session
- [x] `fetch_placeholders`, `fetch_data`, delete/rename propagation
- [x] Hydration callback → DB state (`on_hydration_complete`)
- [x] Dehydrate helper (`tether_cfapi::dehydrate_placeholder_file`) for Free up space
- [x] MSIX packaging assets (`packaging/`, appx manifests)
- [x] **Stabilization**: Handled `STATUS_STACK_BUFFER_OVERRUN` / User Cancelled panics in CFAPI gracefully
- [x] **Stabilization**: Resolved `UNIQUE constraint failed` SQLite crashes during dense folder fetch (`upsert_file_entry` fix)
- [x] **Stabilization**: Addressed auto-download issues and non-CAD placeholder conversion (`mark_placeholder_in_sync`)

## Desktop Connector parity backlog (P0–P4)

### P0 — Core safety & state
- [x] DB: `hydration_state`, `pin_state`, `lock_state`, `base_remote_version_id`, `hydration_reason`
- [x] Stale-base conflict detection + keep-both default (upload path guarded)
- [x] Save-pattern normalization hooks
- [ ] Full upload pipeline with stale-base checks on every path (extend as upload work lands)

### P1 — Explorer semantics
- [x] Manual **Sync Now** (app + API; Explorer context menu = MSIX manifest stub / future shell)
- [x] **Free up space** / **Always keep on this device** (API + dehydrate; pin stored in DB)
- [x] **View Online** / **Copy Link** URL helpers
- [ ] Bulk-delete confirmation + sync-blocking queue (skeleton: `pending_jobs` + flags)

### P2 — Inventor / references
- [x] Reference graph helper + prefetch closure (heuristic `.iam` / `.ipt`)
- [x] IPJ path persistence (`inventor_project_context`)
- [ ] Deeper CAD parsers, Fusion/Docs edge cases

### P3 — Locks, metadata, ops
- [ ] Lock/unlock API + read-only enforcement (`lock_state` reserved)
- [ ] Rich metadata / Data Panel (fields stubbed; API wiring incremental)
- [x] Troubleshooter panel + diagnostics ZIP
- [ ] Auto-update

### P4 — Verify first
- [ ] **Export…** verb semantics (capture from x64 Desktop Connector before coding)
- [ ] Shell rich columns (optional)

## Test matrix (manual / parity)

Documented in [tests/PARITY_TEST_MATRIX.md](tests/PARITY_TEST_MATRIX.md). Run alongside Autodesk Desktop Connector on x64 where applicable.

## Milestones

| Milestone | Criteria |
|-----------|----------|
| M1 Parity core | No silent overwrite; hydration/dehydrate/pin state persisted; stale-base awareness |
| M2 Explorer parity | Sync / View online / Copy link / Free up space / Pin from app API |
| M3 Inventor parity | Reference closure + IPJ context for assembly workflows |
| M4 Ops parity | Troubleshooter, diagnostics bundle, recovery tools expanded |
