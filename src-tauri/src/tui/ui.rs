use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::parser::discover::CodexSessionInfo;
use crate::parser::session::CodexSession;
use crate::parser::toolcall::ToolCall;
use crate::parser::turn::TurnStatus;

use super::app::{App, DetailFocus, ItemPanel, OpenSession, PickerRow, Row, Screen};

pub fn draw(f: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Picker => draw_picker(f, app),
        Screen::Detail => draw_detail(f, app),
    }
    if app.show_help {
        draw_help(f, app.screen);
    }
}

fn draw_picker(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let title = format!(
        " Codex Trace — {} ({} sessions) ",
        app.sessions_dir.display(),
        app.sessions.len()
    );
    let header_text = app.status_message.clone().unwrap_or_default();
    let header = Paragraph::new(header_text)
        .style(Style::default().fg(Color::Red))
        .block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(header, chunks[0]);

    let items = picker_items(app);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Sessions "))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("➤ ");
    f.render_stateful_widget(list, chunks[1], &mut app.picker_state);

    let footer_text = if app.search_mode {
        format!("search: {}_", app.search_query)
    } else {
        "j/k move   Enter open/toggle   /  search   r  refresh   ?  help   q  quit".to_string()
    };
    let footer = Paragraph::new(footer_text).style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[2]);
}

fn picker_items(app: &App) -> Vec<ListItem<'static>> {
    app.filtered_rows
        .iter()
        .map(|row| picker_row_item(app, row))
        .collect()
}

