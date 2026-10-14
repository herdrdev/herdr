//! Read-only chat projection of the live Claude transcript.

use std::time::{Duration, SystemTime};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::state::Palette;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChatRow {
    User {
        body: String,
    },
    Assistant {
        body: String,
    },
    Worked {
        duration: String,
        tools: Vec<String>,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TranscriptChat {
    rows: Vec<ChatRow>,
    pending_tools: Vec<String>,
    pending_since: Option<SystemTime>,
    pending_prose: String,
    content_revision: u64,
    layout: Option<TranscriptLayout>,
}

#[derive(Clone, Debug)]
struct TranscriptLayout {
    content_revision: u64,
    width: u16,
    expand_tools: bool,
    palette: Palette,
    lines: Vec<Line<'static>>,
}

impl PartialEq for TranscriptChat {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.pending_tools == other.pending_tools
            && self.pending_since == other.pending_since
            && self.pending_prose == other.pending_prose
    }
}

impl Eq for TranscriptChat {}

impl TranscriptChat {
    fn push_row(&mut self, row: ChatRow) {
        self.rows.push(row);
        if self.rows.len() > 500 {
            self.rows.drain(..100);
        }
        self.content_revision = self.content_revision.wrapping_add(1);
    }

    pub(crate) fn prepare_layout(
        &mut self,
        palette: &Palette,
        width: u16,
        expand_tools: bool,
    ) -> bool {
        if self.layout.as_ref().is_some_and(|layout| {
            layout.content_revision == self.content_revision
                && layout.width == width
                && layout.expand_tools == expand_tools
                && layout.palette == *palette
        }) {
            return false;
        }
        let lines = layout_lines(&self.rows, palette, width, expand_tools);
        self.layout = Some(TranscriptLayout {
            content_revision: self.content_revision,
            width,
            expand_tools,
            palette: palette.clone(),
            lines,
        });
        true
    }

    fn layout_lines(&self) -> &[Line<'static>] {
        self.layout
            .as_ref()
            .map_or(&[], |layout| layout.lines.as_slice())
    }

    pub(crate) fn update_from(&mut self, mut updated: Self) {
        if self.content_revision == updated.content_revision && self.rows == updated.rows {
            updated.layout = self.layout.take();
        }
        *self = updated;
    }

    pub(crate) fn ingest(&mut self, value: &serde_json::Value, at: Option<SystemTime>) {
        if crate::turns::real_user(value) {
            let body = crate::turns::message_text(value);
            if body.trim().is_empty() || hidden_assistant_text(&body) {
                return;
            }
            self.pending_tools.clear();
            self.pending_since = at;
            self.pending_prose.clear();
            self.push_row(ChatRow::User { body });
            return;
        }
        if value["type"] != "assistant" || value["isSidechain"] == true || value["isMeta"] == true {
            return;
        }
        let Some(items) = value["message"]["content"].as_array() else {
            return;
        };
        let mut prose = Vec::new();
        for item in items {
            match item["type"].as_str() {
                Some("tool_use") => {
                    let name = item["name"].as_str().unwrap_or("tool");
                    self.pending_tools
                        .push(summarize_tool(name, &item["input"]));
                    if self.pending_tools.len() > 500 {
                        self.pending_tools.drain(..100);
                    }
                }
                Some("text") => {
                    if let Some(text) = item["text"].as_str() {
                        prose.push(text);
                    }
                }
                _ => {}
            }
        }
        let body = prose.join("\n");
        if !body.trim().is_empty() && !hidden_assistant_text(&body) {
            if !self.pending_prose.is_empty() {
                self.pending_prose.push('\n');
            }
            self.pending_prose.push_str(&body);
        }
        if !matches!(
            value["message"]["stop_reason"].as_str(),
            Some("end_turn" | "stop_sequence")
        ) {
            return;
        }
        let body = std::mem::take(&mut self.pending_prose);
        if body.trim().is_empty() {
            return;
        }
        if !self.pending_tools.is_empty() {
            let duration = self.pending_since.zip(at).map_or_else(
                || "0s".to_owned(),
                |(start, end)| format_duration(end.duration_since(start).unwrap_or_default()),
            );
            let tools = std::mem::take(&mut self.pending_tools);
            self.push_row(ChatRow::Worked { duration, tools });
        }
        self.pending_since = None;
        self.push_row(ChatRow::Assistant { body });
    }
}

fn summarize_tool(name: &str, input: &serde_json::Value) -> String {
    let argument = match name {
        "Bash" => string_field(input, &["command"]).map(str::to_owned),
        "Read" | "Edit" | "Write" => string_field(input, &["file_path", "path", "file"])
            .map(|path| path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned()),
        "Grep" => string_field(input, &["pattern"]).map(str::to_owned),
        "WebFetch" => string_field(input, &["url"]).map(web_fetch_target),
        _ => first_string_argument(input).map(str::to_owned),
    }
    .or_else(|| first_string_argument(input).map(str::to_owned))
    .map(|argument| compact_text(&argument))
    .filter(|argument| !argument.is_empty());

    argument.map_or_else(|| name.to_owned(), |argument| format!("{name} {argument}"))
}

fn string_field<'a>(value: &'a serde_json::Value, fields: &[&str]) -> Option<&'a str> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(serde_json::Value::as_str))
}

