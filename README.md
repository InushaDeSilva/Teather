# Tether

A Windows tray app built with [Tauri 2](https://tauri.app/) that syncs files via the Windows Cloud Files API (CF API).

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- Windows 10/11 (CF API is Windows-only)

## Running in development

```powershell
cargo run --manifest-path src-tauri/Cargo.toml
```

The app lives in the **system tray** — look for the Tether icon in the bottom-right corner of the taskbar after it starts.

To see verbose logs, set the `RUST_LOG` environment variable first:

```powershell
$env:RUST_LOG = "debug"
cargo run --manifest-path src-tauri/Cargo.toml
```

## Building a release binary

```powershell
cargo build --release --manifest-path src-tauri/Cargo.toml
# Binary: target\release\tether-app.exe
```

## Packaging as MSIX (optional)

See [packaging/README.md](packaging/README.md) for instructions on building and signing an installable `.msix` package using the Windows App CLI.

## Project structure

```
crates/
  tether-cfapi/   — Windows Cloud Files API bindings & sync logic
  tether-core/    — Core sync engine, config, and database layer
src-tauri/        — Tauri shell (tray icon, commands, app entry point)
src/              — Front-end assets (HTML/CSS)
packaging/        — MSIX packaging helpers
```
