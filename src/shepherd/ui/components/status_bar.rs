use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::list_view::put_str;
use super::list_view::text_width;
use crate::app::state::Palette;

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
            Style::default()
                .fg(palette.yellow)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD),
        );
        return;
    }

    let text_style = Style::default().fg(palette.text).bg(palette.surface0);
    let help_style = Style::default().fg(palette.overlay0).bg(palette.surface0);
    let help = fit_help(summary, help_text, area.width);
    let sum_text = format!(" {summary}");
    let sum_w = text_width(&sum_text).min(area.width);
    put_str(buffer, area.x, y, sum_w, &sum_text, text_style);
    put_str(
        buffer,
        area.x + sum_w,
        y,
        area.width - sum_w,
        &help,
        help_style,
    );
}

/// Help hints that fit after the summary. Hints are `" | "`-separated; whole hints are
/// dropped from the middle so the first (navigation) and last (`Esc: salir`) stay visible.
fn fit_help(summary: &str, help_text: &str, width: u16) -> String {
    let budget = width.saturating_sub(text_width(summary) + 2) as usize;
    let hints: Vec<&str> = help_text.split(" | ").collect();
    let join = |hs: &[&str]| format!(" | {} ", hs.join(" | "));
    let mut keep: Vec<&str> = hints.clone();
    while keep.len() > 1 && text_width(join(&keep)) as usize > budget + 1 {
        // drop from the middle first, then the leading hint
        keep.remove(if keep.len() > 2 { keep.len() - 2 } else { 0 });
    }
    join(&keep)
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
    fn fit_help_keeps_first_and_last_hints() {
        let h = "a: uno | b: dos | c: tres | d: cuatro | Esc: salir";
        assert_eq!(fit_help("S", h, 200), format!(" | {h} "));
        let narrow = fit_help("S", h, 30);
        assert!(narrow.contains("Esc: salir"), "{narrow}");
        assert!(text_width(&narrow) <= 30);
        // tiny width never panics and still ends with the exit hint
        assert!(fit_help("resumen largo", h, 5).contains("Esc: salir"));
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
