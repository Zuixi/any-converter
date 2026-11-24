pub mod commands;
pub mod db;
pub mod migration;
pub mod secrets;
pub mod server_manager;
pub mod server_proxy;
pub mod state;

use tauri::{
    LogicalSize, Manager, WebviewWindow,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_window_state::{Builder as WindowStateBuilder, StateFlags};

/// Minimum logical window size — keep in sync with `tauri.conf.json` `minWidth`/`minHeight`.
const MIN_WINDOW_WIDTH: f64 = 1024.0;
const MIN_WINDOW_HEIGHT: f64 = 700.0;

/// A window rectangle in physical pixels; `Rect` positions are top-left.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// Restored geometry can point at a display that is no longer attached. If
/// too little of the window would land on any current monitor, fall back to
/// the minimum size, centered, rather than opening off-screen.
fn ensure_on_screen(window: &WebviewWindow) -> tauri::Result<()> {
    let position = window.outer_position()?;
    let size = window.outer_size()?;
    let window_rect = Rect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let reachable = window.available_monitors()?.iter().any(|monitor| {
        let monitor_position = monitor.position();
        let monitor_size = monitor.size();
        intersects_enough(
            window_rect,
            Rect {
                x: monitor_position.x,
                y: monitor_position.y,
                width: monitor_size.width,
                height: monitor_size.height,
            },
        )
    });
    if !reachable {
        window.set_size(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT))?;
        window.center()?;
    }
    Ok(())
}

/// `true` when at least [`MIN_VISIBLE_WIDTH`]×[`MIN_VISIBLE_HEIGHT`] of the
/// window — or the whole window, if it is smaller than that — overlaps the
/// monitor. Uses `i64` so a far-off saved position cannot overflow.
fn intersects_enough(window: Rect, monitor: Rect) -> bool {
    const MIN_VISIBLE_WIDTH: i64 = 200;
    const MIN_VISIBLE_HEIGHT: i64 = 100;
    let window_left = i64::from(window.x);
    let window_top = i64::from(window.y);
    let monitor_left = i64::from(monitor.x);
    let monitor_top = i64::from(monitor.y);
    let overlap_width = (window_left + i64::from(window.width))
        .min(monitor_left + i64::from(monitor.width))
        - window_left.max(monitor_left);
    let overlap_height = (window_top + i64::from(window.height))
        .min(monitor_top + i64::from(monitor.height))
        - window_top.max(monitor_top);
    let needed_width = MIN_VISIBLE_WIDTH.min(i64::from(window.width));
    let needed_height = MIN_VISIBLE_HEIGHT.min(i64::from(window.height));
    overlap_width >= needed_width && overlap_height >= needed_height
}

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
        // Restore the user's last window size/position. `VISIBLE` is
        // deliberately excluded: this app hides to the tray on close, so a
        // hidden window must not be restored hidden at next launch.
        .plugin(
            WindowStateBuilder::default()
                .with_state_flags(StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED)
                .build(),
        )
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
                ensure_on_screen(&window)?;
                // `visible: false` in tauri.conf.json; show only after the
                // restored geometry has been applied, so the window never
                // flashes at the default position first.
                window.show()?;
                window.set_focus()?;
            }

            let show = MenuItem::with_id(app, "show", "Show AnyConverter", true, None::<&str>)?;
            let reset =
                MenuItem::with_id(app, "reset-window", "Reset Window Size", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &reset, &quit])?;
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
                    "reset-window" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.unmaximize();
                            let _ = window
                                .set_size(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT));
                            let _ = window.center();
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const MONITOR: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn window_fully_on_monitor_is_reachable() {
        assert!(intersects_enough(
            Rect {
                x: 100,
                y: 100,
                width: 1024,
                height: 700,
            },
            MONITOR
        ));
    }

    #[test]
    fn sliver_of_window_on_monitor_is_not_reachable() {
        // Only 50 px of width remain on-screen, below the 200 px threshold.
        assert!(!intersects_enough(
            Rect {
                x: -974,
                y: 100,
                width: 1024,
                height: 700,
            },
            MONITOR
        ));
    }

    #[test]
    fn window_off_screen_is_not_reachable() {
        assert!(!intersects_enough(
            Rect {
                x: 5000,
                y: 5000,
                width: 1024,
                height: 700,
            },
            MONITOR
        ));
    }

    #[test]
    fn monitor_with_negative_origin_is_supported() {
        // A secondary display placed to the left of the primary one.
        let monitor = Rect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert!(intersects_enough(
            Rect {
                x: -1500,
                y: 100,
                width: 1024,
                height: 700,
            },
            monitor
        ));
    }

    #[test]
    fn window_smaller_than_threshold_needs_only_itself_on_screen() {
        assert!(intersects_enough(
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 80,
            },
            MONITOR
        ));
    }
}
