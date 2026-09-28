//! The sidebar's idle star field: a small square panel in the bottom-left
//! corner of the sidebar, under the notepad and above the footer icons, with a
//! pause control docked in its bottom edge.
//!
//! The box is drawn with [`super::widgets::render_panel_shell`], the same
//! bordered shell the rest of the UI uses, rather than a shape invented here.
//! Its geometry is pure so the rows can be reserved before anything draws;
//! [`super::sidebar::workspace_list_rect_for_app`] subtracts them the same way
//! it subtracts the notepad's. The field itself comes from [`starfield`], which
//! knows nothing about terminals, palettes, or state.

mod starfield;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::widgets::render_panel_shell;
use crate::app::state::Palette;
use crate::app::AppState;
use starfield::Level;

/// The box, in cells. Terminal cells are about twice as tall as they are wide,
/// so two columns per row is what actually looks square on screen.
const BOX_COLS: u16 = 12;
const BOX_ROWS: u16 = 6;

/// Rows the current-layout workspace list keeps before the animation may claim any.
const MIN_LIST_ROWS_BESIDE_ANIMATION: u16 = 6;

/// Shown while the field is moving: clicking it stops the animation.
const PAUSE_GLYPH: &str = "‖";
/// Shown while it is frozen: clicking it starts the animation again.
const RESUME_GLYPH: &str = "▸";

fn button_glyph(app: &AppState) -> &'static str {
    if app.hyperspace.paused() {
        RESUME_GLYPH
    } else {
        PAUSE_GLYPH
    }
}

/// How many rows of `content` the animation occupies. All or nothing: a box
/// that cannot be its full size is not a square, so it is not drawn at all.
/// Zero whenever it is off, the sidebar is collapsed or too narrow, or the list
/// would fall below its layout's minimum row count.
pub(crate) fn animation_height(app: &AppState, content: Rect) -> u16 {
    if !app.hyperspace.enabled || app.sidebar_collapsed || content.width < BOX_COLS {
        return 0;
    }
    let workspace_floor = if app.sidebar_sections_layout {
        crate::ui::sidebar::sidebar_workspace_floor(app)
    } else {
        MIN_LIST_ROWS_BESIDE_ANIMATION
    };
    let spare = content.height.saturating_sub(workspace_floor);
    if spare < BOX_ROWS {
        return 0;
    }
    BOX_ROWS
}

/// The box itself, in the bottom-left corner of the content area. The reserved
/// rows span the sidebar's width; the box only takes the left of them.
pub(crate) fn animation_box_rect(app: &AppState, content: Rect) -> Rect {
    let height = animation_height(app, content);
    if height == 0 {
        return Rect::default();
    }
    Rect::new(
        content.x,
        content.bottom().saturating_sub(height),
        BOX_COLS,
        height,
    )
}

/// The pause control: the box's bottom edge, glyph at its left. The whole edge
/// is clickable because one glyph is a cruel target for a mouse.
pub(crate) fn pause_hit_area(app: &AppState, boxed: Rect) -> Rect {
    if boxed.width == 0 || boxed.height == 0 || !app.hyperspace.enabled {
        return Rect::default();
    }
    Rect::new(boxed.x, boxed.bottom().saturating_sub(1), boxed.width, 1)
}

fn level_color(level: Level, palette: &Palette) -> ratatui::style::Color {
    // Every one of these is checked by the chrome contrast floor in
    // `ui::tests::themed_chrome_keeps_a_contrast_floor`; `surface_dim` reads as
    // black-on-black there, so the faint end starts at `overlay0`.
    match level {
        Level::Faint => palette.overlay0,
        Level::Dim => palette.subtext0,
        Level::Bright => palette.accent,
    }
}

pub(crate) fn render_animation(app: &AppState, frame: &mut Frame, boxed: Rect) {
    let hovered = matches!(
        app.hovered_control,
        Some(crate::app::state::ControlId::SidebarAnimationPause)
    );
    render_animation_with_hover(app, frame, boxed, hovered);
}

