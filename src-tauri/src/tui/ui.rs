use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::parser::diff::{DiffLine, DiffLineKind};
use crate::parser::discover::CodexSessionInfo;
use crate::parser::patch::{parse_apply_patch, PatchFile, PatchFileOp};
use crate::parser::session::CodexSession;
use crate::parser::toolcall::{ToolCall, ToolKind};
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
    let header = Paragraph::new(info_bar_line(session))
        .block(Block::default().borders(Borders::ALL).title(title));
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

    let header = Paragraph::new(info_bar_line(&open.session)).block(
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
    primary: Text<'static>,
    secondary: Option<(String, Text<'static>)>,
}

fn item_content(session: &CodexSession, row: Row) -> ItemContent {
    match row {
        Row::TurnHeader(t) => turn_item_content(session, t),
        Row::UserMessage(t) => ItemContent {
            heading: format!("Turn {} — User Message", t + 1),
            primary_title: "Message".to_string(),
            primary: Text::raw(session.turns[t].user_message.clone().unwrap_or_default()),
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
                primary: Text::raw(msg.text.clone()),
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
        primary: Text::raw(lines.join("\n")),
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

    let mut primary_lines: Vec<Line<'static>> = req.into_iter().map(Line::raw).collect();
    let body = tool_body_lines(tool);
    if !body.is_empty() {
        primary_lines.push(Line::default());
        primary_lines.extend(body);
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
        primary: Text::from(primary_lines),
        secondary: Some(("Response".to_string(), Text::raw(resp.join("\n")))),
    }
}

/// Body of the Request panel: a structured red/green diff for `apply_patch`
/// calls (mirroring the web frontend's `PatchDiff`/`DiffLines`), falling back
/// to a plain `patch_changes.unified_diff` dump and then to the raw input
/// text when the body isn't a recognisable patch. Non-patch tool calls just
/// get their input/arguments dumped as before.
fn tool_body_lines(tool: &ToolCall) -> Vec<Line<'static>> {
    if tool.kind == ToolKind::PatchApply {
        if let Some(text) = &tool.input_text {
            if let Some(files) = parse_apply_patch(text) {
                return render_patch_diff(&files);
            }
        }
        if let Some(changes) = &tool.patch_changes {
            let lines = render_patch_changes_fallback(changes);
            if !lines.is_empty() {
                return lines;
            }
        }
    }
    if let Some(text) = &tool.input_text {
        let mut lines = vec![Line::raw("input:")];
        lines.extend(text.lines().map(|l| Line::raw(l.to_string())));
        lines
    } else if let Some(pretty) = pretty_json(&tool.arguments) {
        let mut lines = vec![Line::raw("arguments:")];
        lines.extend(pretty.lines().map(|l| Line::raw(l.to_string())));
        lines
    } else {
        Vec::new()
    }
}

fn patch_op_label_color(op: PatchFileOp) -> (&'static str, Color) {
    match op {
        PatchFileOp::Add => ("add", Color::Green),
        PatchFileOp::Update => ("update", Color::Yellow),
        PatchFileOp::Delete => ("delete", Color::Red),
    }
}

/// Render parsed patch files as per-file, per-hunk diff lines with +/-
/// markers, red/green line tinting, and bold+underline on word-level changed
/// spans — the terminal equivalent of the web UI's `PatchDiff`/`DiffLines`.
fn render_patch_diff(files: &[PatchFile]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for file in files {
        let (op_label, op_color) = patch_op_label_color(file.op);
        let mut header = vec![
            Span::styled(
                format!("[{op_label}] "),
                Style::default().fg(op_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(file.path.clone(), Style::default().fg(Color::Cyan)),
        ];
        if let Some(mv) = &file.move_path {
            header.push(Span::raw(format!(" → {mv}")));
        }
        lines.push(Line::from(header));
        for hunk in &file.hunks {
            if !hunk.header.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("@@ {}", hunk.header),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            lines.extend(hunk.lines.iter().map(diff_line_to_line));
        }
        lines.push(Line::default());
    }
    lines
}

fn diff_line_to_line(dl: &DiffLine) -> Line<'static> {
    let (marker, base_color) = match dl.kind {
        DiffLineKind::Context => (" ", Color::DarkGray),
        DiffLineKind::Removed => ("-", Color::Red),
        DiffLineKind::Added => ("+", Color::Green),
    };
    let mut spans = vec![Span::styled(marker, Style::default().fg(base_color))];
    for seg in &dl.segments {
        let style = if seg.changed {
            Style::default()
                .fg(base_color)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(base_color)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    Line::from(spans)
}

/// Plain (uncoloured) per-file dump of `tool.patch_changes` for when the raw
/// patch body wasn't parseable — mirrors the web UI's fallback view.
fn render_patch_changes_fallback(changes: &serde_json::Value) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let Some(map) = changes.as_object() else {
        return lines;
    };
    for (file, change) in map {
        let change_type = change
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("update");
        lines.push(Line::from(vec![
            Span::styled(
                format!("[{change_type}] "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(file.clone(), Style::default().fg(Color::Cyan)),
        ]));
        if let Some(diff_text) = change.get("unified_diff").and_then(|v| v.as_str()) {
            lines.extend(diff_text.lines().map(|l| Line::raw(l.to_string())));
        } else if let Some(content) = change.get("content").and_then(|v| v.as_str()) {
            lines.extend(content.lines().map(|l| Line::raw(l.to_string())));
        }
        lines.push(Line::default());
    }
    lines
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

/// Persistent status line shown under the panel title on every Detail
/// screen: git branch · model · context-window usage (color-coded) · token
/// count — the terminal equivalent of the web/TUI reference's `InfoBar`.
fn info_bar_line(session: &CodexSession) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push_field = |spans: &mut Vec<Span<'static>>, span: Span<'static>| {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(span);
    };

    if let Some(branch) = session.git.as_ref().and_then(|g| g.branch.as_deref()) {
        push_field(
            &mut spans,
            Span::styled(format!("* {branch}"), Style::default().fg(Color::Magenta)),
        );
    }
    if let Some(model) = last_turn_model(session) {
        push_field(
            &mut spans,
            Span::styled(model, Style::default().fg(Color::Cyan)),
        );
    }
    if let Some((pct, used, window)) = last_context_usage(session) {
        push_field(
            &mut spans,
            Span::styled(
                format!("ctx {pct}%"),
                Style::default().fg(context_color(pct)),
            ),
        );
        push_field(
            &mut spans,
            Span::styled(
                format!("{}/{} tok", format_count(used), format_count(window)),
                dim,
            ),
        );
    }
    if spans.is_empty() {
        spans.push(Span::styled("(no session metadata)", dim));
    }
    Line::from(spans)
}

fn context_color(pct: u8) -> Color {
    if pct < 50 {
        Color::Green
    } else if pct <= 80 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn last_turn_model(session: &CodexSession) -> Option<String> {
    session.turns.iter().rev().find_map(|t| t.model.clone())
}

/// Context-window usage from the most recent turn that reported one — the
/// running KV-cache state, not the session-wide token total.
fn last_context_usage(session: &CodexSession) -> Option<(u8, u64, u64)> {
    session.turns.iter().rev().find_map(|t| {
        let info = t.total_tokens.as_ref()?;
        let used = info.context_window_tokens?;
        let window = info.model_context_window;
        if window == 0 {
            return None;
        }
        let pct = ((used as f64 / window as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8;
        Some((pct, used, window))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_tool_call(
        input_text: Option<&str>,
        patch_changes: Option<serde_json::Value>,
    ) -> ToolCall {
        ToolCall {
            call_id: "call1".to_string(),
            kind: ToolKind::PatchApply,
            name: "apply_patch".to_string(),
            arguments: serde_json::Value::Null,
            input_text: input_text.map(str::to_string),
            output: None,
            exit_code: None,
            command: None,
            cwd: None,
            duration_secs: None,
            mcp_server: None,
            mcp_tool: None,
            plugin_id: None,
            patch_success: None,
            patch_changes,
            web_query: None,
            web_url: None,
            image_prompt: None,
            image_file_path: None,
            worker_session: None,
            status: "success".to_string(),
            subagent_id: None,
            subagent_name: None,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn tool_body_lines_renders_a_parseable_patch_as_a_colored_diff() {
        let patch = [
            "*** Begin Patch",
            "*** Update File: src/main.rs",
            "@@",
            "-    println!(\"old\");",
            "+    println!(\"new\");",
            "*** End Patch",
        ]
        .join("\n");
        let tool = patch_tool_call(Some(&patch), None);
        let lines = tool_body_lines(&tool);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("[update] src/main.rs")));
        assert!(texts.iter().any(|t| t.contains("-    println!(\"old\");")));
        assert!(texts.iter().any(|t| t.contains("+    println!(\"new\");")));
        // The changed word should carry a distinct (bold+underlined) style
        // from the rest of the line, not just plain text.
        let added_line = lines
            .iter()
            .find(|l| line_text(l).contains("println!(\"new\");"))
            .expect("added line present");
        assert!(added_line
            .spans
            .iter()
            .any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn tool_body_lines_falls_back_to_patch_changes_unified_diff_when_input_is_not_a_patch() {
        let changes = serde_json::json!({
            "src/main.rs": { "type": "update", "unified_diff": "-old\n+new" }
        });
        let tool = patch_tool_call(Some("not a patch body"), Some(changes));
        let lines = tool_body_lines(&tool);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("[update] src/main.rs")));
        assert!(texts.iter().any(|t| t == "-old"));
        assert!(texts.iter().any(|t| t == "+new"));
    }

    #[test]
    fn tool_body_lines_falls_back_to_raw_input_text_when_nothing_is_parseable() {
        let tool = patch_tool_call(Some("just some output text"), None);
        let lines = tool_body_lines(&tool);
        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert_eq!(
            texts,
            vec!["input:".to_string(), "just some output text".to_string()]
        );
    }

    #[test]
    fn context_color_is_green_below_50_yellow_in_band_red_above_80() {
        assert_eq!(context_color(0), Color::Green);
        assert_eq!(context_color(49), Color::Green);
        assert_eq!(context_color(50), Color::Yellow);
        assert_eq!(context_color(80), Color::Yellow);
        assert_eq!(context_color(81), Color::Red);
        assert_eq!(context_color(100), Color::Red);
    }

    fn write_session_with_git_and_context(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("rollout-2026-06-01T00-00-00-infobar.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"timestamp":"2026-06-01T00:00:00Z","type":"session_meta","payload":{"id":"infobar","timestamp":"2026-06-01T00:00:00Z","cwd":"/tmp/my-project","git":{"branch":"feature/x"}}}"#,
                r#"{"timestamp":"2026-06-01T00:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-1"}}"#,
                r#"{"timestamp":"2026-06-01T00:00:02Z","type":"turn_context","payload":{"model":"gpt-5.4","cwd":"/tmp/my-project"}}"#,
                r#"{"timestamp":"2026-06-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_output_tokens":0,"total_tokens":2},"last_token_usage":{"input_tokens":40000,"cached_input_tokens":10000,"output_tokens":1000,"reasoning_output_tokens":0,"total_tokens":51000},"model_context_window":100000}}}"#,
                r#"{"timestamp":"2026-06-01T00:00:04Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-1","completed_at":1780357204.0}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        path
    }

    #[test]
    fn info_bar_line_shows_branch_model_and_context_percent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_session_with_git_and_context(tmp.path());
        let session = crate::parser::session::parse_session(&path).unwrap();

        assert_eq!(last_context_usage(&session), Some((51, 51_000, 100_000)));
        let line = info_bar_line(&session);
        let text = line_text(&line);
        assert!(text.contains("* feature/x"), "expected branch in {text:?}");
        assert!(text.contains("gpt-5.4"), "expected model in {text:?}");
        assert!(text.contains("ctx 51%"), "expected context pct in {text:?}");
    }

    #[test]
    fn info_bar_line_placeholder_when_session_has_no_metadata() {
        let session = crate::parser::session::CodexSession {
            id: "bare".to_string(),
            timestamp: String::new(),
            cwd: None,
            originator: None,
            cli_version: None,
            model_provider: None,
            git: None,
            instructions: None,
            turns: Vec::new(),
            is_ongoing: false,
            total_tokens: None,
            thread_name: None,
            spawned_worker_ids: Vec::new(),
            path: String::new(),
            ai_title: None,
            is_headless: false,
            has_missing_spawn_metadata: false,
        };
        let text = line_text(&info_bar_line(&session));
        assert_eq!(text, "(no session metadata)");
    }
}