fn first_string_argument(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Array(values) => values.iter().find_map(serde_json::Value::as_str),
        serde_json::Value::Object(values) => values.values().find_map(serde_json::Value::as_str),
        _ => None,
    }
}

fn compact_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn web_fetch_target(url: &str) -> String {
    let target = url.split_once("://").map_or(url, |(_, target)| target);
    target
        .split(['?', '#'])
        .next()
        .unwrap_or(target)
        .trim_end_matches('/')
        .to_owned()
}

fn hidden_assistant_text(text: &str) -> bool {
    let body = text.trim_start();
    [
        "<system-reminder",
        "<task-notification",
        "<closing-state",
        "```closing-state",
        "Stop hook feedback",
        "<local-command",
    ]
    .iter()
    .any(|prefix| body.starts_with(prefix) || body.contains(prefix))
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 60 {
        format!("{}m {}s", duration.as_secs() / 60, duration.as_secs() % 60)
    } else {
        format!("{}s", duration.as_secs())
    }
}

fn worked_row_lines(duration: &str, tools: &[String], expand: bool, width: usize) -> Vec<String> {
    let tool_count = tools.len();
    let tool_label = if tool_count == 1 { "tool" } else { "tools" };
    let mut lines = vec![format!(
        "Worked for {duration} · {tool_count} {tool_label} ›"
    )];
    if expand {
        lines.extend(
            tools
                .iter()
                .map(|tool| crate::ui::text::truncate_end(tool, width)),
        );
    }
    lines
}

fn layout_lines(
    rows: &[ChatRow],
    palette: &Palette,
    width: u16,
    expand_tools: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::<Line<'static>>::new();
    for row in rows {
        match row {
            ChatRow::User { body } => {
                let inner_width = width.saturating_sub(3).max(1) as usize;
                let body_lines = crate::ui::markdown::transcript_body_lines(
                    palette,
                    Some(body),
                    inner_width,
                    "",
                );
                let fill = Style::default().fg(palette.text).bg(palette.surface0);
                let padding = Line::from(Span::styled(" ".repeat(width as usize), fill));
                lines.push(padding.clone());
                for (index, line) in body_lines.into_iter().enumerate() {
                    let remaining = (width as usize).saturating_sub(2 + line.width());
                    let mut spans = vec![Span::styled(
                        if index == 0 { "› " } else { "  " },
                        fill.fg(palette.overlay1),
                    )];
                    spans.extend(
                        line.spans
                            .into_iter()
                            .map(|span| Span::styled(span.content, fill)),
                    );
                    spans.push(Span::styled(" ".repeat(remaining), fill));
                    lines.push(Line::from(spans));
                }
                lines.push(padding);
                lines.push(Line::default());
            }
            ChatRow::Assistant { body } => {
                let mut body_lines = crate::ui::markdown::transcript_chat_body_lines(
                    palette,
                    Some(body),
                    width.saturating_sub(4).min(100) as usize,
                    "  ",
                );
                lines.append(&mut body_lines);
                lines.push(Line::default());
                lines.push(Line::default());
            }
            ChatRow::Worked { duration, tools } => {
                for row in worked_row_lines(duration, tools, expand_tools, width as usize) {
                    lines.push(Line::from(Span::styled(
                        row,
                        Style::default().fg(palette.overlay0),
                    )));
                }
            }
        }
    }
    if lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
    lines
}