pub(crate) fn render_animation_with_hover(
    app: &AppState,
    frame: &mut Frame,
    boxed: Rect,
    hovered: bool,
) {
    if boxed.width == 0 || boxed.height == 0 || !app.hyperspace.enabled {
        return;
    }
    let palette = &app.palette;
    let bg = palette.sidebar_background();
    let Some(field) = render_panel_shell(frame, boxed, palette.overlay0, bg) else {
        return;
    };
    if field.width == 0 || field.height == 0 {
        return;
    }

    let stars = starfield::field(field.width, field.height, app.hyperspace.step());
    for (index, row) in stars.rows().enumerate() {
        // One span per run of equal brightness, so a row of scattered stars
        // costs a handful of spans rather than one per cell.
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_level = None;
        for cell in row {
            if Some(cell.level) != run_level {
                if let Some(level) = run_level.take() {
                    spans.push(Span::styled(
                        std::mem::take(&mut run),
                        Style::default().fg(level_color(level, palette)).bg(bg),
                    ));
                }
                run_level = Some(cell.level);
            }
            run.push(cell.ch);
        }
        if let Some(level) = run_level {
            spans.push(Span::styled(
                run,
                Style::default().fg(level_color(level, palette)).bg(bg),
            ));
        }
        let y = field.y.saturating_add(index as u16);
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(field.x, y, field.width, 1),
        );
    }

    // The control goes on last, docked one cell into the bottom edge so the
    // corner stays a corner.
    let style = if hovered {
        Style::default().fg(palette.accent).bg(bg)
    } else if app.hyperspace.paused() {
        Style::default().fg(palette.subtext0).bg(bg)
    } else {
        Style::default().fg(palette.overlay1).bg(bg)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(button_glyph(app), style)),
        Rect::new(
            boxed.x.saturating_add(1),
            boxed.bottom().saturating_sub(1),
            1,
            1,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.hyperspace.enabled = true;
        app
    }

    #[test]
    fn a_disabled_animation_reserves_no_rows() {
        let mut app = state();
        app.hyperspace.enabled = false;
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), 0);
        assert_eq!(
            animation_box_rect(&app, Rect::new(0, 0, 26, 40)),
            Rect::default()
        );
    }

    #[test]
    fn a_collapsed_sidebar_reserves_no_rows() {
        let mut app = state();
        app.sidebar_collapsed = true;
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), 0);
    }

    #[test]
    fn the_workspace_list_keeps_its_floor_before_the_animation_gets_anything() {
        let app = state();
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 6)), 0);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 11)), 0);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 12)), BOX_ROWS);
        assert_eq!(animation_height(&app, Rect::new(0, 0, 26, 40)), BOX_ROWS);
    }

    #[test]
    fn a_sidebar_narrower_than_the_box_gets_no_box_at_all() {
        // Half a square is not a square, so it degrades to nothing rather than
        // to a squeezed strip.
        let app = state();
        assert_eq!(animation_height(&app, Rect::new(0, 0, BOX_COLS - 1, 40)), 0);
        assert_eq!(
            animation_height(&app, Rect::new(0, 0, BOX_COLS, 40)),
            BOX_ROWS
        );
    }

    #[test]
    fn the_box_is_square_and_sits_in_the_bottom_left_corner() {
        let app = state();
        let content = Rect::new(3, 2, 26, 40);
        let boxed = animation_box_rect(&app, content);
        assert_eq!(boxed.bottom(), content.bottom());
        assert_eq!(boxed.x, content.x, "left edge of the sidebar");
        assert_eq!((boxed.width, boxed.height), (BOX_COLS, BOX_ROWS));
        assert!(
            boxed.width < content.width,
            "the box no longer spans the sidebar"
        );
        // Two columns per row is what reads as square in a terminal cell grid.
        assert_eq!(boxed.width, boxed.height * 2);
    }

    #[test]
    fn the_button_is_the_boxs_bottom_edge() {
        let app = state();
        let boxed = animation_box_rect(&app, Rect::new(3, 2, 26, 40));
        let button = pause_hit_area(&app, boxed);
        assert_eq!(button.x, boxed.x);
        assert_eq!(button.y, boxed.bottom() - 1);
        assert_eq!((button.width, button.height), (boxed.width, 1));
    }

    #[test]
    fn the_button_glyph_follows_the_pause_state() {
        let mut app = state();
        assert_eq!(button_glyph(&app), PAUSE_GLYPH);
        app.hyperspace.toggle_paused(std::time::Instant::now());
        assert_eq!(button_glyph(&app), RESUME_GLYPH);
    }

    #[test]
    fn an_empty_or_disabled_box_offers_no_button() {
        let mut app = state();
        assert_eq!(pause_hit_area(&app, Rect::default()), Rect::default());
        let boxed = animation_box_rect(&app, Rect::new(0, 0, 26, 40));
        app.hyperspace.enabled = false;
        assert_eq!(pause_hit_area(&app, boxed), Rect::default());
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// A clock that actually moves. `Instant::now() + k` on every call does
    /// not: the tick pushes its next deadline out from the instant it was
    /// handed, so a fixed offset from a fresh `now` never reaches it.
    struct Clock(std::time::Instant);

    impl Clock {
        fn new() -> Self {
            Self(std::time::Instant::now())
        }

        fn advance(&mut self) -> std::time::Instant {
            self.0 += crate::hyperspace::FRAME_INTERVAL;
            self.0
        }
    }

    fn enabled() -> AppState {
        let mut app = AppState::test_new();
        app.hyperspace.enabled = true;
        app
    }

    fn box_text(app: &AppState, boxed: Rect) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).expect("test terminal");
        terminal
            .draw(|frame| render_animation(app, frame, boxed))
            .expect("render");
        let buffer = terminal.backend().buffer().clone();
        (boxed.y..boxed.bottom())
            .map(|y| {
                (boxed.x..boxed.right())
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn the_box_draws_a_border_a_field_and_its_control() {
        let app = enabled();
        let rows = box_text(&app, Rect::new(0, 0, BOX_COLS, BOX_ROWS));
        assert_eq!(rows.len(), usize::from(BOX_ROWS));
        assert!(
            rows[0].starts_with('┌') && rows[0].ends_with('┐'),
            "{rows:?}"
        );
        let last = rows.last().expect("bottom edge");
        assert!(last.starts_with('└') && last.ends_with('┘'), "{rows:?}");
        assert!(
            last.contains(PAUSE_GLYPH),
            "the control is docked in the bottom edge: {rows:?}"
        );
        let field: String = rows[1..rows.len() - 1].join("");
        assert!(
            field.chars().filter(|c| *c == '·' || *c == '*').count() >= 3,
            "star field: {rows:?}"
        );
    }

    #[test]
    fn a_paused_box_is_the_same_frame_every_time() {
        let mut app = enabled();
        app.hyperspace.toggle_paused(std::time::Instant::now());
        let boxed = Rect::new(0, 0, BOX_COLS, BOX_ROWS);
        let first = box_text(&app, boxed);
        let mut clock = Clock::new();
        for _ in 0..8 {
            assert!(!app.hyperspace.tick(clock.advance()), "paused means frozen");
        }
        assert_eq!(first, box_text(&app, boxed));
        assert!(
            first.last().expect("bottom edge").contains(RESUME_GLYPH),
            "{first:?}"
        );
    }

    #[test]
    fn every_frame_keeps_the_border_and_the_control_intact() {
        // The field is drawn inside the border and the control after it, so no
        // frame may eat either.
        let mut app = enabled();
        let boxed = Rect::new(0, 0, BOX_COLS, BOX_ROWS);
        let mut clock = Clock::new();
        for _ in 0..starfield::FRAMES {
            assert!(app.hyperspace.tick(clock.advance()));
            let rows = box_text(&app, boxed);
            assert!(rows[0].starts_with('┌'), "{rows:?}");
            assert!(
                rows.last().expect("bottom edge").contains(PAUSE_GLYPH),
                "{rows:?}"
            );
        }
    }

    /// The animation moving is the whole feature, so prove consecutive frames
    /// actually differ once drawn.
    #[test]
    fn consecutive_frames_draw_different_fields() {
        let mut app = enabled();
        let boxed = Rect::new(0, 0, BOX_COLS, BOX_ROWS);
        let mut clock = Clock::new();
        let mut seen = vec![box_text(&app, boxed)];
        for _ in 0..3 {
            assert!(app.hyperspace.tick(clock.advance()));
            seen.push(box_text(&app, boxed));
        }
        // Not just "the next frame differs": four consecutive frames must all
        // be distinct, which is what "it is animating" means on screen.
        for (index, frame) in seen.iter().enumerate() {
            assert!(
                !seen[..index].contains(frame),
                "frame {index} repeats an earlier one: {seen:?}"
            );
        }
    }

    /// The panel is only shipped if it reaches the real sidebar, under the
    /// notepad and above the footer icons.
    #[test]
    fn the_box_reaches_the_bottom_left_of_the_real_sidebar() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = enabled();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let boxed = app.view.hyperspace_rect;
        assert!(boxed.height > 0, "a full-height sidebar holds the box");
        assert_eq!(boxed.x, app.view.sidebar_rect.x, "left edge");
        assert_eq!(
            boxed.bottom(),
            app.view.sidebar_rect.bottom() - 1,
            "the footer icon row keeps the last row"
        );
        assert!(
            boxed.width < app.view.sidebar_rect.width,
            "it must not span the sidebar"
        );

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let top: String = (boxed.x..boxed.right())
            .map(|x| buffer[(x, boxed.y)].symbol())
            .collect();
        assert!(top.starts_with('┌') && top.ends_with('┐'), "{top:?}");
    }

    /// Switching it off has to give the rows back, not just stop drawing.
    #[test]
    fn turning_it_off_returns_the_rows_to_the_workspace_list() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let mut app = enabled();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let with = crate::ui::workspace_list_rect_for_app(&app, app.view.sidebar_rect);

        app.hyperspace.enabled = false;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let without = crate::ui::workspace_list_rect_for_app(&app, app.view.sidebar_rect);

        assert_eq!(app.view.hyperspace_rect, Rect::default());
        assert_eq!(app.view.hyperspace_pause_hit_area, Rect::default());
        assert_eq!(without.height, with.height + BOX_ROWS);
    }
}
