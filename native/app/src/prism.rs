//! The "prism": streams of coloured pixels that wave across a canvas (port of `prism()` in the mockup).
//!
//! `cells` is pure (same input, same pixels) so it can be tested; `Prism` draws those cells on a canvas.
//! The caller advances `frame` (12 frames make a loop) to animate.

use crate::Message;
use iced::widget::canvas::{self, Frame};
use iced::{mouse, Color, Point, Rectangle, Size, Theme};

const RAMP: [[f64; 3]; 7] = [
    [255.0, 45.0, 85.0],
    [255.0, 122.0, 26.0],
    [255.0, 212.0, 0.0],
    [54.0, 240.0, 122.0],
    [25.0, 211.0, 255.0],
    [77.0, 107.0, 255.0],
    [177.0, 77.0, 255.0],
];
/// 4x4 Bayer matrix (as sixteenths) used to dither the brightness.
const BAYER: [f64; 16] = [
    0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0,
];
/// Frames in one animation loop.
pub const LOOP: u32 = 12;

#[derive(Debug, Clone, Copy)]
pub struct Preset {
    /// Size of one pixel in screen pixels.
    pub px: f64,
    pub streams: u32,
    /// How far across the canvas the streams reach (0..1.05).
    pub reach: f64,
    pub thick: f64,
    pub amp: f64,
    pub wave: f64,
    pub seed: f64,
    pub int: f64,
    /// Fixed hue (0..1) instead of one that follows the stream.
    pub mono: Option<f64>,
    pub bg: Color,
    /// Background dots: (alpha, gap, size).
    pub dots: Option<(f32, u32, u32)>,
    /// Amount of white grain.
    pub grain: f64,
}

const BASE: Preset = Preset {
    px: 2.0,
    streams: 1,
    reach: 0.9,
    thick: 0.35,
    amp: 0.12,
    wave: 6.0,
    seed: 0.0,
    int: 0.85,
    mono: None,
    bg: Color::from_rgb(0.027, 0.027, 0.039),
    dots: None,
    grain: 0.0,
};

impl Preset {
    /// The strip along the top of the window.
    pub const STRIP: Preset = Preset {
        reach: 1.02,
        thick: 0.8,
        amp: 0.04,
        wave: 8.0,
        seed: 1.0,
        ..BASE
    };
    /// The small logo next to the title.
    pub const fn logo(bg: Color) -> Preset {
        Preset {
            streams: 2,
            reach: 0.8,
            thick: 0.2,
            wave: 4.0,
            seed: 5.0,
            bg,
            ..BASE
        }
    }
    /// The sparkline in a KPI cell.
    pub const SPARK: Preset = Preset {
        seed: 2.0,
        bg: Color::from_rgb(0.043, 0.043, 0.063),
        dots: Some((0.4, 4, 1)),
        ..BASE
    };
    /// The 3 px strip across the top of an agent node in the pattern graph.
    pub const NODE: Preset = Preset {
        px: 1.0,
        reach: 1.05,
        thick: 1.0,
        amp: 0.0,
        wave: 1.0,
        seed: 6.0,
        ..BASE
    };
    /// A partition's fill in the Events tab: a stream that is as long as the partition is full.
    pub const fn lag(reach: f64, seed: f64) -> Preset {
        Preset {
            reach,
            thick: 0.7,
            amp: 0.03,
            seed,
            bg: Color::from_rgb(0.031, 0.031, 0.047),
            ..BASE
        }
    }
    /// The underline of the selected tab.
    pub const TABBAR: Preset = Preset {
        px: 1.0,
        reach: 1.05,
        thick: 1.0,
        amp: 0.0,
        wave: 1.0,
        seed: 2.0,
        ..BASE
    };
    /// The "herd pulse": short and dim when something is blocked, long and bright otherwise.
    pub const fn pulse(blocked: bool) -> Preset {
        Preset {
            px: 4.0,
            streams: 3,
            reach: if blocked { 0.34 } else { 0.82 },
            thick: 0.12,
            amp: if blocked { 0.03 } else { 0.08 },
            wave: 5.0,
            seed: 9.0,
            int: if blocked { 0.6 } else { 0.9 },
            bg: Color::from_rgb(0.031, 0.031, 0.047),
            dots: Some((0.5, 12, 1)),
            grain: 0.4,
            ..BASE
        }
    }
}

fn hash(a: f64, b: f64, c: f64) -> f64 {
    let n = (a * 127.1 + b * 311.7 + c * 74.7).sin() * 43758.5453;
    n - n.floor()
}

