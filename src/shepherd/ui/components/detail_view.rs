use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use crate::pane::ScrollMetrics;
use crate::ui::render_scrollbar_buffer;
use super::list_view::put_str;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertLevel {
    Info,
    Warning,
    Success,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DetailContentBlock {
    Heading(String),
    Paragraph(String),
    Checklist(Vec<(bool, String)>),
    BulletList(Vec<String>),
    Notice { text: String, level: AlertLevel },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailHeader {
    pub title: String,
    pub badges: Vec<(String, Style)>,
    pub metadata_line: Option<String>,
    pub actions: Vec<(String, Option<String>)>, // (label, shortcut)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetailViewHits {
    pub header_actions: Vec<(Rect, usize)>,
    pub bottom_actions: Vec<(Rect, usize)>,
    pub scrollbar: Option<Rect>,
    pub scroll_metrics: Option<ScrollMetrics>,
}

pub fn render_detail_view(
    buffer: &mut Buffer,
    area: Rect,
    header: &DetailHeader,
    content: &[DetailContentBlock],
    scroll: usize,
    bottom_actions: &[(&str, &str, bool)], // label, shortcut, primary
    palette: &Palette,
) -> DetailViewHits {
    let mut hits = DetailViewHits::default();
    if area.width < 10 || area.height < 4 {
        return hits;
    }

    let mut current_y = area.y;

    // 1. Header Title & Actions
    let mut title_x = area.x;
    put_str(
        buffer,
        title_x,
        current_y,
        area.width,
        &header.title,
        Style::default().fg(palette.text).add_modifier(Modifier::BOLD),
    );
    title_x += header.title.chars().count() as u16 + 1;

    for (badge_text, badge_style) in &header.badges {
        let bw = badge_text.chars().count() as u16;
        if title_x + bw < area.right() {
            put_str(buffer, title_x, current_y, bw, badge_text, *badge_style);
            title_x += bw + 1;
        }
    }

    // Header actions (right aligned)
    let mut action_right = area.right();
    for (i, (action_label, shortcut)) in header.actions.iter().enumerate().rev() {
        let label = match shortcut {
            Some(sc) => format!("[ {action_label} ({sc}) ]"),
            None => format!("[ {action_label} ]"),
        };
        let w = label.chars().count() as u16;
        if action_right >= area.x + w + 1 {
            let x = action_right - w;
            let rect = Rect::new(x, current_y, w, 1);
            hits.header_actions.push((rect, i));
            put_str(
                buffer,
                x,
                current_y,
                w,
                &label,
                Style::default().fg(palette.accent).add_modifier(Modifier::BOLD),
            );
            action_right = x.saturating_sub(1);
        }
    }

    current_y += 1;

    // Metadata line (e.g. Estado: Open | Asignado: coder-24)
    if let Some(ref meta) = header.metadata_line {
        if current_y < area.bottom() {
            put_str(
                buffer,
                area.x,
                current_y,
                area.width,
                meta,
                Style::default().fg(palette.overlay0),
            );
            current_y += 1;
        }
    }

    // Divider
    if current_y < area.bottom() {
        let sep = "─".repeat(area.width as usize);
        put_str(
            buffer,
            area.x,
            current_y,
            area.width,
            &sep,
            Style::default().fg(palette.surface_dim),
        );
        current_y += 1;
    }

    // Reserve bottom space for bottom actions
    let bottom_height: u16 = if !bottom_actions.is_empty() { 2 } else { 0 };
    let content_bottom = area.bottom().saturating_sub(bottom_height);
    let visible_content_h = content_bottom.saturating_sub(current_y) as usize;

    // Render content lines
    let mut lines: Vec<(String, Style)> = Vec::new();
    for block in content {
        match block {
            DetailContentBlock::Heading(h) => {
                lines.push((String::new(), Style::default()));
                lines.push((
                    h.clone(),
                    Style::default().fg(palette.accent).add_modifier(Modifier::BOLD),
                ));
            }
            DetailContentBlock::Paragraph(p) => {
                for line in p.lines() {
                    lines.push((line.to_string(), Style::default().fg(palette.text)));
                }
            }
            DetailContentBlock::Checklist(items) => {
                for (checked, text) in items {
                    let mark = if *checked { "[x] " } else { "[ ] " };
                    let style = if *checked {
                        Style::default().fg(palette.green)
                    } else {
                        Style::default().fg(palette.text)
                    };
                    lines.push((format!("{mark}{text}"), style));
                }
            }
            DetailContentBlock::BulletList(items) => {
                for text in items {
                    lines.push((
                        format!("• {text}"),
                        Style::default().fg(palette.text),
                    ));
                }
            }
            DetailContentBlock::Notice { text, level } => {
                let color = match level {
                    AlertLevel::Info => palette.accent,
                    AlertLevel::Warning => palette.yellow,
                    AlertLevel::Success => palette.green,
                };
                lines.push((
                    format!("ℹ {text}"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ));
            }
        }
    }

    let total_lines = lines.len();
    let has_scrollbar = total_lines > visible_content_h;
    let content_w = if has_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };

    let start_line = scroll.min(total_lines.saturating_sub(1));
    let end_line = (start_line + visible_content_h).min(total_lines);

    for (row_idx, line_idx) in (start_line..end_line).enumerate() {
        let y = current_y + row_idx as u16;
        let (ref text, ref style) = lines[line_idx];
        put_str(buffer, area.x, y, content_w, text, *style);
    }

    if has_scrollbar {
        let scrollbar_rect = Rect::new(area.right().saturating_sub(1), current_y, 1, visible_content_h as u16);
        let max_offset = total_lines.saturating_sub(visible_content_h);
        let metrics = ScrollMetrics {
            viewport_rows: visible_content_h,
            offset_from_bottom: max_offset.saturating_sub(scroll),
            max_offset_from_bottom: max_offset,
        };
        render_scrollbar_buffer(
            buffer,
            metrics,
            scrollbar_rect,
            palette.surface_dim,
            palette.surface1,
            "█",
        );
        hits.scrollbar = Some(scrollbar_rect);
        hits.scroll_metrics = Some(metrics);
    }

    // 3. Bottom actions
    if !bottom_actions.is_empty() && content_bottom < area.bottom() {
        let act_y = content_bottom + 1;
        let mut act_x = area.x;
        for (i, &(label, shortcut, primary)) in bottom_actions.iter().enumerate() {
            let text = format!("[ {label} ({shortcut}) ]");
            let w = text.chars().count() as u16;
            if act_x + w <= area.right() {
                let rect = Rect::new(act_x, act_y, w, 1);
                hits.bottom_actions.push((rect, i));
                let style = if primary {
                    Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };
                put_str(buffer, act_x, act_y, w, &text, style);
                act_x += w + 2;
            }
        }
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_detail_view_pure_render() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 60, 15);
        let mut buffer = Buffer::empty(area);

        let header = DetailHeader {
            title: "Task #12: Boolean Flags".into(),
            badges: vec![("[OPEN]".into(), Style::default().fg(palette.green))],
            metadata_line: Some("Status: Open | Assigned: coder-24".into()),
            actions: vec![("Edit".to_string(), Some("e".into()))],
        };

        let content = vec![
            DetailContentBlock::Heading("SPECIFICATION:".into()),
            DetailContentBlock::Paragraph("Add support for bool flags.".into()),
            DetailContentBlock::Checklist(vec![
                (true, "Parse --flag".into()),
                (false, "Unit tests".into()),
            ]),
        ];

        let bottom_actions = [("Launch Run", "r", true), ("Archive", "a", false)];

        let hits = render_detail_view(
            &mut buffer,
            area,
            &header,
            &content,
            0,
            &bottom_actions,
            &palette,
        );

        assert_eq!(hits.header_actions.len(), 1);
        assert_eq!(hits.bottom_actions.len(), 2);

        // Check title in buffer
        assert_eq!(buffer[(0, 0)].symbol(), "T");
        // Check header action button
        let (btn_rect, _) = hits.header_actions[0];
        assert_eq!(buffer[(btn_rect.x + 2, btn_rect.y)].symbol(), "E");
    }
}
