use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use super::list_view::put_str;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmptyStateHits {
    pub actions: Vec<(Rect, usize)>,
}

pub fn render_empty_state(
    buffer: &mut Buffer,
    area: Rect,
    title: &str,
    message: &str,
    actions: &[(&str, Option<&str>)], // (label, shortcut)
    palette: &Palette,
) -> EmptyStateHits {
    let mut hits = EmptyStateHits::default();
    if area.width < 10 || area.height < 4 {
        return hits;
    }

    let mut current_y = area.y + area.height / 4;

    // Centered title
    let title_len = title.chars().count() as u16;
    let title_x = area.x + (area.width.saturating_sub(title_len)) / 2;
    put_str(
        buffer,
        title_x,
        current_y,
        area.width,
        title,
        Style::default().fg(palette.accent).add_modifier(Modifier::BOLD),
    );
    current_y += 2;

    // Centered or wrapped message
    for line in message.lines() {
        let line_len = line.chars().count() as u16;
        let line_x = area.x + (area.width.saturating_sub(line_len)) / 2;
        put_str(
            buffer,
            line_x,
            current_y,
            area.width,
            line,
            Style::default().fg(palette.overlay0),
        );
        current_y += 1;
    }

    current_y += 1;

    // Actions buttons
    if !actions.is_empty() && current_y < area.bottom() {
        let mut total_btn_w: u16 = 0;
        let mut btn_texts: Vec<String> = Vec::new();
        for (label, shortcut) in actions {
            let text = match shortcut {
                Some(sc) => format!("[ {label} ({sc}) ]"),
                None => format!("[ {label} ]"),
            };
            total_btn_w += text.chars().count() as u16 + 2;
            btn_texts.push(text);
        }

        let mut btn_x = area.x + (area.width.saturating_sub(total_btn_w)) / 2;
        for (i, text) in btn_texts.iter().enumerate() {
            let w = text.chars().count() as u16;
            if btn_x + w <= area.right() {
                let rect = Rect::new(btn_x, current_y, w, 1);
                hits.actions.push((rect, i));
                let style = if i == 0 {
                    Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette.overlay0)
                };
                put_str(buffer, btn_x, current_y, w, text, style);
                btn_x += w + 2;
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
    fn test_empty_state_pure_render_and_hits() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 60, 12);
        let mut buffer = Buffer::empty(area);

        let actions = [("+ Create First Task", Some("n")), ("Import GitHub", Some("i"))];
        let hits = render_empty_state(
            &mut buffer,
            area,
            "NO TASKS RECORDED",
            "There are no active tasks.\nCreate one to get started.",
            &actions,
            &palette,
        );

        assert_eq!(hits.actions.len(), 2);
        let (rect0, _) = hits.actions[0];
        assert_eq!(buffer[(rect0.x + 2, rect0.y)].symbol(), "+");
    }
}
