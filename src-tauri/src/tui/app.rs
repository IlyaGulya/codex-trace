use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::widgets::ListState;

use crate::parser::discover::{discover_sessions, CodexSessionInfo};
use crate::parser::session::{parse_session, CodexSession};
use crate::parser::toolcall::ToolKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Picker,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailFocus {
    List,
    Item,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemPanel {
    Primary,
    Secondary,
}

#[derive(Debug, Clone)]
pub enum PickerRow {
    DateHeader(String),
    Session(usize),
}

/// One selectable line in a session's detail list. Turn headers, the user
/// message, agent messages, and tool calls are interleaved in stream order
/// (see `build_rows`), mirroring how the desktop UI lays out a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    TurnHeader(usize),
    UserMessage(usize),
    Agent(usize, usize),
    Tool(usize, usize),
}

pub struct OpenSession {
    pub session: CodexSession,
    pub path: PathBuf,
    pub rows: Vec<Row>,
    pub list_state: ListState,
    pub expanded: HashSet<(usize, usize)>,
    pub focus: DetailFocus,
    pub item_panel: ItemPanel,
    pub item_scroll: u16,
    last_mtime: Option<std::time::SystemTime>,
}

impl OpenSession {
    fn new(session: CodexSession, path: PathBuf) -> Self {
        let rows = build_rows(&session);
        let mut list_state = ListState::default();
        if !rows.is_empty() {
            list_state.select(Some(0));
        }
        let last_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        OpenSession {
            session,
            path,
            rows,
            list_state,
            expanded: HashSet::new(),
            focus: DetailFocus::List,
            item_panel: ItemPanel::Primary,
            item_scroll: 0,
            last_mtime,
        }
    }

    fn refresh(&mut self) {
        if let Ok(session) = parse_session(&self.path) {
            self.rows = build_rows(&session);
            self.session = session;
            let len = self.rows.len();
            match self.list_state.selected() {
                Some(sel) if sel >= len => {
                    self.list_state.select(len.checked_sub(1));
                }
                None if len > 0 => self.list_state.select(Some(0)),
                _ => {}
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len() as i32;
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, len - 1);
        self.list_state.select(Some(next as usize));
    }

    fn toggle_expand_selected(&mut self) {
        if let Some(Row::Tool(t, tc)) = self.selected_row() {
            let key = (t, tc);
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
        }
    }

    fn set_all_expanded(&mut self, expand: bool) {
        if expand {
            for row in &self.rows {
                if let Row::Tool(t, tc) = row {
                    self.expanded.insert((*t, *tc));
                }
            }
        } else {
            self.expanded.clear();
        }
    }

    fn enter_item_view(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.focus = DetailFocus::Item;
        self.item_panel = ItemPanel::Primary;
        self.item_scroll = 0;
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.list_state
            .selected()
            .and_then(|i| self.rows.get(i))
            .copied()
    }

    /// If the selected row is a `spawn_agent` call whose worker session was embedded
    /// during parsing, returns a clone the caller can push onto the navigation stack.
    fn selected_worker_session(&self) -> Option<CodexSession> {
        let Row::Tool(t, tc) = self.selected_row()? else {
            return None;
        };
        let tool = self.session.turns.get(t)?.tool_calls.get(tc)?;
        if tool.kind != ToolKind::SpawnAgent {
            return None;
        }
        tool.worker_session.as_ref().map(|b| b.as_ref().clone())
    }
}

/// Flatten a session's turns into a linear list of rows, interleaving agent
/// messages and tool calls by their original stream order so the list reads
/// the way the conversation actually happened.
fn build_rows(session: &CodexSession) -> Vec<Row> {
    let mut rows = Vec::new();
    for (t_idx, turn) in session.turns.iter().enumerate() {
        rows.push(Row::TurnHeader(t_idx));
        if turn.user_message.is_some() {
            rows.push(Row::UserMessage(t_idx));
        }
        let mut combined: Vec<(usize, Row)> = Vec::new();
        for (m_idx, msg) in turn.agent_messages.iter().enumerate() {
            combined.push((msg.order, Row::Agent(t_idx, m_idx)));
        }
        for (tc_idx, order) in turn.tool_call_orders.iter().enumerate() {
            combined.push((*order, Row::Tool(t_idx, tc_idx)));
        }
        combined.sort_by_key(|(order, _)| *order);
        rows.extend(combined.into_iter().map(|(_, row)| row));
    }
    rows
}

fn session_matches(s: &CodexSessionInfo, query: &str) -> bool {
    let fields = [
        Some(s.id.as_str()),
        s.cwd.as_deref(),
        s.git_branch.as_deref(),
        s.model.as_deref(),
        s.thread_name.as_deref(),
        s.ai_title.as_deref(),
        s.worker_nickname.as_deref(),
        s.worker_role.as_deref(),
    ];
    fields
        .into_iter()
        .flatten()
        .any(|f| f.to_lowercase().contains(query))
}

pub struct App {
    pub sessions_dir: PathBuf,
    pub sessions: Vec<CodexSessionInfo>,
    pub filtered_rows: Vec<PickerRow>,
    pub collapsed_groups: HashSet<String>,
    pub picker_state: ListState,
    pub search_mode: bool,
    pub search_query: String,
    pub screen: Screen,
    pub stack: Vec<OpenSession>,
    pub show_help: bool,
    pub status_message: Option<String>,
    pub should_quit: bool,
}

impl App {
    pub fn new(sessions_dir: PathBuf) -> Self {
        let mut app = App {
            sessions_dir,
            sessions: Vec::new(),
            filtered_rows: Vec::new(),
            collapsed_groups: HashSet::new(),
            picker_state: ListState::default(),
            search_mode: false,
            search_query: String::new(),
            screen: Screen::Picker,
            stack: Vec::new(),
            show_help: false,
            status_message: None,
            should_quit: false,
        };
        app.reload_sessions();
        app
    }

