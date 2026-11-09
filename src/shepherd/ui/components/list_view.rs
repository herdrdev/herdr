use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use crate::pane::ScrollMetrics;
use crate::ui::render_scrollbar_buffer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListViewItem {
    pub title: String,
    pub badge: Option<(String, Style)>,
    pub meta: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListViewHits {
    pub rows: Vec<(Rect, usize)>,
    pub filter_tabs: Vec<(Rect, usize)>,
    pub action_button: Option<Rect>,
    pub scrollbar: Option<Rect>,
    pub scroll_metrics: Option<ScrollMetrics>,
}

/// Display width in terminal cells (wide CJK/emoji count 2).
pub fn text_width(s: impl AsRef<str>) -> u16 {
    unicode_width::UnicodeWidthStr::width(s.as_ref()).min(u16::MAX as usize) as u16
}

/// Greedy word wrap by display width; over-long words are hard-split. Never returns an empty vec.
pub fn wrap_text(s: &str, width: u16) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in s.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            loop {
                let need = text_width(&line) + text_width(&word) + u16::from(!line.is_empty());
                if need <= width {
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&word);
                    break;
                }
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    continue;
                }
                // word alone is wider than the line: split it
                let mut head = String::new();
                let mut rest = String::new();
                for ch in word.chars() {
                    if rest.is_empty() && text_width(&head) + text_width(ch.to_string()) <= width {
                        head.push(ch);
                    } else {
                        rest.push(ch);
                    }
                }
                if head.is_empty() {
                    // single glyph wider than the line
                    break;
                }
                out.push(head);
                word = rest;
                if word.is_empty() {
                    break;
                }
            }
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Writes `s` clipped to `max_w` cells; wide glyphs that do not fit are dropped, never split.
pub fn put_str(buffer: &mut Buffer, x: u16, y: u16, max_w: u16, s: &str, style: Style) {
    use unicode_width::UnicodeWidthChar;
    if max_w == 0 || y >= buffer.area.height {
        return;
    }
    let limit = x.saturating_add(max_w).min(buffer.area.width);
    let mut cur_x = x;
    for ch in s.chars() {
        let w = ch.width().unwrap_or(0) as u16;
        if w == 0 {
            continue;
        }
        if cur_x.saturating_add(w) > limit {
            break;
        }
        let cell = &mut buffer[(cur_x, y)];
        cell.set_char(ch);
        cell.set_style(style);
        if w == 2 {
            let c2 = &mut buffer[(cur_x + 1, y)];
            c2.set_symbol("");
            c2.set_style(style);
        }
        cur_x += w;
    }
}

