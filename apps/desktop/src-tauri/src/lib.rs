pub mod commands;
pub mod db;
pub mod secrets;
pub mod server_manager;
pub mod state;

use tauri::Manager;

pub fn run() {
    if let Err(error) = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let state = state::AppState::initialize(app.handle())?;
            app.manage(state);
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
