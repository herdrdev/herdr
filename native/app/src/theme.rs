//! Colors and small style helpers shared by the views.
//!
//! Dark theme of the "prismux · Shepherd" mockup: near-black surfaces,
//! cyan accent, hairline borders and small rounded corners (4-6 px).

use iced::widget::button;
use iced::{font, Background, Border, Color, Font, Padding, Theme};
use shepherd_app_core::snapshot::Status;

// Palette of the "prismux · Shepherd" design (CSS custom properties of the mockup).
/// --bg
pub const BG: Color = Color::from_rgb8(0x07, 0x07, 0x0A);
/// --panel
pub const PANEL: Color = Color::from_rgb8(0x0B, 0x0B, 0x10);
/// Top bar, rail and status bar (#09090D).
pub const BAR: Color = Color::from_rgb8(0x09, 0x09, 0x0D);
/// --panel2: cards, hover.
pub const SURFACE: Color = Color::from_rgb8(0x10, 0x10, 0x17);
/// --panel3: selected items.
pub const SELECTED: Color = Color::from_rgb8(0x15, 0x15, 0x1C);
/// --line: hairline separators.
pub const LINE: Color = Color::from_rgb8(0x1E, 0x1E, 0x27);
/// --line2: borders of controls.
pub const LINE2: Color = Color::from_rgb8(0x2C, 0x2C, 0x38);
/// --text
pub const FG: Color = Color::from_rgb8(0xE7, 0xE7, 0xEE);
/// --hi: titles and emphasis.
pub const HI: Color = Color::from_rgb8(0xF4, 0xF4, 0xF8);
/// --muted
pub const DIM: Color = Color::from_rgb8(0x9A, 0x9A, 0xA8);
/// --cyan: accent / working.
pub const ACCENT: Color = Color::from_rgb8(0x19, 0xD3, 0xFF);
/// --red
pub const BAD: Color = Color::from_rgb8(0xFF, 0x6B, 0x8A);
/// --green
pub const OK: Color = Color::from_rgb8(0x7C, 0xF2, 0x9A);
/// --orange: blocked / needs you.
pub const WARN: Color = Color::from_rgb8(0xFF, 0x8A, 0x1F);
/// --violet
#[allow(dead_code)] // used by the views of phase 3
pub const VIOLET: Color = Color::from_rgb8(0xB9, 0x8C, 0xFF);
/// --idle
pub const IDLE: Color = Color::from_rgb8(0xB4, 0xB4, 0xC0);
/// Dark text over bright fills (#07070A).
const ON_BRIGHT: Color = BG;

/// Body font (`font-family: var(--mono)`); the default font of the app.
pub const MONO: Font = Font::with_name("JetBrains Mono");
#[allow(dead_code)] // used by the views of phase 3
pub const MONO_BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..MONO
};
/// Pixel font for brand and section labels (`--pixel`).
pub const PIXEL: Font = Font::with_name("Silkscreen");
pub const PIXEL_BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..PIXEL
};

/// Fonts bundled with the app (SIL OFL, licenses next to the files).
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
    include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf"),
    include_bytes!("../assets/fonts/Silkscreen-Regular.ttf"),
    include_bytes!("../assets/fonts/Silkscreen-Bold.ttf"),
];

pub fn card_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(SURFACE)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

pub fn surface_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(BAR)),
        border: Border {
            color: LINE,
            width: 0.5,
            radius: 0.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// Padding with a value per side (iced only converts 1 or 2 values).
pub fn pad(top: f32, right: f32, bottom: f32, left: f32) -> Padding {
    Padding {
        top,
        right,
        bottom,
        left,
    }
}

pub fn status_glyph(status: Status) -> &'static str {
    match status {
        Status::Working => "▶",
        Status::Blocked => "!",
        Status::Done => "✓",
        Status::Idle => "–",
        Status::Unknown => "·",
    }
}

pub fn status_name(status: Status) -> &'static str {
    match status {
        Status::Working => "working",
        Status::Blocked => "blocked",
        Status::Done => "finished",
        Status::Idle => "idle",
        Status::Unknown => "Other",
    }
}

pub fn status_color(status: Status) -> Color {
    match status {
        Status::Working => ACCENT,
        Status::Blocked => WARN,
        Status::Done => OK,
        Status::Idle | Status::Unknown => IDLE,
    }
}

fn hovered(status: button::Status) -> bool {
    matches!(status, button::Status::Hovered | button::Status::Pressed)
}

fn rounded(radius: f32) -> Border {
    Border {
        radius: radius.into(),
        ..Border::default()
    }
}