fn picker_row_item(app: &App, row: &PickerRow) -> ListItem<'static> {
    match row {
        PickerRow::DateHeader(group) => {
            let collapsed = app.collapsed_groups.contains(group);
            let arrow = if collapsed { "▶" } else { "▼" };
            ListItem::new(Line::from(Span::styled(
                format!("{arrow} {group}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )))
        }
        PickerRow::Session(idx) => session_list_item(&app.sessions[*idx]),
    }
}

fn session_list_item(s: &CodexSessionInfo) -> ListItem<'static> {
    let status_style = if s.is_ongoing {
        Style::default().fg(Color::Green)
    } else if s.is_archived {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    let status = if s.is_ongoing { "●" } else { " " };
    let prefix = if s.is_inline_worker { "  ↳ " } else { "    " };
    let title = s
        .ai_title
        .clone()
        .or_else(|| s.thread_name.clone())
        .or_else(|| s.cwd.clone())
        .unwrap_or_else(|| s.id.clone());
    let title = truncate(&single_line(&title), 48);
    let model = s.model.clone().unwrap_or_else(|| "-".to_string());
    let tokens = s
        .total_tokens
        .map(format_count)
        .unwrap_or_else(|| "-".to_string());
    let time = fmt_iso(&Some(s.start_time.clone()));

    let mut spans = vec![
        Span::styled(status, status_style),
        Span::raw(prefix),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(model, Style::default().fg(Color::Cyan)),
        Span::raw(format!(
            "   {} turns   {} tok   {}",
            s.turn_count, tokens, time
        )),
    ];
    if let Some(role) = &s.worker_role {
        spans.push(Span::styled(
            format!("  [{role}]"),
            Style::default().fg(Color::Magenta),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn draw_detail(f: &mut Frame, app: &mut App) {
    let depth = app.stack.len();
    let Some(open) = app.stack.last_mut() else {
        return;
    };
    match open.focus {
        DetailFocus::List => draw_detail_list(f, open, depth),
        DetailFocus::Item => draw_detail_item(f, open),
    }
}

fn draw_detail_list(f: &mut Frame, open: &mut OpenSession, depth: usize) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let session = &open.session;
    let breadcrumb = if depth > 1 {
        format!("  (worker depth {depth})")
    } else {
        String::new()
    };
    let live = if session.is_ongoing { "  ● LIVE" } else { "" };
    let title = format!(
        " {}{}{}   {} turns   {} ",
        session.id,
        breadcrumb,
        live,
        session.turns.len(),
        session.cwd.clone().unwrap_or_default()
    );
    let header = Paragraph::new("").block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(header, chunks[0]);

    let items = detail_items(open);
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Turns "))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    f.render_stateful_widget(list, chunks[1], &mut open.list_state);

    let footer = Paragraph::new(
        "j/k move   Tab expand   e/c expand/collapse all   Enter detail   r refresh   Esc back   ? help",
    )
    .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[2]);
}

fn detail_items(open: &OpenSession) -> Vec<ListItem<'static>> {
    open.rows
        .iter()
        .map(|row| detail_row_item(&open.session, *row, open.expanded.contains(&tool_key(*row))))
        .collect()
}

fn tool_key(row: Row) -> (usize, usize) {
    match row {
        Row::Tool(t, tc) => (t, tc),
        _ => (usize::MAX, usize::MAX),
    }
}

fn detail_row_item(session: &CodexSession, row: Row, expanded: bool) -> ListItem<'static> {
    match row {
        Row::TurnHeader(t) => turn_header_item(session, t),
        Row::UserMessage(t) => user_message_item(session, t),
        Row::Agent(t, m) => agent_item(session, t, m),
        Row::Tool(t, tc) => tool_item(session, t, tc, expanded),
    }
}

fn turn_header_item(session: &CodexSession, t: usize) -> ListItem<'static> {
    let turn = &session.turns[t];
    let status = format!("{:?}", turn.status);
    let color = turn_status_color(&turn.status);
    let model = turn.model.clone().unwrap_or_else(|| "-".to_string());
    let dur = fmt_duration_ms(turn.duration_ms);
    let tokens = turn
        .total_tokens
        .as_ref()
        .map(|ti| format_count(ti.total_tokens))
        .unwrap_or_else(|| "-".to_string());
    let line = Line::from(vec![
        Span::styled(
            format!("▶ Turn {} ", t + 1),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status, Style::default().fg(color)),
        Span::raw(format!("   {model}   {dur}   {tokens} tok")),
    ]);
    ListItem::new(line)
}

fn user_message_item(session: &CodexSession, t: usize) -> ListItem<'static> {
    let text = session.turns[t].user_message.clone().unwrap_or_default();
    let preview = truncate(&single_line(&text), 110);
    ListItem::new(Line::from(vec![
        Span::styled(
            "   user  ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(preview),
    ]))
}

fn agent_item(session: &CodexSession, t: usize, m: usize) -> ListItem<'static> {
    let msg = &session.turns[t].agent_messages[m];
    let (label, color) = if msg.is_reasoning {
        ("reason", Color::DarkGray)
    } else {
        ("agent ", Color::White)
    };
    let preview = truncate(&single_line(&msg.text), 110);
    ListItem::new(Line::from(vec![
        Span::styled(format!("   {label} "), Style::default().fg(color)),
        Span::raw(preview),
    ]))
}

fn tool_item(session: &CodexSession, t: usize, tc: usize, expanded: bool) -> ListItem<'static> {
    let tool = &session.turns[t].tool_calls[tc];
    let arrow = if expanded { "▾" } else { "▸" };
    let status_style = Style::default().fg(tool_status_color(&tool.status));
    let mut lines = vec![Line::from(vec![
        Span::raw(format!("   {arrow} ")),
        Span::styled(
            format!("{:?} ", tool.kind),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            tool.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(tool.status.clone(), status_style),
        Span::raw(
            tool.duration_secs
                .map(|d| format!("   {d:.1}s"))
                .unwrap_or_default(),
        ),
    ])];
    if expanded {
        for l in tool_preview_lines(tool) {
            lines.push(Line::from(Span::raw(format!("       {l}"))));
        }
    }
    ListItem::new(lines)
}

fn tool_preview_lines(tool: &ToolCall) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(cmd) = &tool.command {
        out.push(format!("$ {}", cmd.join(" ")));
    }
    if let Some(text) = &tool.input_text {
        out.push(format!("in:   {}", truncate(&single_line(text), 100)));
    } else if let Some(pretty) = pretty_json(&tool.arguments) {
        out.push(format!("args: {}", truncate(&single_line(&pretty), 100)));
    }
    if let Some(output) = &tool.output {
        out.push(format!("out:  {}", truncate(&single_line(output), 100)));
    }
    if let Some(worker) = &tool.worker_session {
        out.push(format!(
            "worker session {} ({} turns) — press Enter to open",
            worker.id,
            worker.turns.len()
        ));
    }
    if out.is_empty() {
        out.push("(no preview — press Enter for full detail)".to_string());
    } else {
        out.push("press Enter for full detail".to_string());
    }
    out
}

