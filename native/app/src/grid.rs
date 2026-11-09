//! The terminal area: paints the server's cell grid and turns mouse input into messages.

use crate::palette;
use iced::mouse;
use iced::widget::canvas;
use iced::{Font, Point, Rectangle, Size, Theme};
use shepherd_app_core::wire::{style, Color as Cell, PaneSurface, PaneSurfacePane};

/// JetBrains Mono at 13 px: 0.6 em of advance, and 17 px of line.
pub const CELL_W: f32 = 7.8;
pub const CELL_H: f32 = 17.0;
pub const FONT_SIZE: f32 = 13.0;

/// What happened in the terminal area, in terminal cells.
#[derive(Debug, Clone)]
pub enum Event {
    Press {
        pane: String,
        focused: bool,
        mouse_reporting: bool,
        column: u16,
        row: u16,
    },
    Release {
        pane: String,
        mouse_reporting: bool,
        column: u16,
        row: u16,
    },
    Scroll {
        pane: String,
        up: bool,
        column: u16,
        row: u16,
    },
}

pub struct Grid<'a> {
    pub surface: &'a PaneSurface,
}

/// The pane under a cell of the surface and the cell relative to its content.
pub fn pane_at(
    surface: &PaneSurface,
    column: u16,
    row: u16,
) -> Option<(&PaneSurfacePane, u16, u16)> {
    surface.panes.iter().find_map(|pane| {
        let r = pane.rect;
        let inside = column >= r.x && column < r.x + r.width && row >= r.y && row < r.y + r.height;
        inside.then(|| {
            let inner = pane.inner_rect;
            (
                pane,
                column.saturating_sub(inner.x),
                row.saturating_sub(inner.y),
            )
        })
    })
}

fn cell_of(bounds: Rectangle, cursor: mouse::Cursor) -> Option<(u16, u16)> {
    let position = cursor.position_in(bounds)?;
    Some(((position.x / CELL_W) as u16, (position.y / CELL_H) as u16))
}

impl canvas::Program<crate::Message> for Grid<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<crate::Message>> {
        let iced::Event::Mouse(event) = event else {
            return None;
        };
        let (column, row) = cell_of(bounds, cursor)?;
        let (pane, rel_column, rel_row) = pane_at(self.surface, column, row)?;
        let id = pane.pane_id.clone();
        let event = match event {
            mouse::Event::ButtonPressed(mouse::Button::Left) => Event::Press {
                pane: id,
                focused: pane.focused,
                mouse_reporting: pane.mouse_reporting,
                column: rel_column,
                row: rel_row,
            },
            mouse::Event::ButtonReleased(mouse::Button::Left) => Event::Release {
                pane: id,
                mouse_reporting: pane.mouse_reporting,
                column: rel_column,
                row: rel_row,
            },
            mouse::Event::WheelScrolled { delta } => {
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                if y == 0.0 {
                    return None;
                }
                Event::Scroll {
                    pane: id,
                    up: y > 0.0,
                    column: rel_column,
                    row: rel_row,
                }
            }
            _ => return None,
        };
        Some(canvas::Action::publish(crate::Message::Grid(event)).and_capture())
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), palette::DEFAULT_BG);
        let data = &self.surface.frame;
        let width = data.width.max(1) as usize;
        for (index, cell) in data.cells.iter().enumerate() {
            if cell.skip {
                continue;
            }
            let origin = Point::new(
                (index % width) as f32 * CELL_W,
                (index / width) as f32 * CELL_H,
            );
            let (mut fg, mut bg) = (
                palette::resolve(Cell::unpack(cell.fg)),
                palette::resolve(Cell::unpack(cell.bg)),
            );
            if cell.modifier & style::REVERSED != 0 {
                (fg, bg) = (
                    Some(bg.unwrap_or(palette::DEFAULT_BG)),
                    Some(fg.unwrap_or(palette::DEFAULT_FG)),
                );
            }
            if let Some(bg) = bg {
                frame.fill_rectangle(origin, Size::new(CELL_W, CELL_H), bg);
            }
            if cell.symbol.trim().is_empty() {
                continue;
            }
            let mut color = fg.unwrap_or(palette::DEFAULT_FG);
            if cell.modifier & style::DIM != 0 {
                color.a = 0.6;
            }
            let font = Font {
                weight: if cell.modifier & style::BOLD != 0 {
                    iced::font::Weight::Bold
                } else {
                    iced::font::Weight::Normal
                },
                style: if cell.modifier & style::ITALIC != 0 {
                    iced::font::Style::Italic
                } else {
                    iced::font::Style::Normal
                },
                ..crate::theme::MONO
            };
            frame.fill_text(canvas::Text {
                content: cell.symbol.clone(),
                position: origin,
                color,
                size: FONT_SIZE.into(),
                font,
                ..canvas::Text::default()
            });
            if cell.modifier & style::UNDERLINED != 0 {
                frame.fill_rectangle(
                    Point::new(origin.x, origin.y + CELL_H - 2.0),
                    Size::new(CELL_W, 1.0),
                    color,
                );
            }
            if cell.modifier & style::CROSSED_OUT != 0 {
                frame.fill_rectangle(
                    Point::new(origin.x, origin.y + CELL_H / 2.0),
                    Size::new(CELL_W, 1.0),
                    color,
                );
            }
        }
        if let Some(cursor) = data.cursor.as_ref().filter(|c| c.visible) {
            frame.fill_rectangle(
                Point::new(cursor.x as f32 * CELL_W, cursor.y as f32 * CELL_H),
                Size::new(CELL_W, CELL_H),
                iced::Color {
                    a: 0.45,
                    ..palette::DEFAULT_FG
                },
            );
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_app_core::wire::{FrameData, SurfaceRect};

    fn pane(id: &str, x: u16, w: u16) -> PaneSurfacePane {
        PaneSurfacePane {
            pane_id: id.into(),
            content_revision: 0,
            rect: SurfaceRect {
                x,
                y: 0,
                width: w,
                height: 10,
            },
            inner_rect: SurfaceRect {
                x: x + 1,
                y: 1,
                width: w - 2,
                height: 8,
            },
            scrollbar_rect: None,
            scroll: None,
            focused: false,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    #[test]
    fn a_click_finds_its_pane_and_the_cell_inside_it() {
        let surface = PaneSurface {
            boot_id: "b".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells: vec![],
                width: 40,
                height: 10,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![pane("left", 0, 20), pane("right", 20, 20)],
            splits: vec![],
        };
        let (p, c, r) = pane_at(&surface, 25, 4).unwrap();
        assert_eq!((p.pane_id.as_str(), c, r), ("right", 4, 3));
        assert_eq!(
            pane_at(&surface, 3, 0).map(|(p, c, r)| (p.pane_id.as_str(), c, r)),
            Some(("left", 2, 0)),
            "the border row clamps to the content"
        );
        assert!(pane_at(&surface, 45, 4).is_none());
    }
}
