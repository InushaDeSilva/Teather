mod commands;
mod state;
mod tray;

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use tauri::{async_runtime, Manager};

use tether_core::config::settings::AppSettings;
use tether_core::sync::engine::SyncEngine;

use state::AppState;

fn parse_shell_args(args: &[String]) -> Option<(String, String)> {
    let mut verb = None;
    let mut relative_path = None;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--shell-verb" => {
                if let Some(value) = args.get(idx + 1) {
                    verb = Some(value.clone());
                    idx += 1;
                }
            }
            "--shell-path" => {
                if let Some(value) = args.get(idx + 1) {
                    relative_path = Some(value.clone());
                    idx += 1;
                }
            }
            _ => {}
        }
        idx += 1;
    }
    match (verb, relative_path) {
        (Some(verb), Some(relative_path)) => Some((verb, relative_path)),
        _ => None,
    }
}

fn dispatch_shell_args(app_state: AppState, args: &[String]) {
    if let Some((verb, relative_path)) = parse_shell_args(args) {
        async_runtime::spawn(async move {
            if let Err(err) = commands::dispatch_shell_verb(&app_state, &verb, &relative_path).await
            {
                tracing::warn!(
                    "shell dispatch failed for verb={} path={}: {}",
                    verb,
                    relative_path,
                    err
                );
            }
        });
    }
}

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
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let app_state = AppState {
                engine: app.state::<AppState>().engine.clone(),
            };
            dispatch_shell_args(app_state, &args);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(app_state)
        .setup(|app| {
            let startup_args: Vec<String> = std::env::args().collect();
            let shell_state = AppState {
                engine: app.state::<AppState>().engine.clone(),
            };
            dispatch_shell_args(shell_state, &startup_args);
            tray::setup_tray(app)?;
            let handle = app.handle().clone();
            
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let tauri::WindowEvent::Focused(false) = event {
                        let _ = w.hide();
                    }
                });
            }

            let engine = app.state::<AppState>().engine.clone();
            async_runtime::spawn(async move {
                use tauri_plugin_notification::NotificationExt;
                let mut notified_prompts = std::collections::HashSet::new();
                let mut tick = tokio::time::interval(Duration::from_secs(3));
                loop {
                    tick.tick().await;
                    
                    let (n, prompts) = {
                        let eng = engine.lock().await;
                        let queued = eng.queue.len().await;
                        let prompts = eng.db.lock()
                            .ok()
                            .and_then(|db| db.list_pending_jobs("action_required", 10).ok())
                            .unwrap_or_default();
                        (queued, prompts)
                    };

                    for p in prompts {
                        if notified_prompts.insert(p.id.clone()) {
                            let _ = handle.notification()
                                .builder()
                                .title("Tether - Action Required")
                                .body("A file sync needs your attention. Click here or open Tether.")
                                .show();
                        }
                    }

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
            commands::auto_discover_drive_folders,
            commands::get_subfolders,
            commands::resolve_drive_folder,
            commands::pause_sync,
            commands::resume_sync,
            commands::set_service_state,
            commands::open_sync_folder,
            commands::start_unified_sync,
            commands::get_sync_session,
            commands::get_view_online_url,
            commands::get_copy_link,
            commands::sync_now,
            commands::get_latest_version,
            commands::set_always_keep_on_device,
            commands::free_up_space,
            commands::delete_local_item,
            commands::request_delete_prompt,
            commands::list_pending_prompts,
            commands::list_operation_journal,
            commands::resolve_pending_prompt,
            commands::shell_dispatch,
            commands::set_inventor_ipj,
            commands::get_inventor_ipj,
            commands::collect_diagnostics_bundle,
            commands::get_troubleshooter_report,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tether");
}
