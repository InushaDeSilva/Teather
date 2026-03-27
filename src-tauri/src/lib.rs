mod commands;
mod state;
mod tray;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use tauri::Manager;

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
            let handle = app.handle().clone();
            let engine = app.state::<AppState>().engine.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(3));
                loop {
                    tick.tick().await;
                    let n = {
                        let eng = engine.lock().await;
                        eng.queue.len().await
                    };
                    let tip = if n == 0 {
                        "Tether — idle".to_string()
                    } else {
                        format!("Tether — {n} sync job(s) queued (hover / open app)")
                    };
                    if let Some(tray) = handle.tray_by_id("tether") {
                        let _ = tray.set_tooltip(Some(&tip));
                    }
                }
            });
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
            commands::get_sync_session,
            commands::get_view_online_url,
            commands::get_copy_link,
            commands::sync_now,
            commands::set_always_keep_on_device,
            commands::free_up_space,
            commands::set_inventor_ipj,
            commands::get_inventor_ipj,
            commands::collect_diagnostics_bundle,
            commands::get_troubleshooter_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tether");
}
