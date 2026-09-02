use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::app::state::{Palette, ProductAnnouncementState, ReleaseNotesState};

pub(crate) const RELEASE_NOTES_MODAL_SIZE: (u16, u16) = (80, 24);
pub(crate) const PRODUCT_ANNOUNCEMENT_MODAL_SIZE: (u16, u16) = (88, 24);

fn release_notes_inline_spans<'a>(
    text: &str,
    base_style: Style,
    code_style: Style,
) -> (usize, Vec<Span<'a>>) {
    let mut spans = Vec::new();
    let mut width = 0;
    let mut remaining = text;

    while let Some(start) = remaining.find('`') {
        let (before, after_start) = remaining.split_at(start);
        if !before.is_empty() {
            width += before.chars().count();
            spans.push(Span::styled(before.to_string(), base_style));
        }

        let after_start = &after_start[1..];
        let Some(end) = after_start.find('`') else {
            let literal = format!("`{after_start}");
            width += literal.chars().count();
            spans.push(Span::styled(literal, base_style));
            remaining = "";
            break;
        };

        let (code, after_end) = after_start.split_at(end);
        width += code.chars().count();
        if !code.is_empty() {
            // Keep short config examples together when Paragraph wraps.
            // Snippets like `new_tab = "prefix+c"` read poorly when they
            // split at the spaces around `=` in narrow announcement modals.
            let display_code = if code.contains('=') {
                code.replace(' ', "\u{00a0}")
            } else {
                code.to_string()
            };
            spans.push(Span::styled(display_code, code_style));
        }
        remaining = &after_end[1..];
    }

    if !remaining.is_empty() {
        width += remaining.chars().count();
        spans.push(Span::styled(remaining.to_string(), base_style));
    }

    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }

    (width, spans)
}

pub(crate) fn release_notes_lines<'a>(body: &'a str, p: &Palette) -> Vec<(usize, Line<'a>)> {
    let mut lines = Vec::new();
    let mut in_fenced_code_block = false;
    let text_style = Style::default().fg(p.text);
    let inline_code_style = Style::default()
        .fg(p.accent)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);

    for raw in body.lines() {
        let trimmed = raw.trim_end();
        if trimmed.trim_start().starts_with("```") {
            in_fenced_code_block = !in_fenced_code_block;
            continue;
        }

        if in_fenced_code_block {
            let code_bg = p.surface1;
            let gutter_style = Style::default().fg(p.accent).bg(code_bg);
            let code_style = Style::default().fg(p.text).bg(code_bg);
            let width = 2 + trimmed.chars().count();
            let mut spans = vec![
                Span::styled("▏", gutter_style),
                Span::styled(" ", code_style),
            ];
            if !trimmed.is_empty() {
                spans.push(Span::styled(trimmed.to_string(), code_style));
            }
            lines.push((width, Line::from(spans)));
            continue;
        }

        if trimmed.is_empty() {
            lines.push((0, Line::raw("")));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("### ") {
            let text = rest.trim().to_string();
            if text.is_empty() {
                lines.push((0, Line::raw("")));
                continue;
            }
            let width = 1 + text.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        text.to_uppercase(),
                        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ));
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("- ") {
            let (text_width, mut spans) =
                release_notes_inline_spans(rest, text_style, inline_code_style);
            let width = 3 + text_width;
            let mut line_spans = vec![Span::styled(" • ", Style::default().fg(p.accent))];
            line_spans.append(&mut spans);
            lines.push((width, Line::from(line_spans)));
            continue;
        }

        let (text_width, mut spans) =
            release_notes_inline_spans(trimmed, text_style, inline_code_style);
        let width = 1 + text_width;
        let mut line_spans = vec![Span::raw(" ")];
        line_spans.append(&mut spans);
        lines.push((width, Line::from(line_spans)));
    }

    lines
}

fn release_notes_preview_line_entries<'a>(
    install_command: &str,
    p: &Palette,
) -> Vec<(usize, Line<'a>)> {
    let title_style = Style::default().fg(p.text).add_modifier(Modifier::BOLD);
    let text_style = Style::default().fg(p.text);
    let inline_code_style = Style::default()
        .fg(p.accent)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);
    let instruction = crate::update::update_install_instruction(install_command);
    let (instruction_width, mut instruction_spans) =
        release_notes_inline_spans(&instruction, text_style, inline_code_style);
    instruction_spans.insert(0, Span::raw(" "));

    vec![
        (
            15,
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "●",
                    Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" update ready", title_style),
            ]),
        ),
        (instruction_width + 1, Line::from(instruction_spans)),
    ]
}

pub(crate) fn release_notes_display_lines<'a>(
    notes: &'a ReleaseNotesState,
    install_command: &str,
    p: &Palette,
) -> Vec<(usize, Line<'a>)> {
    let mut lines = Vec::new();
    if notes.preview {
        lines.extend(release_notes_preview_line_entries(install_command, p));
        lines.push((0, Line::raw("")));
    }
    lines.extend(release_notes_lines(notes.body.as_str(), p));
    lines
}

pub(crate) fn product_announcement_display_lines<'a>(
    announcement: &'a ProductAnnouncementState,
    p: &Palette,
) -> Vec<(usize, Line<'a>)> {
    release_notes_lines(announcement.body.as_str(), p)
}

