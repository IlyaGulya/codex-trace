use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::parser::discover::CodexSessionInfo;
use crate::state::AppState;
use crate::watcher::start_picker_watcher;

#[tauri::command]
pub async fn list_sessions(
    sessions_dir: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<CodexSessionInfo>, String> {
    let mut sessions = state.discover_sessions(&sessions_dir, &Some(app.clone()))?;
    state.apply_watched_ongoing(&mut sessions);
    Ok(sessions)
}

#[tauri::command]
pub async fn watch_picker(
    sessions_dir: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    state.stop_picker_watcher()?;
    let handle = start_picker_watcher(sessions_dir, state.inner().clone(), Some(app));
    state.set_picker_watcher(handle)
}

#[tauri::command]
pub async fn unwatch_picker(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.stop_picker_watcher()
}
