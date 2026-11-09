//! Cell colors → screen colors. The 16 named colors follow a dark theme; 16..=255 are the xterm palette.

use iced::Color;
use shepherd_app_core::wire::Color as Cell;

pub const DEFAULT_FG: Color = Color::from_rgb(0.83, 0.85, 0.89);
pub const DEFAULT_BG: Color = Color::from_rgb(0.08, 0.09, 0.11);

const ANSI: [(u8, u8, u8); 16] = [
    (0x1d, 0x1f, 0x21),
    (0xcc, 0x66, 0x66),
    (0xb5, 0xbd, 0x68),
    (0xf0, 0xc6, 0x74),
    (0x81, 0xa2, 0xbe),
    (0xb2, 0x94, 0xbb),
    (0x8a, 0xbe, 0xb7),
    (0xc5, 0xc8, 0xc6),
    (0x96, 0x98, 0x96),
    (0xde, 0x93, 0x5f),
    (0xa1, 0xc9, 0x7b),
    (0xf9, 0xe2, 0xaf),
    (0x9c, 0xc4, 0xe4),
    (0xd6, 0xb4, 0xe6),
    (0xa8, 0xd8, 0xd3),
    (0xff, 0xff, 0xff),
];

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb8(r, g, b)
}

fn indexed(i: u8) -> Color {
    match i {
        0..=15 => rgb(ANSI[i as usize]),
        16..=231 => {
            let n = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            rgb((level(n / 36), level((n / 6) % 6), level(n % 6)))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            rgb((v, v, v))
        }
    }
}

/// `None` means "the default": the caller picks the theme foreground or background.
pub fn resolve(color: Cell) -> Option<Color> {
    match color {
        Cell::Reset => None,
        // The server numbers the named colors 1..=16 starting at black; the ANSI table starts at 0.
        Cell::Named(n) => Some(rgb(ANSI[(n as usize - 1).min(15)])),
        Cell::Indexed(i) => Some(indexed(i)),
        Cell::Rgb(r, g, b) => Some(Color::from_rgb8(r, g, b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_colors_are_one_based_and_the_cube_and_grays_resolve() {
        assert_eq!(resolve(Cell::Named(1)), Some(rgb(ANSI[0])));
        assert_eq!(resolve(Cell::Named(16)), Some(rgb(ANSI[15])));
        assert_eq!(resolve(Cell::Reset), None);
        assert_eq!(indexed(16), rgb((0, 0, 0)));
        assert_eq!(indexed(231), rgb((255, 255, 255)));
        assert_eq!(indexed(232), rgb((8, 8, 8)));
    }
}