fn draw_detail_item(f: &mut Frame, open: &OpenSession) {
    let area = f.area();
    let Some(row) = open.selected_row() else {
        return;
    };
    let content = item_content(&open.session, row);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let header = Paragraph::new("").block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", content.heading)),
    );
    f.render_widget(header, chunks[0]);

    let (panel_title, text) = match (open.item_panel, &content.secondary) {
        (ItemPanel::Secondary, Some((title, text))) => (title.clone(), text.clone()),
        _ => (content.primary_title.clone(), content.primary.clone()),
    };
    let tabs_title = if let Some((sec_title, _)) = &content.secondary {
        let active = if open.item_panel == ItemPanel::Primary {
            &content.primary_title
        } else {
            sec_title
        };
        format!(
            " {} / {}   (showing: {}, h/l to switch) ",
            content.primary_title, sec_title, active
        )
    } else {
        format!(" {panel_title} ")
    };

    let body = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(tabs_title))
        .wrap(Wrap { trim: false })
        .scroll((open.item_scroll, 0));
    f.render_widget(body, chunks[1]);

    let footer = Paragraph::new("h/l switch panel   j/k scroll   Esc/Enter back   ? help")
        .style(Style::default().fg(Color::Gray));
    f.render_widget(footer, chunks[2]);
}

struct ItemContent {
    heading: String,
    primary_title: String,
    primary: String,
    secondary: Option<(String, String)>,
}

fn item_content(session: &CodexSession, row: Row) -> ItemContent {
    match row {
        Row::TurnHeader(t) => turn_item_content(session, t),
        Row::UserMessage(t) => ItemContent {
            heading: format!("Turn {} — User Message", t + 1),
            primary_title: "Message".to_string(),
            primary: session.turns[t].user_message.clone().unwrap_or_default(),
            secondary: None,
        },
        Row::Agent(t, m) => {
            let msg = &session.turns[t].agent_messages[m];
            let kind = if msg.is_reasoning {
                "Reasoning"
            } else {
                "Agent Message"
            };
            let phase = msg
                .phase
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            ItemContent {
                heading: format!("Turn {} — {kind}{phase}", t + 1),
                primary_title: kind.to_string(),
                primary: msg.text.clone(),
                secondary: None,
            }
        }
        Row::Tool(t, tc) => tool_item_content(session, t, tc),
    }
}