    pub fn reload_sessions(&mut self) {
        match discover_sessions(&self.sessions_dir) {
            Ok(sessions) => {
                self.sessions = sessions;
                self.status_message = None;
            }
            Err(e) => {
                self.sessions = Vec::new();
                self.status_message = Some(format!("failed to scan sessions dir: {e}"));
            }
        }
        self.rebuild_picker_rows();
    }

    fn rebuild_picker_rows(&mut self) {
        let query = self.search_query.to_lowercase();
        let mut rows = Vec::new();
        let mut i = 0;
        while i < self.sessions.len() {
            let group = self.sessions[i].date_group.clone();
            let mut group_indices = Vec::new();
            while i < self.sessions.len() && self.sessions[i].date_group == group {
                if query.is_empty() || session_matches(&self.sessions[i], &query) {
                    group_indices.push(i);
                }
                i += 1;
            }
            if group_indices.is_empty() {
                continue;
            }
            rows.push(PickerRow::DateHeader(group.clone()));
            if !self.collapsed_groups.contains(&group) {
                rows.extend(group_indices.into_iter().map(PickerRow::Session));
            }
        }
        self.filtered_rows = rows;
        let len = self.filtered_rows.len();
        match self.picker_state.selected() {
            Some(sel) if sel >= len => self.picker_state.select(len.checked_sub(1)),
            None if len > 0 => self.picker_state.select(Some(0)),
            _ => {}
        }
    }

    pub fn on_tick(&mut self) {
        if self.screen != Screen::Detail {
            return;
        }
        let Some(open) = self.stack.last_mut() else {
            return;
        };
        let Ok(modified) = std::fs::metadata(&open.path).and_then(|m| m.modified()) else {
            return;
        };
        if Some(modified) != open.last_mtime {
            open.last_mtime = Some(modified);
            open.refresh();
        }
    }

    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.show_help {
            if matches!(
                code,
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter
            ) {
                self.show_help = false;
            }
            return;
        }
        if !self.search_mode && code == KeyCode::Char('?') {
            self.show_help = true;
            return;
        }
        match self.screen {
            Screen::Picker => self.on_key_picker(code),
            Screen::Detail => self.on_key_detail(code),
        }
    }