/// The coloured pixels of one frame: `(column, row, rgb)` in units of `px`.
pub fn cells(size: Size, preset: &Preset, frame: u32) -> Vec<(u32, u32, [u8; 3])> {
    let mut out = Vec::new();
    let px = preset.px;
    let cols = (f64::from(size.width) / px).ceil();
    let rows = (f64::from(size.height) / px).ceil();
    if cols < 1.0 || rows < 1.0 {
        return out;
    }
    let fr = f64::from(frame % LOOP);
    let t = fr / f64::from(LOOP) * std::f64::consts::TAU;
    let reach = (cols * preset.reach).max(2.0);
    let mut paint = |x: f64, y: f64, hue: f64, m: f64| {
        let h = hue.clamp(0.0, 0.999) * 6.0;
        let i = h.floor() as usize;
        let k = h - h.floor();
        let (a, b) = (RAMP[i], RAMP[(i + 1).min(6)]);
        let channel = |n: usize| ((a[n] + (b[n] - a[n]) * k) * m) as u8;
        out.push((x as u32, y as u32, [channel(0), channel(1), channel(2)]));
    };
    for s in 0..preset.streams {
        let s_f = f64::from(s);
        let sd = s_f * 19.7 + preset.seed;
        let cy = rows
            * if preset.streams == 1 {
                0.5
            } else {
                0.22 + 0.56 * s_f / f64::from(preset.streams - 1)
            };
        let thickness = (rows * preset.thick * (0.75 + 0.5 * hash(s_f, 7.0, sd))).max(0.8);
        for x in 0..(reach as u32).min(cols as u32) {
            let xf = f64::from(x);
            let u = xf / reach;
            let tp = (1.0 - u).powf(0.55);
            let yc = cy
                + (u * preset.wave + t + sd).sin() * rows * preset.amp * (0.3 + u)
                + (u * preset.wave * 2.7 - t * 2.0 + sd * 1.3).sin() * rows * preset.amp * 0.25;
            let th = (thickness * tp * (0.85 + 0.15 * (t * 2.0 + u * 11.0 + sd).sin())).max(0.55);
            let (from, to) = ((yc - th).floor() as i64, (yc + th).ceil() as i64);
            for y in from..=to {
                if y < 0 || y as f64 >= rows {
                    continue;
                }
                let yf = y as f64;
                let d = (yf + 0.5 - yc) / th;
                if !(-1.0..=1.0).contains(&d)
                    || hash(xf, yf + s_f * 101.0, fr + sd) > 0.3 + 0.8 * tp
                {
                    continue;
                }
                let v = (1.0 - d * d * 0.6) * (0.45 + 0.55 * tp) * preset.int;
                let dither = BAYER[((x & 3) + ((y as u32 & 3) << 2)) as usize] / 16.0;
                let level = (v * 2.0 + dither).floor() / 2.0;
                if level <= 0.0 {
                    continue;
                }
                paint(
                    xf,
                    yf,
                    preset.mono.unwrap_or(d * 0.5 + 0.5) + u * 0.12,
                    level.min(1.0),
                );
            }
        }
        // sparks flying off the end of the stream
        for q in 0..(rows * 0.5 + 4.0).floor() as u32 {
            let qf = f64::from(q);
            let fx = (reach + hash(qf, s_f, fr + 3.0) * cols * 0.12).floor();
            let fy =
                (cy + (hash(qf, s_f + 9.0, fr + 5.0) - 0.5) * rows * preset.thick * 2.3).floor();
            if fx >= cols || fy < 0.0 || fy >= rows || hash(qf, s_f, 77.0) > 0.45 {
                continue;
            }
            paint(fx, fy, hash(qf, s_f, 31.0), 0.5);
        }
    }
    out
}

/// A canvas that draws one preset; `frame` is the animation step.
pub struct Prism {
    pub preset: Preset,
    pub frame: u32,
}