fn turn_item_content(session: &CodexSession, t: usize) -> ItemContent {
    let turn = &session.turns[t];
    let mut lines = vec![
        format!("turn_id: {}", turn.turn_id),
        format!("status: {:?}", turn.status),
        format!(
            "model: {}",
            turn.model.clone().unwrap_or_else(|| "-".into())
        ),
        format!("cwd: {}", turn.cwd.clone().unwrap_or_else(|| "-".into())),
        format!(
            "reasoning_effort: {}",
            turn.reasoning_effort.clone().unwrap_or_else(|| "-".into())
        ),
        format!("started: {}", fmt_epoch(turn.started_at)),
        format!("completed: {}", fmt_epoch(turn.completed_at)),
        format!("duration: {}", fmt_duration_ms(turn.duration_ms)),
    ];
    if let Some(tokens) = &turn.total_tokens {
        lines.push(format!(
            "tokens: input={} cached={} output={} reasoning={} total={}",
            tokens.input_tokens,
            tokens.cached_input_tokens,
            tokens.output_tokens,
            tokens.reasoning_output_tokens,
            tokens.total_tokens
        ));
    }
    if let Some(err) = &turn.error {
        lines.push(format!("error: {err}"));
    }
    if let Some(reason) = &turn.aborted_reason {
        lines.push(format!("aborted_reason: {reason}"));
    }
    if turn.has_compaction {
        lines.push("compaction: yes".to_string());
        if let Some(meta) = &turn.compaction_meta {
            lines.push(format!(
                "  tokens_before={:?} tokens_after={:?} trigger={:?}",
                meta.tokens_before, meta.tokens_after, meta.compaction_trigger
            ));
            if let Some(summary) = &meta.summary {
                lines.push(format!("  summary: {summary}"));
            }
        }
    }
    if !turn.memories.is_empty() {
        lines.push(format!("memories: {}", turn.memories.len()));
        for m in &turn.memories {
            lines.push(format!("  - {}", single_line(&m.content)));
        }
    }
    if !turn.collab_spawns.is_empty() {
        lines.push("spawned agents:".to_string());
        for spawn in &turn.collab_spawns {
            lines.push(format!(
                "  - {} ({}) -> session {}",
                spawn.agent_nickname, spawn.agent_role, spawn.new_session_id
            ));
        }
    }
    if let Some(name) = &turn.thread_name {
        lines.push(format!("thread_name: {name}"));
    }
    if let Some(trace) = &turn.trace_id {
        lines.push(format!("trace_id: {trace}"));
    }

    ItemContent {
        heading: format!("Turn {}", t + 1),
        primary_title: "Details".to_string(),
        primary: lines.join("\n"),
        secondary: None,
    }
}

fn tool_item_content(session: &CodexSession, t: usize, tc: usize) -> ItemContent {
    let tool = &session.turns[t].tool_calls[tc];

    let mut req = vec![
        format!("call_id: {}", tool.call_id),
        format!("kind: {:?}", tool.kind),
        format!("name: {}", tool.name),
    ];
    if let Some(cmd) = &tool.command {
        req.push(format!("command: {}", cmd.join(" ")));
    }
    if let Some(cwd) = &tool.cwd {
        req.push(format!("cwd: {cwd}"));
    }
    if let Some(server) = &tool.mcp_server {
        let tool_name = tool.mcp_tool.clone().unwrap_or_default();
        req.push(format!("mcp: {server}/{tool_name}"));
    }
    if let Some(q) = &tool.web_query {
        req.push(format!("query: {q}"));
    }
    if let Some(u) = &tool.web_url {
        req.push(format!("url: {u}"));
    }
    if let Some(p) = &tool.image_prompt {
        req.push(format!("prompt: {p}"));
    }
    if let Some(id) = &tool.subagent_id {
        req.push(format!("subagent_id: {id}"));
    }
    if let Some(name) = &tool.subagent_name {
        req.push(format!("subagent_name: {name}"));
    }
    if let Some(text) = &tool.input_text {
        req.push(String::new());
        req.push("input:".to_string());
        req.push(text.clone());
    } else if let Some(pretty) = pretty_json(&tool.arguments) {
        req.push(String::new());
        req.push("arguments:".to_string());
        req.push(pretty);
    }

    let mut resp = vec![format!("status: {}", tool.status)];
    if let Some(code) = tool.exit_code {
        resp.push(format!("exit_code: {code}"));
    }
    if let Some(d) = tool.duration_secs {
        resp.push(format!("duration: {d:.2}s"));
    }
    if let Some(success) = tool.patch_success {
        resp.push(format!("patch_success: {success}"));
    }
    if let Some(path) = &tool.image_file_path {
        resp.push(format!("file: {path}"));
    }
    if let Some(worker) = &tool.worker_session {
        resp.push(format!(
            "worker session: {} ({} turns) — press Enter from the list to open",
            worker.id,
            worker.turns.len()
        ));
    }
    if let Some(output) = &tool.output {
        resp.push(String::new());
        resp.push("output:".to_string());
        resp.push(maybe_pretty(output));
    }
    if let Some(changes) = &tool.patch_changes {
        if let Some(pretty) = pretty_json(changes) {
            resp.push(String::new());
            resp.push("changes:".to_string());
            resp.push(pretty);
        }
    }
    if resp.len() == 1 {
        resp.push("(no output yet)".to_string());
    }

    ItemContent {
        heading: format!("Turn {} — {}", t + 1, tool.name),
        primary_title: "Request".to_string(),
        primary: req.join("\n"),
        secondary: Some(("Response".to_string(), resp.join("\n"))),
    }
}