    fn on_key_picker(&mut self, code: KeyCode) {
        if self.search_mode {
            match code {
                KeyCode::Esc => {
                    self.search_mode = false;
                    self.search_query.clear();
                    self.rebuild_picker_rows();
                }
                KeyCode::Enter => self.search_mode = false,
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.rebuild_picker_rows();
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.rebuild_picker_rows();
                }
                _ => {}
            }
            return;
        }
        match code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('/') => self.search_mode = true,
            KeyCode::Char('r') => self.reload_sessions(),
            KeyCode::Down | KeyCode::Char('j') => self.move_picker(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_picker(-1),
            KeyCode::PageDown => self.move_picker(10),
            KeyCode::PageUp => self.move_picker(-10),
            KeyCode::Home => {
                if !self.filtered_rows.is_empty() {
                    self.picker_state.select(Some(0));
                }
            }
            KeyCode::End => {
                if !self.filtered_rows.is_empty() {
                    self.picker_state.select(Some(self.filtered_rows.len() - 1));
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_picker_row(),
            _ => {}
        }
    }

    fn move_picker(&mut self, delta: i32) {
        if self.filtered_rows.is_empty() {
            return;
        }
        let len = self.filtered_rows.len() as i32;
        let current = self.picker_state.selected().unwrap_or(0) as i32;
        let next = (current + delta).clamp(0, len - 1);
        self.picker_state.select(Some(next as usize));
    }

    fn activate_picker_row(&mut self) {
        let Some(sel) = self.picker_state.selected() else {
            return;
        };
        let Some(row) = self.filtered_rows.get(sel).cloned() else {
            return;
        };
        match row {
            PickerRow::DateHeader(group) => {
                if !self.collapsed_groups.remove(&group) {
                    self.collapsed_groups.insert(group);
                }
                self.rebuild_picker_rows();
            }
            PickerRow::Session(idx) => self.open_session_by_index(idx),
        }
    }

    fn open_session_by_index(&mut self, idx: usize) {
        let Some(info) = self.sessions.get(idx) else {
            return;
        };
        let path = PathBuf::from(&info.path);
        match parse_session(&path) {
            Ok(session) => {
                self.stack.clear();
                self.stack.push(OpenSession::new(session, path));
                self.screen = Screen::Detail;
                self.status_message = None;
            }
            Err(e) => {
                self.status_message = Some(format!("failed to parse session: {e}"));
            }
        }
    }

    fn on_key_detail(&mut self, code: KeyCode) {
        if self.stack.is_empty() {
            self.screen = Screen::Picker;
            return;
        }
        match self.stack.last().unwrap().focus {
            DetailFocus::List => self.on_key_detail_list(code),
            DetailFocus::Item => self.on_key_detail_item(code),
        }
    }

    fn on_key_detail_list(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.stack.pop();
                if self.stack.is_empty() {
                    self.screen = Screen::Picker;
                }
            }
            KeyCode::Char('r') => self.stack.last_mut().unwrap().refresh(),
            KeyCode::Down | KeyCode::Char('j') => self.stack.last_mut().unwrap().move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.stack.last_mut().unwrap().move_selection(-1),
            KeyCode::PageDown => self.stack.last_mut().unwrap().move_selection(10),
            KeyCode::PageUp => self.stack.last_mut().unwrap().move_selection(-10),
            KeyCode::Home => self.stack.last_mut().unwrap().list_state.select(Some(0)),
            KeyCode::End => {
                let open = self.stack.last_mut().unwrap();
                if !open.rows.is_empty() {
                    open.list_state.select(Some(open.rows.len() - 1));
                }
            }
            KeyCode::Tab => self.stack.last_mut().unwrap().toggle_expand_selected(),
            KeyCode::Char('e') => self.stack.last_mut().unwrap().set_all_expanded(true),
            KeyCode::Char('c') => self.stack.last_mut().unwrap().set_all_expanded(false),
            KeyCode::Enter => {
                let worker = self.stack.last().unwrap().selected_worker_session();
                if let Some(session) = worker {
                    let path = PathBuf::from(&session.path);
                    self.stack.push(OpenSession::new(session, path));
                } else {
                    self.stack.last_mut().unwrap().enter_item_view();
                }
            }
            _ => {}
        }
    }

    fn on_key_detail_item(&mut self, code: KeyCode) {
        let open = self.stack.last_mut().unwrap();
        match code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => open.focus = DetailFocus::List,
            KeyCode::Left | KeyCode::Char('h') => open.item_panel = ItemPanel::Primary,
            KeyCode::Right | KeyCode::Char('l') => open.item_panel = ItemPanel::Secondary,
            KeyCode::Down | KeyCode::Char('j') => {
                open.item_scroll = open.item_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                open.item_scroll = open.item_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => open.item_scroll = open.item_scroll.saturating_add(10),
            KeyCode::PageUp => open.item_scroll = open.item_scroll.saturating_sub(10),
            KeyCode::Home => open.item_scroll = 0,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_session_info(id: &str, date_group: &str, start_time: &str) -> CodexSessionInfo {
        CodexSessionInfo {
            id: id.to_string(),
            path: format!("/tmp/{id}.jsonl"),
            cwd: None,
            git_branch: None,
            originator: None,
            model: None,
            cli_version: None,
            thread_name: None,
            turn_count: 1,
            start_time: start_time.to_string(),
            end_time: None,
            total_tokens: None,
            is_ongoing: false,
            is_external_worker: false,
            is_inline_worker: false,
            worker_nickname: None,
            worker_role: None,
            spawned_worker_ids: Vec::new(),
            date_group: date_group.to_string(),
            ai_title: None,
            is_headless: false,
            is_archived: false,
        }
    }

    fn test_app(sessions: Vec<CodexSessionInfo>) -> App {
        let mut app = App {
            sessions_dir: PathBuf::from("/tmp"),
            sessions,
            filtered_rows: Vec::new(),
            collapsed_groups: HashSet::new(),
            picker_state: ListState::default(),
            search_mode: false,
            search_query: String::new(),
            screen: Screen::Picker,
            stack: Vec::new(),
            show_help: false,
            status_message: None,
            should_quit: false,
        };
        app.rebuild_picker_rows();
        app
    }

    #[test]
    fn session_matches_checks_multiple_fields_case_insensitively() {
        let mut s = sample_session_info("s1", "2026/05/07", "2026-05-07T00:00:00Z");
        s.cwd = Some("/home/user/MyProject".to_string());
        assert!(session_matches(&s, "myproject"));
        assert!(!session_matches(&s, "other"));
    }

    #[test]
    fn rebuild_picker_rows_groups_sessions_by_date() {
        let sessions = vec![
            sample_session_info("s1", "2026/05/08", "2026-05-08T00:00:00Z"),
            sample_session_info("s2", "2026/05/07", "2026-05-07T00:00:00Z"),
            sample_session_info("s3", "2026/05/07", "2026-05-07T01:00:00Z"),
        ];
        let app = test_app(sessions);
        assert_eq!(app.filtered_rows.len(), 5); // 2 headers + 3 sessions
        assert!(matches!(&app.filtered_rows[0], PickerRow::DateHeader(g) if g == "2026/05/08"));
        assert!(matches!(app.filtered_rows[1], PickerRow::Session(0)));
        assert!(matches!(&app.filtered_rows[2], PickerRow::DateHeader(g) if g == "2026/05/07"));
    }

    #[test]
    fn rebuild_picker_rows_search_filters_and_skips_empty_groups() {
        let mut sessions = vec![
            sample_session_info("s1", "2026/05/08", "2026-05-08T00:00:00Z"),
            sample_session_info("s2", "2026/05/07", "2026-05-07T00:00:00Z"),
        ];
        sessions[0].cwd = Some("/tmp/alpha".to_string());
        sessions[1].cwd = Some("/tmp/beta".to_string());
        let mut app = test_app(sessions);
        app.search_query = "alpha".to_string();
        app.rebuild_picker_rows();
        // Only the alpha session's date group should survive.
        assert_eq!(app.filtered_rows.len(), 2);
        assert!(matches!(&app.filtered_rows[0], PickerRow::DateHeader(g) if g == "2026/05/08"));
        assert!(matches!(app.filtered_rows[1], PickerRow::Session(0)));
    }

    #[test]
    fn activate_picker_row_collapses_and_expands_a_group() {
        let sessions = vec![sample_session_info(
            "s1",
            "2026/05/08",
            "2026-05-08T00:00:00Z",
        )];
        let mut app = test_app(sessions);
        app.picker_state.select(Some(0));
        app.activate_picker_row();
        assert_eq!(app.filtered_rows.len(), 1); // header only, session hidden
        app.activate_picker_row();
        assert_eq!(app.filtered_rows.len(), 2); // expanded again
    }

    #[test]
    fn move_picker_clamps_at_list_bounds() {
        let sessions = vec![
            sample_session_info("s1", "2026/05/08", "2026-05-08T00:00:00Z"),
            sample_session_info("s2", "2026/05/07", "2026-05-07T00:00:00Z"),
        ];
        let mut app = test_app(sessions);
        app.picker_state.select(Some(0));
        app.move_picker(-5);
        assert_eq!(app.picker_state.selected(), Some(0));
        app.move_picker(100);
        assert_eq!(
            app.picker_state.selected(),
            Some(app.filtered_rows.len() - 1)
        );
    }

    #[test]
    fn search_mode_typing_filters_live_and_escape_clears() {
        let mut sessions = vec![sample_session_info(
            "s1",
            "2026/05/08",
            "2026-05-08T00:00:00Z",
        )];
        sessions[0].cwd = Some("/tmp/alpha".to_string());
        let mut app = test_app(sessions);
        app.search_mode = true;
        app.on_key_picker(KeyCode::Char('z'));
        app.on_key_picker(KeyCode::Char('z'));
        assert_eq!(app.filtered_rows.len(), 0);
        app.on_key_picker(KeyCode::Esc);
        assert!(!app.search_mode);
        assert!(app.search_query.is_empty());
        assert_eq!(app.filtered_rows.len(), 2);
    }

    fn write_session_with_tool_call(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("rollout-2026-05-07T00-00-00-testsess.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-05-07T00:00:00Z","type":"session_meta","payload":{"id":"testsess","timestamp":"2026-05-07T00:00:00Z","cwd":"/tmp"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"Hello agent"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:03Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call1","name":"apply_patch","input":"do the thing"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:04Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call1","output":"{\"output\":\"done\",\"metadata\":{\"exit_code\":0}}"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"Hi there","phase":"final_answer"}}"#,
                r#"{"timestamp":"2026-05-07T00:00:06Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1746576006.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn build_rows_interleaves_user_message_tool_call_and_agent_message_in_order() {
        let tmp = tempdir().unwrap();
        let path = write_session_with_tool_call(tmp.path());
        let session = parse_session(&path).unwrap();
        let rows = build_rows(&session);
        assert_eq!(
            rows,
            vec![
                Row::TurnHeader(0),
                Row::UserMessage(0),
                Row::Tool(0, 0),
                Row::Agent(0, 0),
            ]
        );
    }

    #[test]
    fn open_session_by_index_switches_to_detail_screen_and_builds_rows() {
        let tmp = tempdir().unwrap();
        write_session_with_tool_call(tmp.path());
        let mut app = App::new(tmp.path().to_path_buf());
        assert_eq!(app.sessions.len(), 1);
        app.picker_state.select(Some(1)); // row 0 is the date header
        app.activate_picker_row();
        assert_eq!(app.screen, Screen::Detail);
        assert_eq!(app.stack.len(), 1);
        assert_eq!(app.stack[0].rows.len(), 4);
    }

    #[test]
    fn toggle_expand_selected_only_affects_tool_rows() {
        let tmp = tempdir().unwrap();
        let path = write_session_with_tool_call(tmp.path());
        let session = parse_session(&path).unwrap();
        let mut open = OpenSession::new(session, path);
        // Row 1 is the user message — toggling should have no effect.
        open.list_state.select(Some(1));
        open.toggle_expand_selected();
        assert!(open.expanded.is_empty());
        // Row 2 is the tool call.
        open.list_state.select(Some(2));
        open.toggle_expand_selected();
        assert!(open.expanded.contains(&(0, 0)));
        open.toggle_expand_selected();
        assert!(open.expanded.is_empty());
    }

    #[test]
    fn set_all_expanded_expands_and_collapses_every_tool_call() {
        let tmp = tempdir().unwrap();
        let path = write_session_with_tool_call(tmp.path());
        let session = parse_session(&path).unwrap();
        let mut open = OpenSession::new(session, path);
        open.set_all_expanded(true);
        assert_eq!(open.expanded.len(), 1);
        open.set_all_expanded(false);
        assert!(open.expanded.is_empty());
    }

    #[test]
    fn on_key_detail_list_esc_pops_stack_back_to_picker() {
        let tmp = tempdir().unwrap();
        write_session_with_tool_call(tmp.path());
        let mut app = App::new(tmp.path().to_path_buf());
        app.picker_state.select(Some(1));
        app.activate_picker_row();
        assert_eq!(app.screen, Screen::Detail);
        app.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.stack.is_empty());
        assert_eq!(app.screen, Screen::Picker);
    }

    #[test]
    fn selected_worker_session_returns_none_for_non_spawn_tool() {
        let tmp = tempdir().unwrap();
        let path = write_session_with_tool_call(tmp.path());
        let session = parse_session(&path).unwrap();
        let mut open = OpenSession::new(session, path);
        open.list_state.select(Some(2)); // the custom tool call row
        assert!(open.selected_worker_session().is_none());
    }

    #[test]
    fn selected_worker_session_returns_embedded_session_for_spawn_agent() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("rollout-2026-04-27T16-50-45-parent.jsonl");
        let worker_path = tmp
            .path()
            .join("rollout-2026-04-27T16-50-46-worker-session.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-04-27T04:50:45Z","type":"session_meta","payload":{"id":"parent","timestamp":"2026-04-27T04:50:45Z"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:02Z","type":"response_item","payload":{"type":"function_call","name":"spawn_agent","arguments":"{\"agent_type\":\"worker\",\"message\":\"Collect evidence\"}","call_id":"call_spawn"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_spawn","output":"{\"agent_id\":\"worker-session\",\"nickname\":\"Parfit\"}"}}"#,
                r#"{"timestamp":"2026-04-27T04:52:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1777279924.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            &worker_path,
            r#"{"timestamp":"2026-04-27T04:50:46Z","type":"session_meta","payload":{"id":"worker-session","timestamp":"2026-04-27T04:50:46Z","cwd":"/tmp/worker"}}"#,
        )
        .unwrap();

        let session = parse_session(&path).unwrap();
        let mut open = OpenSession::new(session, path);
        open.list_state.select(Some(1)); // the spawn_agent tool call row (no user message here)
        let worker = open
            .selected_worker_session()
            .expect("worker session embedded");
        assert_eq!(worker.id, "worker-session");
    }
}