fn display_lines_scroll_metrics(
    lines: &[(usize, Line<'_>)],
    scroll: u16,
    body: Rect,
) -> crate::pane::ScrollMetrics {
    let viewport_rows = body.height.max(1) as usize;
    let rows_for_width = |width: u16| release_notes_wrapped_line_count(lines, width.max(1));
    let full_width = body.width.max(1);
    let initial_rows = rows_for_width(full_width);
    let wrap_width = if initial_rows > viewport_rows && full_width > 1 {
        full_width.saturating_sub(1)
    } else {
        full_width
    };
    let max_offset_from_bottom = rows_for_width(wrap_width).saturating_sub(viewport_rows);
    crate::pane::ScrollMetrics {
        offset_from_bottom: max_offset_from_bottom.saturating_sub(usize::from(scroll)),
        max_offset_from_bottom,
        viewport_rows,
    }
}

pub(crate) fn release_notes_scroll_metrics(
    notes: &ReleaseNotesState,
    install_command: &str,
    body: Rect,
    p: &Palette,
) -> crate::pane::ScrollMetrics {
    display_lines_scroll_metrics(
        &release_notes_display_lines(notes, install_command, p),
        notes.scroll,
        body,
    )
}

pub(crate) fn product_announcement_scroll_metrics(
    announcement: &ProductAnnouncementState,
    body: Rect,
    p: &Palette,
) -> crate::pane::ScrollMetrics {
    display_lines_scroll_metrics(
        &product_announcement_display_lines(announcement, p),
        announcement.scroll,
        body,
    )
}

pub(crate) fn release_notes_wrapped_line_count(lines: &[(usize, Line<'_>)], width: u16) -> usize {
    Paragraph::new(
        lines
            .iter()
            .map(|(_, line)| line.clone())
            .collect::<Vec<_>>(),
    )
    .wrap(Wrap { trim: false })
    .line_count(width.max(1))
}

pub(crate) fn release_notes_close_button_rect(area: Rect) -> Rect {
    super::widgets::close_button_rect(area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Palette;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn release_notes_inline_code_spans_are_styled_without_backticks() {
        let palette = Palette::catppuccin();
        let lines = release_notes_lines("- `herdr pane run ...` now works", &palette);

        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0].1), " • herdr pane run ... now works");
        assert_eq!(lines[0].1.spans[1].content.as_ref(), "herdr pane run ...");
        assert_eq!(lines[0].1.spans[1].style.fg, Some(palette.accent));
        assert_eq!(lines[0].1.spans[1].style.bg, Some(palette.surface0));
    }

    #[test]
    fn release_notes_config_inline_code_uses_nonbreaking_spaces() {
        let palette = Palette::catppuccin();
        let lines = release_notes_lines("- After: `new_tab = \"prefix+c\"`", &palette);

        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].1.spans[2].content.as_ref(),
            "new_tab\u{00a0}=\u{00a0}\"prefix+c\""
        );
        assert_eq!(
            line_text(&lines[0].1).replace('\u{00a0}', " "),
            " • After: new_tab = \"prefix+c\""
        );
    }

    #[test]
    fn release_notes_preview_lines_show_update_steps() {
        let palette = Palette::catppuccin();
        let lines = release_notes_preview_line_entries("herdr update", &palette)
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), " ● update ready");
        assert_eq!(
            line_text(&lines[1]),
            " detach, run herdr update, then run Herdr again to reconnect"
        );
        assert_eq!(lines[0].spans[1].style.fg, Some(palette.accent));
        assert_eq!(lines[0].spans[2].style.fg, Some(palette.text));
        assert_eq!(lines[1].spans[2].content.as_ref(), "herdr update");
        assert_eq!(lines[1].spans[2].style.fg, Some(palette.accent));
        assert_eq!(lines[1].spans[2].style.bg, Some(palette.surface0));
    }

    #[test]
    fn release_notes_preview_display_is_part_of_the_scrollable_notes_body() {
        let palette = Palette::catppuccin();
        let notes = ReleaseNotesState {
            version: "0.6.6".into(),
            body: "### Added\n- One".into(),
            scroll: 0,
            preview: true,
        };

        let lines = release_notes_display_lines(&notes, "herdr update", &palette);

        assert_eq!(line_text(&lines[0].1), " ● update ready");
        assert_eq!(
            line_text(&lines[1].1),
            " detach, run herdr update, then run Herdr again to reconnect"
        );
        assert_eq!(line_text(&lines[2].1), "");
        assert_eq!(line_text(&lines[3].1), " ADDED");
        assert_eq!(line_text(&lines[4].1), " • One");
    }

    #[test]
    fn release_notes_fenced_code_blocks_render_as_preformatted_lines() {
        let palette = Palette::catppuccin();
        let lines = release_notes_lines(
            "### Fixed\n```bash\njust check\n- not a bullet\n```\n- after",
            &palette,
        );

        assert_eq!(lines.len(), 4);
        assert_eq!(line_text(&lines[0].1), " FIXED");
        assert_eq!(line_text(&lines[1].1), "▏ just check");
        assert_eq!(line_text(&lines[2].1), "▏ - not a bullet");
        assert_eq!(line_text(&lines[3].1), " • after");
        assert_eq!(lines[1].1.spans[0].style.fg, Some(palette.accent));
        assert_eq!(lines[1].1.spans[0].style.bg, Some(palette.surface1));
        assert_eq!(lines[1].1.spans[1].style.bg, Some(palette.surface1));
        assert_eq!(lines[1].1.spans[2].style.bg, Some(palette.surface1));
    }

    #[test]
    fn release_notes_fenced_code_blocks_preserve_blank_lines() {
        let palette = Palette::catppuccin();
        let lines = release_notes_lines("```\nfirst\n\nsecond\n```", &palette);

        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0].1), "▏ first");
        assert_eq!(line_text(&lines[1].1), "▏ ");
        assert_eq!(line_text(&lines[2].1), "▏ second");
    }
}