fn draw_help(f: &mut Frame, screen: Screen) {
    let area = centered_rect(72, 70, f.area());
    f.render_widget(Clear, area);
    let lines: Vec<&str> = match screen {
        Screen::Picker => vec![
            "Session Picker",
            "",
            "j / k / ↑ / ↓   move selection",
            "Enter / Space   open session, or expand/collapse a date group",
            "/               search sessions   (Esc clears, Enter keeps filter)",
            "r               rescan the sessions directory",
            "?               toggle this help",
            "q / Ctrl+C      quit",
        ],
        Screen::Detail => vec![
            "Session Detail",
            "",
            "j / k / ↑ / ↓        move selection (list) or scroll (detail)",
            "Tab                  expand/collapse a tool call inline",
            "e / c                expand / collapse all tool calls",
            "Enter                open full detail, or drill into a spawned agent",
            "h / l                switch Request / Response panel in detail view",
            "r                    reload the session from disk",
            "Esc / q              back to the list, or back to the picker",
            "?                    toggle this help",
        ],
    };
    let text: Vec<Line> = lines.into_iter().map(Line::from).collect();
    let help = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Help "))
        .wrap(Wrap { trim: false });
    f.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn turn_status_color(status: &TurnStatus) -> Color {
    match status {
        TurnStatus::Complete => Color::Green,
        TurnStatus::Ongoing => Color::Cyan,
        TurnStatus::Error | TurnStatus::Aborted => Color::Red,
        TurnStatus::Cancelled => Color::DarkGray,
    }
}

fn tool_status_color(status: &str) -> Color {
    match status {
        "success" | "completed" | "ok" => Color::Green,
        "error" | "failed" => Color::Red,
        "running" | "pending" => Color::Yellow,
        _ => Color::Gray,
    }
}

fn pretty_json(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Object(m) if m.is_empty() => None,
        _ => serde_json::to_string_pretty(v).ok(),
    }
}

fn maybe_pretty(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| text.to_string())
}

fn fmt_epoch(secs: Option<u64>) -> String {
    match secs {
        Some(s) => chrono::DateTime::<chrono::Utc>::from_timestamp(s as i64, 0)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
            .unwrap_or_else(|| "-".to_string()),
        None => "-".to_string(),
    }
}

fn fmt_duration_ms(ms: Option<u64>) -> String {
    match ms {
        Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.0),
        Some(ms) => format!("{ms}ms"),
        None => "-".to_string(),
    }
}

fn fmt_iso(v: &Option<String>) -> String {
    match v {
        Some(s) if !s.is_empty() => s
            .replace('T', " ")
            .trim_end_matches('Z')
            .split('.')
            .next()
            .unwrap_or(s)
            .to_string(),
        _ => "-".to_string(),
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
