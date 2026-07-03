#![allow(dead_code)]

mod commands;
mod http_api;
mod parser;
mod settings;
mod state;
mod tui;
mod watcher;

use std::sync::Arc;

use tauri::Manager;

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let web_only = args.iter().any(|a| a == "--web");
    let headless = args.iter().any(|a| a == "--headless");
    let tui_mode = args.iter().any(|a| a == "--tui");
    let no_open = args.iter().any(|a| a == "--no-open");
    let desktop = !web_only && !headless && !tui_mode;

    // TUI mode: skip Tauri/WebKit and the HTTP server entirely — a terminal-only
    // session browser for use over SSH or in place of the desktop/web UI.
    if tui_mode {
        let settings = settings::load_settings();
        let sessions_dir =
            match parser::session::resolve_sessions_dir(settings.sessions_dir.as_deref()) {
                Ok(dir) => dir,
                Err(e) => {
                    eprintln!("codex-trace --tui: {e}");
                    std::process::exit(1);
                }
            };
        if let Err(e) = tui::run_tui(sessions_dir) {
            eprintln!("codex-trace --tui: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Headless mode: skip Tauri/WebKit entirely — run only the HTTP server.
    // This eliminates the WebKitWebProcess + WebKitNetworkProcess that Tauri
    // unconditionally spawns even when no window is displayed, which was the
    // dominant cause of high CPU usage in Docker containers.
    if headless {
        eprintln!("Headless mode: HTTP API on http://127.0.0.1:11424");
        let app_state = Arc::new(state::AppState::new());
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(http_api::start_http_server_headless(app_state));
        return;
    }

    let app_state = Arc::new(state::AppState::new());

    let mut builder = tauri::Builder::default();

    if desktop {
        builder = builder.plugin(
            tauri_plugin_single_instance::Builder::new()
                .callback(|app, _args, _cwd| {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                })
                .build(),
        );
    }

    builder
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_opener::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::session::load_session,
            commands::session::watch_session,
            commands::session::unwatch_session,
            commands::picker::list_sessions,
            commands::picker::watch_picker,
            commands::picker::unwatch_picker,
            commands::settings::get_settings,
            commands::settings::set_sessions_dir,
            switch_to_browser,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(http_api::start_http_server(handle));

            if web_only {
                if no_open {
                    eprintln!("Web mode: http://localhost:1420 (background, no browser)");
                } else {
                    eprintln!("Web mode: opening http://localhost:1420 in your browser...");
                    let _ = tauri_plugin_opener::open_url("http://localhost:1420", None::<&str>);
                }
            } else if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
async fn switch_to_browser(app: tauri::AppHandle) -> Result<(), String> {
    tauri_plugin_opener::open_url("http://localhost:1420", None::<&str>)
        .map_err(|e| e.to_string())?;

    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }

    Ok(())
}
