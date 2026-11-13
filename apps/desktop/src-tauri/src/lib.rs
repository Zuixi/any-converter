pub mod commands;
pub mod db;
pub mod migration;
pub mod secrets;
pub mod server_manager;
pub mod server_proxy;
pub mod state;

use tauri::{
    LogicalSize, Manager,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

/// Minimum logical window size — keep in sync with `tauri.conf.json` `minWidth`/`minHeight`.
const MIN_WINDOW_WIDTH: f64 = 1024.0;
const MIN_WINDOW_HEIGHT: f64 = 700.0;

/// Where a bundled Node runtime lives, when the app ships one
/// (`bundle.externalBin` + `scripts/fetch-node.mjs`): beside the app
/// executable, under the name Tauri keeps — `node-<target-triple>(.exe)` —
/// or the plain name. `None` in a dev build, where plugin hosts fall back
/// to the system `node` (crates/server `plugins::host::default_launch`).
fn bundled_node() -> Option<std::path::PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let suffix = std::env::consts::EXE_SUFFIX;
    let mut suffixed = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_plain = if suffix.is_empty() {
            name == "node"
        } else {
            name == format!("node{suffix}")
        };
        if is_plain {
            return Some(entry.path());
        }
        let is_suffixed = name.starts_with("node-")
            && if suffix.is_empty() {
                !name.contains('.')
            } else {
                name.ends_with(suffix)
            };
        if is_suffixed && suffixed.is_none() {
            suffixed = Some(entry.path());
        }
    }
    suffixed
}

/// Point plugin hosts at the Node runtime bundled with the app, when there
/// is one; without it they use the system `node`, as before.
fn use_bundled_node() {
    if let Some(node) = bundled_node() {
        any_converter_server::plugins::host::set_node_override(node.to_string_lossy().into_owned());
        log::info!("plugin hosts use the bundled node at {}", node.display());
    }
}

pub fn run() {
    use_bundled_node();
    if let Err(error) = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let state = state::AppState::initialize(app.handle())?;
            let server_manager = state.server_manager();
            let home = state.home();
            app.manage(state);
            if let Some(error) = app.state::<crate::state::AppState>().startup_error() {
                log::error!("resident gateway auto-start blocked: {error}");
            } else {
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = server_manager.start(home).await {
                        log::error!("failed to start resident gateway: {error}");
                    }
                });
            }
            if let Some(window) = app.get_webview_window("main") {
                window.set_min_size(Some(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT)))?;
            }

            let show = MenuItem::with_id(app, "show", "Show AnyConverter", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            let icon = app
                .default_window_icon()
                .ok_or("desktop icon is missing")?
                .clone();
            TrayIconBuilder::with_id("any-converter-gateway")
                .icon(icon)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<crate::state::AppState>();
                            let _ = state.server_manager().stop().await;
                            app.exit(0);
                        });
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle().clone();
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_server_status,
            commands::server_request,
            commands::start_server,
            commands::stop_server,
            commands::restart_server,
        ])
        .run(tauri::generate_context!())
    {
        log::error!("failed to run any-converter desktop: {error}");
    }
}