/// Sidebar items, tabs and mode buttons (`.tab`, `.ws`, `.ag`): selected lifts to
/// `--panel3` with a `--line2` ring; hover lifts to `--panel2`.
pub fn item_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (background, border) = if selected {
            (
                Some(Background::Color(SELECTED)),
                Border {
                    color: LINE2,
                    width: 1.0,
                    radius: 4.0.into(),
                },
            )
        } else if hovered(status) {
            (Some(Background::Color(SURFACE)), rounded(4.0))
        } else {
            (None, rounded(4.0))
        };
        button::Style {
            background,
            text_color: if selected { HI } else { FG },
            border,
            ..button::Style::default()
        }
    }
}

/// Close buttons (✕): minimal, red tint on hover.
#[allow(dead_code)]
pub fn close_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = hovered(status);
        button::Style {
            background: hovered.then_some(Background::Color(Color { a: 0.12, ..BAD })),
            text_color: if hovered { BAD } else { DIM },
            border: rounded(4.0),
            ..button::Style::default()
        }
    }
}

/// `.btn.pri`: bright fill, dark bold text.
pub fn primary_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if hovered(status) { FG } else { HI };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ON_BRIGHT,
            border: rounded(4.0),
            ..button::Style::default()
        }
    }
}

/// `.btn.warn`: orange fill, dark text.
#[allow(dead_code)] // used by the views of phase 3
pub fn warn_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if hovered(status) {
            Color::from_rgb8(0xFF, 0xA0, 0x4A)
        } else {
            WARN
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ON_BRIGHT,
            border: rounded(4.0),
            ..button::Style::default()
        }
    }
}

/// Destructive actions ("Close agent", "Delete"): red fill, dark text.
pub fn danger_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let background = if hovered(status) {
            Color::from_rgb8(0xFF, 0x8C, 0xA4)
        } else {
            BAD
        };
        button::Style {
            background: Some(Background::Color(background)),
            text_color: ON_BRIGHT,
            border: rounded(4.0),
            ..button::Style::default()
        }
    }
}

/// `.btn` (outlined) and `.btn.ghost` (no border, `ghost = true`).
#[allow(dead_code)] // used by the views of phase 3
pub fn outline_button_style(ghost: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = hovered(status);
        button::Style {
            background: (ghost && hovered).then_some(Background::Color(SURFACE)),
            text_color: if ghost && !hovered { DIM } else { FG },
            border: Border {
                color: if ghost {
                    Color::TRANSPARENT
                } else if hovered {
                    DIM
                } else {
                    LINE2
                },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// `.btn.link`: cyan text, no border.
#[allow(dead_code)] // used by the views of phase 3
pub fn link_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| button::Style {
        background: hovered(status).then_some(Background::Color(SURFACE)),
        text_color: ACCENT,
        border: rounded(4.0),
        ..button::Style::default()
    }
}

/// Small "mini" buttons: no background, subtle hover.
pub fn mini_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = hovered(status);
        button::Style {
            background: hovered.then_some(Background::Color(SURFACE)),
            text_color: if hovered { FG } else { DIM },
            border: rounded(4.0),
            ..button::Style::default()
        }
    }
}

/// `.badge`: small solid pill with dark bold text.
#[allow(dead_code)] // used by the views of phase 3
pub fn badge_style(color: Color) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |_| iced::widget::container::Style {
        background: Some(Background::Color(color)),
        text_color: Some(ON_BRIGHT),
        border: rounded(3.0),
        ..iced::widget::container::Style::default()
    }
}

/// `.kbd`: key cap with a thicker bottom edge is approximated by a 1 px outline.
#[allow(dead_code)] // used by the layout of phase 2
pub fn kbd_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        text_color: Some(FG),
        border: Border {
            color: LINE2,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// `.alert`: tinted box with a 50% border (`color` = orange for warnings, red for errors).
#[allow(dead_code)] // used by the views of phase 3
pub fn alert_style(color: Color) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |_| iced::widget::container::Style {
        background: Some(Background::Color(Color { a: 0.06, ..color })),
        border: Border {
            color: Color { a: 0.5, ..color },
            width: 1.0,
            radius: 6.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// Section / KPI label (`.lbl`, `.k`): pixel font, small, muted.
pub fn label<'a>(content: impl iced::widget::text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    iced::widget::text(content)
        .font(PIXEL)
        .size(10.5)
        .color(DIM)
}

/// `.kpis`: cells separated by 1 px lines (the strip background shows through the row spacing).
pub fn kpi_strip_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(LINE)),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// `.kpi`: one cell of the strip.
pub fn kpi_cell_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(PANEL)),
        ..iced::widget::container::Style::default()
    }
}