pub fn render_list_view(
    buffer: &mut Buffer,
    area: Rect,
    items: &[ListViewItem],
    selected: usize,
    scroll: usize,
    filter_tabs: Option<(&[&str], usize)>,
    action_button: Option<(&str, &str)>,
    palette: &Palette,
) -> ListViewHits {
    let mut hits = ListViewHits::default();
    if area.width < 5 || area.height < 2 {
        return hits;
    }

    let mut current_y = area.y;

    // Header line: optional action button on right
    if let Some((btn_label, shortcut)) = action_button {
        let label = format!("[ {btn_label} ({shortcut}) ]");
        let btn_w = (text_width(&label)).min(area.width);
        let btn_x = area.x.saturating_add(area.width).saturating_sub(btn_w);
        let btn_rect = Rect::new(btn_x, current_y, btn_w, 1);
        put_str(
            buffer,
            btn_x,
            current_y,
            btn_w,
            &label,
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        );
        hits.action_button = Some(btn_rect);
    }

    current_y += 1;

    // Filter tabs if provided
    if let Some((tabs, active_tab_idx)) = filter_tabs {
        let mut tab_x = area.x;
        for (i, &tab_label) in tabs.iter().enumerate() {
            let active = i == active_tab_idx;
            let display = if active {
                format!("[✓ {tab_label}]")
            } else {
                format!("[{tab_label}]")
            };
            let tab_w = text_width(&display);
            if tab_x + tab_w <= area.right() {
                let tab_rect = Rect::new(tab_x, current_y, tab_w, 1);
                hits.filter_tabs.push((tab_rect, i));
                let style = if active {
                    Style::default()
                        .fg(palette.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };
                put_str(buffer, tab_x, current_y, tab_w, &display, style);
                tab_x += tab_w + 1;
            }
        }
        current_y += 1;
    }

    // Separator line
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

    // List items viewport
    let list_height = area.bottom().saturating_sub(current_y) as usize;
    if list_height == 0 {
        return hits;
    }

    let total_items = items.len();
    let has_scrollbar = total_items > list_height;
    let content_w = if has_scrollbar {
        area.width.saturating_sub(1)
    } else {
        area.width
    };

    let mut start_idx = scroll.min(total_items.saturating_sub(1));
    if selected < start_idx {
        start_idx = selected;
    } else if selected >= start_idx + list_height {
        start_idx = (selected + 1).saturating_sub(list_height);
    }
    let scroll = start_idx;
    let end_idx = (start_idx + list_height).min(total_items);

    for (row_offset, idx) in (start_idx..end_idx).enumerate() {
        let item_y = current_y + row_offset as u16;
        let is_selected = idx == selected;
        let row_rect = Rect::new(area.x, item_y, content_w, 1);
        hits.rows.push((row_rect, idx));

        // Background for selected row
        let bg_color = if is_selected {
            palette.surface0
        } else {
            palette.panel_bg
        };
        for x in area.x..area.x + content_w {
            let cell = &mut buffer[(x, item_y)];
            cell.set_char(' ');
            cell.set_bg(bg_color);
        }

        // Selection indicator
        let indicator = if is_selected { "▸ " } else { "  " };
        let marker_style = if is_selected {
            Style::default()
                .fg(palette.accent)
                .bg(bg_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0).bg(bg_color)
        };
        put_str(buffer, area.x, item_y, 2, indicator, marker_style);

        let mut text_x = area.x + 2;

        // Badge (if present)
        if let Some((badge_text, badge_style)) = &items[idx].badge {
            let b_w = text_width(&badge_text);
            if text_x + b_w < area.x + content_w {
                put_str(
                    buffer,
                    text_x,
                    item_y,
                    b_w,
                    badge_text,
                    badge_style.bg(bg_color),
                );
                text_x += b_w + 1;
            }
        }

        // Title
        let remaining_w = (area.x + content_w).saturating_sub(text_x);
        let title_style = if is_selected {
            Style::default()
                .fg(palette.text)
                .bg(bg_color)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(bg_color)
        };
        put_str(
            buffer,
            text_x,
            item_y,
            remaining_w,
            &items[idx].title,
            title_style,
        );
    }

    // Scrollbar if needed
    if has_scrollbar {
        let scrollbar_rect = Rect::new(
            area.right().saturating_sub(1),
            current_y,
            1,
            list_height as u16,
        );
        let max_offset = total_items.saturating_sub(list_height);
        let metrics = ScrollMetrics {
            viewport_rows: list_height,
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

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_text_wraps_splits_and_handles_wide_glyphs() {
        assert_eq!(wrap_text("uno dos tres", 7), vec!["uno dos", "tres"]);
        assert_eq!(wrap_text("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap_text("日本語日本語", 4), vec!["日本", "語日", "本語"]);
        assert_eq!(wrap_text("", 5), vec![""]);
        assert_eq!(wrap_text("x\ny", 5), vec!["x", "y"]);
    }

    #[test]
    fn put_str_clips_wide_glyphs_without_splitting() {
        let mut b = Buffer::empty(Rect::new(0, 0, 6, 1));
        put_str(&mut b, 0, 0, 5, "日本語", Style::default());
        assert_eq!(b[(0, 0)].symbol(), "日");
        assert_eq!(b[(2, 0)].symbol(), "本");
        assert_eq!(b[(4, 0)].symbol(), " "); // third glyph (2 cells) does not fit in 5
        assert_eq!(text_width("日本語"), 6);
    }

    #[test]
    fn selection_is_always_visible_and_hit_rows_are_in_range() {
        let palette = Palette::catppuccin();
        let area = Rect::new(0, 0, 30, 10);
        let items: Vec<ListViewItem> = (0..150)
            .map(|i| ListViewItem {
                title: format!("item {i}"),
                badge: None,
                meta: None,
            })
            .collect();
        for sel in [0usize, 5, 6, 7, 99, 149] {
            let mut b = Buffer::empty(area);
            let hits = render_list_view(&mut b, area, &items, sel, 0, None, None, &palette);
            assert!(
                hits.rows.iter().any(|&(_, i)| i == sel),
                "sel {sel} not visible"
            );
            assert!(hits
                .rows
                .iter()
                .all(|&(r, i)| i < 150 && r.bottom() <= area.bottom()));
        }
        // out-of-range selection/scroll must not panic
        let mut b = Buffer::empty(area);
        render_list_view(&mut b, area, &items, 9999, 9999, None, None, &palette);
        render_list_view(&mut b, area, &[], 3, 3, None, None, &palette);
    }

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_list_view_pure_render_and_selection() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 30, 8);
        let mut buffer = Buffer::empty(area);

        let items = vec![
            ListViewItem {
                title: "Task One".into(),
                badge: Some(("[OPEN]".into(), Style::default().fg(palette.green))),
                meta: None,
            },
            ListViewItem {
                title: "Task Two".into(),
                badge: Some(("[DONE]".into(), Style::default().fg(palette.overlay0))),
                meta: None,
            },
        ];

        let hits = render_list_view(
            &mut buffer,
            area,
            &items,
            0,
            0,
            Some((&["All", "Open"], 1)),
            Some(("+ New", "n")),
            &palette,
        );

        assert_eq!(hits.rows.len(), 2);
        assert_eq!(hits.filter_tabs.len(), 2);
        assert!(hits.action_button.is_some());

        // Row 0 has indicator '▸'
        assert_eq!(buffer[(0, 3)].symbol(), "▸");
        // Row 1 does not have indicator
        assert_eq!(buffer[(0, 4)].symbol(), " ");

        // Action button was rendered
        let btn_rect = hits.action_button.unwrap();
        assert!(buffer[(btn_rect.x + 2, btn_rect.y)].symbol() == "+");
    }

    #[test]
    fn test_list_view_hit_testing() {
        let palette = test_palette();
        let area = Rect::new(10, 5, 25, 10);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 20));

        let items = vec![
            ListViewItem {
                title: "Item A".into(),
                badge: None,
                meta: None,
            },
            ListViewItem {
                title: "Item B".into(),
                badge: None,
                meta: None,
            },
        ];

        let hits = render_list_view(&mut buffer, area, &items, 1, 0, None, None, &palette);
        assert_eq!(hits.rows.len(), 2);
        let (rect0, idx0) = hits.rows[0];
        let (rect1, idx1) = hits.rows[1];
        assert_eq!(idx0, 0);
        assert_eq!(idx1, 1);
        assert_eq!(rect0.x, 10);
        assert_eq!(rect1.y, rect0.y + 1);
    }
}
