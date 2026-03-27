# MSIX packaging (WinApp CLI)

1. Install [Windows App Development CLI](https://aka.ms/winappcli) (`winget install Microsoft.WinAppCli`).
2. Install the Cargo helper: `cargo install cargo-winapp`.
3. From the repo root, the canonical manifest lives at [`src-tauri/appxmanifest.xml`](../src-tauri/appxmanifest.xml) (created with `winapp init src-tauri --use-defaults --setup-sdks none`).
4. Build the app and pack:
   ```powershell
   cd src-tauri
   cargo winapp pack -o ..\packaging\Tether.msix
   ```
   This uses `appxmanifest.xml`, copies `target\debug\tether-app.exe` (or release if you pass `--release`), and signs with `devcert.pfx` (generated on first run if missing).

5. Install cert then MSIX (see `cargo winapp pack` output).

`Package.appxmanifest` in this folder is a duplicate of `src-tauri/appxmanifest.xml` for documentation; **WinApp expects `src-tauri/appxmanifest.xml`**.

**Note:** The CloudFiles COM CLSIDs follow the [Cloud Mirror](https://github.com/microsoft/Windows-classic-samples/tree/master/Samples/CloudMirror) sample. Full Explorer overlays/context menus require implementing those COM surfaces in the app; cfapi sync still works via `cloud-filter` when running the packaged binary.
