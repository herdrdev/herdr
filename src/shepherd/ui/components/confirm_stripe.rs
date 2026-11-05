use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::list_view::put_str;
use crate::app::state::Palette;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfirmStripeHits {
    pub confirm_button: Option<Rect>,
    pub cancel_button: Option<Rect>,
}

pub fn render_confirm_stripe(
    buffer: &mut Buffer,
    area: Rect,
    warning_text: &str,
    confirm_label: &str,
    cancel_label: &str,
    is_destructive: bool,
    palette: &Palette,
) -> ConfirmStripeHits {
    let mut hits = ConfirmStripeHits::default();
    if area.width < 10 || area.height < 2 {
        return hits;
    }

    let border_color = if is_destructive {
        palette.red
    } else {
        palette.accent
    };

    // Draw top separator/border
    let top_border = format!(
        "┌─ CONFIRMACIÓN ─{}",
        "─".repeat(area.width.saturating_sub(18) as usize)
    );
    put_str(
        buffer,
        area.x,
        area.y,
        area.width,
        &top_border,
        Style::default().fg(border_color),
    );

    let content_y = area.y + 1;

    // Background fill for stripe
    for y in content_y..area.bottom().saturating_sub(1) {
        for x in area.x..area.right() {
            let cell = &mut buffer[(x, y)];
            cell.set_char(' ');
            cell.set_bg(palette.surface0);
        }
    }

    // Warning icon and text
    let warn_prefix = if is_destructive { "⚠ " } else { "🔒 " };
    let full_warn = format!("{warn_prefix}{warning_text}");
    let warn_style = if is_destructive {
        Style::default()
            .fg(palette.red)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette.yellow)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD)
    };
    put_str(
        buffer,
        area.x + 2,
        content_y,
        area.width.saturating_sub(4),
        &full_warn,
        warn_style,
    );

    // Buttons line (either same line or line below if height allows)
    let btn_y = if area.height >= 3 {
        content_y + 1
    } else {
        content_y
    };
    let mut btn_x = area.x + 2;

    // If same line, place buttons on right
    if btn_y == content_y {
        let conf_text = format!("[ {confirm_label} ]");
        let can_text = format!("[ {cancel_label} ]");
        let needed_w = conf_text.chars().count() as u16 + can_text.chars().count() as u16 + 4;
        if area.width > needed_w + 10 {
            btn_x = area.right().saturating_sub(needed_w + 2);
        }
    }

    // Confirm button
    let conf_text = format!("[ {confirm_label} ]");
    let conf_w = conf_text.chars().count() as u16;
    if btn_x + conf_w <= area.right() {
        let conf_rect = Rect::new(btn_x, btn_y, conf_w, 1);
        hits.confirm_button = Some(conf_rect);
        let conf_style = if is_destructive {
            Style::default()
                .fg(palette.red)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(palette.green)
                .bg(palette.surface0)
                .add_modifier(Modifier::BOLD)
        };
        put_str(buffer, btn_x, btn_y, conf_w, &conf_text, conf_style);
        btn_x += conf_w + 2;
    }

    // Cancel button
    let can_text = format!("[ {cancel_label} ]");
    let can_w = can_text.chars().count() as u16;
    if btn_x + can_w <= area.right() {
        let can_rect = Rect::new(btn_x, btn_y, can_w, 1);
        hits.cancel_button = Some(can_rect);
        put_str(
            buffer,
            btn_x,
            btn_y,
            can_w,
            &can_text,
            Style::default().fg(palette.overlay0).bg(palette.surface0),
        );
    }

    // Bottom border
    let bot_y = area.bottom().saturating_sub(1);
    if bot_y > content_y {
        let bot_border = format!("└{}┘", "─".repeat(area.width.saturating_sub(2) as usize));
        put_str(
            buffer,
            area.x,
            bot_y,
            area.width,
            &bot_border,
            Style::default().fg(border_color),
        );
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
    fn test_confirm_stripe_pure_render_and_hits() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 70, 4);
        let mut buffer = Buffer::empty(area);

        let hits = render_confirm_stripe(
            &mut buffer,
            area,
            "¿Archivar la tarea #12?",
            "Confirmar (Enter)",
            "Cancelar (Esc)",
            true,
            &palette,
        );

        assert!(hits.confirm_button.is_some());
        assert!(hits.cancel_button.is_some());

        // Check header border text
        assert_eq!(buffer[(3, 0)].symbol(), "C");
        assert_eq!(buffer[(4, 0)].symbol(), "O");
    }
}
