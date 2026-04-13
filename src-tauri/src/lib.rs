mod commands;
mod daemon;
mod wrapper;
mod terminal;

use std::time::Duration;
use tauri::Emitter;
use terminal::bridge::TerminalBridge;
use terminal::pane_manager::PaneManager;
use tracing_subscriber::{fmt, EnvFilter};
use wrapper::server::SocketServer;

pub fn run() {
    // Init tracing: VAELKOR_LOG=debug or default info
    fmt()
        .with_env_filter(
            EnvFilter::try_from_env("VAELKOR_LOG")
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Ensure runtime directories exist before Tauri starts
    if let Err(e) = daemon::session::ensure_dirs() {
        eprintln!("vaelkor: failed to create runtime dirs: {e}");
    }

    let app_state = match daemon::session::data_dir() {
        Ok(dir) => daemon::state::AppState::with_persistence(dir.join("state.json")),
        Err(e) => {
            tracing::warn!("no data dir, state will not persist: {e}");
            daemon::state::AppState::new()
        }
    };
    let pane_manager = PaneManager::new();
    let socket_server = SocketServer::new(app_state.clone(), pane_manager.clone());
    let terminal_bridge = TerminalBridge::new();
    let terminal_bridge_clone = terminal_bridge.clone();
    let session_info = daemon::session::SessionInfo::current();
    if let Err(e) = session_info.write() {
        tracing::warn!("failed to write session file: {e}");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state.clone())
        .manage(socket_server.clone())
        .manage(terminal_bridge)
        .manage(session_info)
        .manage(pane_manager.clone())
        .setup(move |app| {
            // Create vaelkor-main tmux session.
            let pm = pane_manager;
            tauri::async_runtime::spawn(async move {
                if let Err(e) = pm.ensure_main_session().await {
                    tracing::error!("failed to create vaelkor-main: {e:#}");
                }
            });

            // Spawn the socket server in the background
            let server = socket_server.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = server.run().await {
                    tracing::error!("socket server error: {e:#}");
                }
            });

            // Spawn terminal output polling loop (single stream: vaelkor-main)
            let handle = app.handle().clone();
            let bridge = terminal_bridge_clone;
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    if let Some(new_content) = bridge.poll_changes().await {
                        let chunk = terminal::bridge::TerminalChunk {
                            data: new_content,
                        };
                        if let Err(e) = handle.emit("terminal-output", &chunk) {
                            tracing::warn!("emit failed: {e}");
                        }
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_tasks,
            commands::get_task,
            commands::assign_task,
            commands::cancel_task,
            commands::get_agents,
            commands::register_agent,
            commands::get_session_info,
            commands::terminal_attach,
            commands::terminal_detach,
            commands::terminal_send_keys,
            commands::terminal_capture,
            commands::pane_show,
            commands::pane_hide,
            commands::pane_list,
        ])
        .run(tauri::generate_context!())
        .expect("error running vaelkor");
}