impl canvas::Program<Message> for Prism {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &iced::Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let p = &self.preset;
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), p.bg);
        if let Some((alpha, gap, size)) = p.dots {
            let dot = Color::from_rgba(160.0 / 255.0, 160.0 / 255.0, 190.0 / 255.0, alpha * 0.5);
            let mut y = gap / 2;
            while (y as f32) < bounds.height {
                let mut x = gap / 2;
                while (x as f32) < bounds.width {
                    frame.fill_rectangle(
                        Point::new(x as f32, y as f32),
                        Size::new(size as f32, size as f32),
                        dot,
                    );
                    x += gap;
                }
                y += gap;
            }
        }
        let gap = if p.px >= 4.0 { 1.0 } else { 0.0 };
        let cell = Size::new((p.px - gap) as f32, (p.px - gap) as f32);
        for (x, y, [r, g, b]) in cells(bounds.size(), p, self.frame) {
            frame.fill_rectangle(
                Point::new((f64::from(x) * p.px) as f32, (f64::from(y) * p.px) as f32),
                cell,
                Color::from_rgb8(r, g, b),
            );
        }
        if p.grain > 0.0 {
            let n = (f64::from(bounds.width) * f64::from(bounds.height) * 0.0008 * p.grain) as u32;
            let fr = f64::from(self.frame % LOOP);
            for i in 0..n {
                let i = f64::from(i);
                let alpha = (0.06 + 0.22 * hash(i, fr, 3.0)) as f32;
                frame.fill_rectangle(
                    Point::new(
                        (hash(i, fr, 1.0) * f64::from(bounds.width)).floor() as f32,
                        (hash(i, fr, 2.0) * f64::from(bounds.height)).floor() as f32,
                    ),
                    Size::new(1.0, 1.0),
                    Color::from_rgba(1.0, 1.0, 1.0, alpha),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size::new(400.0, 40.0);

    #[test]
    fn a_frame_is_deterministic_and_stays_inside_the_canvas() {
        let a = cells(SIZE, &Preset::STRIP, 3);
        assert!(!a.is_empty(), "the strip paints something");
        assert_eq!(a, cells(SIZE, &Preset::STRIP, 3));
        let (cols, rows) = (
            (SIZE.width / 2.0).ceil() as u32,
            (SIZE.height / 2.0).ceil() as u32,
        );
        assert!(a.iter().all(|(x, y, _)| *x < cols && *y < rows));
    }

    #[test]
    fn the_animation_moves_and_loops() {
        let at = |f| cells(SIZE, &Preset::STRIP, f);
        assert_ne!(at(0), at(1), "frames differ");
        assert_eq!(at(0), at(LOOP), "the loop closes after {LOOP} frames");
    }

    #[test]
    fn a_blocked_herd_pulses_shorter_than_a_healthy_one() {
        let size = Size::new(600.0, 120.0);
        let far = |blocked| {
            cells(size, &Preset::pulse(blocked), 0)
                .iter()
                .map(|(x, ..)| *x)
                .max()
                .unwrap_or(0)
        };
        assert!(far(true) < far(false));
    }

    #[test]
    fn a_node_strip_fills_three_rows_and_moves_between_frames() {
        let strip = Size::new(148.0, 3.0);
        let at = |f| cells(strip, &Preset::NODE, f);
        assert!(at(0).len() > 100, "the strip is mostly covered");
        assert!(at(0).iter().all(|(x, y, _)| *x < 148 && *y < 3));
        assert_ne!(at(0), at(3), "nodes are animated");
    }

    #[test]
    fn a_fuller_partition_paints_a_longer_stream() {
        let size = Size::new(400.0, 14.0);
        let far = |reach| {
            cells(size, &Preset::lag(reach, 0.0), 0)
                .iter()
                .map(|(x, ..)| *x)
                .max()
                .unwrap_or(0)
        };
        assert!(far(0.2) < far(0.9));
    }

    /// Values taken from running the mockup's own `prism()` (prismux · Shepherd.html) under Node for the same
    /// canvas size, preset and frame, so the port cannot drift from the design.
    #[test]
    fn the_port_matches_the_mockups_own_pixels() {
        let strip = cells(Size::new(1280.0, 6.0), &Preset::STRIP, 3);
        assert_eq!(
            strip[..8],
            [
                (0, 0, [127, 68, 10]),
                (0, 1, [53, 239, 124]),
                (0, 2, [35, 60, 127]),
                (1, 0, [127, 68, 10]),
                (1, 1, [26, 119, 62]),
                (1, 2, [35, 60, 127]),
                (2, 0, [127, 68, 10]),
                (2, 1, [53, 239, 124]),
            ]
        );
        let total = |c: &[(u32, u32, [u8; 3])]| -> (usize, u32) {
            (
                c.len(),
                c.iter()
                    .map(|(.., [r, g, b])| u32::from(*r) + u32::from(*g) + u32::from(*b))
                    .sum(),
            )
        };
        let spark = cells(Size::new(120.0, 26.0), &Preset::SPARK, 5);
        assert_eq!(total(&spark), (189, 53142));
        let pulse = Preset {
            grain: 0.0,
            ..Preset::pulse(false)
        };
        assert_eq!(
            total(&cells(Size::new(1000.0, 120.0), &pulse, 4)),
            (2220, 642883)
        );
    }
}