/// `.tab .n`: count badge; orange when the section needs attention.
pub fn count_style(selected: bool, warn: bool) -> iced::widget::container::Style {
    let (background, text_color) = if warn {
        (Color { a: 0.16, ..WARN }, WARN)
    } else if selected {
        (LINE2, FG)
    } else {
        (LINE, DIM)
    };
    iced::widget::container::Style {
        background: Some(Background::Color(background)),
        text_color: Some(text_color),
        border: rounded(3.0),
        ..iced::widget::container::Style::default()
    }
}

/// A bar with a 1 px line along its bottom edge (`border-bottom: 1px solid var(--line)`).
pub fn bottom_line_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(PANEL)),
        border: Border {
            color: LINE,
            width: 0.5,
            radius: 0.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// `.tbar` / `.sbar`: darker bar with a hairline border.
pub fn bar_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(BAR)),
        border: Border {
            color: LINE,
            width: 0.5,
            radius: 0.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// `.inp`: dark field with a `--line2` border; cyan border while focused.
pub fn input_style(
    _: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let focused = matches!(status, iced::widget::text_input::Status::Focused { .. });
    iced::widget::text_input::Style {
        background: Background::Color(BG),
        border: Border {
            color: if focused { ACCENT } else { LINE2 },
            width: 1.0,
            radius: 4.0.into(),
        },
        icon: DIM,
        placeholder: DIM,
        value: FG,
        selection: Color { a: 0.3, ..ACCENT },
    }
}

/// `.graph`: the pattern canvas, darker than the panels, with a hairline border.
pub fn graph_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(Color::from_rgb8(0x08, 0x08, 0x0C))),
        border: Border {
            color: LINE,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// A read-only value shown like an input (`.inp`).
pub fn field_style(_: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Background::Color(BG)),
        border: Border {
            color: LINE2,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// A stable colour for a name (the emitter of a message): the same agent always gets the same one.
pub fn emitter_color(name: &str) -> Color {
    const PALETTE: [Color; 6] = [
        ACCENT,
        OK,
        VIOLET,
        WARN,
        BAD,
        Color::from_rgb(1.0, 0.831, 0.0),
    ];
    // FNV-1a: cheap and the same on every run
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    PALETTE[(hash % PALETTE.len() as u64) as usize]
}

/// The colour of a message kind: needs-you and failures stand out, acknowledgements are calm.
pub fn kind_color(kind: &str, has_topic: bool) -> Color {
    match kind {
        "blocked" | "alert" | "warn" => WARN,
        "error" | "failed" | "fail" => BAD,
        "ack" | "done" | "result" => OK,
        _ if has_topic => ACCENT,
        _ => DIM,
    }
}

/// A multi-line text box (`.inp`): the same dark field as a single-line input, cyan border while focused.
pub fn editor_style(
    _: &Theme,
    status: iced::widget::text_editor::Status,
) -> iced::widget::text_editor::Style {
    let focused = matches!(status, iced::widget::text_editor::Status::Focused { .. });
    iced::widget::text_editor::Style {
        background: Background::Color(BG),
        border: Border {
            color: if focused { ACCENT } else { LINE2 },
            width: 1.0,
            radius: 4.0.into(),
        },
        placeholder: DIM,
        value: FG,
        selection: Color { a: 0.3, ..ACCENT },
    }
}

/// The iced theme of the app: Shepherd's palette, so that the widgets nobody styled by hand (pick lists, check
/// boxes, scrollbars, text selection) wear the same colours as the rest.
pub fn app_theme() -> Theme {
    static THEME: std::sync::OnceLock<Theme> = std::sync::OnceLock::new();
    THEME
        .get_or_init(|| {
            Theme::custom(
                "Prismux",
                iced::theme::Palette {
                    background: BG,
                    text: FG,
                    primary: ACCENT,
                    success: OK,
                    warning: WARN,
                    danger: BAD,
                },
            )
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emitter_keeps_its_colour_and_different_ones_spread() {
        assert_eq!(emitter_color("brain-20"), emitter_color("brain-20"));
        let names = [
            "brain-20", "coder-37", "coder-38", "coder-39", "reviewer", "shepherd",
        ];
        let distinct = names
            .iter()
            .map(|n| format!("{:?}", emitter_color(n)))
            .collect::<std::collections::HashSet<_>>();
        assert!(distinct.len() >= 3, "names do not all land on one colour");
    }

    #[test]
    fn kinds_are_coloured_by_what_they_mean() {
        assert_eq!(kind_color("blocked", true), WARN);
        assert_eq!(kind_color("error", false), BAD);
        assert_eq!(kind_color("ack", true), OK);
        assert_eq!(kind_color("msg", true), ACCENT);
        assert_eq!(kind_color("msg", false), DIM);
    }
}