/// Inspect only the terminal's bottom rows, without copying or restyling cells.
/// The buffer is populated by the normal terminal renderer before this overlay.
fn composer_height(buffer: &Buffer, area: Rect) -> u16 {
    let cap = (u32::from(area.height) * 2 / 5) as u16;
    if area.width == 0 || cap == 0 {
        return 0;
    }
    let bottom = area.bottom();
    let scan_top = bottom.saturating_sub(area.height.min(64));
    let mut found_prompt = false;
    for y in (scan_top..bottom).rev() {
        if !found_prompt {
            // Claude indents its prompt; require it to be the first nonblank cell.
            found_prompt = (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .find(|symbol| !symbol.trim().is_empty())
                .is_some_and(|symbol| symbol == "❯");
        } else if (area.x..area.right())
            .all(|x| matches!(buffer[(x, y)].symbol(), "─" | "━" | "⎯" | "-"))
        {
            return (bottom - y).min(cap);
        }
    }
    4.min(cap)
}

pub(crate) fn render(
    frame: &mut Frame,
    area: Rect,
    chat: &TranscriptChat,
    palette: &Palette,
    scroll: usize,
    return_key: &str,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let composer_height = composer_height(frame.buffer_mut(), area);
    // Leave the live terminal cells and its cursor at their original coordinates.
    // The final cleared row is the blank separator beneath the transcript footer.
    let area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(composer_height),
    );
    frame.render_widget(Clear, area);
    let footer_height = 2.min(area.height);
    let body_height = area.height.saturating_sub(footer_height) as usize;
    let lines = chat.layout_lines();
    let max_scroll = lines.len().saturating_sub(body_height);
    let offset = max_scroll.saturating_sub(scroll.min(max_scroll));
    // Render borrowed cached lines directly; the layout already contains wrapped rows.
    for (row, line) in lines.iter().skip(offset).take(body_height).enumerate() {
        frame.render_widget(line, Rect::new(area.x, area.y + row as u16, area.width, 1));
    }
    let footer = Rect::new(area.x, area.bottom() - footer_height, area.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("chat view · {return_key} to return"),
            Style::default().fg(palette.overlay0),
        )))
        .block(Block::default().borders(Borders::NONE)),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_rows(area: Rect, rule: Option<u16>, prompt: u16) -> Buffer {
        let mut buffer = Buffer::empty(area);
        if let Some(y) = rule {
            for x in area.x..area.right() {
                buffer[(x, y)].set_symbol("─");
            }
        }
        buffer[(area.x + 1, prompt)].set_symbol("❯");
        buffer
    }

    #[test]
    fn transcript_composer_uses_rule_above_bottom_prompt_and_caps_height() {
        let area = Rect::new(3, 2, 40, 20);
        let mut buffer = terminal_rows(area, Some(16), 18);
        // A lower rule belongs beneath the input, and must not start the region.
        for x in area.x..area.right() {
            buffer[(x, 19)].set_symbol("─");
        }
        assert_eq!(composer_height(&buffer, area), 6);
        let oversized = terminal_rows(area, Some(10), 14);
        assert_eq!(composer_height(&oversized, area), 8);
    }

    #[test]
    fn transcript_composer_falls_back_without_rule_or_prompt() {
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(composer_height(&terminal_rows(area, None, 17), area), 4);
        assert_eq!(composer_height(&Buffer::empty(area), area), 4);
        let small = Rect::new(0, 0, 40, 5);
        assert_eq!(composer_height(&Buffer::empty(small), small), 2);
        assert_eq!(composer_height(&Buffer::empty(Rect::ZERO), Rect::ZERO), 0);
    }

    #[test]
    #[ignore = "manual composer scan scaling profile"]
    fn transcript_composer_scan_profile() {
        use std::{hint::black_box, time::Instant};

        let area = Rect::new(0, 0, 120, 40);
        let buffer = terminal_rows(area, Some(34), 36);
        let fallback = Buffer::filled(area, ratatui::buffer::Cell::new("x"));
        for (label, buffer) in [("composer", buffer), ("fallback", fallback)] {
            for count in [1, 15] {
                let start = Instant::now();
                for _ in 0..10_000 {
                    for _ in 0..count {
                        black_box(composer_height(black_box(&buffer), area));
                    }
                }
                println!(
                    "{label}: {count} panes, {:.2} us/frame",
                    start.elapsed().as_secs_f64() * 100.0
                );
            }
        }
    }

    #[test]
    fn transcript_render_preserves_composer_colours_cursor_and_footer_placement() {
        use ratatui::{backend::Backend, backend::TestBackend, Terminal};

        for palette in [Palette::catppuccin(), Palette::catppuccin_latte()] {
            let mut terminal = Terminal::new(TestBackend::new(40, 20)).expect("terminal");
            let area = Rect::new(0, 0, 40, 20);
            let mut live = terminal_rows(area, Some(14), 16);
            let style = Style::default().fg(palette.green).bg(palette.panel_bg);
            live.set_style(Rect::new(0, 14, 40, 6), style);
            live.set_string(2, 16, " typing", style);
            live.set_string(0, 19, "mode line", style);
            let mut chat = TranscriptChat::default();
            chat.push_row(ChatRow::Assistant {
                body: "Transcript above input".into(),
            });
            chat.prepare_layout(&palette, 40, false);
            terminal
                .draw(|frame| {
                    *frame.buffer_mut() = live.clone();
                    frame.set_cursor_position((9, 16));
                    render(frame, area, &chat, &palette, 0, "prefix+t");
                })
                .expect("draw");
            let buffer = terminal.backend().buffer();
            for y in 14..20 {
                for x in 0..40 {
                    assert_eq!(buffer[(x, y)], live[(x, y)]);
                }
            }
            let row = |y| (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            assert!(row(0).contains("Transcript above input"));
            assert!(row(12).contains("chat view · prefix+t to return"));
            assert!(row(13).trim().is_empty());
            assert_eq!(
                terminal
                    .backend_mut()
                    .get_cursor_position()
                    .expect("cursor"),
                (9, 16).into()
            );
        }
    }

    #[test]
    fn transcript_render_visible_window_matches_paragraph_at_scroll_offsets() {
        use ratatui::{backend::TestBackend, widgets::Widget, Terminal};

        for palette in [Palette::catppuccin(), Palette::catppuccin_latte()] {
            for expand_tools in [false, true] {
                let area = Rect::new(3, 2, 32, 20);
                let mut chat = TranscriptChat::default();
                for index in 0..8 {
                    chat.push_row(ChatRow::User {
                        body: format!("Question {index}: wide 界 and wrapping text"),
                    });
                    chat.push_row(ChatRow::Worked {
                        duration: "2s".into(),
                        tools: vec!["Read src/main.rs".into(), "Bash cargo test".into()],
                    });
                    chat.push_row(ChatRow::Assistant {
                        body: format!("Answer {index}\nAnother row"),
                    });
                }
                chat.prepare_layout(&palette, area.width, expand_tools);
                let body = Rect::new(area.x, area.y, area.width, 14);
                let lines = chat.layout_lines();
                let max_scroll = lines.len().saturating_sub(body.height as usize);
                for scroll in [0, 1, 7, max_scroll, usize::MAX] {
                    let offset = max_scroll.saturating_sub(scroll.min(max_scroll));
                    let visible = lines
                        .iter()
                        .skip(offset)
                        .take(body.height as usize)
                        .cloned()
                        .collect::<Vec<_>>();
                    let mut expected = Buffer::empty(Rect::new(0, 0, 40, 24));
                    Paragraph::new(visible).render(body, &mut expected);
                    let mut terminal = Terminal::new(TestBackend::new(40, 24)).expect("terminal");
                    terminal
                        .draw(|frame| render(frame, area, &chat, &palette, scroll, "prefix+t"))
                        .expect("draw");
                    for y in body.y..body.bottom() {
                        for x in body.x..body.right() {
                            assert_eq!(terminal.backend().buffer()[(x, y)], expected[(x, y)]);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn ingest_keeps_prose_and_collapses_tool_calls() {
        let mut chat = TranscriptChat::default();
        chat.ingest(
            &serde_json::json!({"type":"user","message":{"content":"hello"}}),
            None,
        );
        chat.ingest(&serde_json::json!({"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file":"src/main.rs"}}]}}), None);
        chat.ingest(&serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"secret"}]}}), None);
        chat.ingest(&serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}), None);
        assert_eq!(chat.rows.len(), 3);
        assert!(
            matches!(&chat.rows[1], ChatRow::Worked { tools, .. } if tools == &["Read main.rs".to_owned()])
        );
        assert!(!format!("{:?}", chat.rows).contains("secret"));
    }

    #[test]
    fn tool_summaries_use_short_arguments() {
        assert_eq!(
            summarize_tool(
                "Bash",
                &serde_json::json!({"command":"ls -la /private/var/tmp/repro-586"})
            ),
            "Bash ls -la /private/var/tmp/repro-586"
        );
        assert_eq!(
            summarize_tool(
                "Read",
                &serde_json::json!({"file_path":"/private/var/tmp/notes.md"})
            ),
            "Read notes.md"
        );
        assert_eq!(
            summarize_tool("Edit", &serde_json::json!({"file_path":"src/register.tsx"})),
            "Edit register.tsx"
        );
        assert_eq!(
            summarize_tool("Grep", &serde_json::json!({"pattern":"TODO|FIXME"})),
            "Grep TODO|FIXME"
        );
        assert_eq!(
            summarize_tool(
                "WebFetch",
                &serde_json::json!({"url":"https://example.com/docs/page?q=1"})
            ),
            "WebFetch example.com/docs/page"
        );
        assert_eq!(
            summarize_tool(
                "Mystery",
                &serde_json::json!({"first":"first argument","second":"ignored"})
            ),
            "Mystery first argument"
        );
    }

    #[test]
    fn tool_rows_stay_hidden_until_expanded_and_truncate_to_width() {
        let mut chat = TranscriptChat::default();
        chat.ingest(
            &serde_json::json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","name":"Bash","input":{"command":"ls -la /private/var/tmp/repro-586"}},
                {"type":"tool_use","name":"Read","input":{"file_path":"/private/var/tmp/notes.md"}},
                {"type":"tool_use","name":"Mystery","input":{"first":"first argument"}}
            ]}}),
            None,
        );
        chat.ingest(
            &serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}),
            None,
        );

        let palette = Palette::catppuccin();
        chat.prepare_layout(&palette, 32, false);
        let collapsed = chat
            .layout_lines()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>();
        assert!(collapsed
            .iter()
            .any(|line| line == "Worked for 0s · 3 tools ›"));
        assert!(collapsed.iter().all(|line| {
            !line.starts_with("Bash") && !line.starts_with("Read") && !line.starts_with("Mystery")
        }));

        chat.prepare_layout(&palette, 32, true);
        let expanded = chat.layout_lines();
        let rows = expanded.iter().map(Line::to_string).collect::<Vec<_>>();
        assert!(rows.iter().any(|line| line == "Read notes.md"));
        assert!(rows.iter().any(|line| line == "Mystery first argument"));
        let bash_index = rows
            .iter()
            .position(|line| line.starts_with("Bash"))
            .expect("expanded Bash row");
        let bash = &expanded[bash_index];
        assert_eq!(crate::ui::text::display_width(&bash.to_string()), 32);
        assert!(bash.to_string().ends_with('…'));
        assert!(bash
            .spans
            .iter()
            .all(|span| span.style.fg == Some(palette.overlay0)));
        assert!(bash.spans.iter().all(|span| !span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::BOLD)));
    }

    #[test]
    fn user_message_has_no_timestamp_row() {
        let mut chat = TranscriptChat::default();
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(12 * 60 * 60 + 34 * 60);
        chat.ingest(
            &serde_json::json!({"type":"user","message":{"content":"hello"}}),
            Some(at),
        );
        chat.prepare_layout(&Palette::catppuccin(), 80, false);

        let visible = chat
            .layout_lines()
            .iter()
            .filter(|line| line.width() > 0)
            .collect::<Vec<_>>();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].to_string().trim(), "");
        assert_eq!(visible[1].to_string().trim(), "› hello");
        assert_eq!(visible[2].to_string().trim(), "");
    }

    #[test]
    fn assistant_prose_uses_text_colour_for_light_and_dark_palettes() {
        for palette in [
            Palette::github_light_high_contrast(),
            Palette::github_dark_high_contrast(),
        ] {
            let mut chat = TranscriptChat::default();
            chat.push_row(ChatRow::Assistant {
                body: "## Heading\nAssistant prose with `inline code`.".into(),
            });
            chat.prepare_layout(&palette, 80, false);
            let spans = chat
                .layout_lines()
                .iter()
                .flat_map(|line| line.spans.iter())
                .collect::<Vec<_>>();

            assert!(spans.iter().any(|span| {
                span.content.contains("Heading") && span.style.fg == Some(palette.text)
            }));
            assert!(spans.iter().any(|span| {
                span.content.contains("Assistant prose") && span.style.fg == Some(palette.text)
            }));
            assert!(spans.iter().any(|span| {
                span.content.contains("inline code")
                    && span.style.fg == Some(palette.text)
                    && span.style.bg == Some(palette.surface0)
            }));
        }
    }

    #[test]
    fn hides_system_and_task_text() {
        for text in [
            "<system-reminder>hidden",
            "<task-notification>hidden",
            "**Needs you: nothing.**\n```closing-state\n{}\n```",
            "Stop hook feedback",
        ] {
            let mut chat = TranscriptChat::default();
            chat.ingest(
                &serde_json::json!({"type":"assistant","message":{"stop_reason":"end_turn","content":[{"type":"text","text":text}]}}),
                None,
            );
            assert!(chat.rows.is_empty(), "unexpectedly rendered {text:?}");
        }
    }

    #[test]
    fn user_blocks_fill_width_with_padding_and_hanging_prompt() {
        for palette in [
            Palette::github_light_high_contrast(),
            Palette::github_dark_high_contrast(),
        ] {
            for width in [20, 60, 100, 160] {
                let rows = vec![
                    ChatRow::User {
                        body: "Readable user message with enough words to wrap. ".repeat(12),
                    },
                    ChatRow::Assistant {
                        body: "Assistant prose".into(),
                    },
                ];
                let lines = layout_lines(&rows, &palette, width, false);
                let user_lines = lines
                    .iter()
                    .filter(|line| {
                        line.spans
                            .iter()
                            .any(|span| span.style.bg == Some(palette.surface0))
                    })
                    .collect::<Vec<_>>();
                assert!(user_lines.len() > 1);
                assert_eq!(user_lines[0].to_string(), " ".repeat(width as usize));
                assert_eq!(
                    user_lines.last().expect("bottom padding").to_string(),
                    " ".repeat(width as usize)
                );
                for (index, line) in user_lines.iter().enumerate() {
                    assert_eq!(line.width(), width as usize);
                    for span in &line.spans {
                        assert_eq!(span.style.bg, Some(palette.surface0));
                    }
                    if index > 0 && index < user_lines.len() - 1 {
                        assert_eq!(line.spans[0].content, if index == 1 { "› " } else { "  " });
                        assert_eq!(line.spans[0].style.fg, Some(palette.overlay1));
                        assert!(!line.spans[1].content.starts_with(' '));
                        assert_eq!(line.spans[1].style.fg, Some(palette.text));
                    }
                }
                let mut buffer = Buffer::empty(Rect::new(0, 0, width, user_lines.len() as u16));
                for (y, line) in user_lines.iter().enumerate() {
                    buffer.set_line(0, y as u16, line, width);
                    for x in 0..width {
                        assert_eq!(buffer[(x, y as u16)].bg, palette.surface0);
                    }
                }
                assert_eq!(buffer[(0, 1)].symbol(), "›");
                assert_eq!(buffer[(2, 1)].symbol(), "R");
                assert_eq!(lines[user_lines.len()].width(), 0);
                let assistant = lines[user_lines.len() + 1..]
                    .iter()
                    .filter(|line| line.width() > 0)
                    .collect::<Vec<_>>();
                assert!(!assistant.is_empty(), "assistant prose is rendered");
                assert!(assistant
                    .iter()
                    .all(|line| line.to_string().starts_with("  ")));
                assert_eq!(
                    assistant
                        .iter()
                        .map(|line| line.to_string().trim().to_owned())
                        .collect::<Vec<_>>()
                        .join(" "),
                    "Assistant prose"
                );
            }
        }
    }

    #[test]
    fn worked_rows_collapse_and_expand_to_tool_summaries() {
        let duration = "29s";
        let tools = vec!["Read src/main.rs".to_owned(), "Bash cargo check".to_owned()];
        assert_eq!(
            worked_row_lines(duration, &tools, false, 80),
            ["Worked for 29s · 2 tools ›"]
        );
        assert_eq!(
            worked_row_lines(duration, &tools, true, 80),
            [
                "Worked for 29s · 2 tools ›",
                "Read src/main.rs",
                "Bash cargo check"
            ]
        );
    }

    #[test]
    fn layout_cache_reuses_lines_and_rebuilds_for_content_width_and_tool_expansion() {
        let palette = Palette::catppuccin();
        let mut chat = TranscriptChat::default();
        chat.push_row(ChatRow::Assistant {
            body: "A long transcript reply that wraps at narrower widths.".into(),
        });
        chat.push_row(ChatRow::Worked {
            duration: "1s".into(),
            tools: vec!["Read file.rs".into()],
        });

        assert!(chat.prepare_layout(&palette, 40, false));
        assert!(!chat.prepare_layout(&palette, 40, false));
        assert!(chat.prepare_layout(&palette, 30, false));
        assert!(chat.prepare_layout(&palette, 30, true));
        assert!(chat
            .layout_lines()
            .iter()
            .any(|line| line.to_string() == "Read file.rs"));

        chat.push_row(ChatRow::Assistant {
            body: "New content".into(),
        });
        assert!(chat.prepare_layout(&palette, 30, true));
    }

    #[test]
    fn updating_pending_transcript_state_preserves_layout_for_unchanged_rows() {
        let palette = Palette::catppuccin();
        let mut chat = TranscriptChat::default();
        chat.push_row(ChatRow::Assistant {
            body: "Already visible".into(),
        });
        assert!(chat.prepare_layout(&palette, 80, false));

        let mut updated = chat.clone();
        updated.layout = None;
        updated.pending_prose = "More text is still streaming".into();
        chat.update_from(updated);

        assert!(!chat.prepare_layout(&palette, 80, false));
    }

    #[test]
    fn worked_row_uses_singular_tool_label() {
        assert_eq!(
            worked_row_lines("1s", &["Read file.rs".into()], false, 80),
            ["Worked for 1s · 1 tool ›"]
        );
    }
}
