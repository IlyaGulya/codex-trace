use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::parser::session::{parse_session, parse_session_with_progress};
use crate::state::AppState;
use crate::watcher::start_session_watcher;

pub const NO_SESSION_PATH_PROVIDED: &str = "no session path provided";

pub fn load_session_from_path(path: &str) -> Result<crate::parser::session::CodexSession, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    let p = std::path::Path::new(path);
    parse_session(p)
}

/// Load a session, streaming `session-load-progress` events (`{ path, done, total }`) to
/// both the SSE stream and the Tauri event bus as the file is read.
pub fn load_session_with_progress(
    path: &str,
    state: &AppState,
    app: &Option<AppHandle>,
) -> Result<crate::parser::session::CodexSession, String> {
    if path.is_empty() {
        return Err(NO_SESSION_PATH_PROVIDED.to_string());
    }
    let p = std::path::Path::new(path);
    let path_owned = path.to_string();
    parse_session_with_progress(p, &mut |done, total| {
        let data = serde_json::json!({ "path": path_owned, "done": done, "total": total });
        state.broadcast_all(app, "session-load-progress", &data.to_string());
    })
}

#[tauri::command]
pub async fn load_session(
    path: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::parser::session::CodexSession, String> {
    load_session_with_progress(&path, &state, &Some(app))
}

#[tauri::command]
pub async fn watch_session(
    path: String,
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let session = load_session_from_path(&path)?;
    state.stop_session_watcher()?;
    state.set_watched_ongoing(path.clone(), session.is_ongoing);
    let handle = start_session_watcher(path, state.inner().clone(), Some(app));
    state.set_session_watcher(handle)
}

#[tauri::command]
pub async fn unwatch_session(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.clear_watched_ongoing();
    state.stop_session_watcher()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_session_from_path_rejects_empty_path() {
        let result = load_session_from_path("");

        assert_eq!(result.unwrap_err(), "no session path provided");
    }
}
