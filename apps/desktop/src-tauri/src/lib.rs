pub mod commands;
pub mod db;
pub mod secrets;
pub mod server_manager;
pub mod state;

use tauri::{LogicalSize, Manager};

/// Minimum logical window size — keep in sync with `tauri.conf.json` `minWidth`/`minHeight`.
const MIN_WINDOW_WIDTH: f64 = 1024.0;
const MIN_WINDOW_HEIGHT: f64 = 700.0;

pub fn run() {
    if let Err(error) = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let state = state::AppState::initialize(app.handle())?;
            app.manage(state);
            if let Some(window) = app.get_webview_window("main") {
                window.set_min_size(Some(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)))?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::update_setting,
            commands::list_providers,
            commands::create_provider,
            commands::delete_provider,
            commands::list_model_routes,
            commands::create_model_route,
            commands::convert_payload,
            commands::list_request_logs,
            commands::get_usage_summary,
            commands::get_server_status,
            commands::start_server,
            commands::stop_server,
            commands::restart_server,
        ])
        .run(tauri::generate_context!())
    {
        log::error!("failed to run any-converter desktop: {error}");
    }
}
