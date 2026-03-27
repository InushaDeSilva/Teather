mod commands;
mod state;
mod tray;

use std::sync::Arc;
use tokio::sync::Mutex;

use tether_core::config::settings::AppSettings;
use tether_core::sync::engine::SyncEngine;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    let settings = AppSettings::load().expect("Failed to load settings");
    let engine = SyncEngine::new(settings).expect("Failed to initialize sync engine");

    let app_state = AppState {
        engine: Arc::new(Mutex::new(engine)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(app_state)
        .setup(|app| {
            tray::setup_tray(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::check_auth_status,
            commands::get_sync_status,
            commands::start_login,
            commands::get_hubs,
            commands::get_projects,
            commands::get_folders,
            commands::get_drive_view,
            commands::get_subfolders,
            commands::resolve_drive_folder,
            commands::pause_sync,
            commands::resume_sync,
            commands::open_sync_folder,
            commands::start_sync,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tether");
}
