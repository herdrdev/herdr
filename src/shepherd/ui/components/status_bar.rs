use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use super::list_view::put_str;

pub fn render_status_bar(
    buffer: &mut Buffer,
    area: Rect,
    summary: &str,
    help_text: &str,
    notice: Option<&str>,
    palette: &Palette,
) {
    if area.width < 5 || area.height == 0 {
        return;
    }

    let y = area.y;

    // Fill background
    for x in area.x..area.right() {
        let cell = &mut buffer[(x, y)];
        cell.set_char(' ');
        cell.set_bg(palette.surface0);
    }

    if let Some(msg) = notice {
        put_str(
            buffer,
            area.x + 1,
            y,
            area.width.saturating_sub(2),
            msg,
            Style::default().fg(palette.yellow).bg(palette.surface0).add_modifier(Modifier::BOLD),
        );
        return;
    }

    // Left summary
    let sum_text = format!(" {summary}");
    let sum_w = sum_text.chars().count() as u16;
    put_str(
        buffer,
        area.x,
        y,
        sum_w,
        &sum_text,
        Style::default().fg(palette.text).bg(palette.surface0),
    );

    // Separator and help text
    let help_full = format!(" | {help_text} ");
    let _help_w = help_full.chars().count() as u16;
    let help_x = area.x + sum_w;
    if help_x < area.right() {
        let max_w = area.right().saturating_sub(help_x);
        put_str(
            buffer,
            help_x,
            y,
            max_w,
            &help_full,
            Style::default().fg(palette.overlay0).bg(palette.surface0),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_status_bar_pure_render() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);

        render_status_bar(
            &mut buffer,
            area,
            "4 tasks (3 open)",
            "n: new | a: archive | Esc: close",
            None,
            &palette,
        );

        assert_eq!(buffer[(1, 0)].symbol(), "4");
    }

    #[test]
    fn test_status_bar_notice_render() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 80, 1);
        let mut buffer = Buffer::empty(area);

        render_status_bar(
            &mut buffer,
            area,
            "4 tasks",
            "help",
            Some("Error: task already exists"),
            &palette,
        );

        assert_eq!(buffer[(1, 0)].symbol(), "E");
    }
}
