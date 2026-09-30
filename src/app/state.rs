use crate::config::{
    Keybinds, NewTerminalCwdConfig, SoundConfig, TabBarPositionConfig, ToastConfig, ToastDelivery,
};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Direction, Rect};
use ratatui::style::Color;
use std::time::{Duration, Instant};

use crate::detect::AgentState;
use crate::layout::{PaneId, PaneInfo, SplitBorder};
use crate::selection::Selection;

pub(crate) type InstalledPluginRegistry =
    std::collections::HashMap<String, crate::api::schema::InstalledPluginInfo>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPaneRecord {
    pub plugin_id: String,
    pub entrypoint: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PopupPaneState {
    pub pane_id: PaneId,
    pub terminal_id: crate::terminal::TerminalId,
    pub width: Option<crate::popup_size::PopupSize>,
    pub height: Option<crate::popup_size::PopupSize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockEditorSession {
    pub pane_id: PaneId,
    pub terminal_id: crate::terminal::TerminalId,
}

/// Read-only file content shown in the main pane area by the Editor renderer.
/// It stays attach-local and never enters the server protocol or pane runtime.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DockEditorPreview {
    pub(crate) path: std::path::PathBuf,
    pub(crate) content: String,
    pub(crate) notice: Option<String>,
}

// ---------------------------------------------------------------------------
// Selection autoscroll types
// ---------------------------------------------------------------------------

/// Direction of automatic scrolling during text selection drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionAutoscrollDirection {
    Up,
    Down,
}

/// State for automatic scrolling during text selection drag.
///
/// When the cursor hovers in the 1-row hot zone at the top or bottom edge
/// of a pane (or outside the pane), this struct captures the direction and
/// last known mouse position so a recurring 30ms tick can continue scrolling
/// and extending the selection even when the mouse is not moving.
#[derive(Clone, Debug)]
pub(crate) struct SelectionAutoscroll {
    pub direction: SelectionAutoscrollDirection,
    pub last_mouse_screen_col: u16,
    pub last_mouse_screen_row: u16,
    pub inner_rect: Rect,
}

#[derive(Clone)]
pub(crate) struct RightClickPassthroughGesture {
    pub pane_info: PaneInfo,
    pub modifiers: KeyModifiers,
}
use crate::terminal_theme::{HostAppearance, TerminalTheme};
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Theme palette — all UI colors in one place, ready for theming
// ---------------------------------------------------------------------------

/// Group-header status colours (F12-3): the glyph tint for a Linear ticket
/// state, a pull request state, or a Missive conversation state.
///
/// Every entry is an override. `None` follows the theme tone named on the
/// accessor, so a theme that retints `yellow` retints "in progress" with it
/// and only a user who wants a different status vocabulary has to say so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkStatusColors {
    /// Backlog, todo, draft, closed, cancelled, unassigned.
    pub neutral: Option<Color>,
    /// In progress.
    pub active: Option<Color>,
    /// In review.
    pub review: Option<Color>,
    /// Done, closed conversation.
    pub done: Option<Color>,
    /// Triage.
    pub triage: Option<Color>,
    /// Merged pull request.
    pub merged: Option<Color>,
    /// Open pull request, open conversation.
    pub open: Option<Color>,
}

impl WorkStatusColors {
    /// Every entry unset: the built-in themes tint status glyphs from their
    /// own tokens rather than carrying a second copy of them.
    pub const DEFAULT: Self = Self {
        neutral: None,
        active: None,
        review: None,
        done: None,
        triage: None,
        merged: None,
        open: None,
    };
}

/// All colors used by the UI. Derived from a base accent color for now,
/// but structured so a full theme system can replace it later.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // all fields defined for theming — some used later
pub struct Palette {
    /// Primary accent (highlight, active borders).
    pub accent: Color,
    /// Background for the tab bar, floating panels, overlays, and modals.
    pub panel_bg: Color,
    /// Desktop sidebar background. `None` follows `panel_bg`; an explicit
    /// `Some(Color::Reset)` hands the sidebar back to the terminal.
    pub sidebar_bg: Option<Color>,
    /// Background for the active workspace and focused agent rows.
    pub active_row_bg: Color,
    /// Background for the Navigate-mode cursor row in the sidebar.
    pub selection_bg: Color,
    /// Subtle surface background for selected/focused items.
    pub surface0: Color,
    /// Slightly lighter surface for hover/active states.
    pub surface1: Color,
    /// Very dim surface for separators.
    pub surface_dim: Color,
    /// Muted text (secondary info, numbers).
    pub overlay0: Color,
    /// Slightly brighter overlay text.
    pub overlay1: Color,
    /// Main text color — soft white.
    pub text: Color,
    /// Subdued text (workspace numbers, dim labels).
    pub subtext0: Color,
    /// Branch name / special label color.
    pub mauve: Color,
    /// Done / idle states.
    pub green: Color,
    /// Working / running states.
    pub yellow: Color,
    /// Needs attention / blocked states.
    pub red: Color,
    /// Unseen / done notification accent.
    pub blue: Color,
    /// Notification accent / unseen markers.
    pub teal: Color,
    /// Interrupted / warning states.
    pub peach: Color,
    /// Group-header status glyph colours. Unset entries follow the theme.
    pub work_status: WorkStatusColors,
}

/// Resolve a ratatui color to concrete channels for legibility decisions.
///
/// `Rgb` is exact. Named and indexed colors resolve through the xterm default
/// palette: the real terminal may have retinted them, but herdr has to choose
/// a readable foreground with the information it has, and the xterm defaults
/// are a far better estimate than treating the color as unknowable. `Reset`
/// stays `None` — it means "whatever the terminal uses", which is genuinely
/// not knowable here.
pub fn color_rgb(color: Color) -> Option<crate::terminal_theme::RgbColor> {
    use crate::terminal_theme::RgbColor;
    let rgb = |r: u8, g: u8, b: u8| Some(RgbColor { r, g, b });
    match color {
        Color::Rgb(r, g, b) => rgb(r, g, b),
        Color::Reset => None,
        Color::Black => rgb(0, 0, 0),
        Color::Red => rgb(128, 0, 0),
        Color::Green => rgb(0, 128, 0),
        Color::Yellow => rgb(128, 128, 0),
        Color::Blue => rgb(0, 0, 128),
        Color::Magenta => rgb(128, 0, 128),
        Color::Cyan => rgb(0, 128, 128),
        Color::Gray => rgb(192, 192, 192),
        Color::DarkGray => rgb(128, 128, 128),
        Color::LightRed => rgb(255, 0, 0),
        Color::LightGreen => rgb(0, 255, 0),
        Color::LightYellow => rgb(255, 255, 0),
        Color::LightBlue => rgb(0, 0, 255),
        Color::LightMagenta => rgb(255, 0, 255),
        Color::LightCyan => rgb(0, 255, 255),
        Color::White => rgb(255, 255, 255),
        Color::Indexed(index) => match index {
            0..=15 => color_rgb(match index {
                0 => Color::Black,
                1 => Color::Red,
                2 => Color::Green,
                3 => Color::Yellow,
                4 => Color::Blue,
                5 => Color::Magenta,
                6 => Color::Cyan,
                7 => Color::Gray,
                8 => Color::DarkGray,
                9 => Color::LightRed,
                10 => Color::LightGreen,
                11 => Color::LightYellow,
                12 => Color::LightBlue,
                13 => Color::LightMagenta,
                14 => Color::LightCyan,
                _ => Color::White,
            }),
            16..=231 => {
                // 6x6x6 color cube, xterm's non-linear level steps.
                let level = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
                let index = index - 16;
                rgb(level(index / 36), level((index % 36) / 6), level(index % 6))
            }
            _ => {
                let value = 8 + (index - 232) * 10;
                rgb(value, value, value)
            }
        },
    }
}

impl Palette {
    /// Catppuccin Mocha — the default.
    pub fn catppuccin() -> Self {
        Self {
            accent: Color::Rgb(137, 180, 250), // blue
            panel_bg: Color::Rgb(24, 24, 37),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(30, 30, 46),
            selection_bg: Color::Rgb(49, 50, 68),
            surface0: Color::Rgb(49, 50, 68),
            surface1: Color::Rgb(69, 71, 90),
            surface_dim: Color::Rgb(30, 30, 46),
            overlay0: Color::Rgb(108, 112, 134),
            overlay1: Color::Rgb(127, 132, 156),
            text: Color::Rgb(205, 214, 244),
            subtext0: Color::Rgb(166, 173, 200),
            mauve: Color::Rgb(203, 166, 247),
            green: Color::Rgb(166, 227, 161),
            yellow: Color::Rgb(249, 226, 175),
            red: Color::Rgb(243, 139, 168),
            blue: Color::Rgb(137, 180, 250),
            teal: Color::Rgb(148, 226, 213),
            peach: Color::Rgb(250, 179, 135),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Catppuccin Latte — the light Catppuccin flavor.
    pub fn catppuccin_latte() -> Self {
        Self {
            accent: Color::Rgb(30, 102, 245),
            panel_bg: Color::Rgb(239, 241, 245),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(230, 233, 239),
            selection_bg: Color::Rgb(189, 208, 245),
            surface0: Color::Rgb(204, 208, 218),
            surface1: Color::Rgb(188, 192, 204),
            surface_dim: Color::Rgb(230, 233, 239),
            overlay0: Color::Rgb(156, 160, 176),
            overlay1: Color::Rgb(140, 143, 161),
            text: Color::Rgb(76, 79, 105),
            subtext0: Color::Rgb(108, 111, 133),
            mauve: Color::Rgb(136, 57, 239),
            green: Color::Rgb(64, 160, 43),
            yellow: Color::Rgb(223, 142, 29),
            red: Color::Rgb(210, 15, 57),
            blue: Color::Rgb(30, 102, 245),
            teal: Color::Rgb(23, 146, 153),
            peach: Color::Rgb(254, 100, 11),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Terminal 16-color theme.
    pub fn terminal() -> Self {
        Self {
            accent: Color::Blue,
            panel_bg: Color::Reset,
            sidebar_bg: None,
            active_row_bg: Color::DarkGray,
            selection_bg: Color::Reset,
            surface0: Color::Reset,
            surface1: Color::DarkGray,
            surface_dim: Color::DarkGray,
            overlay0: Color::Gray,
            overlay1: Color::White,
            text: Color::Reset,
            subtext0: Color::Gray,
            mauve: Color::Gray,
            green: Color::Green,
            yellow: Color::Yellow,
            red: Color::LightRed,
            blue: Color::Blue,
            teal: Color::Cyan,
            peach: Color::Yellow,
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Tokyo Night — blue-purple aesthetic.
    pub fn tokyo_night() -> Self {
        Self {
            accent: Color::Rgb(122, 162, 247), // blue
            panel_bg: Color::Rgb(26, 27, 38),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(35, 38, 54),
            selection_bg: Color::Rgb(45, 54, 80),
            surface0: Color::Rgb(36, 40, 59),
            surface1: Color::Rgb(65, 72, 104),
            surface_dim: Color::Rgb(26, 27, 38),
            overlay0: Color::Rgb(86, 95, 137),
            overlay1: Color::Rgb(105, 113, 150),
            text: Color::Rgb(192, 202, 245),
            subtext0: Color::Rgb(169, 177, 214),
            mauve: Color::Rgb(187, 154, 247),
            green: Color::Rgb(158, 206, 106),
            yellow: Color::Rgb(224, 175, 104),
            red: Color::Rgb(247, 118, 142),
            blue: Color::Rgb(122, 162, 247),
            teal: Color::Rgb(125, 207, 255),
            peach: Color::Rgb(255, 158, 100),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Tokyo Night Day — the light Tokyo Night style.
    pub fn tokyo_night_day() -> Self {
        Self {
            accent: Color::Rgb(46, 125, 233),
            panel_bg: Color::Rgb(225, 226, 231),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(210, 211, 218),
            selection_bg: Color::Rgb(182, 202, 231),
            surface0: Color::Rgb(196, 200, 218),
            surface1: Color::Rgb(168, 174, 203),
            surface_dim: Color::Rgb(210, 211, 218),
            overlay0: Color::Rgb(137, 144, 179),
            overlay1: Color::Rgb(104, 112, 154),
            text: Color::Rgb(55, 96, 191),
            subtext0: Color::Rgb(97, 114, 176),
            mauve: Color::Rgb(120, 71, 189),
            green: Color::Rgb(88, 117, 57),
            yellow: Color::Rgb(140, 108, 62),
            red: Color::Rgb(245, 42, 101),
            blue: Color::Rgb(46, 125, 233),
            teal: Color::Rgb(17, 140, 116),
            peach: Color::Rgb(177, 92, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Dracula — purple/pink/green.
    pub fn dracula() -> Self {
        Self {
            accent: Color::Rgb(189, 147, 249), // purple
            panel_bg: Color::Rgb(40, 42, 54),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(55, 60, 82),
            selection_bg: Color::Rgb(70, 63, 93),
            surface0: Color::Rgb(68, 71, 90),
            surface1: Color::Rgb(98, 114, 164),
            surface_dim: Color::Rgb(40, 42, 54),
            overlay0: Color::Rgb(98, 114, 164),
            overlay1: Color::Rgb(130, 140, 180),
            text: Color::Rgb(248, 248, 242),
            subtext0: Color::Rgb(210, 210, 220),
            mauve: Color::Rgb(255, 121, 198), // pink
            green: Color::Rgb(80, 250, 123),
            yellow: Color::Rgb(241, 250, 140),
            red: Color::Rgb(255, 85, 85),
            blue: Color::Rgb(139, 233, 253), // cyan-ish
            teal: Color::Rgb(139, 233, 253),
            peach: Color::Rgb(255, 184, 108),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Nord — frosty blue palette.
    pub fn nord() -> Self {
        Self {
            accent: Color::Rgb(136, 192, 208), // frost
            panel_bg: Color::Rgb(46, 52, 64),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(67, 76, 94),
            selection_bg: Color::Rgb(64, 80, 93),
            surface0: Color::Rgb(59, 66, 82),
            surface1: Color::Rgb(67, 76, 94),
            surface_dim: Color::Rgb(46, 52, 64),
            // Nord's own nord3 (#4c566a) is a comment color: on nord0 it lands
            // at 1.69:1, dim enough to lose the sidebar's secondary text. Use
            // the brighter muted tone the Nord UI ports settle on instead.
            overlay0: Color::Rgb(123, 136, 161),
            overlay1: Color::Rgb(143, 156, 178),
            text: Color::Rgb(236, 239, 244),
            subtext0: Color::Rgb(216, 222, 233),
            mauve: Color::Rgb(180, 142, 173),
            green: Color::Rgb(163, 190, 140),
            yellow: Color::Rgb(235, 203, 139),
            red: Color::Rgb(191, 97, 106),
            blue: Color::Rgb(129, 161, 193),
            teal: Color::Rgb(143, 188, 187),
            peach: Color::Rgb(208, 135, 112),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Gruvbox Dark — warm retro palette.
    pub fn gruvbox() -> Self {
        Self {
            accent: Color::Rgb(215, 153, 33), // yellow
            panel_bg: Color::Rgb(40, 40, 40),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(50, 49, 48),
            selection_bg: Color::Rgb(75, 63, 39),
            surface0: Color::Rgb(60, 56, 54),
            surface1: Color::Rgb(80, 73, 69),
            surface_dim: Color::Rgb(40, 40, 40),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(168, 153, 132),
            text: Color::Rgb(235, 219, 178),
            subtext0: Color::Rgb(213, 196, 161),
            mauve: Color::Rgb(211, 134, 155),
            green: Color::Rgb(184, 187, 38),
            yellow: Color::Rgb(250, 189, 47),
            red: Color::Rgb(251, 73, 52),
            blue: Color::Rgb(131, 165, 152),
            teal: Color::Rgb(142, 192, 124),
            peach: Color::Rgb(254, 128, 25),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Gruvbox Light — the light retro palette.
    pub fn gruvbox_light() -> Self {
        Self {
            accent: Color::Rgb(7, 102, 120),
            panel_bg: Color::Rgb(251, 241, 199),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(242, 229, 188),
            selection_bg: Color::Rgb(235, 219, 178),
            surface0: Color::Rgb(235, 219, 178),
            surface1: Color::Rgb(213, 196, 161),
            surface_dim: Color::Rgb(242, 229, 188),
            overlay0: Color::Rgb(146, 131, 116),
            overlay1: Color::Rgb(124, 111, 100),
            text: Color::Rgb(60, 56, 54),
            subtext0: Color::Rgb(80, 73, 69),
            mauve: Color::Rgb(143, 63, 113),
            green: Color::Rgb(121, 116, 14),
            yellow: Color::Rgb(181, 118, 20),
            red: Color::Rgb(157, 0, 6),
            blue: Color::Rgb(7, 102, 120),
            teal: Color::Rgb(66, 123, 88),
            peach: Color::Rgb(175, 58, 3),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// One Dark — Atom's classic dark theme.
    pub fn one_dark() -> Self {
        Self {
            accent: Color::Rgb(97, 175, 239), // blue
            panel_bg: Color::Rgb(40, 44, 52),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(49, 54, 64),
            selection_bg: Color::Rgb(51, 70, 89),
            surface0: Color::Rgb(44, 49, 58),
            surface1: Color::Rgb(62, 68, 81),
            surface_dim: Color::Rgb(40, 44, 52),
            overlay0: Color::Rgb(92, 99, 112),
            overlay1: Color::Rgb(115, 122, 135),
            text: Color::Rgb(171, 178, 191),
            subtext0: Color::Rgb(150, 156, 168),
            mauve: Color::Rgb(198, 120, 221),
            green: Color::Rgb(152, 195, 121),
            yellow: Color::Rgb(229, 192, 123),
            red: Color::Rgb(224, 108, 117),
            blue: Color::Rgb(97, 175, 239),
            teal: Color::Rgb(86, 182, 194),
            peach: Color::Rgb(209, 154, 102),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// One Light — Atom's classic light theme.
    pub fn one_light() -> Self {
        Self {
            accent: Color::Rgb(64, 120, 242),
            panel_bg: Color::Rgb(250, 250, 250),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(216, 219, 226),
            selection_bg: Color::Rgb(205, 219, 248),
            surface0: Color::Rgb(240, 240, 241),
            surface1: Color::Rgb(229, 229, 230),
            surface_dim: Color::Rgb(245, 245, 246),
            overlay0: Color::Rgb(160, 161, 167),
            overlay1: Color::Rgb(104, 107, 119),
            text: Color::Rgb(56, 58, 66),
            subtext0: Color::Rgb(104, 107, 119),
            mauve: Color::Rgb(166, 38, 164),
            green: Color::Rgb(80, 161, 79),
            yellow: Color::Rgb(193, 132, 1),
            red: Color::Rgb(228, 86, 73),
            blue: Color::Rgb(64, 120, 242),
            teal: Color::Rgb(1, 132, 188),
            peach: Color::Rgb(152, 104, 1),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// GitHub Dark High Contrast — Primer's accessible dark palette.
    pub fn github_dark_high_contrast() -> Self {
        Self {
            accent: Color::Rgb(113, 183, 255),
            panel_bg: Color::Rgb(10, 12, 16),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(22, 27, 34),
            selection_bg: Color::Rgb(39, 43, 51),
            surface0: Color::Rgb(39, 43, 51),
            surface1: Color::Rgb(82, 89, 100),
            surface_dim: Color::Rgb(1, 4, 9),
            overlay0: Color::Rgb(122, 130, 142),
            overlay1: Color::Rgb(158, 167, 179),
            text: Color::Rgb(240, 243, 246),
            subtext0: Color::Rgb(189, 196, 204),
            mauve: Color::Rgb(203, 158, 255),
            green: Color::Rgb(38, 205, 77),
            yellow: Color::Rgb(240, 183, 47),
            red: Color::Rgb(255, 148, 146),
            blue: Color::Rgb(113, 183, 255),
            teal: Color::Rgb(57, 197, 207),
            peach: Color::Rgb(255, 183, 87),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// GitHub Light High Contrast — Primer's accessible light palette.
    pub fn github_light_high_contrast() -> Self {
        Self {
            accent: Color::Rgb(3, 73, 180),
            panel_bg: Color::Rgb(255, 255, 255),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(231, 236, 240),
            selection_bg: Color::Rgb(202, 225, 255),
            surface0: Color::Rgb(231, 236, 240),
            surface1: Color::Rgb(172, 182, 192),
            surface_dim: Color::Rgb(231, 236, 240),
            overlay0: Color::Rgb(136, 146, 157),
            overlay1: Color::Rgb(102, 112, 123),
            text: Color::Rgb(14, 17, 22),
            subtext0: Color::Rgb(75, 83, 93),
            mauve: Color::Rgb(98, 44, 188),
            green: Color::Rgb(5, 93, 32),
            yellow: Color::Rgb(116, 69, 0),
            red: Color::Rgb(160, 17, 31),
            blue: Color::Rgb(3, 73, 180),
            teal: Color::Rgb(27, 124, 131),
            peach: Color::Rgb(112, 44, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Solarized Dark — Ethan Schoonover's classic.
    pub fn solarized() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210), // blue
            panel_bg: Color::Rgb(0, 43, 54),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(22, 75, 87),
            selection_bg: Color::Rgb(8, 62, 85),
            surface0: Color::Rgb(7, 54, 66),
            surface1: Color::Rgb(88, 110, 117),
            surface_dim: Color::Rgb(0, 43, 54),
            overlay0: Color::Rgb(88, 110, 117),
            overlay1: Color::Rgb(101, 123, 131),
            text: Color::Rgb(147, 161, 161),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Solarized Light — Ethan Schoonover's light variant.
    pub fn solarized_light() -> Self {
        Self {
            accent: Color::Rgb(38, 139, 210),
            panel_bg: Color::Rgb(253, 246, 227),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(238, 232, 213),
            selection_bg: Color::Rgb(201, 220, 223),
            surface0: Color::Rgb(238, 232, 213),
            surface1: Color::Rgb(147, 161, 161),
            surface_dim: Color::Rgb(238, 232, 213),
            overlay0: Color::Rgb(147, 161, 161),
            overlay1: Color::Rgb(88, 110, 117),
            text: Color::Rgb(101, 123, 131),
            subtext0: Color::Rgb(131, 148, 150),
            mauve: Color::Rgb(211, 54, 130),
            green: Color::Rgb(133, 153, 0),
            yellow: Color::Rgb(181, 137, 0),
            red: Color::Rgb(220, 50, 47),
            blue: Color::Rgb(38, 139, 210),
            teal: Color::Rgb(42, 161, 152),
            peach: Color::Rgb(203, 75, 22),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Kanagawa — inspired by Katsushika Hokusai.
    pub fn kanagawa() -> Self {
        Self {
            accent: Color::Rgb(126, 156, 216), // blue
            panel_bg: Color::Rgb(31, 31, 40),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(54, 54, 70),
            selection_bg: Color::Rgb(50, 56, 75),
            surface0: Color::Rgb(42, 42, 55),
            surface1: Color::Rgb(54, 54, 70),
            surface_dim: Color::Rgb(31, 31, 40),
            overlay0: Color::Rgb(114, 113, 105),
            overlay1: Color::Rgb(135, 134, 125),
            text: Color::Rgb(220, 215, 186),
            subtext0: Color::Rgb(200, 195, 170),
            mauve: Color::Rgb(149, 127, 184),
            green: Color::Rgb(118, 148, 106),
            yellow: Color::Rgb(192, 163, 110),
            red: Color::Rgb(195, 64, 67),
            blue: Color::Rgb(126, 156, 216),
            teal: Color::Rgb(127, 180, 202),
            peach: Color::Rgb(255, 160, 102),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Kanagawa Lotus — the light Kanagawa variant.
    pub fn kanagawa_lotus() -> Self {
        Self {
            accent: Color::Rgb(77, 105, 155),
            panel_bg: Color::Rgb(242, 236, 188),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(213, 206, 163),
            selection_bg: Color::Rgb(220, 213, 172),
            surface0: Color::Rgb(220, 213, 172),
            surface1: Color::Rgb(201, 203, 209),
            surface_dim: Color::Rgb(213, 206, 163),
            overlay0: Color::Rgb(160, 156, 172),
            overlay1: Color::Rgb(138, 137, 128),
            text: Color::Rgb(84, 84, 100),
            subtext0: Color::Rgb(67, 67, 108),
            mauve: Color::Rgb(98, 76, 131),
            green: Color::Rgb(111, 137, 78),
            yellow: Color::Rgb(119, 113, 63),
            red: Color::Rgb(200, 64, 83),
            blue: Color::Rgb(77, 105, 155),
            teal: Color::Rgb(78, 140, 162),
            peach: Color::Rgb(204, 109, 0),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Rosé Pine — muted, elegant.
    pub fn rose_pine() -> Self {
        Self {
            accent: Color::Rgb(196, 167, 231), // iris
            panel_bg: Color::Rgb(25, 23, 36),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(38, 35, 58),
            selection_bg: Color::Rgb(59, 52, 75),
            surface0: Color::Rgb(31, 29, 46),
            surface1: Color::Rgb(38, 35, 58),
            surface_dim: Color::Rgb(38, 35, 58),
            overlay0: Color::Rgb(110, 106, 134),
            overlay1: Color::Rgb(144, 140, 170),
            text: Color::Rgb(224, 222, 244),
            subtext0: Color::Rgb(200, 197, 220),
            mauve: Color::Rgb(196, 167, 231),  // iris
            green: Color::Rgb(49, 116, 143),   // pine
            yellow: Color::Rgb(246, 193, 119), // gold
            red: Color::Rgb(235, 111, 146),    // love
            blue: Color::Rgb(49, 116, 143),    // pine
            teal: Color::Rgb(156, 207, 216),   // foam
            peach: Color::Rgb(234, 154, 151),  // rose
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Rosé Pine Dawn — the light Rosé Pine variant.
    pub fn rose_pine_dawn() -> Self {
        Self {
            accent: Color::Rgb(144, 122, 169),
            panel_bg: Color::Rgb(250, 244, 237),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(227, 217, 207),
            selection_bg: Color::Rgb(242, 233, 225),
            surface0: Color::Rgb(242, 233, 225),
            surface1: Color::Rgb(255, 250, 243),
            surface_dim: Color::Rgb(242, 233, 225),
            overlay0: Color::Rgb(152, 147, 165),
            overlay1: Color::Rgb(121, 117, 147),
            text: Color::Rgb(70, 66, 97),
            subtext0: Color::Rgb(121, 117, 147),
            mauve: Color::Rgb(144, 122, 169),
            green: Color::Rgb(40, 105, 131),
            yellow: Color::Rgb(234, 157, 52),
            red: Color::Rgb(180, 99, 122),
            blue: Color::Rgb(40, 105, 131),
            teal: Color::Rgb(86, 148, 159),
            peach: Color::Rgb(215, 130, 126),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Vesper — minimal high-contrast monochrome with peach and mint accents.
    pub fn vesper() -> Self {
        Self {
            accent: Color::Rgb(255, 199, 153),
            panel_bg: Color::Rgb(26, 26, 26),
            sidebar_bg: None,
            active_row_bg: Color::Rgb(16, 16, 16),
            selection_bg: Color::Rgb(35, 35, 35),
            surface0: Color::Rgb(35, 35, 35),
            surface1: Color::Rgb(40, 40, 40),
            surface_dim: Color::Rgb(16, 16, 16),
            overlay0: Color::Rgb(92, 92, 92),
            overlay1: Color::Rgb(126, 126, 126),
            text: Color::Rgb(255, 255, 255),
            subtext0: Color::Rgb(160, 160, 160),
            mauve: Color::Rgb(255, 209, 168),
            green: Color::Rgb(153, 255, 228),
            yellow: Color::Rgb(255, 199, 153),
            red: Color::Rgb(255, 128, 128),
            blue: Color::Rgb(176, 176, 176),
            teal: Color::Rgb(102, 221, 204),
            peach: Color::Rgb(255, 199, 153),
            work_status: WorkStatusColors::DEFAULT,
        }
    }

    /// Background the desktop sidebar paints under its rows.
    ///
    /// An unset `sidebar_bg` follows the palette's own panel background rather
    /// than the terminal's. Borrowing the terminal background is what makes a
    /// mismatched palette unreadable: theme foregrounds land on a background
    /// chosen by something else. The `terminal` palette keeps `panel_bg` at
    /// `Reset`, so it still inherits, and so does an explicit
    /// `theme.custom.sidebar_bg = "reset"`.
    pub fn sidebar_background(&self) -> Color {
        self.sidebar_bg.unwrap_or(self.panel_bg)
    }

    /// Appearance this palette was built for, inferred from `panel_bg`.
    ///
    /// `None` when the palette leaves its panel background to the terminal
    /// (the `terminal` theme), where no appearance can be claimed.
    pub fn appearance(&self) -> Option<crate::terminal_theme::HostAppearance> {
        Some(color_rgb(self.panel_bg)?.inferred_appearance())
    }

    /// Default terminal colors for child applications when the outer terminal
    /// did not answer OSC 10/11. A `Reset` token stays unknown so the terminal
    /// core can answer with its own effective default.
    pub fn terminal_theme(&self) -> crate::terminal_theme::TerminalTheme {
        crate::terminal_theme::TerminalTheme {
            foreground: color_rgb(self.text),
            background: color_rgb(self.panel_bg),
            ..Default::default()
        }
    }

    /// Resolve a theme by name. Returns None for unknown names.
    /// Status glyph colour for backlog, todo, draft, closed, cancelled and
    /// unassigned work. Defaults to the muted chrome tone (gray).
    pub fn work_status_neutral(&self) -> Color {
        self.work_status.neutral.unwrap_or(self.overlay0)
    }

    /// Status glyph colour for work in progress. Defaults to yellow.
    pub fn work_status_active(&self) -> Color {
        self.work_status.active.unwrap_or(self.yellow)
    }

    /// Status glyph colour for work in review. Defaults to green.
    pub fn work_status_review(&self) -> Color {
        self.work_status.review.unwrap_or(self.green)
    }

    /// Status glyph colour for finished work. Defaults to blue.
    pub fn work_status_done(&self) -> Color {
        self.work_status.done.unwrap_or(self.blue)
    }

    /// Status glyph colour for triage. Defaults to orange.
    pub fn work_status_triage(&self) -> Color {
        self.work_status.triage.unwrap_or(self.peach)
    }

    /// Status glyph colour for a merged pull request. Defaults to purple.
    pub fn work_status_merged(&self) -> Color {
        self.work_status.merged.unwrap_or(self.mauve)
    }

    /// Status glyph colour for an open pull request or conversation.
    /// Defaults to green.
    pub fn work_status_open(&self) -> Color {
        self.work_status.open.unwrap_or(self.green)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match crate::config::canonical_theme_name(name)? {
            "catppuccin" => Some(Self::catppuccin()),
            "catppuccin-latte" => Some(Self::catppuccin_latte()),
            "terminal" => Some(Self::terminal()),
            "tokyo-night" => Some(Self::tokyo_night()),
            "tokyo-night-day" => Some(Self::tokyo_night_day()),
            "dracula" => Some(Self::dracula()),
            "nord" => Some(Self::nord()),
            "gruvbox" => Some(Self::gruvbox()),
            "gruvbox-light" => Some(Self::gruvbox_light()),
            "one-dark" | "onedark" => Some(Self::one_dark()),
            "one-light" | "onelight" => Some(Self::one_light()),
            "github-dark-high-contrast" | "github-dark-hc" => {
                Some(Self::github_dark_high_contrast())
            }
            "github-light-high-contrast" | "github-light-hc" => {
                Some(Self::github_light_high_contrast())
            }
            "solarized" | "solarized-dark" => Some(Self::solarized()),
            "solarized-light" => Some(Self::solarized_light()),
            "kanagawa" => Some(Self::kanagawa()),
            "kanagawa-lotus" => Some(Self::kanagawa_lotus()),
            "rose-pine" => Some(Self::rose_pine()),
            "rose-pine-dawn" => Some(Self::rose_pine_dawn()),
            "vesper" => Some(Self::vesper()),
            _ => None,
        }
    }

    /// Apply custom color overrides on top of this palette.
    pub fn with_overrides(mut self, custom: &crate::config::CustomThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.sidebar_bg {
            // An explicit `reset` is a choice, not an absence: it hands the
            // sidebar back to the terminal background.
            self.sidebar_bg = Some(parse_color(c));
        }
        if let Some(c) = &custom.active_row_bg {
            self.active_row_bg = parse_color(c);
        }
        if let Some(c) = &custom.selection_bg {
            self.selection_bg = parse_color(c);
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        if let Some(c) = &custom.work_status_neutral {
            self.work_status.neutral = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_active {
            self.work_status.active = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_review {
            self.work_status.review = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_done {
            self.work_status.done = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_triage {
            self.work_status.triage = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_merged {
            self.work_status.merged = Some(parse_color(c));
        }
        if let Some(c) = &custom.work_status_open {
            self.work_status.open = Some(parse_color(c));
        }
        self
    }

    pub fn with_mode_overrides(mut self, custom: &crate::config::ModeThemeColors) -> Self {
        use crate::config::parse_color;
        if let Some(c) = &custom.accent {
            self.accent = parse_color(c);
        }
        if let Some(c) = &custom.panel_bg {
            self.panel_bg = parse_color(c);
        }
        if let Some(c) = &custom.sidebar_bg {
            // The fork keeps sidebar_bg optional so an unset token falls back
            // to the panel background instead of painting a hard-coded colour.
            self.sidebar_bg = Some(parse_color(c));
        }
        if let Some(c) = &custom.active_row_bg {
            self.active_row_bg = parse_color(c);
        }
        if let Some(c) = &custom.selection_bg {
            self.selection_bg = parse_color(c);
        }
        if let Some(c) = &custom.surface0 {
            self.surface0 = parse_color(c);
        }
        if let Some(c) = &custom.surface1 {
            self.surface1 = parse_color(c);
        }
        if let Some(c) = &custom.surface_dim {
            self.surface_dim = parse_color(c);
        }
        if let Some(c) = &custom.overlay0 {
            self.overlay0 = parse_color(c);
        }
        if let Some(c) = &custom.overlay1 {
            self.overlay1 = parse_color(c);
        }
        if let Some(c) = &custom.text {
            self.text = parse_color(c);
        }
        if let Some(c) = &custom.subtext0 {
            self.subtext0 = parse_color(c);
        }
        if let Some(c) = &custom.mauve {
            self.mauve = parse_color(c);
        }
        if let Some(c) = &custom.green {
            self.green = parse_color(c);
        }
        if let Some(c) = &custom.yellow {
            self.yellow = parse_color(c);
        }
        if let Some(c) = &custom.red {
            self.red = parse_color(c);
        }
        if let Some(c) = &custom.blue {
            self.blue = parse_color(c);
        }
        if let Some(c) = &custom.teal {
            self.teal = parse_color(c);
        }
        if let Some(c) = &custom.peach {
            self.peach = parse_color(c);
        }
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceCardArea {
    pub ws_idx: usize,
    pub rect: Rect,
    pub indented: bool,
    /// Whether this row renders the repository-header chevron at `rect.x`.
    pub repo_header: bool,
    /// The settled pane represented by this workspace header, when the header
    /// belongs to the Settled section.
    pub settled_pane_id: Option<PaneId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCardArea {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: PaneId,
    pub rect: Rect,
    /// Index into the sidebar rows this frame. A blocked pane owns two rows --
    /// one in its group, one in the tree -- so the ids alone cannot say which
    /// of the two a card came from.
    pub row_idx: usize,
}

/// A sub-cell of a sidebar row that explains itself on hover: a status glyph,
/// an agent dot, or a work-item title the row was too narrow to show in full.
/// The label is resolved while the frame is laid out, so the tooltip never has
/// to walk the sidebar rows again at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarHoverTarget {
    pub rect: Rect,
    pub label: String,
    pub action: Option<SidebarHoverAction>,
    /// The row-wide hover area that reveals lifecycle controls. It has no tooltip.
    pub(crate) row_hover: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarHoverAction {
    Snooze { target: SidebarPaneLifecycleTarget },
    Settle { target: SidebarPaneLifecycleTarget },
    Unsettle { target: SidebarPaneLifecycleTarget },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarPaneLifecycleTarget {
    Local(PaneFocusTarget),
    Remote(crate::api::schema::AgentRef),
}

impl SidebarPaneLifecycleTarget {
    #[cfg(test)]
    pub(crate) fn local(&self) -> Option<&PaneFocusTarget> {
        match self {
            Self::Local(target) => Some(target),
            Self::Remote(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabCardArea {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: PaneId,
    pub depth: u16,
    pub rect: Rect,
}

/// Per-view narrowing for work-item projections. This remains TUI-only state:
/// provider observations are shared runtime facts, while each attached client
/// chooses its own filters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) enum SidebarMachineScope {
    #[serde(rename = "this_machine")]
    ThisMachine,
    #[serde(rename = "all_machines")]
    #[default]
    AllMachines,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct SidebarWorkFilter {
    /// Persisted row-search query shared by every sidebar view.
    pub(crate) query: String,
    /// Whether the sidebar includes remote fleet sessions in its body.
    pub(crate) machine_scope: SidebarMachineScope,
    /// Id of the `[[projects]]` entry the sidebar is scoped to. `None` shows
    /// every project, which is what an unconfigured Herdr always shows.
    pub(crate) project: Option<String>,
    /// Legacy field names preserve existing 2b presentation files.
    pub(crate) team: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) linear_ownership: WorkOwnershipFilter,
    pub(crate) linear_statuses: std::collections::BTreeSet<LinearStatusFilter>,
    pub(crate) github: GithubSidebarFilter,
    pub(crate) missive: MissiveSidebarFilter,
}

impl SidebarWorkFilter {
    pub(crate) fn linear_label(&self) -> String {
        format!(
            "{} · {} · {}",
            self.team.as_deref().unwrap_or("all teams"),
            assignee_filter_label(self.assignee.as_deref()),
            if self.linear_statuses == default_linear_statuses() {
                "active".into()
            } else {
                format!("{} statuses", self.linear_statuses.len())
            }
        )
    }

    pub(crate) fn github_label(&self) -> String {
        format!(
            "{} · drafts {} · {}",
            assignee_filter_label(self.github.assignee.as_deref()),
            if self.github.show_drafts {
                "shown"
            } else {
                "hidden"
            },
            self.github.state.label(),
        )
    }

    pub(crate) fn missive_label(&self) -> String {
        format!(
            "{} · {} · closed {}",
            self.missive.team.as_deref().unwrap_or("all teams"),
            assignee_filter_label(self.missive.assignee.as_deref()),
            if self.missive.show_closed {
                "shown"
            } else {
                "hidden"
            },
        )
    }

    pub(crate) fn matches_linear(
        &self,
        ticket: &crate::work_index::WorkTicket,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        if let Some(team) = self.team.as_deref() {
            let actual_team = ticket.identifier.split_once('-').map(|(team, _)| team);
            if actual_team != Some(team) {
                return false;
            }
        }
        if let Some(selected) = self.assignee.as_deref() {
            let identities = if selected == "me" {
                [
                    session.linear.viewer.as_deref(),
                    session.linear_viewer_identity(),
                ]
            } else {
                [Some(selected), None]
            };
            let assigned = identities.iter().flatten().any(|identity| {
                ticket
                    .assignee
                    .as_deref()
                    .is_some_and(|assignee| assignee.eq_ignore_ascii_case(identity))
            });
            let authored = identities.iter().flatten().any(|identity| {
                ticket.creator.as_ref().is_some_and(|creator| {
                    creator.id.eq_ignore_ascii_case(identity)
                        || creator.name.eq_ignore_ascii_case(identity)
                })
            });
            let matches_ownership = identities.iter().all(Option::is_none)
                || match self.linear_ownership {
                    WorkOwnershipFilter::Assigned => assigned,
                    WorkOwnershipFilter::Authored => authored,
                    WorkOwnershipFilter::Both => assigned || authored,
                };
            if !matches_ownership {
                return false;
            }
        }
        ticket
            .state
            .as_deref()
            .and_then(LinearStatusFilter::from_name)
            .is_none_or(|status| self.linear_statuses.contains(&status))
    }

    pub(crate) fn matches_github(
        &self,
        item: &crate::work_index::WorkItem,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        if !item.source.github {
            return true;
        }
        if item.draft && !self.github.show_drafts {
            return false;
        }
        if item
            .pr_state
            .as_deref()
            .is_some_and(|state| !state.eq_ignore_ascii_case(self.github.state.label()))
        {
            return false;
        }
        let selected = self.github.assignee.as_deref();
        if selected.is_none() {
            return true;
        }
        let resolved = if selected == Some("me") {
            let Some(viewer) = session.github.viewer.as_deref() else {
                return true;
            };
            viewer
        } else {
            selected.unwrap_or_default()
        };
        let assigned = item
            .assignees
            .iter()
            .any(|assignee| assignee.eq_ignore_ascii_case(resolved));
        let authored = item
            .author
            .as_deref()
            .is_some_and(|author| author.eq_ignore_ascii_case(resolved));
        match self.github.ownership {
            WorkOwnershipFilter::Assigned => assigned,
            WorkOwnershipFilter::Authored => authored,
            WorkOwnershipFilter::Both => assigned || authored,
        }
    }

    pub(crate) fn matches_missive_conversation(
        &self,
        conversation: Option<&crate::work_index::MissiveConversation>,
        session: &crate::work_index::WorkIndexSession,
    ) -> bool {
        let Some(conversation) = conversation else {
            return true;
        };
        if conversation.closed && !self.missive.show_closed {
            return false;
        }
        if let Some(selected) = self.missive.team.as_deref() {
            let matches_team = conversation.team.as_ref().is_some_and(|team| {
                team.name.eq_ignore_ascii_case(selected) || team.id.eq_ignore_ascii_case(selected)
            });
            if !matches_team {
                return false;
            }
        }
        let Some(selected) = self.missive.assignee.as_deref() else {
            return true;
        };
        let resolved = if selected == "me" {
            let Some(viewer) = session.missive.viewer.as_deref() else {
                return true;
            };
            viewer
        } else {
            selected
        };
        conversation
            .assignees
            .iter()
            .any(|user| user.name.eq_ignore_ascii_case(resolved))
    }
}

impl Default for SidebarWorkFilter {
    fn default() -> Self {
        Self {
            query: String::new(),
            machine_scope: SidebarMachineScope::default(),
            project: None,
            team: Some("SCA".into()),
            assignee: Some("me".into()),
            linear_ownership: WorkOwnershipFilter::Both,
            linear_statuses: default_linear_statuses(),
            github: GithubSidebarFilter::default(),
            missive: MissiveSidebarFilter::default(),
        }
    }
}

fn assignee_filter_label(value: Option<&str>) -> &str {
    value.unwrap_or("all")
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LinearStatusFilter {
    Draft,
    Backlog,
    Ready,
    Todo,
    InProgress,
    InReview,
    Done,
    Canceled,
    Duplicate,
    Triage,
}

impl LinearStatusFilter {
    pub(crate) const ALL: [Self; 10] = [
        Self::Draft,
        Self::Backlog,
        Self::Ready,
        Self::Todo,
        Self::InProgress,
        Self::InReview,
        Self::Done,
        Self::Canceled,
        Self::Duplicate,
        Self::Triage,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Backlog => "Backlog",
            Self::Ready => "Ready",
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
            Self::Canceled => "Canceled",
            Self::Duplicate => "Duplicate",
            Self::Triage => "Triage",
        }
    }

    fn from_name(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.label().eq_ignore_ascii_case(value))
    }
}

fn default_linear_statuses() -> std::collections::BTreeSet<LinearStatusFilter> {
    LinearStatusFilter::ALL
        .into_iter()
        .filter(|status| {
            !matches!(
                status,
                LinearStatusFilter::Canceled | LinearStatusFilter::Duplicate
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct GithubSidebarFilter {
    pub(crate) assignee: Option<String>,
    pub(crate) ownership: WorkOwnershipFilter,
    pub(crate) show_drafts: bool,
    pub(crate) state: GithubStateFilter,
}

impl Default for GithubSidebarFilter {
    fn default() -> Self {
        Self {
            assignee: Some("me".into()),
            ownership: WorkOwnershipFilter::Both,
            show_drafts: false,
            state: GithubStateFilter::Open,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WorkOwnershipFilter {
    Assigned,
    Authored,
    #[default]
    Both,
}

impl WorkOwnershipFilter {
    pub(crate) const ALL: [Self; 3] = [Self::Assigned, Self::Authored, Self::Both];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Assigned => "assigned",
            Self::Authored => "authored",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GithubStateFilter {
    #[default]
    Open,
    Merged,
    Closed,
}

impl GithubStateFilter {
    pub(crate) const ALL: [Self; 3] = [Self::Open, Self::Merged, Self::Closed];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub(crate) struct MissiveSidebarFilter {
    pub(crate) team: Option<String>,
    pub(crate) assignee: Option<String>,
    pub(crate) show_closed: bool,
}

impl Default for MissiveSidebarFilter {
    fn default() -> Self {
        Self {
            team: None,
            assignee: Some("me".into()),
            show_closed: false,
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarGroupMode {
    #[default]
    Repo,
    /// Flat list of every Space, with no repository grouping above it.
    Spaces,
    RepoPr,
    RepoWorktree,
    LinearTeam,
    Missive,
}

impl SidebarGroupMode {
    pub(crate) const ALL: [Self; 6] = [
        Self::Repo,
        Self::Spaces,
        Self::RepoPr,
        Self::RepoWorktree,
        Self::LinearTeam,
        Self::Missive,
    ];

    pub(crate) const VIEWS: [Self; 5] = [
        Self::Repo,
        Self::Spaces,
        Self::LinearTeam,
        Self::RepoPr,
        Self::Missive,
    ];

    pub(crate) fn view_label(self) -> &'static str {
        match self {
            Self::Repo | Self::RepoWorktree => "Repo",
            Self::Spaces => "Spaces",
            Self::LinearTeam => "Linear",
            Self::RepoPr => "GitHub",
            Self::Missive => "Missive",
        }
    }

    pub(crate) fn next(self) -> Self {
        let index = self.view_index();
        Self::VIEWS[(index + 1) % Self::VIEWS.len()]
    }

    pub(crate) fn view_index(self) -> usize {
        Self::VIEWS
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or(0)
    }

    pub(crate) fn collapse_namespace(self) -> &'static str {
        match self {
            Self::Repo => "repo",
            Self::Spaces => "spaces",
            Self::RepoPr => "repo_pr",
            Self::RepoWorktree => "repo_worktree",
            Self::LinearTeam => "linear_team",
            Self::Missive => "missive",
        }
    }
}

/// How one sidebar group orders its rows. `Default` is the canonical order the
/// projection already builds; every other mode re-sorts only the rows of the
/// group the operator picked. Persisted per group key in the client-local
/// presentation file, so the serde names are a compatibility contract.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarSortMode {
    /// The projection's canonical order; lifecycle changes never move a row.
    #[default]
    Default,
    /// Case-insensitive alphabetical by the row's primary label.
    Name,
    /// Blocked/gated first, then working, then done/idle; ties by most recent
    /// state change.
    Status,
    /// Most recent state change first.
    Recent,
}

impl SidebarSortMode {
    pub(crate) const ALL: [Self; 4] = [Self::Default, Self::Name, Self::Status, Self::Recent];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Name => "Name A→Z",
            Self::Status => "Status",
            Self::Recent => "Recent",
        }
    }

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0)
    }
}

/// Attach-local sidebar state. The headless server swaps one instance into
/// `AppState` while routing input or rendering for that client; the monolithic
/// app keeps its own instance directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarPresentationState {
    pub(crate) focused: bool,
    pub(crate) focus_intent: ClientFocusIntent,
    pub(crate) expanded_workspace_ids: std::collections::HashSet<String>,
    pub(crate) known_workspace_ids: std::collections::HashSet<String>,
    /// Workspace this attach last scrolled into view. Focus changes routed
    /// through the JSON API/CLI run outside the per-client presentation swap,
    /// so each attach compares this against the active workspace when it
    /// renders and scrolls itself instead of relying on the mutating path.
    pub(crate) revealed_workspace_id: Option<String>,
    pub(crate) workspace_scroll: usize,
    pub(crate) mobile_switcher_scroll: usize,
    pub(crate) collapsed_groups: std::collections::HashSet<String>,
    /// Global projection revision last reconciled into this attach.
    pub(crate) projection_revision: u64,
    pub(crate) group_mode: SidebarGroupMode,
    pub(crate) group_menu_open: bool,
    pub(crate) group_menu_selected: usize,
    pub(crate) work_filter: SidebarWorkFilter,
    pub(crate) filter_menu_open: bool,
    pub(crate) filter_menu_selected: usize,
    pub(crate) search_active: bool,
    pub(crate) new_menu: Option<SidebarNewMenuState>,
    pub(crate) new_thread: Option<SidebarNewThreadState>,
    pub(crate) project_menu: Option<SidebarProjectMenuState>,
    pub(crate) selected_work_group: Option<String>,
    pub(crate) object_menu: Option<SidebarObjectMenuState>,
    pub(crate) sort_menu: Option<SidebarSortMenuState>,
    pub(crate) subgroup_picker: Option<SidebarSubgroupPickerState>,
    pub(crate) pod_picker: Option<SidebarPodPickerState>,
    /// Per-group sort choices, keyed by the group's canonical key. Client
    /// presentation: two attaches may sort the same group differently.
    pub(crate) group_sorts: std::collections::HashMap<String, SidebarSortMode>,
    pub(crate) unassigned_expanded_views: std::collections::HashSet<SidebarGroupMode>,
    pub(crate) selected_settled: Option<PaneFocusTarget>,
    pub(crate) selected_remote_agent: Option<crate::api::schema::AgentRef>,
    /// Every transient modal owned by this attach. Shared pane/session facts
    /// never live here; the server swaps this record into `AppState` only
    /// while it handles or renders this client.
    pub(crate) overlay: ClientOverlayState,
}

/// The client-owned overlay tag. Server-owned full-screen modes never enter
/// this enum and remain in `AppState::server_interaction`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ClientOverlay {
    #[default]
    None,
    RenameWorkspace,
    RenameTab,
    RenamePane,
    NewLinkedWorktree,
    OpenExistingWorktree,
    ConfirmRemoveWorktree,
    ConfirmClose,
    ContextMenu,
}

impl ClientOverlay {
    pub(crate) fn mode(self) -> Option<Mode> {
        match self {
            Self::None => None,
            Self::RenameWorkspace => Some(Mode::RenameWorkspace),
            Self::RenameTab => Some(Mode::RenameTab),
            Self::RenamePane => Some(Mode::RenamePane),
            Self::NewLinkedWorktree => Some(Mode::NewLinkedWorktree),
            Self::OpenExistingWorktree => Some(Mode::OpenExistingWorktree),
            Self::ConfirmRemoveWorktree => Some(Mode::ConfirmRemoveWorktree),
            Self::ConfirmClose => Some(Mode::ConfirmClose),
            Self::ContextMenu => Some(Mode::ContextMenu),
        }
    }
}

/// The client-local presentation state that owns the next input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientInputOwner {
    Overlay(ClientOverlay),
    SnoozeMenu,
    SnoozeTime,
    SettledMenu,
    SettledDeleteConfirm,
    AgentPicker,
    SidebarGroupMenu,
    SidebarFilterMenu,
    SidebarNewMenu,
    SidebarNewThread,
    SidebarProjectMenu,
    SidebarObjectMenu,
    SidebarSortMenu,
    SidebarSubgroupPicker,
    SidebarPodPicker,
    PrActionConfirmation,
    DockSurfaceMenu,
}

/// The input and graphics rules resolved from one client's complete presentation.
/// Headless clients share retained geometry while the local renderer borrows its
/// current view, so consumers stay client-bound without allocating per frame.
#[derive(Clone)]
enum ClientPresentationGeometry<'a> {
    Borrowed(&'a [crate::layout::PaneInfo]),
    Shared(std::sync::Arc<Vec<crate::layout::PaneInfo>>),
}

impl ClientPresentationGeometry<'_> {
    fn pane_infos(&self) -> &[crate::layout::PaneInfo] {
        match self {
            Self::Borrowed(pane_infos) => pane_infos,
            Self::Shared(pane_infos) => pane_infos.as_slice(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ClientPresentationPolicy<'a> {
    owner: Option<ClientInputOwner>,
    mode: Mode,
    pomodoro_owns_input: bool,
    tab_surface_visible: bool,
    geometry: ClientPresentationGeometry<'a>,
}

impl ClientPresentationPolicy<'static> {
    pub(crate) fn from_presentations(
        server_mode: Mode,
        sidebar: &SidebarPresentationState,
        dock: &DockPresentationState,
        pomodoro_owns_input: bool,
        tab_surface_visible: bool,
        retained_pane_infos: std::sync::Arc<Vec<crate::layout::PaneInfo>>,
    ) -> Self {
        let owner = ClientInputOwnerState::from_presentations(sidebar, dock).resolve();
        let mode = sidebar
            .overlay
            .kind
            .mode()
            .unwrap_or(match sidebar.focus_intent {
                ClientFocusIntent::FollowShared => server_mode,
                ClientFocusIntent::Pane => Mode::Terminal,
            });
        Self {
            owner,
            mode,
            pomodoro_owns_input,
            tab_surface_visible,
            geometry: ClientPresentationGeometry::Shared(retained_pane_infos),
        }
    }
}

impl<'a> ClientPresentationPolicy<'a> {
    fn from_app(app: &'a AppState) -> Self {
        let pomodoro_owns_input = crate::ui::pomodoro::input_presentation_at(
            app,
            app.screen_rect(),
            app.view_observed_at,
        )
        .owns_input();
        Self {
            owner: ClientInputOwnerState::from_app(app).resolve(),
            mode: app.effective_interaction_mode(),
            pomodoro_owns_input,
            tab_surface_visible: !app.tab_surface_replaced(),
            geometry: ClientPresentationGeometry::Borrowed(&app.view.pane_infos),
        }
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    pub(crate) fn owns_input(&self) -> bool {
        self.pomodoro_owns_input || self.owner.is_some()
    }

    pub(crate) fn mouse_motion_changes_view(&self) -> bool {
        self.owner.is_some() || self.mode.mouse_motion_changes_view()
    }

    pub(crate) fn pane_graphics_visible(&self) -> bool {
        !self.owns_input() && self.mode == Mode::Terminal && self.tab_surface_visible
    }

    pub(crate) fn tab_surface_visible(&self) -> bool {
        self.tab_surface_visible
    }

    pub(crate) fn tab_surface(&self) -> crate::ui::TabSurfaceView<'_> {
        crate::ui::TabSurfaceView {
            pane_infos: self.geometry.pane_infos(),
            split_borders: &[],
        }
    }
}

/// Client-local choice between the shared interaction surface and the focused
/// pane. Focus completions set this instead of rewriting the server-owned mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ClientFocusIntent {
    #[default]
    FollowShared,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerInputOwner {
    Onboarding,
    ReleaseNotes,
    ProductAnnouncement,
    Navigate,
    Prefix,
    Copy,
    Resize,
    GitMenu,
    AddAction,
    Settings,
    GlobalMenu,
    KeybindHelp,
    Navigator,
    CommandPalette,
    WorkLinkPicker,
}

impl ServerInputOwner {
    fn from_mode(mode: Mode) -> Option<Self> {
        match mode {
            Mode::Onboarding => Some(Self::Onboarding),
            Mode::ReleaseNotes => Some(Self::ReleaseNotes),
            Mode::ProductAnnouncement => Some(Self::ProductAnnouncement),
            Mode::Navigate => Some(Self::Navigate),
            Mode::Prefix => Some(Self::Prefix),
            Mode::Copy => Some(Self::Copy),
            Mode::Resize => Some(Self::Resize),
            Mode::GitMenu => Some(Self::GitMenu),
            Mode::AddAction => Some(Self::AddAction),
            Mode::Settings => Some(Self::Settings),
            Mode::GlobalMenu => Some(Self::GlobalMenu),
            Mode::KeybindHelp => Some(Self::KeybindHelp),
            Mode::Navigator => Some(Self::Navigator),
            Mode::CommandPalette => Some(Self::CommandPalette),
            Mode::WorkLinkPicker => Some(Self::WorkLinkPicker),
            Mode::Terminal
            | Mode::RenameWorkspace
            | Mode::RenameTab
            | Mode::RenamePane
            | Mode::NewLinkedWorktree
            | Mode::OpenExistingWorktree
            | Mode::ConfirmRemoveWorktree
            | Mode::ConfirmClose
            | Mode::ContextMenu => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceInputOwner {
    Board,
    Symphony,
    LoopRunHistory,
    AloopRunLog,
    Usage,
    Work,
    EditorPreview,
    DockObjectPreview,
    Home,
    Inbox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockInputOwner {
    Home,
    PullRequest,
    Linear,
    Diff,
    Files,
    Agents,
    Hosts,
    Editor,
    Chooser,
}

/// The one resolved owner for input and cursor routing. Keep this exhaustive:
/// adding a new interactive presentation requires choosing its precedence here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputOwner {
    Pomodoro,
    Client(ClientInputOwner),
    AddProject,
    Server(ServerInputOwner),
    Popup,
    Surface(SurfaceInputOwner),
    Notepad,
    Dock(DockInputOwner),
    Sidebar,
    Pane,
    None,
}

impl InputOwner {
    /// Dock navigation owns its shortcuts but leaves unrelated typing with the
    /// pane visible behind it. The editor dock routes to its own PTY instead.
    pub(crate) fn forwards_unhandled_input_to_pane(self) -> bool {
        match self {
            Self::Pane => true,
            Self::Dock(DockInputOwner::Editor) => false,
            Self::Dock(_) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeOwner {
    Server,
    Client(ClientOverlay),
}

/// Attach-local overlay state. `AppState` keeps matching active slots because
/// the monolithic input and rendering code still consumes them directly; the
/// server moves every slot together through this one record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ClientOverlayState {
    pub(crate) kind: ClientOverlay,
    pub(crate) context_menu: Option<ContextMenuState>,
    pub(crate) snooze: Option<SidebarSnoozeUiState>,
    pub(crate) rename_target: Option<RenameTarget>,
    pub(crate) confirm_close_workspace_id: Option<String>,
    pub(crate) confirm_close_remote_agent_ref: Option<crate::api::schema::AgentRef>,
    pub(crate) name_input: String,
    pub(crate) name_input_replace_on_type: bool,
    pub(crate) creating_new_tab: bool,
    pub(crate) pending_workspace_create_cwd: Option<std::path::PathBuf>,
    pub(crate) rename_tab_prefill: Option<String>,
    pub(crate) worktree_create: Option<WorktreeCreateState>,
    pub(crate) worktree_open: Option<WorktreeOpenState>,
    pub(crate) worktree_remove: Option<WorktreeRemoveState>,
    pub(crate) settled_menu_target: Option<PaneFocusTarget>,
    pub(crate) settled_menu_selected: usize,
    pub(crate) settled_menu_delete_armed: bool,
    pub(crate) agent_picker: Option<AgentPickerState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarSnoozeUiState {
    pub(crate) target: SidebarPaneLifecycleTarget,
    pub(crate) anchor: (u16, u16),
    pub(crate) selected: SidebarSnoozeMenuAction,
    pub(crate) time_draft: Option<String>,
    pub(crate) error: Option<String>,
}

impl SidebarPresentationState {
    pub(crate) fn initialize_group_mode(
        &mut self,
        group_mode: SidebarGroupMode,
        collapsed_overrides: &std::collections::HashMap<String, bool>,
    ) {
        self.group_mode = group_mode;
        self.group_menu_selected = group_mode.view_index();
        self.collapsed_groups = crate::ui::initial_collapsed_sidebar_groups(collapsed_overrides);
    }
}

/// Attach-local project picker opened from the sidebar header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarNewThreadState {
    pub(crate) filter: crate::ui::dropdown::DropdownFilterState,
}

/// Attach-local picker for the sidebar's project scope. Which project a client
/// is looking at is presentation, not a runtime fact: two attaches to the same
/// session may hold different scopes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarProjectMenuState {
    pub(crate) filter: crate::ui::dropdown::DropdownFilterState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SidebarNewMenuState {
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarNewMenuAction {
    NewSpace,
    AddProject,
    NewThread,
    OpenFolder,
}

impl SidebarNewMenuAction {
    pub(crate) const ALL: [Self; 4] = [
        Self::NewSpace,
        Self::AddProject,
        Self::NewThread,
        Self::OpenFolder,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::NewSpace => "New space",
            Self::AddProject => "Add project…",
            Self::NewThread => "New thread",
            Self::OpenFolder => "Open folder…",
        }
    }
}

/// Attach-local state for the action menu anchored to a sidebar work object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarObjectMenuState {
    pub(crate) target: String,
    /// Exact clicked row when mouse-opened. Keyboard-opened menus resolve the
    /// first visible row for the selected object.
    pub(crate) anchor_row: Option<u16>,
    pub(crate) page: SidebarObjectMenuPage,
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum SidebarObjectMenuPage {
    #[default]
    Actions,
    TicketTransitions,
    TicketPriorities,
    Confirmation,
}

/// Attach-local state for the sort dropdown anchored to a sidebar group header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarSortMenuState {
    /// Canonical key of the group being sorted (never namespaced or settled).
    pub(crate) target: String,
    /// Sort the menu marks as current: the group's effective sort, inheritance
    /// included, captured when the menu opened.
    pub(crate) current: SidebarSortMode,
    /// Cell the dropdown hangs from: the clicked sort glyph.
    pub(crate) anchor: (u16, u16),
    pub(crate) selected: usize,
}

/// Picker that assigns a window to a named sidebar subgroup. Opened from the
/// window's context menu; the typed query doubles as the new-subgroup name.
/// Transient like the context menu that opened it, and swapped through the
/// attach-local sidebar presentation state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SidebarSubgroupPickerState {
    pub(crate) ws_idx: usize,
    pub(crate) tab_idx: usize,
    /// Cell the dropdown hangs from: the context menu item the operator chose.
    pub(crate) anchor: (u16, u16),
    pub(crate) filter: crate::ui::dropdown::DropdownFilterState,
}

/// Attach-local picker that assigns one pane to a pod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarPodPickerState {
    pub(crate) ws_idx: usize,
    pub(crate) pane_id: crate::layout::PaneId,
    pub(crate) anchor: (u16, u16),
    pub(crate) filter: crate::ui::dropdown::DropdownFilterState,
}

/// Attach-local dock presentation. The headless server swaps one instance into
/// `AppState` while routing input or rendering that client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockPresentationState {
    pub(crate) width: u16,
    pub(crate) collapsed: bool,
    /// An explicit surface pick wins until the next sidebar/work view change.
    pub(crate) surface_override: bool,
    /// Active surface. `None` while the dock is a chooser with nothing open.
    pub(crate) tab: Option<DockSurface>,
    pub(crate) open_surfaces: Vec<DockSurface>,
    pub(crate) tab_bindings: Vec<Option<DockTabBinding>>,
    pub(crate) active_tab_index: Option<usize>,
    pub(crate) hovered_control: Option<ControlId>,
    pub(crate) hover_started_at: Option<Instant>,
    pub(crate) hover_tooltip_visible: bool,
    pub(crate) pane_tabs: std::collections::HashMap<PaneFocusTarget, PaneDockTabs>,
    pub(crate) followed_pane: Option<PaneFocusTarget>,
    pub(crate) context_objects: Vec<DockObjectRef>,
    pub(crate) suppressed_context: std::collections::HashSet<DockObjectRef>,
    pub(crate) maximized: bool,
    pub(crate) surface_menu: Option<DockSurfaceMenu>,
    pub(crate) chooser_focused: bool,
    pub(crate) scroll: u16,
    /// Object shown in the pane area while the dock is collapsed. This is
    /// attach-local presentation state; opening a thread clears the preview.
    pub(crate) object_preview: Option<DockObjectRef>,
    /// Rich PR/ticket view state keyed by stable object identity. This is
    /// attach-local TUI state and never enters the session protocol.
    pub(crate) object_views: std::collections::HashMap<WorkItemKey, ObjectViewState>,
    pub(crate) editor_focused: bool,
    pub(crate) editor_preview: Option<DockEditorPreview>,
    pub(crate) diff_focused: bool,
    pub(crate) pr_focused: bool,
    pub(crate) pr_checkout_menu: Option<PrCheckoutChoice>,
    pub(crate) pr_action_menu: Option<PrActionMenuState>,
    pub(crate) pr_action_confirmation: Option<PrActionConfirmation>,
    pub(crate) diff_ignore_whitespace: bool,
    pub(crate) diff_selected: usize,
    pub(crate) diff_collapsed: std::collections::HashSet<String>,
    pub(crate) diff_request: Option<super::diff::DiffRefreshRequest>,
    pub(crate) diff_active_key: Option<DiffCacheKey>,
    pub(crate) files_focused: bool,
    pub(crate) files_selection: Option<std::path::PathBuf>,
    pub(crate) files_filter: String,
    pub(crate) files_collapsed: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) files_sort: crate::files::FileSort,
    pub(crate) files_search_active: bool,
    pub(crate) files_last_click: Option<(std::path::PathBuf, std::time::Instant)>,
    pub(crate) agents_focused: bool,
    pub(crate) agents_selection: Option<String>,
    pub(crate) hosts_focused: bool,
    pub(crate) hosts_selection: Option<String>,
    pub(crate) linear_focused: bool,
    pub(crate) ticket_start_menu: Option<PrCheckoutChoice>,
    pub(crate) ticket_action_menu: Option<crate::ui::ticket_actions::TicketActionMenuState>,
    pub(crate) ticket_comment_draft: Option<String>,
    /// Comment composer and write authority belong to the client that staged
    /// them. Shared work-item facts stay in the work index.
    pub(crate) comment_draft: Option<String>,
    pub(crate) pending_write: Option<crate::work_index::WorkItemWrite>,
    pub(crate) write_notice: Option<String>,
    /// Selection inside the home tab. Stored as a work-item key, never an
    /// index, so it survives snapshot refreshes and list reordering.
    pub(crate) home_selection: Option<WorkItemKey>,
    pub(crate) home_ticket_selection: Option<WorkItemKey>,
    pub(crate) home_poll_selection: Option<WorkItemKey>,
    pub(crate) home_section: DockHomeSection,
    pub(crate) home_detail_tab: DockHomeDetailTab,
    pub(crate) home_focused: bool,
    /// Focus target last considered for automatic PR-tab selection. Keeping
    /// this attach-local lets an explicit selection survive while pane focus
    /// remains unchanged.
    pub(crate) home_followed_pane: Option<PaneFocusTarget>,
    /// Which Symphony job this client's dock surface follows. Attach-local like
    /// every other surface selection: two clients read the same snapshot but
    /// each picks its own job.
    pub(crate) symphony: Option<SymphonyDockSelection>,
}

impl Default for DockPresentationState {
    fn default() -> Self {
        Self {
            width: crate::ui::DOCK_DEFAULT_WIDTH,
            collapsed: true,
            surface_override: false,
            tab: None,
            open_surfaces: Vec::new(),
            tab_bindings: Vec::new(),
            active_tab_index: None,
            hovered_control: None,
            hover_started_at: None,
            hover_tooltip_visible: false,
            pane_tabs: std::collections::HashMap::new(),
            followed_pane: None,
            context_objects: Vec::new(),
            suppressed_context: std::collections::HashSet::new(),
            maximized: false,
            surface_menu: None,
            chooser_focused: false,
            scroll: 0,
            object_preview: None,
            object_views: std::collections::HashMap::new(),
            editor_focused: false,
            editor_preview: None,
            diff_focused: false,
            pr_focused: false,
            pr_checkout_menu: None,
            pr_action_menu: None,
            pr_action_confirmation: None,
            diff_ignore_whitespace: false,
            diff_selected: 0,
            diff_collapsed: std::collections::HashSet::new(),
            diff_request: None,
            diff_active_key: None,
            files_focused: false,
            files_selection: None,
            files_filter: String::new(),
            files_collapsed: std::collections::HashSet::new(),
            files_sort: crate::files::FileSort::Name,
            files_search_active: false,
            files_last_click: None,
            agents_focused: false,
            agents_selection: None,
            hosts_focused: false,
            hosts_selection: None,
            linear_focused: false,
            ticket_start_menu: None,
            ticket_action_menu: None,
            ticket_comment_draft: None,
            comment_draft: None,
            pending_write: None,
            write_notice: None,
            home_selection: None,
            home_ticket_selection: None,
            home_poll_selection: None,
            home_section: DockHomeSection::Prs,
            home_detail_tab: DockHomeDetailTab::Overview,
            home_focused: false,
            home_followed_pane: None,
            symphony: None,
        }
    }
}

impl DockPresentationState {
    pub(crate) fn hover_tooltip_deadline(&self) -> Option<Instant> {
        if self.hover_tooltip_visible || self.hovered_control.is_none() {
            return None;
        }
        self.hover_started_at
            .map(|started| started + super::HOVER_TOOLTIP_DELAY)
    }

    pub(crate) fn reveal_hover_tooltip_at(&mut self, now: Instant) -> bool {
        if self
            .hover_tooltip_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            self.hover_tooltip_visible = true;
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeCreateState {
    pub source_workspace_id: String,
    pub source_checkout_path: std::path::PathBuf,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub branch: String,
    pub checkout_path: std::path::PathBuf,
    pub error: Option<String>,
    pub creating: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRemoveState {
    pub workspace_id: String,
    pub repo_root: std::path::PathBuf,
    pub path: std::path::PathBuf,
    pub error: Option<String>,
    pub removing: bool,
    pub force_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenEntry {
    pub path: std::path::PathBuf,
    pub branch: Option<String>,
    pub is_linked_worktree: bool,
    pub already_open_ws_idx: Option<usize>,
}

impl WorktreeOpenEntry {
    pub(crate) fn display_name(&self) -> String {
        self.branch.clone().unwrap_or_else(|| {
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| self.path.display().to_string())
        })
    }

    pub(crate) fn status_label(&self) -> &'static str {
        if self.already_open_ws_idx.is_some() {
            "open"
        } else if self.branch.is_some() {
            ""
        } else if self.is_linked_worktree {
            "detached"
        } else {
            "root"
        }
    }

    fn search_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.display_name(),
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
            self.path.display(),
            self.status_label()
        )
        .to_lowercase()
    }

    fn matches_query(&self, query: &str) -> bool {
        text_matches_query(query, &self.search_text())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOpenState {
    pub source_workspace_id: String,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub entries: Vec<WorktreeOpenEntry>,
    pub selected: usize,
    pub query: String,
    pub search_focused: bool,
    pub error: Option<String>,
}

impl WorktreeOpenState {
    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        let query = self.query.trim();
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                (query.is_empty() || entry.matches_query(query)).then_some(idx)
            })
            .collect()
    }

    pub(crate) fn selected_entry_index(&self) -> Option<usize> {
        let indices = self.filtered_indices();
        if indices.contains(&self.selected) {
            Some(self.selected)
        } else {
            indices.first().copied()
        }
    }

    pub(crate) fn normalize_selection(&mut self) {
        if let Some(selected) = self.selected_entry_index() {
            self.selected = selected;
        }
    }

    pub(crate) fn select_previous_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[pos.saturating_sub(1)];
    }

    pub(crate) fn select_next_filtered(&mut self) {
        let indices = self.filtered_indices();
        let Some(current) = self.selected_entry_index() else {
            return;
        };
        let pos = indices.iter().position(|idx| *idx == current).unwrap_or(0);
        self.selected = indices[(pos + 1).min(indices.len().saturating_sub(1))];
    }
}

pub(crate) fn text_matches_query(query: &str, text: &str) -> bool {
    let haystack = text.to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|needle| haystack.contains(needle))
}

/// Computed view geometry — derived from AppState + terminal size.
/// Updated before each render, consumed by render and mouse handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewLayout {
    Desktop,
    Mobile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DockSurface {
    Home,
    Terminal,
    Files,
    Diff,
    Pr,
    Linear,
    Missive,
    Agents,
    Hosts,
    Editor,
    Shortcuts,
    Context,
    Scratchpad,
    Symphony,
}

/// Stable identity of one work object shown by a compact dock tab.
/// This is client presentation state. The referenced object remains owned by
/// the pane work context and work-index snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DockObjectRef {
    pub(crate) surface: DockSurface,
    pub(crate) key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockTabOrigin {
    Context,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockTabBinding {
    pub(crate) object: DockObjectRef,
    pub(crate) origin: DockTabOrigin,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneDockTabs {
    pub(crate) open_surfaces: Vec<DockSurface>,
    pub(crate) bindings: Vec<Option<DockTabBinding>>,
    pub(crate) active_index: Option<usize>,
    pub(crate) context_objects: Vec<DockObjectRef>,
    pub(crate) suppressed_context: std::collections::HashSet<DockObjectRef>,
}

impl DockSurface {
    /// Every surface the chooser can open, in menu order.
    pub const ALL: [Self; 14] = [
        Self::Terminal,
        Self::Files,
        Self::Diff,
        Self::Pr,
        Self::Linear,
        Self::Missive,
        Self::Agents,
        Self::Hosts,
        Self::Home,
        Self::Editor,
        Self::Shortcuts,
        Self::Context,
        Self::Scratchpad,
        Self::Symphony,
    ];

    /// The card grid of the empty dock, each with its single-key shortcut.
    pub const CARDS: [Self; 13] = [
        Self::Terminal,
        Self::Files,
        Self::Diff,
        Self::Pr,
        Self::Linear,
        Self::Missive,
        Self::Agents,
        Self::Hosts,
        Self::Home,
        Self::Editor,
        Self::Shortcuts,
        Self::Context,
        Self::Scratchpad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Editor => "edit",
            Self::Shortcuts => "keys",
            Self::Context => "ctx",
            Self::Scratchpad => "note",
            Self::Terminal => "term",
            Self::Files => "files",
            Self::Diff => "diff",
            Self::Pr => "pr",
            Self::Linear => "linear",
            Self::Missive => "missive",
            Self::Agents => "agents",
            Self::Hosts => "hosts",
            Self::Symphony => "flow",
        }
    }

    /// Title used by the chooser card grid and the `+` menu.
    pub fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Editor => "Editor",
            Self::Shortcuts => "Shortcuts",
            Self::Context => "Context",
            Self::Scratchpad => "Scratchpad",
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::Pr => "PR",
            Self::Linear => "Linear",
            Self::Missive => "Missive",
            Self::Agents => "Agents",
            Self::Hosts => "Hosts",
            Self::Symphony => "Symphony",
        }
    }

    /// Single-key shortcut of the empty-dock card grid.
    pub fn shortcut(self) -> Option<char> {
        match self {
            Self::Terminal => Some('T'),
            Self::Files => Some('F'),
            Self::Diff => Some('D'),
            Self::Pr => Some('P'),
            Self::Linear => Some('L'),
            Self::Agents => Some('A'),
            Self::Hosts => Some('R'),
            Self::Missive => None,
            _ => None,
        }
    }

    /// Shortcut shown by and dispatched from the empty-panel card grid.
    pub fn card_shortcut(self) -> char {
        match self {
            Self::Terminal => 'T',
            Self::Files => 'F',
            Self::Diff => 'D',
            Self::Pr => 'P',
            Self::Linear => 'L',
            Self::Missive => 'M',
            Self::Agents => 'A',
            Self::Hosts => 'R',
            Self::Home => 'H',
            Self::Editor => 'E',
            Self::Shortcuts => 'K',
            Self::Context => 'X',
            Self::Scratchpad => 'N',
            Self::Symphony => 'Y',
        }
    }

    pub fn from_card_shortcut(key: char) -> Option<Self> {
        let key = key.to_ascii_uppercase();
        Self::CARDS
            .into_iter()
            .find(|surface| surface.card_shortcut() == key)
    }

    pub fn from_shortcut(key: char) -> Option<Self> {
        let key = key.to_ascii_uppercase();
        Self::CARDS
            .into_iter()
            .find(|surface| surface.shortcut() == Some(key))
    }

    /// Placeholder body until the surface gets its implementation slice.
    pub fn placeholder(self) -> Option<String> {
        matches!(self, Self::Terminal | Self::Files)
            .then(|| format!("{}: coming in a later slice", self.title()))
    }
}

/// The Symphony job the dock surface is showing. Identity only: phase, wait and
/// age are read back out of the live snapshot every frame, so an open panel
/// tracks the workflow rather than a copy of it that goes stale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymphonyDockSelection {
    pub(crate) workflow_id: String,
    pub(crate) run_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DockSurfaceMenu {
    pub(crate) selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DiffCacheKey {
    pub(crate) root: std::path::PathBuf,
    pub(crate) base: String,
    pub(crate) ignore_whitespace: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffFileSummary {
    pub(crate) path: String,
    pub(crate) display_path: String,
    pub(crate) additions: usize,
    pub(crate) deletions: usize,
    pub(crate) binary: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffFileContent {
    pub(crate) committed: Vec<super::diff::DiffLine>,
    pub(crate) uncommitted: Vec<super::diff::DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DiffCacheEntry {
    pub(crate) branch: String,
    pub(crate) files: Vec<DiffFileSummary>,
    pub(crate) contents: std::collections::HashMap<String, DiffFileContent>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockFileRowHitArea {
    pub(crate) path: std::path::PathBuf,
    pub(crate) kind: crate::files::FileTreeRowKind,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockAgentRowHitArea {
    pub(crate) id: String,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DockHostRowHitArea {
    pub(crate) name: String,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HomeHitTarget {
    QueueRow(usize),
    NewTask,
    Reply,
    Detach,
    Prompt,
    Agent,
    Model,
    Effort,
    Access,
    Context,
    Project,
    Repo,
    Directory,
    Machine,
    Workspace,
    Ref,
    Target,
    PickerOption(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HomeHitArea {
    pub(crate) target: HomeHitTarget,
    pub(crate) rect: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UsageHitTarget {
    Cost,
    Tokens,
    Hours24,
    Days7,
    Days30,
    Days90,
    Model,
    Day,
    Rescan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageHitArea {
    pub(crate) target: UsageHitTarget,
    pub(crate) rect: Rect,
}

/// Which pane-toggle button in the tab row was pressed. TUI-only presentation
/// state: it is resolved into the existing split/close runtime calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneToggleDirection {
    Below,
    Right,
}

impl PaneToggleDirection {
    pub(crate) fn nav(self) -> crate::layout::NavDirection {
        match self {
            PaneToggleDirection::Below => crate::layout::NavDirection::Down,
            PaneToggleDirection::Right => crate::layout::NavDirection::Right,
        }
    }

    pub(crate) fn split(self) -> crate::api::schema::SplitDirection {
        match self {
            PaneToggleDirection::Below => crate::api::schema::SplitDirection::Down,
            PaneToggleDirection::Right => crate::api::schema::SplitDirection::Right,
        }
    }
}

pub struct ViewState {
    pub layout: ViewLayout,
    /// Full-width top status row (tmux-parity). Empty on mobile / tiny heights.
    pub status_bar_rect: Rect,
    pub sidebar_rect: Rect,
    /// Sidebar-footer entry for the settings screen.
    pub(crate) sidebar_footer_settings_hit_area: Rect,
    /// Sidebar-footer toggle for pending ask subtitles.
    pub(crate) sidebar_footer_ask_subtitles_hit_area: Rect,
    /// Sidebar-footer entry for the full-screen pull-request view.
    pub(crate) sidebar_footer_work_hit_area: Rect,
    /// Sidebar-footer entry for the client-local historical usage view.
    pub(crate) sidebar_footer_usage_hit_area: Rect,
    /// Header and breakdown controls inside the historical usage view.
    pub(crate) usage_hit_areas: Vec<UsageHitArea>,
    /// Sidebar-footer entry for the full-screen Linear ticket view.
    pub(crate) sidebar_footer_ticket_hit_area: Rect,
    /// Sidebar-footer entry for the full-screen Missive conversation view.
    pub(crate) sidebar_footer_missive_hit_area: Rect,
    /// The notepad panel at the bottom of the sidebar. Empty when it is off.
    pub(crate) notepad_rect: Rect,
    /// Clickable fold control in the active Usage-tab header.
    pub(crate) notepad_usage_toggle_hit_area: Rect,
    /// Clickable tabs in the notepad header: note names, the Context tab and
    /// the agent tab.
    pub(crate) notepad_tab_hit_areas: Vec<(crate::notepad::NotepadTabTarget, Rect)>,
    /// The agent tab's body rows, derived in view computation so clicks
    /// resolve to the exact row the operator saw. Empty on the note tabs.
    pub(crate) notepad_agent_rows: Vec<crate::ui::notepad_agent::NotepadAgentRow>,
    /// Maximum attach-local agent-tab offset for the last computed geometry.
    pub(crate) notepad_agent_max_scroll: usize,
    /// Visible Usage-tab rows, materialized with their click actions.
    pub(crate) notepad_usage_rows: Vec<crate::ui::notepad_usage::NotepadUsageRow>,
    /// Per-row hover targets derived from the same visible rows as rendering.
    pub(crate) notepad_usage_hit_areas: Vec<Rect>,
    /// Maximum attach-local Usage-tab offset for the last computed geometry.
    pub(crate) notepad_usage_max_scroll: usize,
    /// The break-timer countdown in the sidebar footer row.
    pub(crate) pomodoro_hit_area: Rect,
    /// Per-machine notification toggle beside the break timer.
    pub(crate) notification_hit_area: Rect,
    /// Cycle mode chooser beside the notification toggle.
    pub(crate) window_cycle_mode_hit_area: Rect,
    /// Checklist control for sections-layout area visibility.
    pub(crate) sidebar_areas_hit_area: Rect,
    /// The idle animation's panel under the notepad. Empty when it is off.
    pub(crate) hyperspace_rect: Rect,
    /// Its pause button, in the panel's bottom-left corner.
    pub(crate) hyperspace_pause_hit_area: Rect,
    /// Sidebar-footer entry for refreshing work and Git metadata.
    pub(crate) sidebar_footer_refresh_hit_area: Rect,
    pub(crate) sidebar_footer_board_hit_area: Rect,
    pub workspace_card_areas: Vec<WorkspaceCardArea>,
    pub agent_card_areas: Vec<AgentCardArea>,
    /// Hover targets for sidebar row internals and lifecycle controls. Row-wide
    /// hover targets use `ControlId::SidebarRowHover` and have no tooltip.
    pub(crate) sidebar_hover_targets: Vec<SidebarHoverTarget>,
    pub(crate) visible_agent_activity_instants: Vec<Instant>,
    /// Elapsed wall-clock ages for visible notepad rows. Keeping the elapsed
    /// value avoids constructing an `Instant` before host uptime on Windows.
    pub(crate) visible_notepad_agent_ages: Vec<Duration>,
    pub tab_bar_rect: Rect,
    pub tab_hit_areas: Vec<Rect>,
    pub tab_scroll_left_hit_area: Rect,
    pub tab_scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
    pub repo_editor_button_hit_area: Rect,
    pub add_action_button_hit_area: Rect,
    pub user_action_hit_areas: Vec<(usize, Rect)>,
    pub add_action_close_hit_area: Rect,
    pub add_action_field_hit_areas: Vec<(AddActionField, Rect)>,
    pub add_action_cancel_hit_area: Rect,
    pub add_action_save_hit_area: Rect,
    pub(crate) add_project_layout: crate::ui::add_project::AddProjectLayout,
    pub git_menu_button_hit_area: Rect,
    pub git_menu_popup_rect: Rect,
    pub git_menu_first_visible: usize,
    pub git_menu_row_hit_areas: Vec<Rect>,
    pub pane_toggle_below_hit_area: Rect,
    pub pane_toggle_right_hit_area: Rect,
    pub terminal_area: Rect,
    pub work_context_link_rows: Vec<WorkContextLinkRow>,
    pub mobile_header_rect: Rect,
    pub mobile_menu_hit_area: Rect,
    /// Client-side hover and click target for the compact diagnostic marker.
    pub(crate) config_diagnostic_hit_area: Rect,
    pub toast_hit_area: Rect,
    /// `(queue index, row rect)` for each row the home view is showing.
    pub home_row_hit_areas: Vec<(usize, Rect)>,
    /// All clickable home controls, including composer fields and open pickers.
    pub(crate) home_hit_areas: Vec<HomeHitArea>,
    pub pane_infos: Vec<PaneInfo>,
    pub split_borders: Vec<SplitBorder>,
    pub dock_rect: Rect,
    pub dock_handle_rect: Rect,
    pub dock_divider_rect: Rect,
    pub dock_tab_bar_rect: Rect,
    pub dock_tab_hit_areas: Vec<Rect>,
    /// Close glyph of the active tab; empty when no surface is open.
    pub dock_tab_close_rect: Rect,
    /// The `+` that opens the surface chooser.
    pub dock_plus_rect: Rect,
    /// The `⤢` that maximises the dock.
    pub dock_maximize_rect: Rect,
    pub dock_auto_open_rect: Rect,
    /// One rect per `DockSurface::CARDS` entry of the empty-dock grid.
    pub dock_surface_card_hit_areas: Vec<Rect>,
    /// Geometry of the open `+` menu.
    pub(crate) dock_surface_menu_layout: Option<crate::ui::dropdown::DropdownLayout>,
    pub dock_home_section_hit_areas: Vec<Rect>,
    pub dock_home_tab_hit_areas: Vec<Rect>,
    pub(crate) dock_home_tab_keys: Vec<WorkItemKey>,
    pub dock_home_detail_tab_hit_areas: Vec<Rect>,
    pub(crate) dock_file_row_hit_areas: Vec<DockFileRowHitArea>,
    pub(crate) dock_files_refresh_rect: Rect,
    pub(crate) dock_files_sort_rect: Rect,
    pub(crate) editor_preview_refresh_rect: Rect,
    pub(crate) editor_preview_open_rect: Rect,
    pub(crate) dock_agent_row_hit_areas: Vec<DockAgentRowHitArea>,
    pub(crate) dock_host_row_hit_areas: Vec<DockHostRowHitArea>,
    pub dock_body_rect: Rect,
    pub scratchpad_link_rows: Vec<ScratchpadLinkRow>,
    /// Left-aligned status-bar buttons, computed once per frame so the rendered
    /// label and the clickable rect can never disagree.
    pub status_buttons: Vec<StatusButton>,
    /// Ticket and pull-request links of the focused pane, drawn after the
    /// status-row title. Clicking one opens it in the dock.
    pub(crate) status_work_links: Vec<StatusWorkLink>,
    /// The status row's right-aligned segments, fitted once per frame so the
    /// title and the links can be laid out beside what will actually be drawn.
    pub(crate) status_segments: Vec<crate::ui::status::Segment>,
    /// Where each fitted status segment is drawn, for hover tooltips.
    pub(crate) status_segment_hit_areas: Vec<(StatusSegmentKind, Rect)>,
    /// Remote host of the focused pane, resolved once during view computation.
    /// Status layout and render only read this projection.
    pub(crate) focused_remote_host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusButtonAction {
    Home,
    Work,
    BlockedFilter,
    Attention,
    Dock,
    /// Expand or collapse the usage detail in the status row.
    StatusDetail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusButton {
    pub rect: Rect,
    pub label: String,
    pub action: StatusButtonAction,
    /// Drawn with the accent instead of the dim overlay: the inbox has work in
    /// it, or the surface this button opens is already showing.
    pub active: bool,
}

/// A work link the focused pane carries, named in the status row beside the
/// title. Computed once per frame with the label it renders, so the clickable
/// rect and the drawn text can never disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StatusWorkLink {
    pub rect: Rect,
    pub label: String,
    pub object: DockObjectRef,
}

/// A work link rendered in the dock's Context tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkContextLinkRow {
    pub rect: Rect,
    pub copy_value: String,
}

/// A scratchpad link row opens its URL; the work-context rows copy instead, so
/// the two cannot share a type without making the click semantics ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScratchpadLinkRow {
    pub rect: Rect,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkLinkPickerAction {
    Open,
    Copy,
}

/// One running agent a pane's clicked text can be handed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentPickerCandidate {
    pub ws_idx: usize,
    pub pane_id: PaneId,
    /// What the sidebar calls this agent, so the picker names it the same way.
    pub label: String,
}

/// The second step of "Send to agent...": which running agent gets the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentPickerState {
    pub candidates: Vec<AgentPickerCandidate>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkLinkPickerState {
    pub candidates: Vec<crate::work_context::WorkLinkCandidate>,
    pub action: WorkLinkPickerAction,
    pub return_mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Onboarding,
    ReleaseNotes,
    ProductAnnouncement,
    Navigate,
    Prefix,
    Copy,
    Terminal,
    RenameWorkspace,
    RenameTab,
    RenamePane,
    NewLinkedWorktree,
    OpenExistingWorktree,
    ConfirmRemoveWorktree,
    Resize,
    ConfirmClose,
    ContextMenu,
    GitMenu,
    AddAction,
    Settings,
    GlobalMenu,
    KeybindHelp,
    Navigator,
    CommandPalette,
    WorkLinkPicker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddActionField {
    Name,
    Key,
    Command,
    RunOnWorktreeCreate,
    OpenInBottomPane,
}

impl AddActionField {
    pub(crate) const ALL: [Self; 5] = [
        Self::Name,
        Self::Key,
        Self::Command,
        Self::RunOnWorktreeCreate,
        Self::OpenInBottomPane,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AddActionState {
    pub(crate) name: String,
    pub(crate) key: String,
    pub(crate) command: String,
    pub(crate) run_on_worktree_create: bool,
    pub(crate) open_in_bottom_pane: bool,
    pub(crate) field: AddActionField,
    pub(crate) error: Option<String>,
}

impl Default for AddActionState {
    fn default() -> Self {
        Self {
            name: String::new(),
            key: String::new(),
            command: String::new(),
            run_on_worktree_create: false,
            open_in_bottom_pane: true,
            field: AddActionField::Name,
            error: None,
        }
    }
}

impl Mode {
    /// Classify every mode by the state owner that may store it. Keep this
    /// match exhaustive so a new mode cannot silently acquire an owner.
    fn owner(self) -> ModeOwner {
        match self {
            Self::RenameWorkspace => ModeOwner::Client(ClientOverlay::RenameWorkspace),
            Self::RenameTab => ModeOwner::Client(ClientOverlay::RenameTab),
            Self::RenamePane => ModeOwner::Client(ClientOverlay::RenamePane),
            Self::NewLinkedWorktree => ModeOwner::Client(ClientOverlay::NewLinkedWorktree),
            Self::OpenExistingWorktree => ModeOwner::Client(ClientOverlay::OpenExistingWorktree),
            Self::ConfirmRemoveWorktree => ModeOwner::Client(ClientOverlay::ConfirmRemoveWorktree),
            Self::ConfirmClose => ModeOwner::Client(ClientOverlay::ConfirmClose),
            Self::ContextMenu => ModeOwner::Client(ClientOverlay::ContextMenu),
            Self::Onboarding
            | Self::ReleaseNotes
            | Self::ProductAnnouncement
            | Self::Navigate
            | Self::Prefix
            | Self::Copy
            | Self::Terminal
            | Self::Resize
            | Self::GitMenu
            | Self::AddAction
            | Self::Settings
            | Self::GlobalMenu
            | Self::KeybindHelp
            | Self::Navigator
            | Self::CommandPalette
            | Self::WorkLinkPicker => ModeOwner::Server,
        }
    }

    pub(crate) fn mouse_motion_changes_view(self) -> bool {
        matches!(
            self,
            Self::GlobalMenu
                | Self::ContextMenu
                | Self::GitMenu
                | Self::Navigator
                | Self::CommandPalette
        )
    }

    /// Whether keys in this mode are commands/navigation (an ASCII input source is wanted) rather
    /// than free text. This is an explicit **allowlist** of the prefix command/navigation realm:
    /// any mode NOT listed defaults to leaving the user's IME alone (the safe default), so adding a
    /// new text-entry or overlay mode can never silently force ASCII. Used by
    /// `sync_prefix_input_source` (gated by `switch_ascii_input_source_in_prefix`) so multi-level
    /// prefix commands keep ASCII until they return to the terminal.
    ///
    /// Known limitation: the search boxes in `Navigator` and `KeybindHelp` are also held on ASCII,
    /// since this `Mode`-level predicate can't see `search_focused` (non-ASCII filtering there
    /// would need a runtime check).
    pub(crate) fn wants_ascii_input(self) -> bool {
        matches!(
            self,
            Mode::Prefix
                | Mode::Navigate
                | Mode::Navigator
                | Mode::CommandPalette
                | Mode::Copy
                | Mode::Resize
                | Mode::ConfirmClose
                | Mode::ConfirmRemoveWorktree
                | Mode::ContextMenu
                | Mode::GitMenu
                | Mode::GlobalMenu
                | Mode::KeybindHelp
                | Mode::WorkLinkPicker
        )
    }
}

/// The raw server-owned mode. The contained `Mode` stays private to this
/// module so consumers must choose between server state and client-effective
/// interaction explicitly.
pub(crate) struct ServerInteractionState {
    mode: Mode,
}

impl ServerInteractionState {
    pub(crate) fn new(mode: Mode) -> Self {
        assert!(
            matches!(mode.owner(), ModeOwner::Server),
            "client-owned overlay cannot be stored as the server mode"
        );
        Self { mode }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitAction {
    Pull,
    Commit,
    Push,
    CreatePr,
}

impl GitAction {
    pub(crate) const ALL: [Self; 4] = [Self::Pull, Self::Commit, Self::Push, Self::CreatePr];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Pull => "⇣ Pull",
            Self::Commit => "⊙ Commit",
            Self::Push => "⇡ Push",
            Self::CreatePr => "⑂ Create PR",
        }
    }

    #[cfg(test)]
    pub(crate) fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Pull => &["git", "pull", "--rebase"],
            Self::Commit => &["git", "commit"],
            Self::Push => &["git", "push"],
            Self::CreatePr => &["gh", "pr", "create", "--fill"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavigatorTarget {
    Workspace {
        ws_idx: usize,
    },
    Tab {
        ws_idx: usize,
        tab_idx: usize,
    },
    Pane {
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NavigatorRow {
    pub target: NavigatorTarget,
    pub depth: u8,
    pub label: String,
    pub meta: String,
    pub status: AgentState,
    pub attention_tier: crate::terminal::state::AttentionTier,
    pub seen: bool,
    pub stale: bool,
    pub is_current: bool,
    pub is_workspace: bool,
    pub is_tab: bool,
    pub expanded: bool,
    pub search_text: String,
    /// Whether this row itself matched the active query/state filter, as
    /// opposed to being included as ancestor context or cascaded subtree of a
    /// matching workspace or tab. Always true when no filter is active.
    pub matched: bool,
}

/// One rendered line in the navigator body. Spacer lines separate workspace
/// groups visually and are not selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorDisplayLine {
    Spacer,
    Row(usize),
}

pub(crate) fn navigator_display_lines(rows: &[NavigatorRow]) -> Vec<NavigatorDisplayLine> {
    let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));
    for (idx, row) in rows.iter().enumerate() {
        if row.is_workspace && !lines.is_empty() {
            lines.push(NavigatorDisplayLine::Spacer);
        }
        lines.push(NavigatorDisplayLine::Row(idx));
    }
    lines
}

pub(crate) fn navigator_display_index_of_row(
    lines: &[NavigatorDisplayLine],
    row_idx: usize,
) -> Option<usize> {
    lines
        .iter()
        .position(|line| *line == NavigatorDisplayLine::Row(row_idx))
}

pub(crate) fn navigator_first_row_at_or_after(
    lines: &[NavigatorDisplayLine],
    line_idx: usize,
) -> Option<usize> {
    lines.get(line_idx..)?.iter().find_map(|line| match line {
        NavigatorDisplayLine::Row(idx) => Some(*idx),
        NavigatorDisplayLine::Spacer => None,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NavigatorStateFilter {
    Blocked,
    Working,
    Idle,
    Done,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct NavigatorState {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
    pub search_focused: bool,
    pub state_filter: Option<NavigatorStateFilter>,
    pub expanded_workspaces: std::collections::HashSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CommandPaletteState {
    pub query: String,
    pub selected: usize,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeState {
    pub pane_id: PaneId,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub entry_offset_from_bottom: usize,
    pub selection: Option<CopyModeSelection>,
    pub search: CopyModeSearchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSelection {
    Character,
    Linewise { anchor_row: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeSearchDirection {
    Forward,
    Backward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CopyModeSearchPrompt {
    pub direction: CopyModeSearchDirection,
    pub query: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CopyModeSearchState {
    pub prompt: Option<CopyModeSearchPrompt>,
    pub query: String,
    pub direction: Option<CopyModeSearchDirection>,
    pub matches: Vec<crate::pane::TerminalTextMatch>,
    pub current: Option<usize>,
    pub geometry: Option<(u16, u16)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentPanelSort {
    #[default]
    Spaces,
    Priority,
}

// ---------------------------------------------------------------------------
// Settings UI state
// ---------------------------------------------------------------------------

/// Which section of the settings panel is focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Sidebar,
    Theme,
    Indicators,
    Sound,
    SidebarPanels,
    Toast,
    PaneLabels,
    Keybindings,
    Providers,
    Integrations,
    SourceControl,
    Archive,
    About,
}

impl SettingsSection {
    pub const ALL: &[Self] = &[
        Self::General,
        Self::Sidebar,
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::SidebarPanels,
        Self::Toast,
        Self::PaneLabels,
        Self::Keybindings,
        Self::Providers,
        Self::Integrations,
        Self::SourceControl,
        Self::Archive,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Sidebar => "sidebar",
            Self::Theme => "theme",
            Self::Indicators => "indicators",
            Self::Sound => "sound",
            Self::SidebarPanels => "sidebar panels",
            Self::Toast => "toasts",
            Self::PaneLabels => "pane labels",
            Self::Keybindings => "keybindings",
            Self::Providers => "providers",
            Self::Integrations => "integrations",
            Self::SourceControl => "source control",
            Self::Archive => "archive",
            Self::About => "about",
        }
    }

    /// The glyph in front of the nav row, mirroring the design's icon column.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::General => "⚙",
            Self::Sidebar => "▤",
            Self::Theme => "◐",
            Self::Indicators => "●",
            Self::Sound => "♪",
            Self::SidebarPanels => "▤",
            Self::Toast => "▣",
            Self::PaneLabels => "▭",
            Self::Keybindings => "⌨",
            Self::Providers => "⚛",
            Self::Integrations => "⊞",
            Self::SourceControl => "⑂",
            Self::Archive => "▤",
            Self::About => "ⓘ",
        }
    }

    /// Position in the unfiltered nav column.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|section| *section == self)
            .unwrap_or(0)
    }
}

/// The sections a search query leaves in the nav column, in nav order.
///
/// An empty query is not a filter, it is the whole list. A query that matches
/// nothing returns nothing, so the column shows the operator that their query
/// is the reason the list is empty rather than silently ignoring it.
pub fn settings_sections_matching(query: &str) -> Vec<SettingsSection> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return SettingsSection::ALL.to_vec();
    }
    SettingsSection::ALL
        .iter()
        .copied()
        .filter(|section| section.label().contains(&query))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarPanelSettingTarget {
    Note(String),
    NotepadTab(crate::notepad::NotepadTabTarget),
    Goals,
    Pomodoro,
    Animation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarPanelSettingItem {
    pub(crate) target: SidebarPanelSettingTarget,
    pub(crate) label: String,
    pub(crate) visible: bool,
}

pub(crate) fn settings_sidebar_panel_items(state: &AppState) -> Vec<SidebarPanelSettingItem> {
    let mut note_names = state.sidebar_note_names.clone();
    note_names.extend(state.notepad.files.iter().map(|file| file.name.clone()));
    note_names.extend(
        state
            .notepad
            .visible_tabs
            .iter()
            .filter_map(|tab| tab.strip_prefix("note:").map(str::to_string)),
    );
    let mut items = Vec::new();
    for name in note_names {
        if name.is_empty()
            || name.contains(['/', '\\'])
            || items.iter().any(|item: &SidebarPanelSettingItem| {
                matches!(&item.target, SidebarPanelSettingTarget::Note(existing) if existing == &name)
            })
        {
            continue;
        }
        let key = format!("note:{name}");
        items.push(SidebarPanelSettingItem {
            target: SidebarPanelSettingTarget::Note(name.clone()),
            label: format!("Note · {name}"),
            visible: state.notepad.visible_tabs.iter().any(|tab| tab == &key),
        });
    }
    for (target, label) in [
        (crate::notepad::NotepadTabTarget::Context, "Context tab"),
        (crate::notepad::NotepadTabTarget::Agent, "Agent tab"),
        (crate::notepad::NotepadTabTarget::Usage, "Usage tab"),
    ] {
        items.push(SidebarPanelSettingItem {
            target: SidebarPanelSettingTarget::NotepadTab(target),
            label: label.to_string(),
            visible: state.notepad.is_tab_visible(target),
        });
    }
    items.push(SidebarPanelSettingItem {
        target: SidebarPanelSettingTarget::Goals,
        label: "Goals section".to_string(),
        visible: state.goals.enabled,
    });
    items.push(SidebarPanelSettingItem {
        target: SidebarPanelSettingTarget::Pomodoro,
        label: "Pomodoro widget".to_string(),
        visible: state.pomodoro.sidebar_visible,
    });
    items.push(SidebarPanelSettingItem {
        target: SidebarPanelSettingTarget::Animation,
        label: "Animation".to_string(),
        visible: state.hyperspace.enabled,
    });
    items
}

pub(crate) fn sidebar_panel_note_names(
    configured_files: &[String],
    pomodoro_log_file: &str,
) -> Vec<String> {
    let mut names = vec!["notes".to_string()];
    for configured in configured_files {
        let name = configured.trim().trim_end_matches(".md");
        if !name.is_empty()
            && !name.contains(['/', '\\'])
            && !names.iter().any(|existing| existing == name)
        {
            names.push(name.to_string());
        }
    }
    let log_name = pomodoro_log_file.trim().trim_end_matches(".md");
    if !log_name.is_empty()
        && !log_name.contains(['/', '\\'])
        && !names.iter().any(|existing| existing == log_name)
    {
        names.push(log_name.to_string());
    }
    names
}

/// All built-in theme names in display order.
pub const THEME_NAMES: &[&str] = crate::config::THEME_NAMES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuListState {
    pub highlighted: usize,
}

impl MenuListState {
    pub fn new(highlighted: usize) -> Self {
        Self { highlighted }
    }

    pub fn move_prev(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.highlighted = (self.highlighted + 1).min(item_count - 1);
        }
    }

    pub fn hover(&mut self, idx: Option<usize>) {
        if let Some(idx) = idx {
            self.highlighted = idx;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionListState {
    pub selected: usize,
}

impl SelectionListState {
    pub fn new(selected: usize) -> Self {
        Self { selected }
    }

    pub fn move_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn move_next(&mut self, item_count: usize) {
        if item_count > 0 {
            self.selected = (self.selected + 1).min(item_count - 1);
        }
    }

    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
    }
}

#[derive(Debug, Clone)]
pub struct ThemeRuntimeConfig {
    pub manual_name: String,
    pub dark_name: String,
    pub light_name: String,
    pub auto_switch: bool,
    pub host_appearance: crate::config::HostAppearanceOverride,
    /// The appearance a runtime source asserted (the theme API, the attach-time
    /// environment relay, or the theme toggle), kept apart from the config
    /// value so a config reload can restore it. Where OSC 11 never answers,
    /// this is the only appearance signal the server has.
    pub runtime_host_appearance: Option<crate::config::HostAppearanceOverride>,
    pub custom: Option<crate::config::CustomThemeColors>,
    pub legacy_accent: Option<String>,
}

pub struct SettingsState {
    /// Which section tab is active.
    pub section: SettingsSection,
    /// Selected item index within the current section.
    pub list: SelectionListState,
    /// The palette before opening settings (for cancel/restore).
    pub original_palette: Option<Palette>,
    /// The theme name before opening settings.
    pub original_theme: Option<String>,
    /// The archive row's delete has been pressed once and waits for the
    /// confirming second press. Only armed while `ui.confirm_close` is on.
    pub archive_delete_armed: bool,
    /// The section-nav filter query. TUI-only: nothing outside the settings
    /// screen reads it and it is never persisted.
    pub search: String,
    /// Typed characters go to the filter line rather than the section list.
    pub search_active: bool,
    /// The keybindings row whose chord is being captured, if any. TUI-only:
    /// capture is a settings-screen interaction, not a session fact.
    pub keybind_capture: Option<KeybindCapture>,
}

/// A keybindings row waiting for the operator to press the chord they want.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindCapture {
    /// Index into `settings_keybinding_rows`.
    pub row: usize,
    /// The refusal shown inline under the row, from the same validator the
    /// add-action modal uses.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceDropTarget {
    Before(usize),
    End,
}

pub(crate) enum DragTarget {
    WorkspaceReorder {
        source_id: crate::app::InputSourceId,
        source_ws_idx: usize,
        drop_target: Option<WorkspaceDropTarget>,
    },
    TabReorder {
        source_id: crate::app::InputSourceId,
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: Option<usize>,
    },
    PodAssign {
        source_id: crate::app::InputSourceId,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        public_pane_id: String,
        hover: Option<crate::groups::GroupId>,
    },
    WorkspaceListScrollbar {
        grab_row_offset: u16,
    },
    PaneSplit {
        path: Vec<bool>,
        direction: Direction,
        area: Rect,
        grab_offset: u16,
    },
    PaneScrollbar {
        pane_id: crate::layout::PaneId,
        grab_row_offset: u16,
    },
    ReleaseNotesScrollbar {
        grab_row_offset: u16,
    },
    ProductAnnouncementScrollbar {
        grab_row_offset: u16,
    },
    KeybindHelpScrollbar {
        grab_row_offset: u16,
    },
    SidebarDivider,
    DockDivider,
    /// The notepad strip's top edge, dragged vertically to resize the panel.
    NotepadDivider,
}

/// Active mouse drag on a split border or sidebar divider.
pub(crate) struct DragState {
    pub target: DragTarget,
}

pub(crate) struct WorkspacePressState {
    pub ws_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct TabPressState {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct AgentPressState {
    pub ws_idx: usize,
    pub pane_id: crate::layout::PaneId,
    pub public_pane_id: String,
    pub start_col: u16,
    pub start_row: u16,
}

pub(crate) struct RemoteAgentPressState {
    pub agent_ref: crate::api::schema::AgentRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextMenuKind {
    Workspace {
        workspace_id: String,
        /// Render-time cache only; `workspace_id` is authoritative.
        ws_idx: usize,
    },
    GitWorkspace {
        workspace_id: String,
        /// Render-time cache only; `workspace_id` is authoritative.
        ws_idx: usize,
        is_linked_worktree: bool,
        has_worktree_children: bool,
        collapsed: bool,
    },
    Tab {
        workspace_id: String,
        tab_id: String,
        /// Render-time caches only; stable IDs are authoritative.
        ws_idx: usize,
        tab_idx: usize,
        /// Exact local pane represented by the sidebar row. Top tab chrome
        /// leaves this empty because it represents the whole tab.
        settle_pane_id: Option<PaneId>,
        /// Exact local pane represented by the sidebar row.
        snooze_target: Option<PaneId>,
        /// Snapshot of the tab's star at open time, so the entry can read
        /// "Star" or "Unstar" without the menu reaching back into state.
        starred: bool,
        /// Snapshot of the tab's subgroup membership at open time, so the menu
        /// offers "Remove from subgroup" exactly when there is one to remove.
        has_subgroup: bool,
    },
    Pane {
        workspace_id: String,
        tab_id: String,
        /// Render-time caches only; stable IDs are authoritative.
        ws_idx: usize,
        tab_idx: usize,
        pane_id: PaneId,
        source_pane_id: Option<PaneId>,
        has_manual_label: bool,
        right_click_passthrough: bool,
        /// Work link under the click, when the clicked cell carries one that is
        /// not already bound to every pane of this window.
        linkable_work_link: Option<PaneMenuWorkLinkAction>,
        /// Whatever link the clicked cell carries, matched to no pattern. A
        /// pull request and a paste-bin URL are equally copyable.
        link: Option<String>,
        /// The path the clicked cell names, when it names one that exists.
        path: Option<PaneMenuPath>,
        /// Editors on this host, in menu order. Resolved when the menu opens so
        /// the entries never promise a command that is not installed.
        open_with: Vec<PaneOpenWith>,
        /// The text the clicked cell offers an agent: the pane's selection when
        /// there is one, otherwise the link or path under the cursor.
        send_text: Option<String>,
        /// Whether any agent other than this pane is running, so the menu only
        /// offers a picker that would have something to pick.
        has_agent_targets: bool,
    },
    RemoteAgent {
        agent_ref: crate::api::schema::AgentRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RenameTarget {
    Workspace {
        workspace_id: String,
    },
    Tab {
        workspace_id: String,
        tab_id: String,
    },
    Pane {
        workspace_id: String,
        pane_id: PaneId,
    },
    Pod {
        record: crate::groups::GroupRecord,
    },
}

/// Label of the pane menu entry that opens the clicked link in a browser.
pub const OPEN_LINK_ITEM: &str = "Open link";
/// Label of the pane menu entry that copies the clicked link.
pub const COPY_LINK_ITEM: &str = "Copy link";
/// Label of the pane menu entry that spawns an agent on the clicked text.
pub const SEND_TO_NEW_AGENT_ITEM: &str = "Send to new agent";
/// Label of the pane menu entry that hands the clicked text to a running
/// agent. The trailing ellipsis is the promise of a second step.
pub const SEND_TO_EXISTING_AGENT_ITEM: &str = "Send to agent...";

/// A path a pane printed, resolved against that pane's cwd and known to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneMenuPath {
    pub path: std::path::PathBuf,
    /// The `:line` a compiler or `rg` put after the path, when there was one.
    pub line: Option<u32>,
    pub is_dir: bool,
}

impl PaneMenuPath {
    /// The folder this path belongs to: itself when it is one, its parent
    /// otherwise. A path with no parent has no folder worth opening.
    pub fn directory(&self) -> Option<&std::path::Path> {
        if self.is_dir {
            Some(self.path.as_path())
        } else {
            // A bare filename's parent is the empty path, which is not a
            // folder anything can be opened in.
            self.path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
        }
    }
}

/// An editor the pane menu can hand a path to.
///
/// Each one gets two entries, because opening the file and opening the folder
/// around it are different jobs: one is "read this line", the other is "work
/// here".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOpenWith {
    Nvim,
    Vellum,
    Zed,
}

impl PaneOpenWith {
    pub const ALL: [Self; 3] = [Self::Nvim, Self::Vellum, Self::Zed];

    pub fn file_item(self) -> &'static str {
        match self {
            Self::Nvim => "Open file in nvim",
            Self::Vellum => "Open file in Vellum",
            Self::Zed => "Open file in Zed",
        }
    }

    pub fn directory_item(self) -> &'static str {
        match self {
            Self::Nvim => "Open folder in nvim",
            Self::Vellum => "Open folder in Vellum",
            Self::Zed => "Open folder in Zed",
        }
    }

    /// The command that has to exist before the entry is worth offering. Zed
    /// runs on the Mac, so what this host needs is the wrapper that sends it
    /// there, not Zed itself.
    pub fn command(self) -> &'static str {
        match self {
            Self::Nvim => "nvim",
            Self::Vellum => "vellum",
            Self::Zed => "zedr",
        }
    }
}
/// Labels of the session-star entries in the tab context menu.
pub const STAR_ITEM: &str = "Star";
pub const UNSTAR_ITEM: &str = "Unstar";
/// Labels of the sidebar-subgroup entries in the tab context menu.
pub const MOVE_TO_SUBGROUP_ITEM: &str = "Move to subgroup…";
pub const REMOVE_FROM_SUBGROUP_ITEM: &str = "Remove from subgroup";
pub const SETTLE_ITEM: &str = "Settle";
pub const UNSETTLE_ITEM: &str = "Unsettle";
pub const SNOOZE_ITEM: &str = "Snooze ▸";
pub const SET_TIME_ITEM: &str = "Set time…";
pub const CHANGE_TIME_ITEM: &str = "Change time…";
pub const UNSNOOZE_ITEM: &str = "Unsnooze (unset time)";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarSnoozePreset {
    Duration(u64),
    TomorrowMorning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarSnoozeMenuAction {
    Preset(SidebarSnoozePreset),
    SetTime,
    Unsnooze,
}

pub(crate) const SNOOZE_MENU_ITEMS: [(&str, SidebarSnoozeMenuAction); 5] = [
    (
        "Snooze for 15 minutes",
        SidebarSnoozeMenuAction::Preset(SidebarSnoozePreset::Duration(15 * 60)),
    ),
    (
        "Snooze for 1 hour",
        SidebarSnoozeMenuAction::Preset(SidebarSnoozePreset::Duration(60 * 60)),
    ),
    (
        "Snooze for 4 hours",
        SidebarSnoozeMenuAction::Preset(SidebarSnoozePreset::Duration(4 * 60 * 60)),
    ),
    (
        "Snooze until tomorrow at 09:00",
        SidebarSnoozeMenuAction::Preset(SidebarSnoozePreset::TomorrowMorning),
    ),
    ("Set time…", SidebarSnoozeMenuAction::SetTime),
];

pub(crate) const SNOOZED_MENU_ITEMS: [(&str, SidebarSnoozeMenuAction); 2] = [
    (UNSNOOZE_ITEM, SidebarSnoozeMenuAction::Unsnooze),
    (CHANGE_TIME_ITEM, SidebarSnoozeMenuAction::SetTime),
];

pub(crate) fn sidebar_snooze_menu_items(
    snoozed: bool,
) -> &'static [(&'static str, SidebarSnoozeMenuAction)] {
    if snoozed {
        &SNOOZED_MENU_ITEMS
    } else {
        &SNOOZE_MENU_ITEMS
    }
}

/// Label of the pane menu entry that binds the clicked pull request to the window.
pub const LINK_PR_TO_WINDOW_ITEM: &str = "Link PR to this window";
/// Label of the pane menu entry that binds the clicked ticket to the window.
pub const LINK_TICKET_TO_WINDOW_ITEM: &str = "Link ticket to this window";
/// Label of the pane menu entry that drops the clicked pull request again.
pub const UNLINK_PR_FROM_WINDOW_ITEM: &str = "Unlink PR from this window";
/// Label of the pane menu entry that drops the clicked ticket again.
pub const UNLINK_TICKET_FROM_WINDOW_ITEM: &str = "Unlink ticket from this window";

/// Work item a right-clicked link resolves to, ready to bind to a window.
///
/// Windows, not panes, are what the sidebar groups, so a link click binds every
/// pane of the clicked tab rather than only the pane under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneMenuWorkLink {
    PullRequest(String),
    Ticket(String),
}

impl PaneMenuWorkLink {
    /// The manual patch that binds this link, leaving every other field alone.
    pub fn patch(&self) -> crate::work_context::PaneWorkContextPatch {
        match self {
            Self::PullRequest(url) => crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec![url.clone()]),
                ..Default::default()
            },
            Self::Ticket(id) => crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec![id.clone()]),
                ..Default::default()
            },
        }
    }

    /// Short human label for this link, as it appears in the link toast.
    pub fn short_label(&self) -> String {
        match self {
            Self::PullRequest(url) => url
                .rsplit('/')
                .next()
                .filter(|segment| !segment.is_empty())
                .map(|number| format!("#{number}"))
                .unwrap_or_else(|| url.clone()),
            Self::Ticket(id) => id.clone(),
        }
    }

    /// The manual patch that drops this link, leaving every other field alone.
    ///
    /// Clearing the field is what a declaration can express: a pull request the
    /// hook or git tier observed is not the human's to remove, which is why the
    /// menu only offers this for a link the window carries manually.
    pub fn clear_patch(&self) -> crate::work_context::PaneWorkContextPatch {
        let field = match self {
            Self::PullRequest(_) => crate::work_context::PaneWorkContextField::PrUrls,
            Self::Ticket(_) => crate::work_context::PaneWorkContextField::TicketIds,
        };
        crate::work_context::PaneWorkContextPatch {
            clear_fields: vec![field],
            ..Default::default()
        }
    }

    /// Whether a work context already carries this exact binding.
    pub fn is_bound_in(&self, context: &crate::work_context::PaneWorkContext) -> bool {
        match self {
            Self::PullRequest(url) => context
                .pr_urls
                .iter()
                .any(|bound| bound.eq_ignore_ascii_case(url)),
            Self::Ticket(id) => context
                .ticket_ids
                .iter()
                .any(|bound| bound.eq_ignore_ascii_case(id)),
        }
    }
}

/// A work link click, resolved to the one action that would change something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneMenuWorkLinkAction {
    pub link: PaneMenuWorkLink,
    /// Whether activating it removes the manual binding instead of adding one.
    pub unlink: bool,
}

impl PaneMenuWorkLinkAction {
    pub fn link(link: PaneMenuWorkLink) -> Self {
        Self {
            link,
            unlink: false,
        }
    }

    pub fn unlink(link: PaneMenuWorkLink) -> Self {
        Self { link, unlink: true }
    }

    pub fn menu_item(&self) -> &'static str {
        match (&self.link, self.unlink) {
            (PaneMenuWorkLink::PullRequest(_), false) => LINK_PR_TO_WINDOW_ITEM,
            (PaneMenuWorkLink::PullRequest(_), true) => UNLINK_PR_FROM_WINDOW_ITEM,
            (PaneMenuWorkLink::Ticket(_), false) => LINK_TICKET_TO_WINDOW_ITEM,
            (PaneMenuWorkLink::Ticket(_), true) => UNLINK_TICKET_FROM_WINDOW_ITEM,
        }
    }

    pub fn patch(&self) -> crate::work_context::PaneWorkContextPatch {
        if self.unlink {
            self.link.clear_patch()
        } else {
            self.link.patch()
        }
    }

    pub fn toast_title(&self) -> String {
        let verb = if self.unlink { "unlinked" } else { "linked" };
        format!("{verb} {}", self.link.short_label())
    }
}

/// Right-click context menu state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuState {
    pub kind: ContextMenuKind,
    pub x: u16,
    pub y: u16,
    pub selected: ContextMenuAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMenuAction {
    RenameWorkspace,
    CloseWorkspace,
    NewWorktree,
    OpenWorktree,
    DeleteWorktree,
    CollapseWorkspace,
    ExpandWorkspace,
    NewTab,
    RenameTab,
    StarTab,
    UnstarTab,
    MoveToSubgroup,
    RemoveFromSubgroup,
    Snooze,
    Unsnooze,
    SetTime,
    ChangeTime,
    Settle,
    Unsettle,
    CloseTab,
    RenamePane,
    WorkLink,
    OpenLink,
    CopyLink,
    OpenFile(PaneOpenWith),
    OpenFolder(PaneOpenWith),
    SendToNewAgent,
    SendToExistingAgent,
    ClearPaneName,
    SwapPane,
    SplitRight,
    SplitDown,
    Zoom,
    UseHerdrRightClick,
    SendRightClicksToPane,
    ClosePane,
}

impl ContextMenuAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::RenameWorkspace | Self::RenameTab => "Rename",
            Self::CloseWorkspace | Self::CloseTab => "Close",
            Self::NewWorktree => "New worktree",
            Self::OpenWorktree => "Open worktree...",
            Self::DeleteWorktree => "Delete worktree checkout...",
            Self::CollapseWorkspace => "Collapse",
            Self::ExpandWorkspace => "Expand",
            Self::NewTab => "New tab",
            Self::StarTab => STAR_ITEM,
            Self::UnstarTab => UNSTAR_ITEM,
            Self::MoveToSubgroup => MOVE_TO_SUBGROUP_ITEM,
            Self::RemoveFromSubgroup => REMOVE_FROM_SUBGROUP_ITEM,
            Self::Snooze => SNOOZE_ITEM,
            Self::Unsnooze => UNSNOOZE_ITEM,
            Self::SetTime => SET_TIME_ITEM,
            Self::ChangeTime => CHANGE_TIME_ITEM,
            Self::Settle => SETTLE_ITEM,
            Self::Unsettle => UNSETTLE_ITEM,
            Self::RenamePane => "Rename pane",
            Self::WorkLink => "work link",
            Self::OpenLink => OPEN_LINK_ITEM,
            Self::CopyLink => COPY_LINK_ITEM,
            Self::OpenFile(target) => target.file_item(),
            Self::OpenFolder(target) => target.directory_item(),
            Self::SendToNewAgent => SEND_TO_NEW_AGENT_ITEM,
            Self::SendToExistingAgent => SEND_TO_EXISTING_AGENT_ITEM,
            Self::ClearPaneName => "Clear pane name",
            Self::SwapPane => "Swap with focused pane",
            Self::SplitRight => "Split right",
            Self::SplitDown => "Split down",
            Self::Zoom => "Zoom",
            Self::UseHerdrRightClick => "Use Herdr right-click menu",
            Self::SendRightClicksToPane => "Send right-clicks to pane",
            Self::ClosePane => "Close pane",
        }
    }
}

impl ContextMenuState {
    pub(crate) fn label_for_action(&self, action: ContextMenuAction) -> &'static str {
        match (action, &self.kind) {
            (
                ContextMenuAction::CloseWorkspace,
                ContextMenuKind::GitWorkspace {
                    is_linked_worktree: false,
                    has_worktree_children: true,
                    ..
                },
            ) => "Close group",
            (
                ContextMenuAction::WorkLink,
                ContextMenuKind::Pane {
                    linkable_work_link: Some(action),
                    ..
                },
            ) => action.menu_item(),
            (action, _) => action.label(),
        }
    }

    #[cfg(test)]
    pub fn items(&self) -> Vec<&'static str> {
        self.items_for_pane_state(false, true, true)
    }

    pub(crate) fn actions_for_pane_state(
        &self,
        pane_snoozed: bool,
        pane_settleable: bool,
        pane_snoozeable: bool,
    ) -> Vec<ContextMenuAction> {
        match &self.kind {
            ContextMenuKind::Workspace { .. } => vec![
                ContextMenuAction::RenameWorkspace,
                ContextMenuAction::CloseWorkspace,
            ],
            ContextMenuKind::RemoteAgent { .. } => vec![ContextMenuAction::Unsettle],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => vec![
                ContextMenuAction::RenameWorkspace,
                ContextMenuAction::CloseWorkspace,
                ContextMenuAction::NewWorktree,
                ContextMenuAction::OpenWorktree,
            ],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: true,
                ..
            } => vec![
                ContextMenuAction::RenameWorkspace,
                ContextMenuAction::CloseWorkspace,
                ContextMenuAction::DeleteWorktree,
            ],
            ContextMenuKind::GitWorkspace {
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed,
                ..
            } => vec![
                ContextMenuAction::RenameWorkspace,
                ContextMenuAction::CloseWorkspace,
                ContextMenuAction::NewWorktree,
                ContextMenuAction::OpenWorktree,
                if *collapsed {
                    ContextMenuAction::ExpandWorkspace
                } else {
                    ContextMenuAction::CollapseWorkspace
                },
            ],
            ContextMenuKind::Tab {
                starred,
                has_subgroup,
                settle_pane_id,
                snooze_target,
                ..
            } => {
                let mut items = vec![
                    ContextMenuAction::NewTab,
                    ContextMenuAction::RenameTab,
                    if *starred {
                        ContextMenuAction::UnstarTab
                    } else {
                        ContextMenuAction::StarTab
                    },
                    ContextMenuAction::MoveToSubgroup,
                ];
                if *has_subgroup {
                    items.push(ContextMenuAction::RemoveFromSubgroup);
                }
                if snooze_target.is_some() && (pane_snoozed || pane_snoozeable) {
                    if pane_snoozed {
                        items.extend([ContextMenuAction::Unsnooze, ContextMenuAction::ChangeTime]);
                    } else {
                        items.extend([ContextMenuAction::Snooze, ContextMenuAction::SetTime]);
                    }
                }
                if settle_pane_id.is_some() && pane_settleable {
                    items.push(ContextMenuAction::Settle);
                }
                items.push(ContextMenuAction::CloseTab);
                items
            }
            ContextMenuKind::Pane {
                source_pane_id,
                has_manual_label,
                right_click_passthrough,
                linkable_work_link,
                link,
                path,
                open_with,
                send_text,
                has_agent_targets,
                ..
            } => {
                let mut items = vec![ContextMenuAction::RenamePane];
                if pane_snoozed {
                    items.extend([ContextMenuAction::Unsnooze, ContextMenuAction::ChangeTime]);
                } else if pane_snoozeable {
                    items.extend([ContextMenuAction::Snooze, ContextMenuAction::SetTime]);
                }
                if let Some(action) = linkable_work_link {
                    let _ = action;
                    items.push(ContextMenuAction::WorkLink);
                }
                if let Some(link) = link {
                    if crate::app::actions::safe_web_url(link).is_some() {
                        items.push(ContextMenuAction::OpenLink);
                    }
                    items.push(ContextMenuAction::CopyLink);
                }
                if let Some(path) = path {
                    for target in open_with {
                        if !path.is_dir {
                            items.push(ContextMenuAction::OpenFile(*target));
                        }
                        if path.directory().is_some() {
                            items.push(ContextMenuAction::OpenFolder(*target));
                        }
                    }
                }
                if send_text.is_some() {
                    items.push(ContextMenuAction::SendToNewAgent);
                    if *has_agent_targets {
                        items.push(ContextMenuAction::SendToExistingAgent);
                    }
                }
                if *has_manual_label {
                    items.push(ContextMenuAction::ClearPaneName);
                }
                if source_pane_id.is_some() {
                    items.push(ContextMenuAction::SwapPane);
                }
                items.extend([
                    ContextMenuAction::SplitRight,
                    ContextMenuAction::SplitDown,
                    ContextMenuAction::Zoom,
                ]);
                items.push(if *right_click_passthrough {
                    ContextMenuAction::UseHerdrRightClick
                } else {
                    ContextMenuAction::SendRightClicksToPane
                });
                items.push(ContextMenuAction::ClosePane);
                items
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn items_for_pane_state(
        &self,
        pane_snoozed: bool,
        pane_settleable: bool,
        pane_snoozeable: bool,
    ) -> Vec<&'static str> {
        let actions = self.actions_for_pane_state(pane_snoozed, pane_settleable, pane_snoozeable);
        actions
            .into_iter()
            .map(|action| self.label_for_action(action))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    NeedsAttention,
    Finished,
    UpdateInstalled,
    WorkLinked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastNotification {
    pub kind: ToastKind,
    pub title: String,
    pub context: String,
    pub position: Option<crate::config::ToastHerdrPosition>,
    pub target: Option<ToastTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAgentNotification {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub state: AgentState,
    pub deadline: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNotificationDelivery {
    pub pane_id: PaneId,
    pub workspace_id: String,
    pub agent_label: String,
    pub known_agent: Option<crate::detect::Agent>,
    pub kind: ToastKind,
    pub toast: Option<ToastNotification>,
    pub client_notification: Option<ToastNotification>,
    pub sound: Option<crate::sound::Sound>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFeedback {
    pub message: String,
}

pub struct ReleaseNotesState {
    pub version: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

pub struct ProductAnnouncementState {
    pub version: String,
    pub id: String,
    pub title: String,
    pub body: String,
    pub scroll: u16,
    pub preview: bool,
}

#[derive(Default)]
pub struct KeybindHelpState {
    pub scroll: u16,
    pub query: String,
    pub search_focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarWidthSource {
    ConfigDefault,
    Persisted,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PaneFocusTarget {
    pub workspace_id: String,
    pub pane_id: PaneId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneSettlementChange {
    pub(crate) workspace_id: String,
    pub(crate) pane_id: PaneId,
    pub(crate) settled_at: Option<u64>,
    pub(crate) lock_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneSnoozeChange {
    pub(crate) workspace_id: String,
    pub(crate) pane_id: PaneId,
    pub(crate) deadline: Option<u64>,
    pub(crate) reason: Option<crate::api::schema::PaneUnsnoozeReason>,
}

/// Renderer selected for the full terminal area before overlays are applied.
pub(crate) enum TerminalAreaSurface<'a> {
    Board,
    EditorPreview,
    Symphony(&'a SymphonyDetail),
    LoopRunHistory(&'a LoopRunHistoryDetail),
    AloopRunLog(&'a AloopRunDetail),
    Usage,
    Work,
    DockObjectPreview,
    Home,
    Inbox(&'a crate::app::inbox::InboxState),
    Tab,
    Empty,
}

/// All application state — pure data, no channels or async runtime.
/// Testable without PTYs or a tokio runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabBarStatusSegment {
    Zoom,
    Text(Option<String>),
}

pub struct AppState {
    /// Clock snapshot captured during `compute_view`; renderers consume it
    /// without reading the clock or mutating shared runtime state.
    pub(crate) view_observed_at: Instant,
    /// Wall-clock companion to `view_observed_at`, used for persisted UNIX
    /// deadlines without reading the clock in row construction or rendering.
    pub(crate) view_observed_unix_s: u64,
    /// Server-owned observation facts loaded from the fleet receipt and loop
    /// registry files. The UI detail projection reads these values without
    /// touching the filesystem during render.
    pub(crate) loop_run_history: crate::loop_runs::RunHistory,
    pub(crate) loop_registry: crate::loop_runs::LoopRegistry,
    pub(crate) loop_run_history_detail: Option<LoopRunHistoryDetail>,
    /// Full-screen log of one aloop run selected in the sidebar (MAT-159
    /// AC7). TUI presentation state: the run record itself is server data in
    /// the fleet snapshot.
    pub(crate) aloop_run_detail: Option<AloopRunDetail>,
    pub(crate) symphony_snapshot: crate::symphony::Snapshot,
    /// Server-owned fleet inventory, refreshed off the render thread.
    pub(crate) fleet_snapshot: crate::fleet::Snapshot,
    /// Server-owned snapshot of this authority's groups and memberships.
    /// The runtime refreshes it at group lifecycle boundaries so pure UI
    /// projections do not read the durable group store.
    pub(crate) local_group_snapshot: Option<crate::groups::GroupAuthoritySnapshot>,
    /// This server's configured component in cross-host agent references.
    pub(crate) agent_host_name: String,
    /// Uppercase local host label cached for TUI chrome presentation.
    pub(crate) chrome_host_label: String,
    /// Server-owned day items. Columns and stale flags are derived from this
    /// durable membership plus current pane facts and are never stored here.
    pub(crate) day_board: crate::day::DayBoard,
    pub(crate) day_stale_after: Duration,
    /// Client presentation and a snapshot of the server-owned weekly note.
    pub(crate) board_view: Option<crate::board::BoardView>,
    /// Board snapshot retained while a linked terminal has focus.
    pub(crate) board_return: Option<crate::board::BoardView>,
    /// Local row identity materialized when the sidebar projection changes.
    pub(crate) local_agent_panel_identities:
        std::collections::HashMap<PaneId, crate::ui::AgentPanelLocalIdentity>,
    /// TUI projection materialized only when the fleet snapshot changes.
    pub(crate) remote_agent_panel_entries: Vec<std::sync::Arc<crate::ui::RemoteAgentPanelEntry>>,
    /// Producer-derived aloop rows are immutable between fleet refreshes.
    /// Keeping the projection here prevents layout and render passes from
    /// repeating the nested run/finding/stable-id scan.
    pub(crate) aloop_projection: Option<std::sync::Arc<crate::aloop::SectionProjection>>,
    /// Local panes backed by remote-focus operations. Agent identity remains
    /// owned by `RemoteFocusOperations`; this marker only hides proxy chrome.
    pub(crate) remote_focus_proxy_panes: std::collections::HashSet<crate::layout::PaneId>,
    /// Selected remote row for the client presentation currently being
    /// handled or rendered.
    pub(crate) sidebar_selected_remote_agent: Option<crate::api::schema::AgentRef>,
    pub(crate) symphony_detail: Option<SymphonyDetail>,
    /// Which job the dock's Symphony surface is bound to. Client presentation
    /// state: the runtime knows nothing about which panel is open.
    pub(crate) dock_symphony: Option<SymphonyDockSelection>,
    /// Open work projection view. `Some` means the view owns the screen and the
    /// keyboard, like the Symphony and loop-history details above it.
    pub(crate) work_view: Option<WorkViewState>,
    /// Defaults owned by the TUI client.
    pub(crate) linear_default_layout: LinearViewLayout,
    /// Hover state is presentation-only and never enters the session protocol.
    pub(crate) hovered_control: Option<ControlId>,
    pub(crate) hover_started_at: Option<Instant>,
    pub(crate) hover_tooltip_visible: bool,
    /// Client-local historical usage view and its scan result.
    pub(crate) usage_view: Option<UsageViewState>,
    /// Cached local usage loaded before the first background rescan.
    pub(crate) usage_snapshot: Option<crate::provider_usage::UsageSnapshot>,
    /// Local list-price table used only by the usage presentation.
    pub(crate) usage_pricing: crate::config::UsageConfig,
    /// Set by client-local input and drained into the background scan job.
    pub(crate) request_usage_scan: bool,
    /// Open inbox cursor. `Some` means the inbox overlay owns the screen and the
    /// keyboard, exactly like the Symphony and loop-history details above it.
    pub(crate) inbox: Option<crate::app::inbox::InboxState>,
    /// Open home view. `Some` means home owns the screen, the same way `inbox`
    /// does; the two are mutually exclusive because each wants the whole frame.
    pub(crate) home: Option<crate::app::home::HomeState>,
    /// Client-local provider choices retained when Home closes.
    pub(crate) home_agent_choices: Vec<crate::app::home::HomeAgentChoice>,
    /// Provider choices resolved outside `HomeState`, ready for the next Home open.
    pub(crate) home_catalog: crate::app::home_catalog::HomeCatalog,
    /// Launch lanes resolved from `[[launch_profiles]]`, refreshed on config
    /// reload. Shared session fact rather than presentation state: it decides
    /// what a dispatch actually runs.
    pub(crate) launch_profiles: Vec<crate::app::launch_profiles::LaunchProfile>,
    /// Checkout groups resolved from `[[projects]]`, scanned once at config
    /// time. Never rescanned from the render path: the scan reads directories.
    pub(crate) projects: Vec<crate::app::projects::Project>,
    /// Machines the composer can dispatch to, resolved from the configured
    /// fleet. Shared runtime fact: it decides where a launch actually runs.
    pub(crate) machines: Vec<crate::app::machines::Machine>,
    /// Ref snapshots are TUI-only picker data, keyed by the repository's common root.
    pub(crate) home_ref_cache:
        std::collections::HashMap<std::path::PathBuf, crate::app::home_refs::HomeRefCacheEntry>,
    /// Opening the ref picker asks the runtime layer to refresh this repository.
    pub(crate) request_home_ref_refresh: Option<std::path::PathBuf>,
    /// A settings section that needs the tool probes was entered.
    /// Drained by `App::start_requested_tool_probes`, which owns the
    /// thread the pure state layer cannot spawn.
    pub(crate) request_tool_probes: bool,
    /// Unsent text typed by a human in each pane. Home replies must not touch a
    /// pane while this draft exists because the terminal owns that edit buffer.
    pub(crate) pending_human_drafts: std::collections::HashMap<PaneId, String>,
    /// Server-owned native metric snapshot consumed by pure rendering.
    pub(crate) status_metrics: Option<crate::platform::status_metrics::StatusMetricsSnapshot>,
    pub(crate) status_git_cwd: Option<std::path::PathBuf>,
    pub(crate) status_git_branch: Option<String>,
    pub(crate) status_git_ahead_behind: Option<(usize, usize)>,
    /// Runtime-resolved cwd of the focused pane, projected from the same
    /// source the Git refresh uses so rendering never re-derives a weaker one.
    pub(crate) status_focused_cwd: Option<std::path::PathBuf>,
    pub(crate) status_focus_projection_initialized: bool,
    /// Git root observed for a pane cwd, keyed by that cwd, or `None` when the
    /// cwd is not inside a repository. Filled by the background Git work
    /// context refresh so pure rendering can answer "is this pane in a repo?"
    /// without touching the filesystem.
    pub(crate) git_root_for_cwd:
        std::collections::HashMap<std::path::PathBuf, Option<std::path::PathBuf>>,
    /// Vim-family command resolved outside the pure render path.
    pub(crate) repo_editor_argv: Option<Vec<String>>,
    /// Pane whose most recent mouse event was forwarded to its terminal.
    pub(crate) forwarded_pane_input: Option<PaneId>,
    /// Whether the full-width top status row is enabled by configuration.
    pub(crate) status_bar_enabled: bool,
    /// Wall clock at the last background refresh, so reset countdowns are
    /// computed from a server-owned instant instead of inside rendering.
    pub(crate) status_now_unix: Option<i64>,
    /// Account-level provider quota, refreshed off the render thread.
    pub(crate) provider_usage: crate::provider_usage::ProviderUsageSnapshot,
    /// Reachability of the wider internet, as folded from periodic probes.
    pub(crate) connectivity: crate::connectivity::Connectivity,
    /// Expanded status bar: percentages and reset times instead of bars alone.
    pub(crate) status_bar_expanded: bool,
    /// Show per-device fleet dots after `fleet:` in the status row.
    pub(crate) fleet_status: bool,
    /// Whether the disk segment was visible last frame, which is what gives the
    /// show/hide threshold its hysteresis.
    pub(crate) status_disk_visible: bool,
    pub(crate) full_lifecycle_hook_authority_timeout: std::time::Duration,
    pub(crate) hide_done_after: std::time::Duration,
    pub(crate) reap_done_after: std::time::Duration,
    pub(crate) reap_done_panes: bool,
    pub(crate) settle_after: std::time::Duration,
    /// Grace window a finished work reading must survive before the pane
    /// settles (`session.settle_finished_after_minutes`).
    pub(crate) settle_finished_after: std::time::Duration,
    /// How long a Done pane stays quiet before `auto_settle_done` settles it
    /// (`session.settle_done_after_minutes`).
    pub(crate) settle_done_after: std::time::Duration,
    /// Delay before a settled pane becomes read-only and its agent is stopped.
    pub(crate) settled_read_only_after: std::time::Duration,
    pub terminals:
        std::collections::HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    /// Runtime-only facts reported or observed for each pane. Session snapshots
    /// deliberately omit this cache.
    pub(crate) agent_states: crate::agent_state::AgentStateStore,
    /// Terminal ids whose size is currently owned by a direct attach client.
    pub direct_attach_resize_locks: std::collections::HashSet<crate::terminal::TerminalId>,
    pub(crate) pane_id_aliases: std::collections::HashMap<u32, PaneId>,
    pub(crate) public_pane_id_aliases: std::collections::HashMap<String, PaneId>,
    pub workspaces: Vec<Workspace>,
    pub active: Option<usize>,
    pub(crate) previous_pane_focus: Option<PaneFocusTarget>,
    pub selected: usize,
    /// Overlay discriminator for the client presentation currently being
    /// handled or rendered. This is swapped with `ClientOverlayState::kind`.
    pub(crate) client_overlay: ClientOverlay,
    pub(crate) server_interaction: ServerInteractionState,
    /// Stable workspace identity captured when the close confirmation opens.
    pub(crate) confirm_close_workspace_id: Option<String>,
    /// Remote pane selected for the same close confirmation flow.
    pub(crate) confirm_close_remote_agent_ref: Option<crate::api::schema::AgentRef>,
    /// Stable target for an existing-object rename. Creation dialogs use their
    /// own pending payload because no target exists yet.
    pub(crate) rename_target: Option<RenameTarget>,
    pub should_quit: bool,
    /// In monolithic --no-session mode, detach exits the app because there is no server to detach from.
    pub detach_exits: bool,
    /// Set when the current client should detach from the persistent session.
    /// The server's event loop checks this and handles client detach.
    pub detach_requested: bool,
    pub request_new_workspace: bool,
    pub request_new_tab: bool,
    /// Set by a click on a tab-row pane toggle, drained by the runtime loop
    /// into the same split/close calls the keybindings use.
    pub(crate) request_pane_toggle: Option<PaneToggleDirection>,
    /// The top-bar repository editor button was activated.
    pub(crate) request_open_repo_editor: bool,
    /// Notepad work that needs the filesystem, queued by the input handlers.
    pub(crate) notepad_request: Option<crate::app::input::NotepadRequest>,
    /// Git action chosen from the tab-row menu, drained by the runtime loop.
    pub(crate) request_git_action: Option<GitAction>,
    pub(crate) request_user_action: Option<usize>,
    pub(crate) request_save_add_action: bool,
    pub(crate) request_pr_command: Option<PrCommandRequest>,
    /// A click landed on a tab's pin glyph. Drained by the app loop, which is
    /// the layer that owns the API client the mutation has to travel through.
    pub request_pin_toggle: Option<(usize, usize)>,
    pub request_new_linked_worktree: Option<usize>,
    pub request_open_existing_worktree: Option<usize>,
    pub request_new_workspace_cwd: Option<std::path::PathBuf>,
    pub request_remove_linked_worktree: Option<usize>,
    pub request_submit_worktree_create: bool,
    pub request_submit_worktree_open: bool,
    pub request_submit_worktree_remove: bool,
    pub request_reload_config: bool,
    /// Set when the headless server should ask attached clients to reload
    /// their client-local sound config from disk.
    pub request_client_config_reload: bool,
    /// Effective sound permission requested by the notification toggle for
    /// the client that supplied the input.
    pub(crate) request_client_notification_config: Option<bool>,
    /// Width to persist in the attached client's local presentation state.
    pub(crate) dock_width_persistence_request: Option<u16>,
    /// Notepad panel height to persist in the host's local presentation state.
    pub(crate) notepad_height_persistence_request: Option<u16>,
    pub(crate) sidebar_group_mode_persistence_request: Option<SidebarGroupMode>,
    pub(crate) sidebar_group_sort_persistence_request: Option<(String, SidebarSortMode)>,
    pub(crate) sidebar_group_collapsed_persistence_request: Option<(String, bool)>,
    pub(crate) sidebar_view_scan_request: bool,
    pub(crate) sidebar_work_filter_persistence_request: Option<SidebarWorkFilter>,
    /// Set when UI interaction requested a clipboard write that must be
    /// handled by the outer App/event loop instead of directly from AppState.
    pub request_clipboard_write: Option<Vec<u8>>,
    pub creating_new_tab: bool,
    pub requested_new_tab_name: Option<String>,
    pub pending_workspace_create_cwd: Option<std::path::PathBuf>,
    pub rename_pane_target: Option<PaneId>,
    pub worktree_create: Option<WorktreeCreateState>,
    pub worktree_open: Option<WorktreeOpenState>,
    pub worktree_remove: Option<WorktreeRemoveState>,
    pub worktree_directory: std::path::PathBuf,
    pub collapsed_space_keys: std::collections::HashSet<String>,
    pub prio_panel_collapsed: bool,
    /// Sidebar group headers the user folded away, keyed by title. Separate from
    /// `collapsed_space_keys`, which folds one space inside the tree; this folds
    /// a whole group, the tree included.
    pub collapsed_sidebar_groups: std::collections::HashSet<String>,
    pub(crate) sidebar_group_mode: SidebarGroupMode,
    /// Whether the keyboard belongs to the sidebar.
    ///
    /// The sidebar's bare-key shortcuts are only reachable through this flag.
    /// It was previously implied by whatever selection or menu state a click
    /// happened to leave behind, so `m`, `n`, Enter and the search field kept
    /// answering long after the operator had moved on to type in a pane. Set by
    /// a click that lands in the sidebar without handing the keyboard to a
    /// pane, cleared by every path that focuses a pane.
    pub(crate) sidebar_focused: bool,
    pub(crate) client_focus_intent: ClientFocusIntent,
    pub(crate) sidebar_group_menu_open: bool,
    pub(crate) sidebar_group_menu_selected: usize,
    pub(crate) sidebar_work_filter: SidebarWorkFilter,
    pub(crate) sidebar_filter_menu_open: bool,
    pub(crate) sidebar_filter_menu_selected: usize,
    /// Typed input goes to the persisted sidebar row query while this is set.
    pub(crate) sidebar_search_active: bool,
    /// Sidebar-only view gate: show just the starred sessions. Pure client
    /// presentation state — the star itself lives on the tab.
    pub(crate) sidebar_starred_only: bool,
    /// Downward creation menu anchored to the sidebar header.
    pub(crate) sidebar_new_menu: Option<SidebarNewMenuState>,
    /// Checklist menu for sections-layout area visibility.
    pub(crate) sidebar_areas_menu_selected: Option<usize>,
    /// Cycle mode menu anchored beside the sidebar footer icons.
    pub(crate) window_cycle_menu_open: bool,
    pub(crate) window_cycle_menu_selected: usize,
    /// Downward recent-project picker. Project paths are derived at render time.
    pub(crate) sidebar_new_thread: Option<SidebarNewThreadState>,
    pub(crate) sidebar_project_menu: Option<SidebarProjectMenuState>,
    /// Server-owned request and busy state for the sidebar refresh operation.
    /// These fields deliberately stay outside `SidebarPresentationState` so
    /// input from an attached client is visible to the runtime work loop.
    pub(crate) sidebar_refresh_requested: bool,
    pub(crate) sidebar_refreshing: bool,
    /// Dim work-item header the operator selected with the mouse. Enter on it
    /// starts a thread for that ticket or conversation.
    pub(crate) sidebar_selected_work_group: Option<String>,
    /// Downward action menu for a Linear, GitHub, or Missive sidebar object.
    /// This and the write it may stage are client-local presentation.
    pub(crate) sidebar_object_menu: Option<SidebarObjectMenuState>,
    /// Downward sort dropdown for one sidebar group. Client-local presentation;
    /// the choices themselves live in `sidebar_group_sorts`.
    pub(crate) sidebar_sort_menu: Option<SidebarSortMenuState>,
    /// Picker assigning a window to a named sidebar subgroup. Transient UI
    /// state; the assignment itself lives on the tab and persists with the
    /// session.
    pub(crate) sidebar_subgroup_picker: Option<SidebarSubgroupPickerState>,
    /// Transient client-local query and cursor for pod assignment.
    pub(crate) sidebar_pod_picker: Option<SidebarPodPickerState>,
    /// Per-group sidebar sort choices keyed by canonical group key. Only groups
    /// with an explicit non-default choice have an entry.
    pub(crate) sidebar_group_sorts: std::collections::HashMap<String, SidebarSortMode>,
    /// Views whose Unassigned section has expanded past its ten newest rows.
    /// Attach-local TUI state; provider objects remain shared work-index facts.
    pub(crate) sidebar_unassigned_expanded_views: std::collections::HashSet<SidebarGroupMode>,
    pub(crate) sidebar_selected_settled: Option<PaneFocusTarget>,
    pub(crate) settled_view: Option<crate::app::settled_view::SettledViewState>,
    pub(crate) sidebar_snooze: Option<SidebarSnoozeUiState>,
    pub(crate) sidebar_settled_menu_target: Option<PaneFocusTarget>,
    pub(crate) sidebar_settled_menu_selected: usize,
    /// The settled menu's delete row has been pressed once and is waiting for
    /// the confirming second press. Only armed while `ui.confirm_close` is on.
    pub(crate) sidebar_settled_menu_delete_armed: bool,
    pub(crate) pending_pane_settlement_changes: Vec<PaneSettlementChange>,
    pub(crate) pending_status_transitions: Vec<crate::status_log::PendingTransition>,
    pub(crate) pending_pane_snooze_changes: Vec<PaneSnoozeChange>,
    pub request_complete_onboarding: bool,
    pub name_input: String,
    pub name_input_replace_on_type: bool,
    /// Label the rename-tab modal was prefilled with, so an unedited Enter stays a
    /// no-op even when the live derived label changes while the modal is open.
    pub rename_tab_prefill: Option<String>,
    pub release_notes: Option<ReleaseNotesState>,
    pub product_announcement: Option<ProductAnnouncementState>,
    pub keybind_help: KeybindHelpState,
    pub navigator: NavigatorState,
    pub command_palette: CommandPaletteState,
    pub work_link_picker: Option<WorkLinkPickerState>,
    pub(crate) agent_picker: Option<AgentPickerState>,
    pub(crate) add_action: Option<AddActionState>,
    pub copy_mode: Option<CopyModeState>,
    pub(crate) sidebar_presentation: SidebarPresentationState,
    /// Monotonic client-only revision for changes that replace the sidebar row
    /// projection. Each attach resets its own offsets when it next reconciles.
    pub(crate) sidebar_projection_revision: u64,
    pub(crate) workspace_picker_forces_spaces_tree: bool,
    pub workspace_scroll: usize,
    pub tab_scroll: usize,
    pub tab_scroll_follow_active: bool,
    pub mobile_switcher_scroll: usize,
    // View geometry (computed before render, consumed by render + mouse)
    pub view: ViewState,
    pub(crate) drag: Option<DragState>,
    pub(crate) workspace_presses:
        std::collections::HashMap<crate::app::InputSourceId, WorkspacePressState>,
    pub(crate) tab_presses: std::collections::HashMap<crate::app::InputSourceId, TabPressState>,
    pub(crate) agent_presses: std::collections::HashMap<crate::app::InputSourceId, AgentPressState>,
    pub(crate) remote_agent_presses:
        std::collections::HashMap<crate::app::InputSourceId, RemoteAgentPressState>,
    pub selection: Option<Selection>,
    pub selection_autoscroll: Option<SelectionAutoscroll>,
    pub context_menu: Option<ContextMenuState>,
    pub(crate) git_menu: MenuListState,
    // Notifications
    pub update_available: Option<String>,
    pub update_install_command: String,
    pub latest_release_notes_available: bool,
    pub update_dismissed: bool,
    pub config_diagnostic: Option<String>,
    pub toast: Option<ToastNotification>,
    pub pending_agent_notifications: std::collections::HashMap<PaneId, PendingAgentNotification>,
    pub copy_feedback: Option<CopyFeedback>,
    /// Last reported focus state for the outer terminal hosting herdr.
    /// None means unsupported or not yet reported, which preserves active-pane suppression.
    pub outer_terminal_focus: Option<bool>,
    // Config
    pub prefix_code: KeyCode,
    pub prefix_mods: KeyModifiers,
    /// Virtual terminal size (columns, rows) used when no client is attached.
    pub(crate) headless_size: (u16, u16),
    pub default_sidebar_width: u16,
    pub sidebar_width: u16,
    pub sidebar_min_width: u16,
    pub sidebar_max_width: u16,
    pub dock_width: u16,
    pub dock_collapsed: bool,
    /// Configured tabs for a fresh attach. TUI-only state; never enters the API.
    pub(crate) dock_default_surfaces: Vec<DockSurface>,
    /// Set by an explicit dock pick and cleared by `follow_view`.
    /// Attach-local TUI state; it never enters server state or the JSON API.
    pub(crate) dock_surface_override: bool,
    /// Active dock surface, `None` when nothing is open and the dock shows the
    /// surface chooser. TUI presentation state; never leaves the client.
    pub dock_tab: Option<DockSurface>,
    /// Surfaces open as tabs, in strip order. TUI presentation state.
    pub dock_open_surfaces: Vec<DockSurface>,
    /// Optional work-object binding for each entry in `dock_open_surfaces`.
    /// Parallel storage keeps existing surface input paths small while allowing
    /// several PR, Linear, or Missive tabs at once.
    pub(crate) dock_tab_bindings: Vec<Option<DockTabBinding>>,
    pub(crate) dock_active_tab_index: Option<usize>,
    /// Per-pane tab memory and context suppression. This is swapped per client
    /// and never enters server or protocol state.
    pub(crate) dock_pane_tabs: std::collections::HashMap<PaneFocusTarget, PaneDockTabs>,
    pub(crate) dock_followed_pane: Option<PaneFocusTarget>,
    pub(crate) dock_context_objects: Vec<DockObjectRef>,
    /// Whether a work link the focused pane carries opens the dock on its own.
    /// Off by default: a link arrives while the human is reading the pane, and
    /// taking a third of the width for it interrupts that. The status row names
    /// the link instead, and the click on that name is what opens the dock.
    pub(crate) open_dock_on_work_link: bool,
    pub(crate) dock_suppressed_context: std::collections::HashSet<DockObjectRef>,
    /// Dock takes the whole main area. TUI presentation state.
    pub dock_maximized: bool,
    /// Open `+` chooser dropdown. TUI presentation state.
    pub(crate) dock_surface_menu: Option<DockSurfaceMenu>,
    /// Keyboard focus sits on the chooser card grid. TUI presentation state.
    pub(crate) dock_chooser_focused: bool,
    pub dock_scroll: u16,
    /// Object preview hosted in the pane area while the dock is collapsed.
    /// The indexed provider object remains shared work-index data.
    pub(crate) dock_object_preview: Option<DockObjectRef>,
    /// Rich PR/ticket view state for dock hosts. The indexed object remains a
    /// shared runtime fact; tab, picker, and scroll are client presentation.
    pub(crate) dock_object_views: std::collections::HashMap<WorkItemKey, ObjectViewState>,
    pub(crate) dock_editor_focused: bool,
    /// Diff interaction state is attach-local TUI state. The whitespace choice
    /// survives surface switches for the lifetime of the client session.
    pub(crate) dock_diff_focused: bool,
    /// Dock-hosted PR interaction state. TUI presentation state: the
    /// pull request itself is a shared work-index fact, the open menu and the
    /// staged confirmation are not.
    pub(crate) dock_pr_focused: bool,
    pub(crate) dock_pr_checkout_menu: Option<PrCheckoutChoice>,
    pub(crate) dock_pr_action_menu: Option<PrActionMenuState>,
    /// `git diff -w` for the Diff surface. Initialised from
    /// `ui.hide_whitespace_in_diff` and written by both the dock's own
    /// whitespace toggle and the General settings row, so the two never drift.
    pub(crate) dock_diff_ignore_whitespace: bool,
    pub(crate) dock_diff_selected: usize,
    pub(crate) dock_diff_collapsed: std::collections::HashSet<String>,
    pub(crate) dock_diff_request: Option<super::diff::DiffRefreshRequest>,
    pub(crate) dock_diff_active_key: Option<DiffCacheKey>,
    pub(crate) dock_diff_cache: std::collections::HashMap<DiffCacheKey, DiffCacheEntry>,
    pub(crate) dock_diff_resolved_requests:
        std::collections::HashMap<super::diff::DiffRefreshRequest, DiffCacheKey>,
    pub(crate) dock_files_focused: bool,
    pub(crate) dock_files_selection: Option<std::path::PathBuf>,
    pub(crate) dock_files_filter: String,
    pub(crate) dock_files_collapsed: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) dock_files_sort: crate::files::FileSort,
    pub(crate) dock_files_search_active: bool,
    pub(crate) dock_files_last_click: Option<(std::path::PathBuf, std::time::Instant)>,
    /// Selection and keyboard ownership for the focus-following Agents tree.
    /// Both fields are attach-local TUI presentation state.
    pub(crate) dock_agents_focused: bool,
    pub(crate) dock_agents_selection: Option<String>,
    /// Selection and keyboard ownership for the fleet Hosts surface.
    /// Both fields are attach-local TUI presentation state.
    pub(crate) dock_hosts_focused: bool,
    pub(crate) dock_hosts_selection: Option<String>,
    pub(crate) dock_linear_focused: bool,
    pub(crate) dock_ticket_start_menu: Option<PrCheckoutChoice>,
    pub(crate) dock_ticket_action_menu: Option<crate::ui::ticket_actions::TicketActionMenuState>,
    pub(crate) dock_ticket_comment_draft: Option<String>,
    /// Cached client-side file snapshots, keyed by repository root.
    pub(crate) dock_file_cache:
        std::collections::HashMap<std::path::PathBuf, crate::files::FileTreeSnapshot>,
    pub(crate) dock_files_root: Option<std::path::PathBuf>,
    pub(crate) dock_files_cwd: Option<std::path::PathBuf>,
    pub(crate) dock_files_roots_by_cwd:
        std::collections::HashMap<std::path::PathBuf, std::path::PathBuf>,
    pub(crate) files_icons: crate::config::FilesIconConfig,
    pub(crate) nerd_font: bool,
    pub(crate) space_icons: std::collections::BTreeMap<String, String>,
    /// Selection inside the dock home tab, swapped per client through
    /// `DockPresentationState`. A key, never an index.
    pub(crate) dock_home_selection: Option<WorkItemKey>,
    pub(crate) dock_home_ticket_selection: Option<WorkItemKey>,
    pub(crate) dock_home_poll_selection: Option<WorkItemKey>,
    /// A comment being typed against the selected work item.
    /// True when the focused pane resolves to no pull request and no ticket.
    /// Without this the selection falls back to the first row, so switching to
    /// an unrelated tab kept showing the previous item as though it were the
    /// current one.
    pub(crate) dock_home_focus_unbound: bool,
    pub(crate) dock_comment_draft: Option<String>,
    /// A write staged by a button but not yet confirmed. Nothing leaves herdr
    /// until the user presses the confirm key with this set.
    pub(crate) dock_pending_write: Option<crate::work_index::WorkItemWrite>,
    /// Active slot for the client-local PR write gate. Swapped through
    /// `DockPresentationState`; pull-request facts stay in the work index.
    pub(crate) pr_action_confirmation: Option<PrActionConfirmation>,
    /// The outcome of the last write, shown until the next one is staged.
    pub(crate) dock_write_notice: Option<String>,
    pub(crate) dock_home_section: DockHomeSection,
    pub(crate) dock_home_detail_tab: DockHomeDetailTab,
    pub(crate) dock_home_focused: bool,
    /// Focus target last considered for automatic dock-home selection, swapped
    /// per client through `DockPresentationState`.
    pub(crate) dock_home_followed_pane: Option<PaneFocusTarget>,
    /// Server-global work index snapshot. Set on every applied
    /// `WorkIndexRefreshed`; the dock home enriches rows from it.
    pub(crate) work_index_snapshot: Option<crate::work_index::Snapshot>,
    /// Missive config facts copied into client state for the Settings view.
    /// The token value is never retained, only whether its named variable exists.
    pub(crate) settings_missive_team: Option<String>,
    pub(crate) settings_missive_token_present: bool,
    /// Provider viewers and assignee directories resolved by the work-index
    /// runtime once for this app session.
    pub(crate) work_index_session: crate::work_index::WorkIndexSession,
    /// Server-global on-demand detail facts, keyed by stable work identity.
    /// Client-local selection decides which entry is rendered, but never owns
    /// or duplicates the fetched data.
    pub(crate) work_item_detail_cache: crate::work_index::WorkItemDetailCache,
    /// Detail keys in the one bounded batch currently running. This is runtime
    /// observation state, not a client-local loading flag inferred by render.
    pub(crate) work_item_detail_loading: std::collections::HashSet<WorkItemKey>,
    /// Whether the work index is configured on. Mirrored so the dock home can
    /// distinguish "off" from "on but not observed yet" instead of rendering
    /// one indistinguishable `unknown` for both.
    pub(crate) work_index_enabled: bool,
    /// Merge method behind the primary PR action, applied on live config reload.
    pub(crate) pr_merge_method: crate::config::MergeMethodConfig,
    /// `source_control.branch_prefix`: the prefix Herdr puts in front of a
    /// branch name derived from a ticket.
    pub(crate) branch_prefix: String,
    /// `source_control.commit_message_model`: passed to the non-interactive
    /// commit-message generator. Empty leaves `git commit` on its editor path.
    pub(crate) commit_message_model: String,
    /// `source_control.commit_stage_all`: opt-in staging before commit-message
    /// generation. False preserves the caller's staged set exactly.
    pub(crate) commit_stage_all: bool,
    pub(crate) work_index_linear_team_configured: bool,
    pub(crate) dock_editor_sessions: std::collections::HashMap<PaneId, DockEditorSession>,
    pub(crate) dock_editor_preview: Option<DockEditorPreview>,
    pub(crate) dock_editor_errors: std::collections::HashMap<PaneId, String>,
    pub(crate) dock_editor_requested_paths: std::collections::HashMap<PaneId, std::path::PathBuf>,
    pub(crate) scratchpad: crate::scratchpad::ScratchpadDoc,
    /// The sidebar notepad: a folder of Markdown notes edited in place.
    pub(crate) notepad: crate::notepad::NotepadState,
    /// Configured note names available to Sidebar panels before files are discovered.
    pub(crate) sidebar_note_names: Vec<String>,
    /// Cached goals for the focused session. Rendering never reads the file.
    pub(crate) goals: crate::goals::GoalsPanelState,
    /// The break reminder shown next to it.
    pub(crate) pomodoro: crate::pomodoro::PomodoroState,
    /// The idle star field pinned under both of them.
    pub(crate) hyperspace: crate::hyperspace::HyperspaceState,
    pub mobile_width_threshold: u16,
    pub sidebar_width_source: SidebarWidthSource,
    pub sidebar_width_auto: bool,
    pub sidebar_collapsed: bool,
    /// Whether the sidebar is showing only rows that require human attention.
    pub blocked_filter: bool,
    /// Whether window and blocked-agent cycles include fleet agents.
    pub window_cycle_mode: crate::config::WindowCycleModeConfig,
    /// Whether cycles skip spaces and sections hidden by collapse.
    pub skip_collapsed_cycle: bool,
    pub sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig,
    /// Ratio of sidebar height allocated to the workspaces section.
    pub sidebar_section_split: f32,
    pub agent_panel_scroll: usize,
    pub agent_panel_sort: AgentPanelSort,
    /// Transient session-wide compatibility projection for indexed Agent focus.
    pub status_indicators: crate::config::StatusIndicatorStyle,
    /// TUI-only foreground opacity of blue-dot Working rows.
    pub working_row_opacity_percent: u8,
    /// Transient session-wide projection override for the built-in Agents view.
    pub agent_view_override: Option<crate::api::schema::AgentViewSetParams>,
    pub sidebar_agents: crate::config::AgentsSidebarConfig,
    pub sidebar_spaces: crate::config::SpacesSidebarConfig,
    pub sidebar_areas: crate::config::SidebarAreasConfig,
    /// Config-selected sidebar presentation. It is view state, never session data.
    pub sidebar_sections_layout: bool,
    /// Whether the sections Spaces tree shows one-line blocked-agent asks.
    pub sidebar_show_ask_subtitles: bool,
    pub sidebar_header_plain: bool,
    pub next_agent_state_change_seq: u64,
    /// Capture mouse input for Herdr's own mouse UI. When false, Herdr only
    /// captures mouse while the focused pane app requests mouse reporting.
    pub mouse_capture: bool,
    pub copy_on_select: bool,
    pub right_click_passthrough_modifiers: Option<KeyModifiers>,
    pub right_click_passthrough: Option<RightClickPassthroughGesture>,
    pub redraw_on_focus_gained: bool,
    pub mouse_scroll_lines: usize,
    pub confirm_close: bool,
    /// Group workspaces that check out the same repository under one project
    /// header even when the checkout roots differ (`ui.combine_repos_across_hosts`).
    pub combine_repos_across_hosts: bool,
    /// Workspace preselected for a new Home thread (`ui.new_thread_workspace`).
    pub new_thread_workspace: crate::config::NewThreadWorkspaceConfig,
    /// Directory the add-project picker starts in (`ui.add_project_start_dir`).
    /// Empty keeps the last used directory.
    pub add_project_start_dir: String,
    /// Settle a pane when its linked work finishes (`session.auto_settle_finished`).
    pub auto_settle_finished: bool,
    /// Settle a pane after `settle_after` of inactivity (`session.auto_settle_inactive`).
    pub auto_settle_inactive: bool,
    /// Settle a resumable Done pane instead of leaving it to be reaped
    /// (`session.auto_settle_done`).
    pub auto_settle_done: bool,
    /// Stop resumable agent processes when their pane settles (`session.settle_stops_agent`).
    pub settle_stops_agent: bool,
    /// Nudge a natively resumed agent to continue (`session.nudge_resumed_agents`).
    pub nudge_resumed_agents: bool,
    /// Prompt submitted by the resume nudge (`session.resume_nudge_message`).
    pub resume_nudge_message: String,
    /// Nudge stalled agent panes (`session.auto_nudge_stalled_agents`).
    pub auto_nudge_stalled_agents: bool,
    /// Quiet period before an agent status report becomes stale.
    pub agent_stale_after: std::time::Duration,
    pub agent_subagent_stale_after: std::time::Duration,
    /// Initial quiet period before a stalled pane is nudged.
    pub nudge_after: std::time::Duration,
    /// Maximum nudges sent during one stale-status episode.
    pub max_nudges: u32,
    /// Maximum stalled-agent nudges sent to one pane without human input.
    /// Zero disables this cap.
    pub max_nudges_without_human: u32,
    /// Prompt submitted to a stalled pane (`session.stall_nudge_message`).
    pub stall_nudge_message: String,
    pub prompt_new_tab_name: bool,
    pub prompt_new_workspace_name: bool,
    pub pane_borders: bool,
    pub pane_outer_borders: bool,
    pub pane_scrollbars: bool,
    pub show_pull_button: bool,
    pub show_pane_toggle_buttons: bool,
    pub pane_gaps: bool,
    pub show_agent_labels_on_pane_borders: bool,
    pub hide_tab_bar_when_single_tab: bool,
    pub tab_bar_position: TabBarPositionConfig,
    pub tab_bar_right: Vec<TabBarStatusSegment>,
    pub tab_bar_right_separator: String,
    pub pane_history_persistence: bool,
    /// Expose the focused pane's cursor anchor to the outer terminal even when
    /// the pane requested `?25l`. See `[experimental] reveal_hidden_cursor_for_cjk_ime`.
    pub reveal_hidden_cursor_for_cjk_ime: bool,
    /// Restrict cursor reveal to focused panes whose detected agent matches
    /// one of these. When false, apply to any focused pane.
    pub cjk_ime_agent_filter_configured: bool,
    pub cjk_ime_agents: Vec<crate::detect::Agent>,
    /// DECSCUSR shape parameter (1–6) for the IME anchor cursor.
    pub cjk_ime_cursor_shape: u8,
    /// While prefix mode is active, switch the macOS host input source to an
    /// ASCII-capable layout so prefix commands register as ASCII even when a
    /// CJK IME is active. macOS only; a no-op elsewhere. See
    /// `[experimental] switch_ascii_input_source_in_prefix`.
    pub switch_ascii_input_source_in_prefix: bool,
    pub kitty_graphics_enabled: bool,
    pub default_shell: String,
    pub shell_mode: crate::config::ShellModeConfig,
    pub new_terminal_cwd: NewTerminalCwdConfig,
    pub pane_scrollback_limit_bytes: usize,
    #[allow(dead_code)] // kept for backward compat; palette.accent is the source of truth
    pub accent: Color,
    pub sound: SoundConfig,
    pub local_sound_playback: bool,
    pub toast_config: ToastConfig,
    /// Delivery restored when the per-machine notification toggle is turned on.
    pub last_non_off_toast_delivery: ToastDelivery,
    pub keybinds: Keybinds,
    /// UI color palette — all sidebar/UI colors centralized for theming.
    pub palette: Palette,
    /// Currently applied theme name (for settings UI).
    pub theme_name: String,
    /// Runtime theme configuration used to resolve manual and auto-switch palettes.
    pub theme_runtime: ThemeRuntimeConfig,
    /// Last known foreground host terminal appearance.
    pub host_terminal_appearance: Option<HostAppearance>,
    /// True when the foreground host explicitly reported appearance via Mode 2031.
    pub host_terminal_appearance_explicit: bool,
    /// Set when the active palette contradicts the appearance the host terminal
    /// reports, e.g. a light theme pinned in front of a dark terminal. Surfaced
    /// as a config-reload diagnostic so the mismatch is named, not guessed at.
    pub theme_appearance_mismatch: Option<String>,
    /// Settings panel state.
    pub settings: SettingsState,
    /// Session cache of the settings Providers/Integrations probes. TUI-only:
    /// nothing outside the settings screen reads it.
    pub tool_probes: crate::app::probes::ToolProbeState,
    /// Cached integration recommendations for onboarding/settings UI.
    pub integration_recommendations: Vec<crate::integration::IntegrationRecommendation>,
    /// Cached detection manifest source/version summaries for runtime/API status.
    pub agent_manifest_summaries: Vec<crate::detect::manifest::AgentManifestSummary>,
    /// Cached remote detection manifest update diagnostics for runtime/API status.
    pub agent_manifest_update_status: crate::detect::manifest_update::ManifestUpdateStatus,
    /// Result messages from the latest integration install action.
    pub integration_install_messages: Vec<String>,
    /// Installed or linked plugins known to this running Herdr instance.
    pub(crate) installed_plugins: InstalledPluginRegistry,
    /// Pane ids opened through the plugin pane API.
    pub(crate) plugin_panes: std::collections::HashMap<PaneId, PluginPaneRecord>,
    /// Session-modal terminal popup. This is intentionally outside workspace layouts.
    pub(crate) popup_pane: Option<PopupPaneState>,
    /// Recent plugin action/event command executions.
    pub(crate) plugin_command_logs: Vec<crate::api::schema::PluginCommandLogInfo>,
    pub(crate) next_plugin_command_log_id: u64,
    pub(crate) plugin_commands_in_flight: usize,
    /// Highlight state for the bottom-right global launcher menu.
    pub global_menu: MenuListState,
    /// Resolved host terminal default colors for theming embedded panes.
    pub host_terminal_theme: TerminalTheme,
    /// Last known foreground host terminal cell size in pixels.
    pub(crate) host_cell_size: crate::kitty_graphics::HostCellSize,
    /// Exact pixel provenance only while one confirmed SGR report is dispatched.
    pub(crate) host_mouse_pixels: Option<crate::input::mouse::HostPixels>,
    /// Set when a persisted session snapshot would change.
    pub session_dirty: bool,
    /// Monotonic revision used to avoid clearing mutations made during a save.
    pub(crate) session_dirty_revision: u64,
    pub(crate) session_epoch: String,
    pub(crate) session_event_revision: u64,
    /// Terminal runtimes that should be shut down by the app/runtime layer
    /// after state has detached their terminal metadata.
    pub(crate) terminal_runtime_shutdowns: Vec<crate::terminal::TerminalId>,
}

#[derive(Clone, Copy)]
struct ClientInputOwnerState {
    settled_delete_armed: bool,
    settled_menu_open: bool,
    snooze_time_open: bool,
    snooze_menu_open: bool,
    overlay: ClientOverlay,
    agent_picker_open: bool,
    subgroup_picker_open: bool,
    pod_picker_open: bool,
    sort_menu_open: bool,
    object_menu_open: bool,
    project_menu_open: bool,
    new_thread_open: bool,
    new_menu_open: bool,
    filter_menu_open: bool,
    group_menu_open: bool,
    pr_confirmation_open: bool,
    dock_surface_menu_open: bool,
}

impl ClientInputOwnerState {
    fn from_app(app: &AppState) -> Self {
        Self {
            settled_delete_armed: app.sidebar_settled_menu_delete_armed,
            settled_menu_open: app.sidebar_settled_menu_target.is_some(),
            snooze_time_open: app
                .sidebar_snooze
                .as_ref()
                .is_some_and(|snooze| snooze.time_draft.is_some()),
            snooze_menu_open: app.sidebar_snooze.is_some(),
            overlay: app.client_overlay,
            agent_picker_open: app.agent_picker.is_some(),
            subgroup_picker_open: app.sidebar_subgroup_picker.is_some(),
            pod_picker_open: app.sidebar_pod_picker.is_some(),
            sort_menu_open: app.sidebar_sort_menu.is_some(),
            object_menu_open: app.sidebar_object_menu.is_some(),
            project_menu_open: app.sidebar_project_menu.is_some(),
            new_thread_open: app.sidebar_new_thread.is_some(),
            new_menu_open: app.sidebar_new_menu.is_some(),
            filter_menu_open: app.sidebar_filter_menu_open,
            group_menu_open: app.sidebar_group_menu_open,
            pr_confirmation_open: app.pr_action_confirmation.is_some(),
            dock_surface_menu_open: app.dock_surface_menu.is_some(),
        }
    }

    fn from_presentations(
        sidebar: &SidebarPresentationState,
        dock: &DockPresentationState,
    ) -> Self {
        Self {
            settled_delete_armed: sidebar.overlay.settled_menu_delete_armed,
            settled_menu_open: sidebar.overlay.settled_menu_target.is_some(),
            snooze_time_open: sidebar
                .overlay
                .snooze
                .as_ref()
                .is_some_and(|snooze| snooze.time_draft.is_some()),
            snooze_menu_open: sidebar.overlay.snooze.is_some(),
            overlay: sidebar.overlay.kind,
            agent_picker_open: sidebar.overlay.agent_picker.is_some(),
            subgroup_picker_open: sidebar.subgroup_picker.is_some(),
            pod_picker_open: sidebar.pod_picker.is_some(),
            sort_menu_open: sidebar.sort_menu.is_some(),
            object_menu_open: sidebar.object_menu.is_some(),
            project_menu_open: sidebar.project_menu.is_some(),
            new_thread_open: sidebar.new_thread.is_some(),
            new_menu_open: sidebar.new_menu.is_some(),
            filter_menu_open: sidebar.filter_menu_open,
            group_menu_open: sidebar.group_menu_open,
            pr_confirmation_open: dock.pr_action_confirmation.is_some(),
            dock_surface_menu_open: dock.surface_menu.is_some(),
        }
    }

    fn resolve(self) -> Option<ClientInputOwner> {
        if self.settled_delete_armed {
            return Some(ClientInputOwner::SettledDeleteConfirm);
        }
        if self.settled_menu_open {
            return Some(ClientInputOwner::SettledMenu);
        }
        if self.snooze_time_open {
            return Some(ClientInputOwner::SnoozeTime);
        }
        if self.snooze_menu_open {
            return Some(ClientInputOwner::SnoozeMenu);
        }
        if self.overlay != ClientOverlay::None {
            return Some(ClientInputOwner::Overlay(self.overlay));
        }
        if self.agent_picker_open {
            return Some(ClientInputOwner::AgentPicker);
        }
        if self.subgroup_picker_open {
            return Some(ClientInputOwner::SidebarSubgroupPicker);
        }
        if self.pod_picker_open {
            return Some(ClientInputOwner::SidebarPodPicker);
        }
        if self.sort_menu_open {
            return Some(ClientInputOwner::SidebarSortMenu);
        }
        if self.object_menu_open {
            return Some(ClientInputOwner::SidebarObjectMenu);
        }
        if self.project_menu_open {
            return Some(ClientInputOwner::SidebarProjectMenu);
        }
        if self.new_thread_open {
            return Some(ClientInputOwner::SidebarNewThread);
        }
        if self.new_menu_open {
            return Some(ClientInputOwner::SidebarNewMenu);
        }
        if self.filter_menu_open {
            return Some(ClientInputOwner::SidebarFilterMenu);
        }
        if self.group_menu_open {
            return Some(ClientInputOwner::SidebarGroupMenu);
        }
        if self.pr_confirmation_open {
            return Some(ClientInputOwner::PrActionConfirmation);
        }
        if self.dock_surface_menu_open {
            return Some(ClientInputOwner::DockSurfaceMenu);
        }
        None
    }
}

impl AppState {
    pub(crate) fn client_presentation_policy(&self) -> ClientPresentationPolicy<'_> {
        ClientPresentationPolicy::from_app(self)
    }

    /// Theme reported to child terminals. Real host answers take precedence;
    /// missing defaults follow the palette currently painted by Herdr.
    pub(crate) fn pane_terminal_theme(&self) -> TerminalTheme {
        let palette = self.palette.terminal_theme();
        TerminalTheme {
            foreground: self.host_terminal_theme.foreground.or(palette.foreground),
            background: self.host_terminal_theme.background.or(palette.background),
            palette: self.host_terminal_theme.palette,
        }
    }

    /// Appearance reported to child terminals. Built-in palettes are concrete;
    /// the terminal-inheriting palette falls back to Ghostty's dark default.
    pub(crate) fn pane_terminal_appearance(&self) -> HostAppearance {
        self.theme_runtime
            .host_appearance
            .appearance()
            .or(self.host_terminal_appearance)
            .or_else(|| self.palette.appearance())
            .unwrap_or(HostAppearance::Dark)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LoopRunHistoryDetail {
    pub(crate) loop_id: String,
    pub(crate) history: crate::loop_runs::RunHistory,
    pub(crate) observed_at: std::time::SystemTime,
    pub(crate) producer_host: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AloopRunDetail {
    pub(crate) loop_name: String,
    pub(crate) host: String,
    pub(crate) run: std::sync::Arc<crate::aloop::RunRecord>,
    pub(crate) observed_at: std::time::SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymphonyDetail {
    pub(crate) snapshot: crate::symphony::Snapshot,
    pub(crate) selected: usize,
    pub(crate) observed_at: std::time::SystemTime,
}

impl SymphonyDetail {
    pub(crate) fn replace_snapshot(&mut self, snapshot: crate::symphony::Snapshot) {
        let selected_identity = self
            .snapshot
            .workflows
            .get(self.selected)
            .map(|workflow| (workflow.workflow_id.as_str(), workflow.run_id.as_str()));
        let selected = selected_identity
            .and_then(|(workflow_id, run_id)| {
                snapshot.workflows.iter().position(|workflow| {
                    workflow.workflow_id == workflow_id && workflow.run_id == run_id
                })
            })
            .unwrap_or_else(|| {
                self.selected
                    .min(snapshot.workflows.len().saturating_sub(1))
            });
        self.snapshot = snapshot;
        self.selected = selected;
        self.observed_at = std::time::SystemTime::now();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkProjection {
    PullRequests,
    Tickets,
    Missive,
    Agents,
    ReviewQueue,
}

impl WorkProjection {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PullRequests => "PRs",
            Self::Tickets => "tickets",
            Self::Missive => "Missive",
            Self::Agents => "agents",
            Self::ReviewQueue => "review queue",
        }
    }

    pub(crate) fn rotate_left(self) -> Self {
        match self {
            Self::PullRequests => Self::ReviewQueue,
            Self::Tickets => Self::PullRequests,
            Self::Missive => Self::Tickets,
            Self::Agents => Self::Missive,
            Self::ReviewQueue => Self::Agents,
        }
    }

    pub(crate) fn rotate_right(self) -> Self {
        match self {
            Self::PullRequests => Self::Tickets,
            Self::Tickets => Self::Missive,
            Self::Missive => Self::Agents,
            Self::Agents => Self::ReviewQueue,
            Self::ReviewQueue => Self::PullRequests,
        }
    }
}

/// Stable identity of a projected work row. Selection is stored as this key,
/// not an index, so it survives snapshot refreshes and projection rotations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkItemKey {
    pub(crate) repo: String,
    pub(crate) pr_number: Option<u64>,
    pub(crate) pr_url: Option<String>,
    pub(crate) ticket_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DockHomeSection {
    #[default]
    Prs,
    Tickets,
    /// Closing-block gates across every pane: the questions waiting on a
    /// human answer, gathered where the rest of the work lives.
    XPolls,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DockHomeDetailTab {
    #[default]
    Overview,
    Comments,
    Actions,
    Files,
    Commits,
    Ticket,
}

impl DockHomeDetailTab {
    pub(crate) const ALL: [Self; 6] = [
        Self::Overview,
        Self::Comments,
        Self::Actions,
        Self::Files,
        Self::Commits,
        Self::Ticket,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Comments => "comments",
            Self::Actions => "actions",
            Self::Files => "files",
            Self::Commits => "commits",
            Self::Ticket => "ticket",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkViewState {
    pub(crate) projection: WorkProjection,
    /// `None` means "no explicit selection yet" and resolves to the first row.
    pub(crate) selected: Option<WorkItemKey>,
    pub(crate) repo_filter: Option<String>,
    pub(crate) enabled: bool,
    /// `None` means the enabled index has not been collected yet.
    pub(crate) snapshot: Option<crate::work_index::Snapshot>,
    pub(crate) hint: Option<String>,
    pub(crate) search: String,
    pub(crate) search_focused: bool,
    pub(crate) sort: crate::ui::work_list_detail::PrSort,
    pub(crate) ticket_sort: crate::ui::work_list_detail::TicketSort,
    pub(crate) open_only: bool,
    pub(crate) ticket_open_only: bool,
    /// The full-screen host uses the same per-object presentation state as the
    /// dock host. Keys preserve tabs and scroll while list selection changes.
    pub(crate) object_views: std::collections::HashMap<WorkItemKey, ObjectViewState>,
    pub(crate) checkout_menu: Option<PrCheckoutChoice>,
    pub(crate) pr_action_menu: Option<PrActionMenuState>,
    pub(crate) reviewer_picker: Option<ReviewerPickerState>,
    pub(crate) ticket_start_menu: Option<PrCheckoutChoice>,
    pub(crate) ticket_transition_menu: Option<TicketTransitionChoice>,
    pub(crate) ticket_more_menu: Option<crate::ui::ticket_actions::TicketActionMenuState>,
    pub(crate) missive_start_menu: Option<PrCheckoutChoice>,
    pub(crate) selected_missive: Option<String>,
    pub(crate) missive_detail_scroll: u16,
    pub(crate) ticket_comment_draft: Option<String>,
    pub(crate) pending_write: Option<crate::work_index::WorkItemWrite>,
    pub(crate) refreshing: bool,
    pub(crate) ticket_layout: LinearViewLayout,
    pub(crate) board_column: usize,
    pub(crate) board_rows: [usize; 5],
    pub(crate) board_scroll: [usize; 5],
    pub(crate) board_detail_open: bool,
    pub(crate) board_last_click: Option<(WorkItemKey, std::time::Instant)>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum LinearViewLayout {
    #[default]
    List,
    Board,
}

impl From<crate::config::LinearLayoutConfig> for LinearViewLayout {
    fn from(value: crate::config::LinearLayoutConfig) -> Self {
        match value {
            crate::config::LinearLayoutConfig::List => Self::List,
            crate::config::LinearLayoutConfig::Board => Self::Board,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarFooterItem {
    Board,
    Settings,
    AskSubtitles,
    PullRequests,
    Usage,
    Linear,
    Missive,
    Refresh,
    WindowCycleMode,
    Notifications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlId {
    ConfigDiagnostic,
    SidebarNewThread,
    SidebarStarFilter,
    SidebarNewMenu,
    SidebarMore,
    SidebarFooter(SidebarFooterItem),
    SidebarHover(usize),
    SidebarRowHover(u16),
    NotepadUsageRow(usize),
    NotepadUsageToggle,
    SidebarAnimationPause,
    DockTab(usize),
    DockClose,
    DockAdd,
    DockAutoOpen,
    TopBarScrollLeft,
    TopBarScrollRight,
    TopBarNewTab,
    TopBarRepoEditor,
    TopBarAddAction,
    TopBarUserAction(usize),
    TopBarGitMenu,
    TopBarPaneBelow,
    TopBarPaneRight,
    StatusSegment(StatusSegmentKind),
}

/// One right-aligned status-row segment, named for its hover explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusSegmentKind {
    Provider(crate::provider_usage::QuotaProvider),
    Link,
    Agents,
    /// The `fleet:` label; clicking it toggles the device dots.
    FleetLabel,
    /// One other device that needs attention, by fleet host name.
    FleetDevice(usize),
    RemoteHost,
    Hostname,
    Cpu,
    Memory,
    Disk,
}

impl AppState {
    /// Status-row segment under a cell, from the last computed layout.
    pub(crate) fn status_segment_kind_at(&self, col: u16, row: u16) -> Option<StatusSegmentKind> {
        self.view
            .status_segment_hit_areas
            .iter()
            .find(|(_, rect)| {
                col >= rect.x
                    && col < rect.x.saturating_add(rect.width)
                    && row >= rect.y
                    && row < rect.y.saturating_add(rect.height)
            })
            .map(|(kind, _)| *kind)
    }

    pub(crate) fn set_hovered_control_at(&mut self, control: Option<ControlId>, now: Instant) {
        if self.hovered_control == control {
            return;
        }
        self.hovered_control = control;
        self.hover_started_at = control.map(|_| now);
        self.hover_tooltip_visible = false;
    }

    pub(crate) fn clear_hovered_control(&mut self) {
        self.hovered_control = None;
        self.hover_started_at = None;
        self.hover_tooltip_visible = false;
    }

    pub(crate) fn hover_tooltip_deadline(&self) -> Option<Instant> {
        if self.hover_tooltip_visible || self.hovered_control.is_none() {
            return None;
        }
        self.hover_started_at
            .map(|started| started + super::HOVER_TOOLTIP_DELAY)
    }

    pub(crate) fn reveal_hover_tooltip_at(&mut self, now: Instant) -> bool {
        if self
            .hover_tooltip_deadline()
            .is_some_and(|deadline| now >= deadline)
        {
            self.hover_tooltip_visible = true;
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageMetric {
    #[default]
    Cost,
    Tokens,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageRange {
    Hours24,
    Days7,
    #[default]
    Days30,
    Days90,
}

impl UsageRange {
    pub(crate) fn seconds(self) -> i64 {
        match self {
            Self::Hours24 => 24 * 60 * 60,
            Self::Days7 => 7 * 24 * 60 * 60,
            Self::Days30 => 30 * 24 * 60 * 60,
            Self::Days90 => 90 * 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum UsageBreakdown {
    #[default]
    Model,
    Day,
}

/// Client-local controls and scan data for the full-screen usage view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageViewState {
    pub(crate) metric: UsageMetric,
    pub(crate) range: UsageRange,
    pub(crate) breakdown: UsageBreakdown,
    pub(crate) snapshot: Option<crate::provider_usage::UsageSnapshot>,
    pub(crate) scanning: bool,
}

impl UsageViewState {
    pub(crate) fn new(snapshot: Option<crate::provider_usage::UsageSnapshot>) -> Self {
        Self {
            metric: UsageMetric::default(),
            range: UsageRange::default(),
            breakdown: UsageBreakdown::default(),
            scanning: snapshot.is_none(),
            snapshot,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TicketTransitionChoice {
    #[default]
    Todo,
    InProgress,
    InReview,
    Done,
}

impl TicketTransitionChoice {
    pub(crate) const ALL: [Self; 4] = [Self::Todo, Self::InProgress, Self::InReview, Self::Done];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::InReview => "In Review",
            Self::Done => "Done",
        }
    }

    pub(crate) fn move_by(self, delta: i8) -> Self {
        let index = Self::ALL
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0) as i8;
        Self::ALL[(index + delta).clamp(0, 3) as usize]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PrDetailTab {
    #[default]
    Overview,
    Files,
    Diff,
    Checks,
    Timeline,
}

impl PrDetailTab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Overview,
        Self::Files,
        Self::Diff,
        Self::Checks,
        Self::Timeline,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Files => "Files",
            Self::Diff => "Diff",
            Self::Checks => "Checks",
            Self::Timeline => "Timeline",
        }
    }

    pub(crate) fn next(self) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// A named section of a work-object detail. Collapse state is keyed on this
/// rather than on the header text, which carries a count that changes.
///
/// A section keeps its identity across the sub-tabs that show it: collapsing
/// checks on the overview collapses them on the checks sub-tab too, because
/// the reader collapsed "checks", not "this copy of checks".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum DetailSection {
    LinkedPrs,
    Description,
    AcceptanceCriteria,
    Checks,
    Comments,
    Files,
    Timeline,
}

impl DetailSection {
    /// Digit that toggles the section at `index` among the ones on screen,
    /// pressed with alt. Only the first nine get one.
    pub(crate) fn toggle_digit(index: usize) -> Option<char> {
        if index >= 9 {
            return None;
        }
        char::from_digit(index as u32 + 1, 10)
    }
}

/// Host-independent state for a rich work-object detail. Both the full work
/// view and the dock store this exact type and call the same renderer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ObjectViewState {
    pub(crate) tab: PrDetailTab,
    pub(crate) scroll: u16,
    /// Selected row while the narrow-width sub-tab picker is open.
    pub(crate) tab_picker: Option<usize>,
    pub(crate) reviewer_picker: Option<ReviewerPickerState>,
    /// Comments the reader expanded, keyed by content rather than by position:
    /// the comment list is rebuilt on every observation, and a new comment
    /// shifts the index of every older one.
    pub(crate) expanded_comments: std::collections::BTreeSet<u64>,
    /// Sections the reader collapsed. Empty is the default, so a detail opens
    /// showing everything it has.
    pub(crate) collapsed_sections: std::collections::BTreeSet<DetailSection>,
}

impl ObjectViewState {
    pub(crate) fn comment_is_expanded(&self, identity: u64) -> bool {
        self.expanded_comments.contains(&identity)
    }

    pub(crate) fn toggle_comment(&mut self, identity: u64) {
        if !self.expanded_comments.remove(&identity) {
            self.expanded_comments.insert(identity);
        }
    }

    pub(crate) fn section_is_collapsed(&self, section: DetailSection) -> bool {
        self.collapsed_sections.contains(&section)
    }

    pub(crate) fn toggle_section(&mut self, section: DetailSection) {
        if !self.collapsed_sections.remove(&section) {
            self.collapsed_sections.insert(section);
        }
    }

    /// Expand every comment, or collapse them all when they already are.
    /// Seeding the set rather than holding a separate override keeps one
    /// source of truth, so a following per-comment toggle behaves normally.
    /// Rebuilding it from the current list also drops identities left behind
    /// by comments that have since been edited away.
    pub(crate) fn toggle_all_comments(&mut self, identities: impl IntoIterator<Item = u64>) {
        let identities = identities.into_iter().collect::<Vec<_>>();
        let all_expanded = !identities.is_empty()
            && identities
                .iter()
                .all(|identity| self.expanded_comments.contains(identity));
        self.expanded_comments.clear();
        if !all_expanded {
            self.expanded_comments.extend(identities);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PrCheckoutChoice {
    #[default]
    CurrentCheckout,
    NewWorktree,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PrActionMenuState {
    pub(crate) selected: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReviewerPickerState {
    pub(crate) filter: crate::ui::dropdown::DropdownFilterState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrActionConfirmation {
    pub(crate) key: WorkItemKey,
    pub(crate) action: crate::ui::work_list_detail::PrActionKind,
}

impl PrActionConfirmation {
    pub(crate) fn title(&self) -> &'static str {
        use crate::ui::work_list_detail::PrActionKind;
        match self.action {
            PrActionKind::Merge(_) => "Merge pull request?",
            PrActionKind::ConvertToDraft => "Convert pull request to draft?",
            PrActionKind::MarkReady => "Mark pull request ready?",
            PrActionKind::EnableAutoMerge(_) => "Enable auto-merge?",
            PrActionKind::DisableAutoMerge => "Disable auto-merge?",
            PrActionKind::Close => "Close pull request?",
            _ => "Confirm pull request action?",
        }
    }

    pub(crate) fn body(&self) -> String {
        use crate::ui::work_list_detail::PrActionKind;
        let number = self.key.pr_number.unwrap_or_default();
        match self.action {
            PrActionKind::Merge(method) => {
                format!("This merges #{number} using {}.", method.label())
            }
            PrActionKind::ConvertToDraft => format!("This converts #{number} to a draft."),
            PrActionKind::MarkReady => format!("This marks #{number} ready for review."),
            PrActionKind::EnableAutoMerge(method) => format!(
                "This enables auto-merge for #{number} using {}.",
                method.label()
            ),
            PrActionKind::DisableAutoMerge => {
                format!("This disables auto-merge for #{number}.")
            }
            PrActionKind::Close => format!("This closes #{number}."),
            _ => format!("This changes #{number}."),
        }
    }

    pub(crate) fn confirm_label(&self) -> &'static str {
        use crate::ui::work_list_detail::PrActionKind;
        match self.action {
            PrActionKind::Merge(_) => "Merge",
            PrActionKind::ConvertToDraft => "Convert",
            PrActionKind::MarkReady => "Mark ready",
            PrActionKind::EnableAutoMerge(_) => "Enable",
            PrActionKind::DisableAutoMerge => "Disable",
            PrActionKind::Close => "Close",
            _ => "Confirm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrCommandRequest {
    pub(crate) repo: String,
    pub(crate) number: u64,
    pub(crate) action: PrCommandAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrCommandAction {
    Merge(crate::config::MergeMethodConfig),
    OpenOnGithub,
}

impl WorkViewState {
    pub(crate) fn new(enabled: bool, snapshot: Option<crate::work_index::Snapshot>) -> Self {
        Self {
            projection: WorkProjection::PullRequests,
            selected: None,
            repo_filter: None,
            enabled,
            snapshot,
            hint: None,
            search: String::new(),
            search_focused: false,
            sort: crate::ui::work_list_detail::PrSort::Updated,
            ticket_sort: crate::ui::work_list_detail::TicketSort::Updated,
            open_only: true,
            ticket_open_only: false,
            object_views: std::collections::HashMap::new(),
            checkout_menu: None,
            pr_action_menu: None,
            reviewer_picker: None,
            ticket_start_menu: None,
            ticket_transition_menu: None,
            ticket_more_menu: None,
            missive_start_menu: None,
            selected_missive: None,
            missive_detail_scroll: 0,
            ticket_comment_draft: None,
            pending_write: None,
            refreshing: false,
            ticket_layout: LinearViewLayout::List,
            board_column: 0,
            board_rows: [0; 5],
            board_scroll: [0; 5],
            board_detail_open: false,
            board_last_click: None,
        }
    }

    pub(crate) fn object_view(&self, key: &WorkItemKey) -> ObjectViewState {
        self.object_views.get(key).cloned().unwrap_or_default()
    }

    pub(crate) fn object_view_mut(&mut self, key: WorkItemKey) -> &mut ObjectViewState {
        self.object_views.entry(key).or_default()
    }
}

impl AppState {
    pub(crate) fn swap_symphony_detail(&mut self, other: &mut Option<SymphonyDetail>) {
        std::mem::swap(&mut self.symphony_detail, other);
    }

    pub(crate) fn toggle_symphony(&mut self) {
        if self.symphony_detail.is_some() {
            self.symphony_detail = None;
        } else {
            self.symphony_detail = Some(SymphonyDetail {
                snapshot: self.symphony_snapshot.clone(),
                selected: 0,
                observed_at: std::time::SystemTime::now(),
            });
        }
    }

    /// Bind the dock's Symphony surface to `workflow` and bring it up.
    pub(crate) fn bind_symphony_dock(&mut self, workflow: &crate::symphony::Workflow) {
        self.dock_symphony = Some(SymphonyDockSelection {
            workflow_id: workflow.workflow_id.clone(),
            run_id: workflow.run_id.clone(),
        });
        self.dock_collapsed = false;
        self.open_dock_surface(DockSurface::Symphony);
    }

    /// Bind the Symphony surface to `workflow` on the pane that currently has
    /// focus. Dock tabs are per pane and only move at the `compute_view()`
    /// reconcile, so a surface opened right after a focus change would land on
    /// the pane focus just left. Reconcile first so it opens on the new one.
    pub(crate) fn bind_symphony_dock_to_focused_pane(
        &mut self,
        workflow: &crate::symphony::Workflow,
    ) {
        self.reconcile_dock_context_tabs();
        self.bind_symphony_dock(workflow);
    }

    /// The workflow the dock surface is bound to, as it stands in the current
    /// snapshot. `None` once the job closes and leaves the snapshot.
    pub(crate) fn dock_symphony_workflow(&self) -> Option<&crate::symphony::Workflow> {
        let selection = self.dock_symphony.as_ref()?;
        self.symphony_snapshot.workflows.iter().find(|workflow| {
            workflow.workflow_id == selection.workflow_id && workflow.run_id == selection.run_id
        })
    }

    /// The dashboard URL when `(column, row)` lands on the Symphony surface's
    /// dashboard link. Uses the same rect the surface draws, so a click can
    /// never open a link that was not on screen.
    pub(crate) fn dock_symphony_dashboard_click(&self, column: u16, row: u16) -> Option<String> {
        let rect = crate::ui::dock_symphony_dashboard_link_rect(self, self.view.dock_body_rect)?;
        (row == rect.y && column >= rect.x && column < rect.right())
            .then(|| crate::ui::dock_symphony_dashboard_url(self))
            .flatten()
    }

    pub(crate) fn clear_symphony(&mut self) {
        self.symphony_detail = None;
    }

    pub(crate) fn swap_work_view(&mut self, other: &mut Option<WorkViewState>) {
        std::mem::swap(&mut self.work_view, other);
    }

    pub(crate) fn clear_work_view(&mut self) {
        self.work_view = None;
    }

    pub(crate) fn swap_usage_view(&mut self, other: &mut Option<UsageViewState>) {
        std::mem::swap(&mut self.usage_view, other);
    }

    pub(crate) fn toggle_usage_view(&mut self) {
        if self.usage_view.is_some() {
            self.usage_view = None;
            return;
        }
        self.work_view = None;
        self.board_view = None;
        self.usage_view = Some(UsageViewState::new(self.usage_snapshot.clone()));
        self.request_usage_scan = true;
        self.follow_view(SidebarGroupMode::Repo);
    }

    pub(crate) fn clear_usage_view(&mut self) {
        self.usage_view = None;
    }

    pub(crate) fn swap_loop_run_history_detail(
        &mut self,
        other: &mut Option<LoopRunHistoryDetail>,
    ) {
        std::mem::swap(&mut self.loop_run_history_detail, other);
    }

    pub(crate) fn swap_aloop_run_detail(&mut self, other: &mut Option<AloopRunDetail>) {
        std::mem::swap(&mut self.aloop_run_detail, other);
    }

    pub(crate) fn show_loop_run_history(
        &mut self,
        loop_id: String,
        history: crate::loop_runs::RunHistory,
        observed_at: std::time::SystemTime,
    ) {
        self.loop_run_history_detail = Some(LoopRunHistoryDetail {
            loop_id,
            history,
            observed_at,
            producer_host: None,
        });
    }

    pub(crate) fn clear_loop_run_history(&mut self) {
        self.loop_run_history_detail = None;
    }

    pub(crate) fn show_aloop_run_detail(
        &mut self,
        loop_name: String,
        host: String,
        run: std::sync::Arc<crate::aloop::RunRecord>,
    ) {
        self.aloop_run_detail = Some(AloopRunDetail {
            loop_name,
            host,
            run,
            observed_at: std::time::SystemTime::now(),
        });
    }

    pub(crate) fn clear_aloop_run_detail(&mut self) {
        self.aloop_run_detail = None;
    }

    pub(crate) fn aloop_projection(
        &self,
    ) -> Option<std::sync::Arc<crate::aloop::SectionProjection>> {
        self.aloop_projection
            .clone()
            .or_else(|| crate::aloop::project(&self.fleet_snapshot).map(std::sync::Arc::new))
    }

    /// Reveal the scratchpad without spawning an editor: the dock opens if it was
    /// collapsed and selects the Note tab. Reading the note is the common case;
    /// editing it is the deliberate one.
    pub(crate) fn show_scratchpad_tab(&mut self) {
        self.dock_collapsed = false;
        self.open_dock_surface(DockSurface::Scratchpad);
        self.dock_editor_focused = false;
        self.dock_diff_focused = false;
        self.dock_files_focused = false;
        self.dock_agents_focused = false;
        self.dock_hosts_focused = false;
    }

    /// Drop dock keyboard focus when a terminal pane takes input.
    ///
    /// The key dispatch chain runs every dock handler before the
    /// `Mode::Terminal` arm and gates each one on its own `dock_*_focused`
    /// flag. Nothing lowered those flags when focus moved back to a pane, so a
    /// flag left set by an earlier dock interaction kept consuming bare letters
    /// the operator meant for the shell: `a`/`m`/`x` on Home, `l`/`c` on a pull
    /// request, a surface shortcut in the chooser.
    pub(crate) fn release_dock_focus_to_pane(&mut self) {
        self.sidebar_focused = false;
        self.dock_home_focused = false;
        self.dock_pr_focused = false;
        self.dock_linear_focused = false;
        self.dock_diff_focused = false;
        self.dock_files_focused = false;
        self.dock_agents_focused = false;
        self.dock_hosts_focused = false;
        self.dock_editor_focused = false;
        self.dock_chooser_focused = false;
    }

    /// Hand the keyboard to a pane the operator clicked into.
    ///
    /// Clicking pane content is the unambiguous "I am typing here now" signal,
    /// so it also drops the sidebar selection. That selection is set by clicking
    /// a sidebar row and is the only gate on the sidebar work-group and object
    /// menu keys, which would otherwise answer `m`, `n`, and Enter while the
    /// operator types into the shell.
    pub(crate) fn release_surface_focus_to_pane(&mut self) {
        self.release_dock_focus_to_pane();
        self.sidebar_selected_work_group = None;
        self.sidebar_selected_remote_agent = None;
        self.sidebar_object_menu = None;
        self.sidebar_selected_settled = None;
        self.sidebar_settled_menu_target = None;
        self.sidebar_settled_menu_delete_armed = false;
    }

    /// Hand the keyboard to a terminal-area surface the sidebar just opened.
    ///
    /// `input_owner` answers `Sidebar` ahead of every terminal-area surface, so
    /// a view opened from a sidebar row inherits a sidebar that still owns the
    /// keyboard. The sidebar arm consumes Esc and returns, the view's own key
    /// handler never runs, and the operator has no way to close what they just
    /// opened.
    pub(crate) fn release_sidebar_focus_to_surface(&mut self) {
        self.sidebar_focused = false;
    }

    pub(crate) fn toggle_loop_run_history(&mut self) {
        if self.loop_run_history_detail.is_some() {
            self.clear_loop_run_history();
            return;
        }
        self.show_loop_run_history(
            crate::loop_runs::ALL_LOOPS_ID.to_string(),
            crate::loop_runs::RunHistory {
                runs: crate::loop_runs::runs_for_loop(&self.loop_run_history, None),
                skipped_lines: self.loop_run_history.skipped_lines,
            },
            std::time::SystemTime::now(),
        );
    }

    /// MAT-159 AC8: a loop header in the Aloops sidebar section opens the
    /// existing MAT-126 run-history table filtered to that loop.
    pub(crate) fn open_aloop_loop_history(&mut self, loop_name: &str) {
        self.clear_aloop_run_detail();
        self.release_sidebar_focus_to_surface();
        let producer_host = self
            .fleet_snapshot
            .aloop
            .as_ref()
            .map(|producer| producer.host.clone());
        let history = self
            .fleet_snapshot
            .aloop
            .as_ref()
            .map(|producer| {
                let runs = producer
                    .data
                    .loops
                    .iter()
                    .find(|loop_runs| loop_runs.loop_name == loop_name)
                    .map(|loop_runs| {
                        loop_runs
                            .runs
                            .iter()
                            .map(|run| crate::loop_runs::RunRecord {
                                run_id: run.at.clone(),
                                skill: "aloop".to_string(),
                                session: Some(producer.host.clone()),
                                pr: None,
                                ticket: None,
                                loop_id: Some(loop_name.to_string()),
                                start: run.at.clone(),
                                end: None,
                                wall_min: Some(run.duration_ms as f64 / 60_000.0),
                                blocked_min: None,
                                gates: Vec::new(),
                                human_touches: None,
                                touches_by_type: std::collections::BTreeMap::new(),
                                interrupted_focus: None,
                                review_rounds: None,
                                out_tokens: None,
                                outcome: crate::loop_runs::RunOutcome::Terminal(if run.exit == 0 {
                                    "ok".to_string()
                                } else {
                                    format!("exit {}", run.exit)
                                }),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                crate::loop_runs::RunHistory {
                    runs,
                    skipped_lines: producer
                        .data
                        .loops
                        .iter()
                        .find(|loop_runs| loop_runs.loop_name == loop_name)
                        .map_or(0, |loop_runs| loop_runs.skipped_lines),
                }
            })
            .unwrap_or_default();
        self.show_loop_run_history(loop_name.to_string(), history, std::time::SystemTime::now());
        if let Some(detail) = self.loop_run_history_detail.as_mut() {
            detail.producer_host = producer_host;
        }
    }

    /// MAT-159 AC7: selecting a run in the Aloops section opens its recorded
    /// log excerpt. Resolves the record out of the fleet snapshot so the view
    /// never reads the producer host from the input path.
    pub(crate) fn open_aloop_run_log(&mut self, loop_name: &str, at: &str) -> bool {
        let Some(snapshot) = self.fleet_snapshot.aloop.as_ref() else {
            return false;
        };
        if !snapshot.reachable() {
            return false;
        }
        let Some(run) = snapshot
            .data
            .loops
            .iter()
            .find(|loop_runs| loop_runs.loop_name == loop_name)
            .and_then(|loop_runs| loop_runs.runs.iter().find(|run| run.at == at))
        else {
            return false;
        };
        let host = snapshot.host.clone();
        let run = std::sync::Arc::clone(run);
        self.release_sidebar_focus_to_surface();
        self.show_aloop_run_detail(loop_name.to_string(), host, run);
        true
    }
}

impl AppState {
    pub(crate) fn set_dock_width(&mut self, width: u16) {
        if self.dock_width != width {
            self.dock_width = width;
            self.dock_width_persistence_request = Some(width);
        }
    }

    pub(crate) fn take_dock_width_persistence_request(&mut self) -> Option<u16> {
        self.dock_width_persistence_request.take()
    }

    pub(crate) fn take_notepad_height_persistence_request(&mut self) -> Option<u16> {
        self.notepad_height_persistence_request.take()
    }

    pub(crate) fn set_sidebar_group_mode(&mut self, mode: SidebarGroupMode) {
        if self.sidebar_group_mode == mode {
            self.sidebar_group_menu_open = false;
            return;
        }
        self.sidebar_group_mode = mode;
        self.follow_view(mode);
        self.sidebar_group_menu_selected = mode.view_index();
        self.sidebar_group_menu_open = false;
        self.sidebar_group_mode_persistence_request = Some(mode);
        self.sidebar_view_scan_request = true;
        self.sidebar_selected_work_group = None;
        self.sidebar_object_menu = None;
        self.sidebar_selected_settled = None;
        self.sidebar_settled_menu_target = None;
        self.sidebar_settled_menu_delete_armed = false;
        self.sidebar_filter_menu_open = false;
        self.workspace_scroll = 0;
        self.mark_sidebar_projection_changed();
    }

    pub(crate) fn cycle_sidebar_group_mode(&mut self) {
        self.set_sidebar_group_mode(self.sidebar_group_mode.next());
    }

    pub(crate) fn take_sidebar_group_mode_persistence_request(
        &mut self,
    ) -> Option<SidebarGroupMode> {
        self.sidebar_group_mode_persistence_request.take()
    }

    pub(crate) fn take_sidebar_view_scan_request(&mut self) -> bool {
        std::mem::take(&mut self.sidebar_view_scan_request)
    }

    pub(crate) fn set_sidebar_work_filter(&mut self, filter: SidebarWorkFilter) {
        self.sidebar_filter_menu_open = false;
        if self.sidebar_work_filter == filter {
            return;
        }
        self.sidebar_work_filter = filter.clone();
        self.sidebar_work_filter_persistence_request = Some(filter);
        self.sidebar_selected_work_group = None;
        self.sidebar_object_menu = None;
        self.workspace_scroll = 0;
        self.mark_sidebar_projection_changed();
    }

    pub(crate) fn take_sidebar_work_filter_persistence_request(
        &mut self,
    ) -> Option<SidebarWorkFilter> {
        self.sidebar_work_filter_persistence_request.take()
    }

    /// The sort one sidebar group is explicitly set to, `Default` when the
    /// group never chose one. Inheritance from an enclosing group is resolved
    /// by the row builders, which walk parent-first.
    pub(crate) fn sidebar_group_sort(&self, key: &str) -> SidebarSortMode {
        self.sidebar_group_sorts
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn set_sidebar_group_sort(&mut self, key: String, mode: SidebarSortMode) {
        self.sidebar_sort_menu = None;
        if self.sidebar_group_sort(key.as_str()) == mode {
            return;
        }
        if mode == SidebarSortMode::Default {
            self.sidebar_group_sorts.remove(&key);
        } else {
            self.sidebar_group_sorts.insert(key.clone(), mode);
        }
        self.sidebar_group_sort_persistence_request = Some((key, mode));
        self.mark_sidebar_projection_changed();
    }

    pub(crate) fn take_sidebar_group_sort_persistence_request(
        &mut self,
    ) -> Option<(String, SidebarSortMode)> {
        self.sidebar_group_sort_persistence_request.take()
    }

    pub(crate) fn take_sidebar_group_collapsed_persistence_request(
        &mut self,
    ) -> Option<(String, bool)> {
        self.sidebar_group_collapsed_persistence_request.take()
    }

    pub(crate) fn request_sidebar_refresh(&mut self) -> bool {
        if self.sidebar_refreshing {
            return false;
        }
        self.sidebar_refresh_requested = true;
        self.sidebar_refreshing = true;
        true
    }

    pub(crate) fn swap_sidebar_presentation(&mut self, other: &mut SidebarPresentationState) {
        std::mem::swap(&mut self.sidebar_focused, &mut other.focused);
        std::mem::swap(&mut self.client_focus_intent, &mut other.focus_intent);
        std::mem::swap(
            &mut self.sidebar_presentation.expanded_workspace_ids,
            &mut other.expanded_workspace_ids,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.known_workspace_ids,
            &mut other.known_workspace_ids,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.revealed_workspace_id,
            &mut other.revealed_workspace_id,
        );
        std::mem::swap(&mut self.workspace_scroll, &mut other.workspace_scroll);
        std::mem::swap(
            &mut self.mobile_switcher_scroll,
            &mut other.mobile_switcher_scroll,
        );
        std::mem::swap(
            &mut self.collapsed_sidebar_groups,
            &mut other.collapsed_groups,
        );
        std::mem::swap(
            &mut self.sidebar_presentation.projection_revision,
            &mut other.projection_revision,
        );
        std::mem::swap(&mut self.sidebar_group_mode, &mut other.group_mode);
        std::mem::swap(
            &mut self.sidebar_group_menu_open,
            &mut other.group_menu_open,
        );
        std::mem::swap(
            &mut self.sidebar_group_menu_selected,
            &mut other.group_menu_selected,
        );
        std::mem::swap(&mut self.sidebar_work_filter, &mut other.work_filter);
        std::mem::swap(
            &mut self.sidebar_filter_menu_open,
            &mut other.filter_menu_open,
        );
        std::mem::swap(
            &mut self.sidebar_filter_menu_selected,
            &mut other.filter_menu_selected,
        );
        std::mem::swap(&mut self.sidebar_search_active, &mut other.search_active);
        std::mem::swap(&mut self.sidebar_new_menu, &mut other.new_menu);
        std::mem::swap(&mut self.sidebar_new_thread, &mut other.new_thread);
        std::mem::swap(&mut self.sidebar_project_menu, &mut other.project_menu);
        std::mem::swap(
            &mut self.sidebar_selected_work_group,
            &mut other.selected_work_group,
        );
        std::mem::swap(&mut self.sidebar_object_menu, &mut other.object_menu);
        std::mem::swap(&mut self.sidebar_sort_menu, &mut other.sort_menu);
        std::mem::swap(
            &mut self.sidebar_subgroup_picker,
            &mut other.subgroup_picker,
        );
        std::mem::swap(&mut self.sidebar_pod_picker, &mut other.pod_picker);
        std::mem::swap(&mut self.sidebar_group_sorts, &mut other.group_sorts);
        std::mem::swap(
            &mut self.sidebar_unassigned_expanded_views,
            &mut other.unassigned_expanded_views,
        );
        std::mem::swap(
            &mut self.sidebar_selected_settled,
            &mut other.selected_settled,
        );
        std::mem::swap(
            &mut self.sidebar_selected_remote_agent,
            &mut other.selected_remote_agent,
        );
        std::mem::swap(&mut self.client_overlay, &mut other.overlay.kind);

        std::mem::swap(&mut self.context_menu, &mut other.overlay.context_menu);
        std::mem::swap(&mut self.sidebar_snooze, &mut other.overlay.snooze);
        std::mem::swap(&mut self.rename_target, &mut other.overlay.rename_target);
        std::mem::swap(
            &mut self.confirm_close_workspace_id,
            &mut other.overlay.confirm_close_workspace_id,
        );
        std::mem::swap(
            &mut self.confirm_close_remote_agent_ref,
            &mut other.overlay.confirm_close_remote_agent_ref,
        );
        std::mem::swap(
            &mut self.worktree_create,
            &mut other.overlay.worktree_create,
        );
        std::mem::swap(&mut self.worktree_open, &mut other.overlay.worktree_open);
        std::mem::swap(
            &mut self.worktree_remove,
            &mut other.overlay.worktree_remove,
        );
        std::mem::swap(
            &mut self.sidebar_settled_menu_target,
            &mut other.overlay.settled_menu_target,
        );
        std::mem::swap(
            &mut self.sidebar_settled_menu_selected,
            &mut other.overlay.settled_menu_selected,
        );
        std::mem::swap(
            &mut self.sidebar_settled_menu_delete_armed,
            &mut other.overlay.settled_menu_delete_armed,
        );
        std::mem::swap(&mut self.agent_picker, &mut other.overlay.agent_picker);
        std::mem::swap(&mut self.name_input, &mut other.overlay.name_input);
        std::mem::swap(
            &mut self.name_input_replace_on_type,
            &mut other.overlay.name_input_replace_on_type,
        );
        std::mem::swap(
            &mut self.creating_new_tab,
            &mut other.overlay.creating_new_tab,
        );
        std::mem::swap(
            &mut self.pending_workspace_create_cwd,
            &mut other.overlay.pending_workspace_create_cwd,
        );
        std::mem::swap(
            &mut self.rename_tab_prefill,
            &mut other.overlay.rename_tab_prefill,
        );
    }

    pub(crate) fn server_mode(&self) -> Mode {
        self.server_interaction.mode
    }

    pub(crate) fn set_server_mode(&mut self, mode: Mode) {
        self.server_interaction = ServerInteractionState::new(mode);
        self.client_focus_intent = ClientFocusIntent::FollowShared;
    }

    pub(crate) fn effective_interaction_mode(&self) -> Mode {
        self.client_overlay
            .mode()
            .unwrap_or(match self.client_focus_intent {
                ClientFocusIntent::FollowShared => self.server_interaction.mode,
                ClientFocusIntent::Pane => Mode::Terminal,
            })
    }

    pub(crate) fn focus_client_on_pane(&mut self) {
        self.client_focus_intent = ClientFocusIntent::Pane;
        self.release_surface_focus_to_pane();
    }

    pub(crate) fn focus_client_on_sidebar(&mut self) {
        self.client_focus_intent = ClientFocusIntent::FollowShared;
        self.release_dock_focus_to_pane();
        self.sidebar_collapsed = false;
        self.sidebar_focused = true;
    }

    pub(crate) fn toggle_sidebar_collapsed(&mut self) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        if self.sidebar_collapsed {
            self.sidebar_focused = false;
        }
    }

    pub(crate) fn input_owner(&self) -> InputOwner {
        if let Some(owner) = ClientInputOwnerState::from_app(self).resolve() {
            return InputOwner::Client(owner);
        }
        // A focused note editor or read-only dock tab owns input ahead of
        // shared modes and underlying surfaces. Config reloads can hide Notes
        // without clearing focus, so use the panel's current visibility.
        let readonly_notepad_tab =
            self.notepad.context_active || self.notepad.agent_tab || self.notepad.usage_tab;
        if self.notepad.focused
            && (self.notepad.enabled || readonly_notepad_tab)
            && (!self.sidebar_sections_layout || self.sidebar_areas.notes)
        {
            return InputOwner::Notepad;
        }
        if self.add_project_active() {
            return InputOwner::AddProject;
        }
        if self.sidebar_focused && !self.sidebar_collapsed {
            return InputOwner::Sidebar;
        }
        if self.client_focus_intent == ClientFocusIntent::Pane {
            return InputOwner::Pane;
        }
        if let Some(owner) = ServerInputOwner::from_mode(self.server_mode()) {
            return InputOwner::Server(owner);
        }
        if self.popup_pane.is_some() {
            return InputOwner::Popup;
        }
        match self.terminal_area_surface() {
            TerminalAreaSurface::Board => return InputOwner::Surface(SurfaceInputOwner::Board),
            TerminalAreaSurface::EditorPreview => {
                return InputOwner::Surface(SurfaceInputOwner::EditorPreview)
            }
            TerminalAreaSurface::Symphony(_) => {
                return InputOwner::Surface(SurfaceInputOwner::Symphony)
            }
            TerminalAreaSurface::LoopRunHistory(_) => {
                return InputOwner::Surface(SurfaceInputOwner::LoopRunHistory)
            }
            TerminalAreaSurface::AloopRunLog(_) => {
                return InputOwner::Surface(SurfaceInputOwner::AloopRunLog)
            }
            TerminalAreaSurface::Usage => return InputOwner::Surface(SurfaceInputOwner::Usage),
            TerminalAreaSurface::Work => return InputOwner::Surface(SurfaceInputOwner::Work),
            TerminalAreaSurface::DockObjectPreview => {
                return InputOwner::Surface(SurfaceInputOwner::DockObjectPreview)
            }
            TerminalAreaSurface::Home => return InputOwner::Surface(SurfaceInputOwner::Home),
            TerminalAreaSurface::Inbox(_) => return InputOwner::Surface(SurfaceInputOwner::Inbox),
            TerminalAreaSurface::Tab => {}
            TerminalAreaSurface::Empty => return InputOwner::None,
        }
        let dock_owner = if self.dock_collapsed {
            None
        } else if self.dock_editor_focused && self.dock_tab == Some(DockSurface::Editor) {
            Some(DockInputOwner::Editor)
        } else if self.dock_home_focused && self.dock_tab == Some(DockSurface::Home) {
            Some(DockInputOwner::Home)
        } else if self.dock_pr_focused && self.dock_tab == Some(DockSurface::Pr) {
            Some(DockInputOwner::PullRequest)
        } else if self.dock_linear_focused && self.dock_tab == Some(DockSurface::Linear) {
            Some(DockInputOwner::Linear)
        } else if self.dock_diff_focused && self.dock_tab == Some(DockSurface::Diff) {
            Some(DockInputOwner::Diff)
        } else if self.dock_files_focused && self.dock_tab == Some(DockSurface::Files) {
            Some(DockInputOwner::Files)
        } else if self.dock_agents_focused && self.dock_tab == Some(DockSurface::Agents) {
            Some(DockInputOwner::Agents)
        } else if self.dock_hosts_focused && self.dock_tab == Some(DockSurface::Hosts) {
            Some(DockInputOwner::Hosts)
        } else if self.dock_chooser_focused && self.dock_tab.is_none() {
            Some(DockInputOwner::Chooser)
        } else {
            None
        };
        if let Some(owner) = dock_owner {
            return InputOwner::Dock(owner);
        }
        InputOwner::Pane
    }

    pub(crate) fn input_owner_with_pomodoro(&self, prompt_visible: bool) -> InputOwner {
        if prompt_visible {
            InputOwner::Pomodoro
        } else {
            self.input_owner()
        }
    }

    pub(crate) fn open_client_overlay(&mut self, overlay: ClientOverlay) {
        self.client_overlay = overlay;
    }

    pub(crate) fn close_client_overlay(&mut self) {
        self.client_overlay = ClientOverlay::None;
        self.confirm_close_remote_agent_ref = None;
    }

    pub(crate) fn reconcile_client_modal_target(&mut self) {
        let rename_target_exists = match self.rename_target.as_ref() {
            Some(RenameTarget::Workspace { workspace_id }) => self
                .workspaces
                .iter()
                .any(|workspace| workspace.id == *workspace_id),
            Some(RenameTarget::Tab {
                workspace_id,
                tab_id,
            }) => self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == *workspace_id)
                .is_some_and(|workspace| {
                    workspace.tabs.iter().any(|tab| {
                        crate::workspace::public_tab_id_for_number(workspace_id, tab.number)
                            == *tab_id
                    })
                }),
            Some(RenameTarget::Pane {
                workspace_id,
                pane_id,
            }) => self
                .workspaces
                .iter()
                .find(|workspace| workspace.id == *workspace_id)
                .is_some_and(|workspace| workspace.pane_state(*pane_id).is_some()),
            Some(RenameTarget::Pod { record }) => self
                .local_group_snapshot
                .iter()
                .chain(
                    self.fleet_snapshot
                        .group_catalogs
                        .iter()
                        .filter_map(|catalog| catalog.snapshot.as_ref()),
                )
                .flat_map(|snapshot| snapshot.groups.iter())
                .any(|candidate| {
                    candidate.id == record.id && candidate.revision == record.revision
                }),
            None => self.pending_workspace_create_cwd.is_some() || self.creating_new_tab,
        };
        if matches!(
            self.client_overlay,
            ClientOverlay::RenameWorkspace | ClientOverlay::RenameTab | ClientOverlay::RenamePane
        ) && !rename_target_exists
        {
            self.rename_target = None;
            self.rename_pane_target = None;
            self.rename_tab_prefill = None;
            self.name_input.clear();
            self.name_input_replace_on_type = false;
            self.close_client_overlay();
        }

        if self.client_overlay == ClientOverlay::ConfirmClose
            && self.confirm_close_remote_agent_ref.is_none()
            && self
                .confirm_close_workspace_id
                .as_ref()
                .is_none_or(|target| {
                    !self
                        .workspaces
                        .iter()
                        .any(|workspace| workspace.id == *target)
                })
        {
            self.confirm_close_workspace_id = None;
            self.close_client_overlay();
        }

        if self.sidebar_snooze.as_ref().is_some_and(|snooze| {
            self.sidebar_lifecycle_target_snoozed(&snooze.target)
                .is_none()
        }) {
            self.sidebar_snooze = None;
        }
    }

    pub(crate) fn sidebar_lifecycle_target_snoozed(
        &self,
        target: &SidebarPaneLifecycleTarget,
    ) -> Option<bool> {
        match target {
            SidebarPaneLifecycleTarget::Local(target) => {
                let ws_idx = self
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == target.workspace_id)?;
                let pane = self.workspaces.get(ws_idx)?.pane_state(target.pane_id)?;
                let snoozed = pane.snoozed_until().is_some();
                (snoozed || self.pane_can_snooze(ws_idx, target.pane_id)).then_some(snoozed)
            }
            SidebarPaneLifecycleTarget::Remote(agent_ref) => self
                .remote_agent_panel_entries
                .iter()
                .find(|entry| {
                    entry.agent_ref == *agent_ref
                        && entry.host_fresh
                        && !entry.settled
                        && !crate::ui::sidebar::entry_needs_human_attention(entry)
                })
                .map(|entry| entry.snoozed_until.is_some()),
        }
    }

    /// Open `surface` as a tab and make it active. Already-open surfaces are
    /// only reactivated, so the strip order never shuffles under the user.
    pub(crate) fn open_dock_surface(&mut self, surface: DockSurface) {
        self.dock_surface_override = true;
        let object = match surface {
            DockSurface::Pr | DockSurface::Linear | DockSurface::Missive => self
                .dock_context_objects
                .iter()
                .find(|object| object.surface == surface)
                .cloned(),
            _ => None,
        };
        if let Some(object) = object {
            self.open_dock_object(object, DockTabOrigin::User);
        } else {
            self.select_dock_surface(surface);
        }
    }

    fn select_dock_surface(&mut self, surface: DockSurface) {
        self.align_dock_tab_bindings();
        let index = self
            .dock_open_surfaces
            .iter()
            .zip(&self.dock_tab_bindings)
            .position(|(open, binding)| *open == surface && binding.is_none())
            .unwrap_or_else(|| {
                let index = self.dock_open_surfaces.len();
                self.dock_open_surfaces.push(surface);
                self.dock_tab_bindings.push(None);
                index
            });
        self.select_dock_tab_index(index);
    }

    pub(crate) fn open_dock_object(&mut self, object: DockObjectRef, origin: DockTabOrigin) {
        self.dock_object_preview = None;
        self.align_dock_tab_bindings();
        let existing = self.dock_tab_bindings.iter().position(|binding| {
            binding
                .as_ref()
                .is_some_and(|binding| binding.object == object)
        });
        let index = existing.unwrap_or_else(|| {
            let index = self.dock_open_surfaces.len();
            self.dock_open_surfaces.push(object.surface);
            self.dock_tab_bindings
                .push(Some(DockTabBinding { object, origin }));
            index
        });
        if origin == DockTabOrigin::User {
            if let Some(binding) = self
                .dock_tab_bindings
                .get_mut(index)
                .and_then(Option::as_mut)
            {
                binding.origin = DockTabOrigin::User;
            }
        }
        self.select_dock_tab_index(index);
    }

    fn align_dock_tab_bindings(&mut self) {
        self.dock_tab_bindings
            .resize(self.dock_open_surfaces.len(), None);
        self.dock_tab_bindings
            .truncate(self.dock_open_surfaces.len());
    }

    pub(crate) fn select_dock_tab_index(&mut self, index: usize) {
        self.align_dock_tab_bindings();
        let Some(surface) = self.dock_open_surfaces.get(index).copied() else {
            return;
        };
        self.dock_active_tab_index = Some(index);
        self.dock_tab = Some(surface);
        self.dock_surface_menu = None;
        self.dock_chooser_focused = false;
        if surface == DockSurface::Editor {
            self.retry_dock_editor();
        }
    }

    pub(crate) fn active_dock_object(&self, surface: DockSurface) -> Option<&DockObjectRef> {
        let index = self.active_dock_tab_index()?;
        let binding = self.dock_tab_bindings.get(index)?.as_ref()?;
        (binding.object.surface == surface).then_some(&binding.object)
    }

    /// Object rendered by a dock-compatible host. A collapsed dock can lend
    /// the same rich renderer to the pane area without creating a dock tab.
    pub(crate) fn presented_dock_object(&self, surface: DockSurface) -> Option<&DockObjectRef> {
        self.dock_collapsed
            .then_some(self.dock_object_preview.as_ref())
            .flatten()
            .filter(|object| object.surface == surface)
            .or_else(|| self.active_dock_object(surface))
    }

    pub(crate) fn active_dock_tab_index(&self) -> Option<usize> {
        if let Some(index) = self.dock_active_tab_index {
            if self.dock_open_surfaces.get(index).copied() == self.dock_tab {
                return Some(index);
            }
        }
        let active = self.dock_tab?;
        self.dock_open_surfaces
            .iter()
            .position(|surface| *surface == active)
    }

    pub(crate) fn dock_tab_label(&self, index: usize) -> String {
        let Some(surface) = self.dock_open_surfaces.get(index).copied() else {
            return String::new();
        };
        let Some(object) = self
            .dock_tab_bindings
            .get(index)
            .and_then(Option::as_ref)
            .map(|binding| &binding.object)
        else {
            return surface.label().to_string();
        };
        self.dock_object_label(object)
    }

    /// The short name of a bound object: `#312`, `SCA-3296`, a trimmed Missive
    /// subject. The dock tab strip and the status row share it so one link
    /// reads the same wherever it is offered.
    pub(crate) fn dock_object_label(&self, object: &DockObjectRef) -> String {
        match object.surface {
            DockSurface::Pr => object
                .key
                .rsplit('/')
                .next()
                .and_then(|number| number.parse::<u64>().ok())
                .map(|number| format!("#{number}"))
                .unwrap_or_else(|| "pr".into()),
            DockSurface::Linear => object.key.clone(),
            DockSurface::Missive => self
                .work_index_snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot.conversations.iter().find(|conversation| {
                        conversation.app_url == object.key
                            || conversation.web_url == object.key
                            || object.key.ends_with(&conversation.id)
                    })
                })
                .map(|conversation| crate::ui::text::truncate_end(&conversation.subject, 12))
                .unwrap_or_else(|| crate::ui::text::truncate_end(&object.key, 12)),
            surface => surface.label().to_string(),
        }
    }

    pub(crate) fn dock_tab_title(&self, index: usize) -> String {
        let Some(binding) = self.dock_tab_bindings.get(index).and_then(Option::as_ref) else {
            return self
                .dock_open_surfaces
                .get(index)
                .map(|surface| surface.title().to_string())
                .unwrap_or_default();
        };
        match binding.object.surface {
            DockSurface::Pr => {
                self.work_index_snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot.items.iter().find(|item| {
                            item.pr_url.as_deref() == Some(binding.object.key.as_str())
                        })
                    })
                    .and_then(|item| item.pr_title.clone())
                    .unwrap_or_else(|| binding.object.key.clone())
            }
            DockSurface::Linear => self
                .work_index_snapshot
                .as_ref()
                .into_iter()
                .flat_map(|snapshot| snapshot.items.iter())
                .flat_map(|item| item.ticket_details.iter())
                .find(|ticket| ticket.identifier.eq_ignore_ascii_case(&binding.object.key))
                .and_then(|ticket| ticket.title.clone())
                .unwrap_or_else(|| binding.object.key.clone()),
            DockSurface::Missive => self
                .work_index_snapshot
                .as_ref()
                .and_then(|snapshot| {
                    snapshot.conversations.iter().find(|conversation| {
                        conversation.app_url == binding.object.key
                            || conversation.web_url == binding.object.key
                            || binding.object.key.ends_with(&conversation.id)
                    })
                })
                .map(|conversation| conversation.subject.clone())
                .unwrap_or_else(|| binding.object.key.clone()),
            surface => surface.title().to_string(),
        }
    }

    fn focused_dock_context_objects(&self) -> Vec<DockObjectRef> {
        let Some(workspace) = self.active.and_then(|index| self.workspaces.get(index)) else {
            return Vec::new();
        };
        let Some(context) = workspace
            .focused_pane_id()
            .and_then(|pane_id| workspace.terminal_id(pane_id))
            .and_then(|terminal_id| self.terminals.get(terminal_id))
            .map(crate::terminal::TerminalState::effective_work_context)
        else {
            return Vec::new();
        };
        let mut objects = Vec::new();
        if let Some(primary) = crate::ui::dock::pr::primary_pr_url(context) {
            objects.push(DockObjectRef {
                surface: DockSurface::Pr,
                key: primary.to_string(),
            });
        }
        objects.extend(
            context
                .pr_urls
                .iter()
                .filter(|url| Some(url.as_str()) != crate::ui::dock::pr::primary_pr_url(context))
                .cloned()
                .map(|key| DockObjectRef {
                    surface: DockSurface::Pr,
                    key,
                }),
        );
        objects.extend(context.ticket_ids.iter().cloned().map(|key| DockObjectRef {
            surface: DockSurface::Linear,
            key,
        }));
        objects.extend(
            context
                .missive_urls
                .iter()
                .cloned()
                .map(|key| DockObjectRef {
                    surface: DockSurface::Missive,
                    key,
                }),
        );
        objects
    }

    fn save_current_pane_dock_tabs(&mut self) {
        let Some(target) = self.dock_followed_pane.clone() else {
            return;
        };
        self.align_dock_tab_bindings();
        self.dock_pane_tabs.insert(
            target,
            PaneDockTabs {
                open_surfaces: self.dock_open_surfaces.clone(),
                bindings: self.dock_tab_bindings.clone(),
                active_index: self.active_dock_tab_index(),
                context_objects: self.dock_context_objects.clone(),
                suppressed_context: self.dock_suppressed_context.clone(),
            },
        );
    }

    /// Reconcile object tabs at the existing `compute_view()` mutation boundary.
    /// Focus, inferred-context, and declared-context changes all arrive here
    /// before geometry or rendering consumes the presentation state.
    /// Hand the ticketless Linear tab over to the object the attach created.
    ///
    /// The picker tab carries no binding, so context reconciliation adds a
    /// second, bound Linear tab beside it and leaves the operator looking at
    /// the empty one. Reconcile first so the bound tab exists, then drop the
    /// unbound tab and select the replacement. Dropping it first does not work:
    /// reconciliation re-adds a tab for `dock_tab` when the active index no
    /// longer resolves.
    pub(crate) fn replace_unbound_linear_tab_with_its_object(&mut self, ticket_id: &str) {
        self.reconcile_dock_context_tabs();
        let bound = self
            .dock_open_surfaces
            .iter()
            .zip(&self.dock_tab_bindings)
            .any(|(surface, binding)| {
                *surface == DockSurface::Linear
                    && binding
                        .as_ref()
                        .is_some_and(|binding| binding.object.key.eq_ignore_ascii_case(ticket_id))
            });
        if !bound {
            return;
        }
        if let Some(index) = self
            .dock_open_surfaces
            .iter()
            .zip(&self.dock_tab_bindings)
            .position(|(surface, binding)| *surface == DockSurface::Linear && binding.is_none())
        {
            self.dock_open_surfaces.remove(index);
            self.dock_tab_bindings.remove(index);
        }
        // Select the tab for the ticket that was just chosen. An older
        // user-bound Linear tab can sit ahead of it, and landing on that one
        // would show a different ticket than the operator asked for.
        if let Some(index) = self
            .dock_open_surfaces
            .iter()
            .zip(&self.dock_tab_bindings)
            .position(|(surface, binding)| {
                *surface == DockSurface::Linear
                    && binding
                        .as_ref()
                        .is_some_and(|binding| binding.object.key.eq_ignore_ascii_case(ticket_id))
            })
        {
            self.select_dock_tab_index(index);
            self.dock_linear_focused = true;
        }
    }

    pub(crate) fn reconcile_dock_context_tabs(&mut self) {
        if let Some(surface) = self.dock_tab {
            if self.active_dock_tab_index().is_none() {
                self.dock_open_surfaces.push(surface);
                self.dock_tab_bindings.push(None);
                self.dock_active_tab_index = self.dock_open_surfaces.len().checked_sub(1);
            }
        }
        let target = self.current_pane_focus_target();
        if self.dock_followed_pane.is_none() {
            self.dock_followed_pane = target.clone();
        } else if self.dock_followed_pane != target {
            self.save_current_pane_dock_tabs();
            let restored = target
                .as_ref()
                .and_then(|target| self.dock_pane_tabs.get(target).cloned())
                .unwrap_or_else(|| PaneDockTabs {
                    open_surfaces: self.dock_default_surfaces.clone(),
                    bindings: vec![None; self.dock_default_surfaces.len()],
                    active_index: (!self.dock_default_surfaces.is_empty()).then_some(0),
                    ..PaneDockTabs::default()
                });
            self.dock_open_surfaces = restored.open_surfaces;
            self.dock_tab_bindings = restored.bindings;
            self.dock_active_tab_index = restored.active_index;
            self.dock_context_objects = restored.context_objects;
            self.dock_suppressed_context = restored.suppressed_context;
            self.clear_hovered_control();
            self.dock_followed_pane = target;
            self.dock_tab = self
                .dock_active_tab_index
                .and_then(|index| self.dock_open_surfaces.get(index).copied());
        }

        let objects = self.focused_dock_context_objects();
        if objects == self.dock_context_objects {
            return;
        }
        self.align_dock_tab_bindings();
        self.clear_hovered_control();
        let active = self.active_dock_tab_index().and_then(|index| {
            self.dock_open_surfaces.get(index).copied().map(|surface| {
                (
                    surface,
                    self.dock_tab_bindings
                        .get(index)
                        .and_then(Option::as_ref)
                        .map(|binding| binding.object.clone()),
                )
            })
        });
        self.dock_suppressed_context.clear();
        let mut index = 0;
        while index < self.dock_tab_bindings.len() {
            let remove = self.dock_tab_bindings[index]
                .as_ref()
                .is_some_and(|binding| binding.origin == DockTabOrigin::Context);
            if remove {
                self.dock_tab_bindings.remove(index);
                self.dock_open_surfaces.remove(index);
            } else {
                index += 1;
            }
        }
        self.dock_context_objects = objects.clone();
        // Context tabs are the opt-in half of this: without them the objects
        // are still tracked, because the status row names them and a click
        // there opens the dock deliberately.
        if self.open_dock_on_work_link {
            for object in objects {
                if !self.dock_tab_bindings.iter().any(|binding| {
                    binding
                        .as_ref()
                        .is_some_and(|binding| binding.object == object)
                }) {
                    self.dock_open_surfaces.push(object.surface);
                    self.dock_tab_bindings.push(Some(DockTabBinding {
                        object,
                        origin: DockTabOrigin::Context,
                    }));
                }
            }
        }
        let retained_active = active.and_then(|(surface, object)| {
            self.dock_open_surfaces
                .iter()
                .copied()
                .zip(&self.dock_tab_bindings)
                .position(|(candidate_surface, binding)| {
                    candidate_surface == surface
                        && binding.as_ref().map(|binding| &binding.object) == object.as_ref()
                })
        });
        if let Some(index) = retained_active.or_else(|| {
            self.open_dock_on_work_link
                .then(|| {
                    self.dock_tab_bindings
                        .iter()
                        .position(|binding| binding.is_some())
                })
                .flatten()
        }) {
            // The context tab is always adopted; only revealing the dock is
            // opt-in. Without this gate every focus change on a pane that knows
            // its PR would take width from the panes being read.
            if self.open_dock_on_work_link {
                self.dock_collapsed = false;
            }
            self.select_dock_tab_index(index);
        } else if self.dock_open_surfaces.is_empty() {
            self.dock_tab = None;
            self.dock_active_tab_index = None;
            self.dock_chooser_focused = true;
        }
    }

    /// Keep the dock on the object companion for a sidebar or full-screen
    /// work view. A later explicit surface pick remains visible until another
    /// view change calls this function.
    pub(crate) fn follow_view(&mut self, view: SidebarGroupMode) {
        let surface = match view {
            SidebarGroupMode::Repo | SidebarGroupMode::RepoWorktree | SidebarGroupMode::Spaces => {
                None
            }
            SidebarGroupMode::LinearTeam => Some(DockSurface::Linear),
            SidebarGroupMode::RepoPr => Some(DockSurface::Pr),
            SidebarGroupMode::Missive => Some(DockSurface::Missive),
        };
        if let Some(index) = surface.and_then(|surface| {
            self.dock_open_surfaces
                .iter()
                .zip(&self.dock_tab_bindings)
                .position(|(open, binding)| *open == surface && binding.is_some())
        }) {
            self.select_dock_tab_index(index);
        }
        self.dock_surface_override = false;
    }

    /// Close `surface`. The active surface moves to the neighbour that took its
    /// place, or to `None` when the dock is left empty and becomes a chooser.
    pub(crate) fn close_dock_surface(&mut self, surface: DockSurface) {
        self.align_dock_tab_bindings();
        let Some(index) = self
            .active_dock_tab_index()
            .filter(|index| self.dock_open_surfaces.get(*index) == Some(&surface))
            .or_else(|| {
                self.dock_open_surfaces
                    .iter()
                    .position(|open| *open == surface)
            })
        else {
            return;
        };
        let was_active = self.active_dock_tab_index() == Some(index);
        if let Some(binding) = self.dock_tab_bindings.get(index).and_then(Option::as_ref) {
            if binding.origin == DockTabOrigin::Context {
                self.dock_suppressed_context.insert(binding.object.clone());
            }
        }
        self.dock_open_surfaces.remove(index);
        self.dock_tab_bindings.remove(index);
        self.clear_hovered_control();
        if !was_active {
            return;
        }
        self.dock_active_tab_index = (!self.dock_open_surfaces.is_empty())
            .then(|| index.min(self.dock_open_surfaces.len().saturating_sub(1)));
        self.dock_tab = self
            .dock_active_tab_index
            .and_then(|index| self.dock_open_surfaces.get(index).copied());
        self.dock_scroll = 0;
        if self.dock_tab.is_none() {
            self.dock_editor_focused = false;
            self.dock_home_focused = false;
            self.dock_diff_focused = false;
            self.dock_files_focused = false;
            self.dock_agents_focused = false;
            self.dock_hosts_focused = false;
            self.dock_chooser_focused = true;
        }
    }

    pub(crate) fn toggle_dock_maximized(&mut self) {
        self.dock_maximized = !self.dock_maximized;
    }

    /// Adjacent open surface, wrapping. `None` when nothing is open.
    pub(crate) fn adjacent_dock_tab_index(&self, forward: bool) -> Option<usize> {
        if self.dock_open_surfaces.is_empty() {
            return None;
        }
        let current = self.active_dock_tab_index().unwrap_or(0);
        Some(if forward {
            (current + 1) % self.dock_open_surfaces.len()
        } else {
            (current + self.dock_open_surfaces.len() - 1) % self.dock_open_surfaces.len()
        })
    }

    pub(crate) fn swap_dock_presentation(&mut self, other: &mut DockPresentationState) {
        std::mem::swap(&mut self.dock_width, &mut other.width);
        std::mem::swap(&mut self.dock_collapsed, &mut other.collapsed);
        std::mem::swap(&mut self.dock_surface_override, &mut other.surface_override);
        std::mem::swap(&mut self.dock_tab, &mut other.tab);
        std::mem::swap(&mut self.dock_open_surfaces, &mut other.open_surfaces);
        std::mem::swap(&mut self.dock_tab_bindings, &mut other.tab_bindings);
        std::mem::swap(&mut self.dock_active_tab_index, &mut other.active_tab_index);
        std::mem::swap(&mut self.hovered_control, &mut other.hovered_control);
        std::mem::swap(&mut self.hover_started_at, &mut other.hover_started_at);
        std::mem::swap(
            &mut self.hover_tooltip_visible,
            &mut other.hover_tooltip_visible,
        );
        std::mem::swap(&mut self.dock_pane_tabs, &mut other.pane_tabs);
        std::mem::swap(&mut self.dock_followed_pane, &mut other.followed_pane);
        std::mem::swap(&mut self.dock_context_objects, &mut other.context_objects);
        std::mem::swap(
            &mut self.dock_suppressed_context,
            &mut other.suppressed_context,
        );
        std::mem::swap(&mut self.dock_maximized, &mut other.maximized);
        std::mem::swap(&mut self.dock_surface_menu, &mut other.surface_menu);
        std::mem::swap(&mut self.dock_chooser_focused, &mut other.chooser_focused);
        std::mem::swap(&mut self.dock_scroll, &mut other.scroll);
        std::mem::swap(&mut self.dock_object_preview, &mut other.object_preview);
        std::mem::swap(&mut self.dock_object_views, &mut other.object_views);
        std::mem::swap(&mut self.dock_editor_focused, &mut other.editor_focused);
        std::mem::swap(&mut self.dock_editor_preview, &mut other.editor_preview);
        std::mem::swap(&mut self.dock_diff_focused, &mut other.diff_focused);
        std::mem::swap(&mut self.dock_pr_focused, &mut other.pr_focused);
        std::mem::swap(&mut self.dock_pr_checkout_menu, &mut other.pr_checkout_menu);
        std::mem::swap(&mut self.dock_pr_action_menu, &mut other.pr_action_menu);
        std::mem::swap(
            &mut self.pr_action_confirmation,
            &mut other.pr_action_confirmation,
        );
        std::mem::swap(
            &mut self.dock_diff_ignore_whitespace,
            &mut other.diff_ignore_whitespace,
        );
        std::mem::swap(&mut self.dock_diff_selected, &mut other.diff_selected);
        std::mem::swap(&mut self.dock_diff_collapsed, &mut other.diff_collapsed);
        std::mem::swap(&mut self.dock_diff_request, &mut other.diff_request);
        std::mem::swap(&mut self.dock_diff_active_key, &mut other.diff_active_key);
        std::mem::swap(&mut self.dock_files_focused, &mut other.files_focused);
        std::mem::swap(&mut self.dock_files_selection, &mut other.files_selection);
        std::mem::swap(&mut self.dock_files_filter, &mut other.files_filter);
        std::mem::swap(&mut self.dock_files_collapsed, &mut other.files_collapsed);
        std::mem::swap(&mut self.dock_files_sort, &mut other.files_sort);
        std::mem::swap(
            &mut self.dock_files_search_active,
            &mut other.files_search_active,
        );
        std::mem::swap(&mut self.dock_files_last_click, &mut other.files_last_click);
        std::mem::swap(&mut self.dock_agents_focused, &mut other.agents_focused);
        std::mem::swap(&mut self.dock_agents_selection, &mut other.agents_selection);
        std::mem::swap(&mut self.dock_hosts_focused, &mut other.hosts_focused);
        std::mem::swap(&mut self.dock_hosts_selection, &mut other.hosts_selection);
        std::mem::swap(&mut self.dock_linear_focused, &mut other.linear_focused);
        std::mem::swap(
            &mut self.dock_ticket_start_menu,
            &mut other.ticket_start_menu,
        );
        std::mem::swap(
            &mut self.dock_ticket_action_menu,
            &mut other.ticket_action_menu,
        );
        std::mem::swap(
            &mut self.dock_ticket_comment_draft,
            &mut other.ticket_comment_draft,
        );
        std::mem::swap(&mut self.dock_comment_draft, &mut other.comment_draft);
        std::mem::swap(&mut self.dock_pending_write, &mut other.pending_write);
        std::mem::swap(&mut self.dock_write_notice, &mut other.write_notice);
        std::mem::swap(&mut self.dock_home_selection, &mut other.home_selection);
        std::mem::swap(&mut self.dock_symphony, &mut other.symphony);
        std::mem::swap(
            &mut self.dock_home_ticket_selection,
            &mut other.home_ticket_selection,
        );
        std::mem::swap(
            &mut self.dock_home_poll_selection,
            &mut other.home_poll_selection,
        );
        std::mem::swap(&mut self.dock_home_section, &mut other.home_section);
        std::mem::swap(&mut self.dock_home_detail_tab, &mut other.home_detail_tab);
        std::mem::swap(&mut self.dock_home_focused, &mut other.home_focused);
        std::mem::swap(
            &mut self.dock_home_followed_pane,
            &mut other.home_followed_pane,
        );
    }

    pub(crate) fn reconcile_sidebar_presentation(&mut self) {
        if self.sidebar_presentation.projection_revision != self.sidebar_projection_revision {
            self.workspace_scroll = 0;
            self.mobile_switcher_scroll = 0;
            self.sidebar_presentation.projection_revision = self.sidebar_projection_revision;
        }
        let current_ids = self
            .workspaces
            .iter()
            .map(|workspace| workspace.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let first_attach = self.sidebar_presentation.known_workspace_ids.is_empty();
        let newly_added = current_ids
            .difference(&self.sidebar_presentation.known_workspace_ids)
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        self.sidebar_presentation
            .expanded_workspace_ids
            .retain(|workspace_id| current_ids.contains(workspace_id));
        self.sidebar_presentation.revealed_workspace_id = self
            .sidebar_presentation
            .revealed_workspace_id
            .take()
            .filter(|workspace_id| current_ids.contains(workspace_id));
        // A client attaches to the complete session projection, not just the
        // currently active workspace. Restricting this to the active space
        // makes inactive, completed and agentless tabs disappear from both
        // the expanded sidebar and its compact presentation. Disclosure stays
        // client-local after this initial (or newly-created workspace) setup.
        if first_attach {
            self.sidebar_presentation
                .expanded_workspace_ids
                .extend(current_ids.iter().cloned());
        } else {
            self.sidebar_presentation
                .expanded_workspace_ids
                .extend(newly_added);
        }
        self.sidebar_presentation.known_workspace_ids = current_ids;
    }

    pub(crate) fn mark_sidebar_projection_changed(&mut self) {
        self.refresh_local_agent_panel_identities();
        self.sidebar_projection_revision = self.sidebar_projection_revision.wrapping_add(1);
        self.workspace_scroll = 0;
        self.mobile_switcher_scroll = 0;
    }

    pub(crate) fn refresh_local_agent_panel_identities(&mut self) {
        self.local_agent_panel_identities =
            crate::ui::local_agent_panel_identities(&self.workspaces, &self.agent_host_name);
    }

    pub(crate) fn sidebar_shows_spaces_tree(&self) -> bool {
        // The final sidebar is always a stable ownership projection. Priority
        // remains available for indexed Agent focus, but may not reorder rows.
        true
    }

    pub(crate) fn begin_workspace_picker_presentation(&mut self) {
        self.mobile_switcher_scroll = 0;
        self.workspace_picker_forces_spaces_tree = true;
    }

    pub(crate) fn end_workspace_picker_presentation(&mut self) {
        self.workspace_picker_forces_spaces_tree = false;
    }

    pub(crate) fn workspace_agents_expanded(&self, ws_idx: usize) -> bool {
        self.workspaces.get(ws_idx).is_some_and(|workspace| {
            (self.sidebar_presentation.known_workspace_ids.is_empty()
                && self.active == Some(ws_idx))
                || self
                    .sidebar_presentation
                    .expanded_workspace_ids
                    .contains(&workspace.id)
        })
    }

    pub(crate) fn next_agent_activity_age_change(&self, now: Instant) -> Option<Instant> {
        let sidebar = self
            .view
            .visible_agent_activity_instants
            .iter()
            .filter_map(|observed_at| {
                crate::activity_age::next_coarse_change_at(Some(*observed_at), now)
            })
            .min();
        let notepad = self
            .view
            .visible_notepad_agent_ages
            .iter()
            .filter_map(|age| crate::activity_age::next_change_after_elapsed(*age, now))
            .min();
        sidebar.into_iter().chain(notepad).min()
    }

    pub(crate) fn toggle_workspace_agent_disclosure(&mut self, ws_idx: usize) -> bool {
        let Some(workspace_id) = self
            .workspaces
            .get(ws_idx)
            .map(|workspace| workspace.id.clone())
        else {
            return false;
        };
        if crate::ui::sidebar_thread_entries(self).iter().all(|entry| {
            entry
                .local_target()
                .is_none_or(|target| target.ws_idx != ws_idx)
        }) {
            return false;
        }
        if !self
            .sidebar_presentation
            .expanded_workspace_ids
            .remove(&workspace_id)
        {
            self.sidebar_presentation
                .expanded_workspace_ids
                .insert(workspace_id);
        }
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    pub(crate) fn toggle_prio_panel(&mut self) -> bool {
        self.prio_panel_collapsed = !self.prio_panel_collapsed;
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
        true
    }

    pub(crate) fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
        self.session_dirty_revision = self.session_dirty_revision.saturating_add(1);
    }

    pub(crate) fn remove_alias_shadowed_by_new_pane(&mut self, pane_id: PaneId) {
        self.pane_id_aliases.remove(&pane_id.raw());
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound.enabled
    }

    pub fn toast_delivery(&self) -> ToastDelivery {
        self.toast_config.delivery
    }

    pub fn notifications_enabled(&self) -> bool {
        self.toast_config.delivery != ToastDelivery::Off || self.sound.enabled
    }

    pub fn agent_border_labels_enabled(&self) -> bool {
        self.show_agent_labels_on_pane_borders
    }

    pub(crate) fn pane_exposes_host_cursor(
        &self,
        _ws_idx: usize,
        _pane_id: crate::layout::PaneId,
    ) -> bool {
        true
    }

    pub(crate) fn integration_updates_available(&self) -> bool {
        self.integration_recommendations
            .iter()
            .any(|item| item.state == crate::integration::IntegrationStatusKind::Outdated)
    }

    pub(crate) fn refresh_agent_manifest_summaries(&mut self) {
        self.agent_manifest_summaries = crate::detect::manifest::manifest_summaries();
    }

    pub(crate) fn global_menu_attention_badge_visible(&self) -> bool {
        self.update_available.is_some() || self.integration_updates_available()
    }

    pub(crate) fn global_menu_item_has_badge(&self, item: &str) -> bool {
        (item == "update ready" && self.update_available.is_some())
            || (item == "settings" && self.integration_updates_available())
    }

    pub(crate) fn settings_section_has_badge(&self, section: SettingsSection) -> bool {
        section == SettingsSection::Integrations && self.integration_updates_available()
    }

    pub(crate) fn terminal_area_surface(&self) -> TerminalAreaSurface<'_> {
        let preview_is_in_dock = !self.dock_collapsed && self.dock_tab == Some(DockSurface::Editor);
        if self.dock_editor_preview.is_some() && !preview_is_in_dock {
            TerminalAreaSurface::EditorPreview
        } else if let Some(detail) = self.symphony_detail.as_ref() {
            TerminalAreaSurface::Symphony(detail)
        } else if let Some(detail) = self.loop_run_history_detail.as_ref() {
            TerminalAreaSurface::LoopRunHistory(detail)
        } else if let Some(detail) = self.aloop_run_detail.as_ref() {
            TerminalAreaSurface::AloopRunLog(detail)
        } else if self.usage_view.is_some() {
            TerminalAreaSurface::Usage
        } else if self.board_view.is_some() {
            TerminalAreaSurface::Board
        } else if self.work_view.is_some() {
            TerminalAreaSurface::Work
        } else if self.dock_collapsed && self.dock_object_preview.is_some() {
            TerminalAreaSurface::DockObjectPreview
        } else if self.home.is_some() {
            TerminalAreaSurface::Home
        } else if let Some(inbox) = self.inbox.as_ref() {
            TerminalAreaSurface::Inbox(inbox)
        } else if self
            .active
            .and_then(|ws_idx| self.workspaces.get(ws_idx))
            .is_some()
        {
            TerminalAreaSurface::Tab
        } else {
            TerminalAreaSurface::Empty
        }
    }

    /// True when the terminal-area renderer does not display the active tab.
    pub(crate) fn tab_surface_replaced(&self) -> bool {
        !matches!(self.terminal_area_surface(), TerminalAreaSurface::Tab)
    }

    pub(crate) fn app_surface_pane_ids(&self) -> std::collections::HashSet<PaneId> {
        self.app_surface_pane_ids_with_tab_visibility(!self.tab_surface_replaced())
    }

    pub(crate) fn app_surface_pane_ids_with_tab_visibility(
        &self,
        tab_visible: bool,
    ) -> std::collections::HashSet<PaneId> {
        let mut pane_ids = std::collections::HashSet::new();
        if let Some(popup) = &self.popup_pane {
            pane_ids.insert(popup.pane_id);
        }
        if !tab_visible {
            return pane_ids;
        }
        let Some(tab) = self
            .active
            .and_then(|ws_idx| self.workspaces.get(ws_idx))
            .and_then(crate::workspace::Workspace::active_tab)
        else {
            return pane_ids;
        };
        if tab.zoomed {
            pane_ids.insert(tab.layout.focused());
        } else {
            pane_ids.extend(tab.panes.keys().copied());
        }
        pane_ids
    }

    pub(crate) fn focused_pane_requests_mouse_capture_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.effective_interaction_mode() == Mode::Terminal
            && self
                .active
                .and_then(|idx| self.focused_runtime_in_workspace(terminal_runtimes, idx))
                .is_some_and(crate::terminal::TerminalRuntime::mouse_reporting_enabled)
    }

    pub(crate) fn should_capture_host_mouse_from(
        &self,
        terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> bool {
        self.mouse_capture
            || self.popup_pane.is_some()
            || self.focused_pane_requests_mouse_capture_from(terminal_runtimes)
    }

    pub fn is_prefix_key(&self, key: &crate::input::TerminalKey) -> bool {
        crate::config::terminal_key_matches_combo(key, (self.prefix_code, self.prefix_mods))
    }

    pub fn estimate_pane_size(&self) -> (u16, u16) {
        if let Some(info) = self.view.pane_infos.first() {
            (info.rect.height, info.rect.width)
        } else {
            (self.headless_size.1, self.headless_size.0)
        }
    }

    /// Returns true when the given (workspace, tab, pane) refers to the
    /// currently focused pane in the active workspace's active tab.
    pub(crate) fn runtime_for_pane_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        #[cfg(test)]
        if let Some(runtime) = self.workspaces.get(ws_idx)?.test_runtimes.get(&pane_id) {
            return Some(runtime);
        }
        #[cfg(test)]
        if let Some(runtime) = self
            .workspaces
            .get(ws_idx)?
            .tabs
            .iter()
            .find_map(|tab| tab.runtimes.get(&pane_id))
        {
            return Some(runtime);
        }
        let terminal_id = self.workspaces.get(ws_idx)?.terminal_id(pane_id)?;
        terminal_runtimes.get(terminal_id)
    }

    #[cfg(test)]
    pub(crate) fn runtime_for_pane<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        self.workspaces.iter().find_map(|ws| {
            #[cfg(test)]
            if let Some(runtime) = ws.test_runtimes.get(&pane_id) {
                return Some(runtime);
            }
            #[cfg(test)]
            if let Some(runtime) = ws.tabs.iter().find_map(|tab| tab.runtimes.get(&pane_id)) {
                return Some(runtime);
            }
            let terminal_id = ws.terminal_id(pane_id)?;
            terminal_runtimes.get(terminal_id)
        })
    }

    pub(crate) fn focused_runtime_in_workspace<'a>(
        &'a self,
        terminal_runtimes: &'a crate::terminal::TerminalRuntimeRegistry,
        ws_idx: usize,
    ) -> Option<&'a crate::terminal::TerminalRuntime> {
        let ws = self.workspaces.get(ws_idx)?;
        let pane_id = ws.focused_pane_id()?;
        self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
    }

    pub fn is_active_pane(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> bool {
        let Some(active_ws_idx) = self.active else {
            return false;
        };
        if ws_idx != active_ws_idx {
            return false;
        }
        let Some(ws) = self.workspaces.get(ws_idx) else {
            return false;
        };
        if tab_idx != ws.active_tab_index() {
            return false;
        }
        ws.active_tab().map(|tab| tab.layout.focused()) == Some(pane_id)
    }
}

#[cfg(test)]
pub fn key_matches(
    key: &crossterm::event::KeyEvent,
    expected_code: KeyCode,
    expected_mods: KeyModifiers,
) -> bool {
    crate::config::terminal_key_matches_combo(
        &crate::input::TerminalKey::from(*key),
        (expected_code, expected_mods),
    )
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
impl AppState {
    /// Create an AppState for testing — no channels, no PTYs.
    pub fn test_new() -> Self {
        Self {
            view_observed_at: std::time::Instant::now(),
            view_observed_unix_s: super::settled::unix_seconds(std::time::SystemTime::now()),
            loop_run_history: crate::loop_runs::RunHistory::default(),
            loop_registry: crate::loop_runs::LoopRegistry::default(),
            loop_run_history_detail: None,
            aloop_run_detail: None,
            symphony_snapshot: crate::symphony::Snapshot::default(),
            fleet_snapshot: crate::fleet::Snapshot::default(),
            local_group_snapshot: None,
            agent_host_name: "localhost".to_string(),
            chrome_host_label: "TESTHOST".to_string(),
            day_board: crate::day::DayBoard::default(),
            day_stale_after: Duration::from_secs(600),
            board_view: None,
            board_return: None,
            local_agent_panel_identities: std::collections::HashMap::new(),
            remote_agent_panel_entries: Vec::new(),
            aloop_projection: None,
            remote_focus_proxy_panes: std::collections::HashSet::new(),
            sidebar_selected_remote_agent: None,
            symphony_detail: None,
            dock_symphony: None,
            work_view: None,
            linear_default_layout: LinearViewLayout::List,
            hovered_control: None,
            hover_started_at: None,
            hover_tooltip_visible: false,
            usage_view: None,
            usage_snapshot: None,
            usage_pricing: crate::config::UsageConfig::default(),
            request_usage_scan: false,
            inbox: None,
            home: None,
            home_agent_choices: Vec::new(),
            home_catalog: crate::app::home_catalog::HomeCatalog::fallback(),
            launch_profiles: crate::app::launch_profiles::resolve(&[]),
            projects: Vec::new(),
            machines: crate::app::machines::resolve(&crate::config::FleetConfig::default()),
            home_ref_cache: std::collections::HashMap::new(),
            request_home_ref_refresh: None,
            request_tool_probes: false,
            pending_human_drafts: std::collections::HashMap::new(),
            status_metrics: Some(crate::platform::status_metrics::StatusMetricsSnapshot {
                metrics: crate::platform::status_metrics::status_metrics_fixture(),
                sampled_at: std::time::Instant::now(),
            }),
            status_git_cwd: None,
            status_git_branch: None,
            status_git_ahead_behind: None,
            status_focused_cwd: None,
            status_focus_projection_initialized: false,
            git_root_for_cwd: std::collections::HashMap::new(),
            repo_editor_argv: None,
            forwarded_pane_input: None,
            status_bar_enabled: true,
            status_now_unix: None,
            provider_usage: crate::provider_usage::ProviderUsageSnapshot::default(),
            connectivity: crate::connectivity::Connectivity::default(),
            status_bar_expanded: false,
            fleet_status: true,
            status_disk_visible: false,
            full_lifecycle_hook_authority_timeout: std::time::Duration::from_secs(
                crate::config::Config::default()
                    .agent_detection
                    .full_lifecycle_hook_authority_timeout_seconds,
            ),
            hide_done_after: std::time::Duration::from_secs(30 * 60),
            reap_done_after: std::time::Duration::from_secs(4 * 60 * 60),
            reap_done_panes: true,
            settle_after: std::time::Duration::from_secs(3 * 24 * 60 * 60),
            settle_finished_after: std::time::Duration::from_secs(10 * 60),
            settle_done_after: std::time::Duration::from_secs(30 * 60),
            settled_read_only_after: std::time::Duration::from_secs(15 * 60),
            terminals: std::collections::HashMap::new(),
            agent_states: crate::agent_state::AgentStateStore::default(),
            direct_attach_resize_locks: std::collections::HashSet::new(),
            pane_id_aliases: std::collections::HashMap::new(),
            public_pane_id_aliases: std::collections::HashMap::new(),
            workspaces: Vec::new(),
            active: None,
            previous_pane_focus: None,
            selected: 0,
            client_overlay: ClientOverlay::None,
            server_interaction: ServerInteractionState::new(Mode::Navigate),
            confirm_close_workspace_id: None,
            confirm_close_remote_agent_ref: None,
            rename_target: None,
            should_quit: false,
            detach_exits: false,
            detach_requested: false,
            request_new_workspace: false,
            request_new_tab: false,
            request_pane_toggle: None,
            request_open_repo_editor: false,
            notepad_request: None,
            request_git_action: None,
            request_user_action: None,
            request_save_add_action: false,
            request_pr_command: None,
            request_pin_toggle: None,
            request_new_linked_worktree: None,
            request_open_existing_worktree: None,
            request_new_workspace_cwd: None,
            request_remove_linked_worktree: None,
            request_submit_worktree_create: false,
            request_submit_worktree_open: false,
            request_submit_worktree_remove: false,
            request_reload_config: false,
            request_client_config_reload: false,
            request_client_notification_config: None,
            dock_width_persistence_request: None,
            notepad_height_persistence_request: None,
            sidebar_group_mode_persistence_request: None,
            sidebar_group_sort_persistence_request: None,
            sidebar_group_collapsed_persistence_request: None,
            sidebar_view_scan_request: false,
            sidebar_work_filter_persistence_request: None,
            request_clipboard_write: None,
            creating_new_tab: false,
            requested_new_tab_name: None,
            pending_workspace_create_cwd: None,
            rename_pane_target: None,
            worktree_create: None,
            worktree_open: None,
            worktree_remove: None,
            worktree_directory: std::path::PathBuf::from("/tmp/herdr-worktrees"),
            collapsed_space_keys: std::collections::HashSet::new(),
            collapsed_sidebar_groups: crate::ui::initial_collapsed_sidebar_groups(
                &std::collections::HashMap::new(),
            ),
            sidebar_group_mode: SidebarGroupMode::Repo,
            sidebar_focused: false,
            client_focus_intent: ClientFocusIntent::FollowShared,
            sidebar_group_menu_open: false,
            sidebar_group_menu_selected: 0,
            sidebar_work_filter: SidebarWorkFilter::default(),
            sidebar_filter_menu_open: false,
            sidebar_filter_menu_selected: 0,
            sidebar_search_active: false,
            sidebar_starred_only: false,
            sidebar_new_menu: None,
            sidebar_areas_menu_selected: None,
            window_cycle_menu_open: false,
            window_cycle_menu_selected: 0,
            sidebar_new_thread: None,
            sidebar_project_menu: None,
            sidebar_refresh_requested: false,
            sidebar_refreshing: false,
            sidebar_selected_work_group: None,
            sidebar_object_menu: None,
            sidebar_sort_menu: None,
            sidebar_subgroup_picker: None,
            sidebar_pod_picker: None,
            sidebar_group_sorts: std::collections::HashMap::new(),
            sidebar_unassigned_expanded_views: std::collections::HashSet::new(),
            sidebar_selected_settled: None,
            settled_view: None,
            sidebar_snooze: None,
            sidebar_settled_menu_target: None,
            sidebar_settled_menu_selected: 0,
            sidebar_settled_menu_delete_armed: false,
            pending_pane_settlement_changes: Vec::new(),
            pending_status_transitions: Vec::new(),
            pending_pane_snooze_changes: Vec::new(),
            request_complete_onboarding: false,
            name_input: String::new(),
            name_input_replace_on_type: false,
            rename_tab_prefill: None,
            release_notes: None,
            product_announcement: None,
            keybind_help: KeybindHelpState::default(),
            navigator: NavigatorState::default(),
            command_palette: CommandPaletteState::default(),
            work_link_picker: None,
            agent_picker: None,
            add_action: None,
            copy_mode: None,
            sidebar_presentation: SidebarPresentationState::default(),
            sidebar_projection_revision: 0,
            workspace_picker_forces_spaces_tree: false,
            workspace_scroll: 0,
            tab_scroll: 0,
            tab_scroll_follow_active: true,
            mobile_switcher_scroll: 0,
            view: ViewState {
                layout: ViewLayout::Desktop,
                status_bar_rect: Rect::default(),
                sidebar_rect: Rect::default(),
                sidebar_footer_settings_hit_area: Rect::default(),
                sidebar_footer_ask_subtitles_hit_area: Rect::default(),
                sidebar_footer_work_hit_area: Rect::default(),
                sidebar_footer_usage_hit_area: Rect::default(),
                usage_hit_areas: Vec::new(),
                sidebar_footer_ticket_hit_area: Rect::default(),
                sidebar_footer_missive_hit_area: Rect::default(),
                notepad_rect: Rect::default(),
                notepad_usage_toggle_hit_area: Rect::default(),
                notepad_tab_hit_areas: Vec::new(),
                notepad_agent_rows: Vec::new(),
                notepad_agent_max_scroll: 0,
                notepad_usage_rows: Vec::new(),
                notepad_usage_hit_areas: Vec::new(),
                notepad_usage_max_scroll: 0,
                pomodoro_hit_area: Rect::default(),
                notification_hit_area: Rect::default(),
                window_cycle_mode_hit_area: Rect::default(),
                sidebar_areas_hit_area: Rect::default(),
                hyperspace_rect: Rect::default(),
                hyperspace_pause_hit_area: Rect::default(),
                sidebar_footer_refresh_hit_area: Rect::default(),
                sidebar_footer_board_hit_area: Rect::default(),
                workspace_card_areas: Vec::new(),
                agent_card_areas: Vec::new(),
                sidebar_hover_targets: Vec::new(),
                visible_agent_activity_instants: Vec::new(),
                visible_notepad_agent_ages: Vec::new(),
                tab_bar_rect: Rect::default(),
                tab_hit_areas: Vec::new(),
                tab_scroll_left_hit_area: Rect::default(),
                tab_scroll_right_hit_area: Rect::default(),
                new_tab_hit_area: Rect::default(),
                repo_editor_button_hit_area: Rect::default(),
                add_action_button_hit_area: Rect::default(),
                user_action_hit_areas: Vec::new(),
                add_action_close_hit_area: Rect::default(),
                add_action_field_hit_areas: Vec::new(),
                add_action_cancel_hit_area: Rect::default(),
                add_action_save_hit_area: Rect::default(),
                add_project_layout: crate::ui::add_project::AddProjectLayout::default(),
                git_menu_button_hit_area: Rect::default(),
                git_menu_popup_rect: Rect::default(),
                git_menu_first_visible: 0,
                git_menu_row_hit_areas: Vec::new(),
                pane_toggle_below_hit_area: Rect::default(),
                pane_toggle_right_hit_area: Rect::default(),
                terminal_area: Rect::default(),
                work_context_link_rows: Vec::new(),
                mobile_header_rect: Rect::default(),
                mobile_menu_hit_area: Rect::default(),
                config_diagnostic_hit_area: Rect::default(),
                toast_hit_area: Rect::default(),
                home_row_hit_areas: Vec::new(),
                home_hit_areas: Vec::new(),
                pane_infos: Vec::new(),
                split_borders: Vec::new(),
                dock_rect: Rect::default(),
                dock_handle_rect: Rect::default(),
                dock_divider_rect: Rect::default(),
                dock_tab_bar_rect: Rect::default(),
                dock_tab_hit_areas: Vec::new(),
                dock_tab_close_rect: Rect::default(),
                dock_plus_rect: Rect::default(),
                dock_maximize_rect: Rect::default(),
                dock_auto_open_rect: Rect::default(),
                dock_surface_card_hit_areas: Vec::new(),
                dock_surface_menu_layout: None,
                dock_home_section_hit_areas: Vec::new(),
                dock_home_tab_hit_areas: Vec::new(),
                dock_home_tab_keys: Vec::new(),
                dock_home_detail_tab_hit_areas: Vec::new(),
                dock_file_row_hit_areas: Vec::new(),
                dock_files_refresh_rect: Rect::default(),
                dock_files_sort_rect: Rect::default(),
                editor_preview_refresh_rect: Rect::default(),
                editor_preview_open_rect: Rect::default(),
                dock_agent_row_hit_areas: Vec::new(),
                dock_host_row_hit_areas: Vec::new(),
                dock_body_rect: Rect::default(),
                scratchpad_link_rows: Vec::new(),
                status_buttons: Vec::new(),
                status_work_links: Vec::new(),
                status_segments: Vec::new(),
                status_segment_hit_areas: Vec::new(),
                focused_remote_host: None,
            },
            drag: None,
            workspace_presses: std::collections::HashMap::new(),
            tab_presses: std::collections::HashMap::new(),
            agent_presses: std::collections::HashMap::new(),
            remote_agent_presses: std::collections::HashMap::new(),
            selection: None,
            selection_autoscroll: None,
            context_menu: None,
            git_menu: MenuListState::new(0),
            update_available: None,
            update_install_command: "herdr update".into(),
            latest_release_notes_available: false,
            update_dismissed: false,
            config_diagnostic: None,
            toast: None,
            pending_agent_notifications: std::collections::HashMap::new(),
            copy_feedback: None,
            outer_terminal_focus: None,
            prefix_code: KeyCode::Char('b'),
            prefix_mods: KeyModifiers::CONTROL,
            headless_size: (
                crate::config::DEFAULT_HEADLESS_COLS,
                crate::config::DEFAULT_HEADLESS_ROWS,
            ),
            default_sidebar_width: 26,
            sidebar_width: 26,
            sidebar_min_width: 18,
            sidebar_max_width: 36,
            dock_width: crate::ui::DOCK_DEFAULT_WIDTH,
            dock_collapsed: true,
            dock_surface_override: false,
            dock_default_surfaces: Vec::new(),
            dock_tab: None,
            dock_open_surfaces: Vec::new(),
            dock_tab_bindings: Vec::new(),
            dock_active_tab_index: None,
            dock_pane_tabs: std::collections::HashMap::new(),
            dock_followed_pane: None,
            dock_context_objects: Vec::new(),
            open_dock_on_work_link: false,
            dock_suppressed_context: std::collections::HashSet::new(),
            dock_maximized: false,
            dock_surface_menu: None,
            dock_chooser_focused: false,
            dock_scroll: 0,
            dock_object_preview: None,
            dock_object_views: std::collections::HashMap::new(),
            dock_editor_focused: false,
            dock_diff_focused: false,
            dock_pr_focused: false,
            dock_pr_checkout_menu: None,
            dock_pr_action_menu: None,
            dock_diff_ignore_whitespace: false,
            dock_diff_selected: 0,
            dock_diff_collapsed: std::collections::HashSet::new(),
            dock_diff_request: None,
            dock_diff_active_key: None,
            dock_diff_cache: std::collections::HashMap::new(),
            dock_diff_resolved_requests: std::collections::HashMap::new(),
            dock_files_focused: false,
            dock_files_selection: None,
            dock_files_filter: String::new(),
            dock_files_collapsed: std::collections::HashSet::new(),
            dock_files_sort: crate::files::FileSort::Name,
            dock_files_search_active: false,
            dock_files_last_click: None,
            dock_agents_focused: false,
            dock_agents_selection: None,
            dock_hosts_focused: false,
            dock_hosts_selection: None,
            dock_linear_focused: false,
            dock_ticket_start_menu: None,
            dock_ticket_action_menu: None,
            dock_ticket_comment_draft: None,
            dock_file_cache: std::collections::HashMap::new(),
            dock_files_root: None,
            dock_files_cwd: None,
            dock_files_roots_by_cwd: std::collections::HashMap::new(),
            files_icons: crate::config::FilesIconConfig::Nerd,
            nerd_font: true,
            space_icons: std::collections::BTreeMap::new(),
            dock_home_selection: None,
            dock_home_ticket_selection: None,
            dock_home_poll_selection: None,
            dock_home_focus_unbound: false,
            dock_comment_draft: None,
            dock_pending_write: None,
            pr_action_confirmation: None,
            dock_write_notice: None,
            dock_home_section: DockHomeSection::Prs,
            dock_home_detail_tab: DockHomeDetailTab::Overview,
            dock_home_focused: false,
            dock_home_followed_pane: None,
            work_index_snapshot: None,
            settings_missive_team: None,
            settings_missive_token_present: false,
            work_index_session: crate::work_index::WorkIndexSession::default(),
            work_item_detail_cache: crate::work_index::WorkItemDetailCache::default(),
            work_item_detail_loading: std::collections::HashSet::new(),
            work_index_enabled: false,
            pr_merge_method: crate::config::MergeMethodConfig::Merge,
            branch_prefix: crate::config::DEFAULT_BRANCH_PREFIX.into(),
            commit_message_model: String::new(),
            commit_stage_all: false,
            work_index_linear_team_configured: false,
            dock_editor_sessions: std::collections::HashMap::new(),
            dock_editor_preview: None,
            dock_editor_errors: std::collections::HashMap::new(),
            dock_editor_requested_paths: std::collections::HashMap::new(),
            scratchpad: crate::scratchpad::ScratchpadDoc::default(),
            notepad: {
                // Keep broad UI fixtures layout-neutral. Tests for shipped
                // sidebar defaults load NotepadConfig::default() explicitly.
                let mut notepad = crate::notepad::NotepadState::default();
                notepad.set_visible_tabs(Vec::new());
                notepad
            },
            sidebar_note_names: sidebar_panel_note_names(
                &crate::config::NotepadConfig::default().files,
                &crate::config::PomodoroConfig::default().log_file,
            ),
            goals: crate::goals::GoalsPanelState::default(),
            pomodoro: crate::pomodoro::PomodoroState::default(),
            // Off in fixtures, the way the break timer is: a decorative panel
            // must not silently move every existing sidebar layout assertion.
            // Tests that care about it set `hyperspace.enabled = true`.
            hyperspace: crate::hyperspace::HyperspaceState::new(false, std::time::Instant::now()),
            mobile_width_threshold: crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD,
            sidebar_width_source: SidebarWidthSource::ConfigDefault,
            sidebar_width_auto: false,
            sidebar_collapsed: false,
            blocked_filter: false,
            window_cycle_mode: crate::config::WindowCycleModeConfig::default(),
            skip_collapsed_cycle: false,
            sidebar_collapsed_mode: crate::config::SidebarCollapsedModeConfig::Compact,
            sidebar_section_split: 0.5,
            prio_panel_collapsed: false,
            agent_panel_scroll: 0,
            agent_panel_sort: AgentPanelSort::Spaces,
            status_indicators: crate::config::StatusIndicatorStyle::Dots,
            working_row_opacity_percent: 100,
            agent_view_override: None,
            sidebar_agents: crate::config::AgentsSidebarConfig::default(),
            sidebar_spaces: crate::config::SpacesSidebarConfig::default(),
            sidebar_areas: crate::config::SidebarAreasConfig::default(),
            sidebar_sections_layout: false,
            sidebar_show_ask_subtitles: false,
            sidebar_header_plain: false,
            next_agent_state_change_seq: 0,
            mouse_capture: true,
            copy_on_select: true,
            right_click_passthrough_modifiers: None,
            right_click_passthrough: None,
            redraw_on_focus_gained: true,
            mouse_scroll_lines: crate::config::DEFAULT_MOUSE_SCROLL_LINES,
            confirm_close: true,
            combine_repos_across_hosts: false,
            new_thread_workspace: crate::config::NewThreadWorkspaceConfig::CurrentCheckout,
            add_project_start_dir: String::new(),
            auto_settle_finished: true,
            auto_settle_inactive: true,
            auto_settle_done: true,
            settle_stops_agent: true,
            nudge_resumed_agents: true,
            resume_nudge_message: "continue".to_string(),
            auto_nudge_stalled_agents: false,
            agent_stale_after: std::time::Duration::from_secs(5 * 60),
            agent_subagent_stale_after: std::time::Duration::from_secs(30 * 60),
            nudge_after: std::time::Duration::from_secs(5 * 60),
            max_nudges: 3,
            max_nudges_without_human: 5,
            stall_nudge_message:
                "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue.".to_string(),
            prompt_new_tab_name: true,
            prompt_new_workspace_name: false,
            pane_borders: true,
            pane_outer_borders: true,
            pane_scrollbars: true,
            show_pull_button: false,
            show_pane_toggle_buttons: false,
            pane_gaps: false,
            show_agent_labels_on_pane_borders: false,
            hide_tab_bar_when_single_tab: false,
            // Deliberately not the shipped default (`Hidden`): the UI tests that
            // exercise tab-row geometry need a tab row to measure.
            tab_bar_position: TabBarPositionConfig::Top,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: " ".into(),
            pane_history_persistence: false,
            reveal_hidden_cursor_for_cjk_ime: false,
            cjk_ime_agent_filter_configured: false,
            cjk_ime_agents: Vec::new(),
            cjk_ime_cursor_shape: 2, // steady_block
            switch_ascii_input_source_in_prefix: false,
            kitty_graphics_enabled: false,
            default_shell: String::new(),
            shell_mode: crate::config::ShellModeConfig::Auto,
            new_terminal_cwd: NewTerminalCwdConfig::Follow,
            pane_scrollback_limit_bytes: crate::config::DEFAULT_SCROLLBACK_LIMIT_BYTES,
            accent: Color::Cyan,
            sound: SoundConfig {
                enabled: false,
                ..SoundConfig::default()
            },
            local_sound_playback: false,
            toast_config: ToastConfig::default(),
            last_non_off_toast_delivery: ToastDelivery::Terminal,
            keybinds: Keybinds::default(),
            palette: Palette::catppuccin(),
            theme_name: "catppuccin".to_string(),
            theme_runtime: ThemeRuntimeConfig {
                manual_name: "catppuccin".to_string(),
                dark_name: "catppuccin".to_string(),
                light_name: "catppuccin-latte".to_string(),
                auto_switch: false,
                host_appearance: crate::config::HostAppearanceOverride::Auto,
                runtime_host_appearance: None,
                custom: None,
                legacy_accent: None,
            },
            host_terminal_appearance: None,
            host_terminal_appearance_explicit: false,
            theme_appearance_mismatch: None,
            tool_probes: crate::app::probes::ToolProbeState::Idle,
            settings: SettingsState {
                section: SettingsSection::Theme,
                list: SelectionListState::new(0),
                original_palette: None,
                original_theme: None,
                archive_delete_armed: false,
                search: String::new(),
                search_active: false,
                keybind_capture: None,
            },
            integration_recommendations: Vec::new(),
            agent_manifest_summaries: Vec::new(),
            agent_manifest_update_status:
                crate::detect::manifest_update::ManifestUpdateStatus::default(),
            integration_install_messages: Vec::new(),
            installed_plugins: std::collections::HashMap::new(),
            plugin_panes: std::collections::HashMap::new(),
            popup_pane: None,
            plugin_command_logs: Vec::new(),
            next_plugin_command_log_id: 1,
            plugin_commands_in_flight: 0,
            global_menu: MenuListState::new(0),
            host_terminal_theme: TerminalTheme::default(),
            host_cell_size: crate::kitty_graphics::HostCellSize::default(),
            host_mouse_pixels: None,
            session_dirty: false,
            session_dirty_revision: 0,
            session_epoch: format!("{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()),
            session_event_revision: 0,
            terminal_runtime_shutdowns: Vec::new(),
        }
    }

    /// Populate missing `TerminalState` entries for every pane so tests that
    /// read or write terminal metadata don't need to manually create them.
    pub fn ensure_test_terminals(&mut self) {
        use crate::terminal::TerminalState;
        for ws in &self.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    if !self.terminals.contains_key(&pane.attached_terminal_id) {
                        let cwd = ws.identity_cwd.clone();
                        self.terminals.insert(
                            pane.attached_terminal_id.clone(),
                            TerminalState::new(pane.attached_terminal_id.clone(), cwd),
                        );
                    }
                }
            }
        }
    }

    pub fn test_with_adversarial_identity_state() -> Self {
        let mut state = Self::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_adversarial_identity_state()];
        state.active = Some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        state
    }

    pub fn assert_invariants_for_test(&self) {
        if self.workspaces.is_empty() {
            assert!(
                self.active.is_none(),
                "empty app state must not have active workspace {:?}",
                self.active
            );
            assert_eq!(
                self.selected, 0,
                "empty app state should keep selected workspace at 0"
            );
            assert!(
                self.pane_id_aliases.is_empty(),
                "empty app state must not keep raw pane aliases"
            );
            assert!(
                self.public_pane_id_aliases.is_empty(),
                "empty app state must not keep public pane aliases"
            );
            assert!(
                self.previous_pane_focus.is_none(),
                "empty app state must not keep previous pane focus"
            );
            assert!(
                self.plugin_panes.is_empty(),
                "empty app state must not keep plugin pane records"
            );
            assert!(
                self.pending_agent_notifications.is_empty(),
                "empty app state must not keep pending agent notifications"
            );
            assert!(
                self.copy_mode.is_none(),
                "empty app state must not keep copy mode"
            );
            assert!(
                self.rename_pane_target.is_none(),
                "empty app state must not keep rename pane target"
            );
            assert!(
                self.selection.is_none(),
                "empty app state must not keep text selection"
            );
            assert!(
                self.selection_autoscroll.is_none(),
                "empty app state must not keep selection autoscroll"
            );
            if let Some(toast) = &self.toast {
                assert!(
                    toast.target.is_none(),
                    "empty app state must not keep pane-targeted toast"
                );
            }
            assert!(
                self.right_click_passthrough.is_none(),
                "empty app state must not keep right-click passthrough gesture"
            );
            assert!(
                self.drag.is_none(),
                "empty app state must not keep drag state"
            );
            assert!(
                self.workspace_presses.is_empty(),
                "empty app state must not keep workspace press state"
            );
            assert!(
                self.tab_presses.is_empty(),
                "empty app state must not keep tab press state"
            );
            assert!(
                self.remote_agent_presses.is_empty(),
                "empty app state must not keep remote agent press state"
            );
            assert!(
                self.context_menu.is_none(),
                "empty app state must not keep context menu"
            );
            assert!(
                self.host_mouse_pixels.is_none(),
                "empty app state must not keep host mouse pixel provenance"
            );
            return;
        }

        assert!(
            self.selected < self.workspaces.len(),
            "selected workspace {} out of bounds for {} workspaces",
            self.selected,
            self.workspaces.len()
        );
        let active = self
            .active
            .expect("non-empty app state must have active workspace");
        assert!(
            active < self.workspaces.len(),
            "active workspace {} out of bounds for {} workspaces",
            active,
            self.workspaces.len()
        );

        let mut workspace_ids = std::collections::HashSet::new();
        let mut workspace_id_to_idx = std::collections::HashMap::new();
        let mut pane_ids = std::collections::HashSet::new();
        let mut attached_terminal_ids = std::collections::HashSet::new();
        for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            assert!(
                workspace_ids.insert(ws.id.clone()),
                "duplicate workspace id {} at workspace index {}",
                ws.id,
                ws_idx
            );
            workspace_id_to_idx.insert(ws.id.clone(), ws_idx);
            ws.assert_invariants_for_test();

            for tab in &ws.tabs {
                for (pane_id, pane) in &tab.panes {
                    assert!(
                        pane_ids.insert(*pane_id),
                        "pane {:?} appears in more than one workspace",
                        pane_id
                    );
                    assert!(
                        attached_terminal_ids.insert(pane.attached_terminal_id.clone()),
                        "terminal {} is attached to more than one app pane",
                        pane.attached_terminal_id
                    );
                    assert!(
                        self.terminals.contains_key(&pane.attached_terminal_id),
                        "pane {:?} is attached to missing terminal {}",
                        pane_id,
                        pane.attached_terminal_id
                    );
                }
            }
        }

        let assert_live_pane = |pane_id: PaneId, context: &str| {
            assert!(
                pane_ids.contains(&pane_id),
                "{context} references missing pane {:?}",
                pane_id
            );
        };
        let assert_workspace_pane = |workspace_id: &str, pane_id: PaneId, context: &str| {
            let ws_idx = workspace_id_to_idx
                .get(workspace_id)
                .copied()
                .unwrap_or_else(|| panic!("{context} references missing workspace {workspace_id}"));
            assert!(
                self.workspaces[ws_idx].pane_state(pane_id).is_some(),
                "{context} references pane {:?} outside workspace {}",
                pane_id,
                workspace_id
            );
        };
        let assert_workspace_index = |ws_idx: usize, context: &str| {
            assert!(
                ws_idx < self.workspaces.len(),
                "{context} references workspace index {} out of bounds for {} workspaces",
                ws_idx,
                self.workspaces.len()
            );
        };
        let assert_tab_index = |ws_idx: usize, tab_idx: usize, context: &str| {
            assert_workspace_index(ws_idx, context);
            assert!(
                tab_idx < self.workspaces[ws_idx].tabs.len(),
                "{context} references tab index {} out of bounds for workspace {} with {} tabs",
                tab_idx,
                ws_idx,
                self.workspaces[ws_idx].tabs.len()
            );
        };

        for (&raw, &pane_id) in &self.pane_id_aliases {
            assert_live_pane(pane_id, &format!("raw pane alias {raw}"));
        }
        for (public_id, &pane_id) in &self.public_pane_id_aliases {
            assert_live_pane(pane_id, &format!("public pane alias {public_id}"));
        }
        if let Some(focus) = &self.previous_pane_focus {
            assert_workspace_pane(&focus.workspace_id, focus.pane_id, "previous pane focus");
        }
        if let Some(toast) = &self.toast {
            if let Some(target) = &toast.target {
                assert_workspace_pane(&target.workspace_id, target.pane_id, "toast target");
            }
        }
        for (&pane_id, notification) in &self.pending_agent_notifications {
            assert_eq!(
                pane_id, notification.pane_id,
                "pending agent notification map key must match payload pane id"
            );
            assert_workspace_pane(
                &notification.workspace_id,
                notification.pane_id,
                "pending agent notification",
            );
        }
        if let Some(popup) = &self.popup_pane {
            assert!(
                self.terminals.contains_key(&popup.terminal_id),
                "popup {:?} references missing terminal {}",
                popup.pane_id,
                popup.terminal_id
            );
            assert!(
                !attached_terminal_ids.contains(&popup.terminal_id),
                "popup terminal {} must not be attached to a tiled pane",
                popup.terminal_id
            );
        }
        for &pane_id in self.plugin_panes.keys() {
            assert_live_pane(pane_id, "plugin pane record");
        }
        if let Some(copy_mode) = &self.copy_mode {
            assert_live_pane(copy_mode.pane_id, "copy mode");
        }
        if let Some(pane_id) = self.rename_pane_target {
            assert_live_pane(pane_id, "rename pane target");
        }
        if let Some(selection) = &self.selection {
            assert_live_pane(selection.pane_id, "text selection");
        } else {
            assert!(
                self.selection_autoscroll.is_none(),
                "selection autoscroll must not remain without an active text selection"
            );
        }
        if let Some(gesture) = &self.right_click_passthrough {
            assert_live_pane(gesture.pane_info.id, "right-click passthrough gesture");
        }
        if let Some(drag) = &self.drag {
            match &drag.target {
                DragTarget::WorkspaceReorder {
                    source_ws_idx,
                    drop_target,
                    ..
                } => {
                    assert_workspace_index(*source_ws_idx, "workspace drag source");
                    if let Some(WorkspaceDropTarget::Before(ws_idx)) = drop_target {
                        assert_workspace_index(*ws_idx, "workspace drag target");
                    }
                }
                DragTarget::TabReorder {
                    ws_idx,
                    source_tab_idx,
                    insert_idx,
                    ..
                } => {
                    assert_tab_index(*ws_idx, *source_tab_idx, "tab drag source");
                    if let Some(insert_idx) = insert_idx {
                        assert!(
                            *insert_idx <= self.workspaces[*ws_idx].tabs.len(),
                            "tab drag insert index {} out of bounds for workspace {} with {} tabs",
                            insert_idx,
                            ws_idx,
                            self.workspaces[*ws_idx].tabs.len()
                        );
                    }
                }
                DragTarget::PaneScrollbar { pane_id, .. } => {
                    assert_live_pane(*pane_id, "pane scrollbar drag")
                }
                _ => {}
            }
        }
        for press in self.workspace_presses.values() {
            assert_workspace_index(press.ws_idx, "workspace press");
        }
        for press in self.tab_presses.values() {
            assert_tab_index(press.ws_idx, press.tab_idx, "tab press");
        }
        if let Some(snooze) = &self.sidebar_snooze {
            assert!(
                self.sidebar_lifecycle_target_snoozed(&snooze.target)
                    .is_some(),
                "snooze UI target must remain actionable"
            );
        }
        if let Some(menu) = &self.context_menu {
            match menu.kind {
                ContextMenuKind::Workspace { ws_idx, .. }
                | ContextMenuKind::GitWorkspace { ws_idx, .. } => {
                    assert_workspace_index(ws_idx, "context menu workspace")
                }
                ContextMenuKind::Tab {
                    ws_idx, tab_idx, ..
                } => assert_tab_index(ws_idx, tab_idx, "context menu tab"),
                ContextMenuKind::Pane {
                    ws_idx,
                    tab_idx,
                    pane_id,
                    source_pane_id,
                    ..
                } => {
                    assert_tab_index(ws_idx, tab_idx, "context menu pane tab");
                    assert!(
                        self.workspaces[ws_idx].tabs[tab_idx]
                            .panes
                            .contains_key(&pane_id),
                        "context menu pane references pane {:?} outside workspace {} tab {}",
                        pane_id,
                        ws_idx,
                        tab_idx
                    );
                    if let Some(source_pane_id) = source_pane_id {
                        assert_live_pane(source_pane_id, "context menu source pane");
                    }
                }
                ContextMenuKind::RemoteAgent { .. } => {}
            }
        }
    }

    pub fn insert_test_runtime(
        &mut self,
        pane_id: crate::layout::PaneId,
        runtime: crate::terminal::TerminalRuntime,
    ) {
        if let Some(ws) = self
            .workspaces
            .iter_mut()
            .find(|ws| ws.terminal_id(pane_id).is_some())
        {
            ws.insert_test_runtime(pane_id, runtime);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    type SurfaceSetup = (&'static str, fn(&mut AppState));

    fn show_editor_preview(state: &mut AppState) {
        state.dock_editor_preview = Some(DockEditorPreview {
            path: "/tmp/preview.rs".into(),
            content: "preview".into(),
            notice: None,
        });
    }

    fn show_symphony(state: &mut AppState) {
        state.toggle_symphony();
    }

    fn show_loop_history(state: &mut AppState) {
        state.toggle_loop_run_history();
    }

    fn show_aloop_run_log(state: &mut AppState) {
        state.show_aloop_run_detail(
            "nightly".into(),
            "ub2".into(),
            std::sync::Arc::new(crate::aloop::RunRecord {
                at: "2026-09-18T09:59:00Z".into(),
                at_unix_s: 1_758_186_740,
                duration_ms: 1_200,
                exit: 0,
                findings: 0,
                stable_ids: Vec::new(),
                log_excerpt: "clean log tail".into(),
            }),
        );
    }

    fn show_usage(state: &mut AppState) {
        state.toggle_usage_view();
    }

    fn show_work(state: &mut AppState) {
        state.work_view = Some(WorkViewState::new(false, None));
    }

    fn show_dock_object_preview(state: &mut AppState) {
        state.dock_collapsed = true;
        state.dock_object_preview = Some(DockObjectRef {
            surface: DockSurface::Linear,
            key: "SCA-1".into(),
        });
    }

    fn show_home(state: &mut AppState) {
        state.home = Some(crate::app::home::HomeState::default());
    }

    fn show_inbox(state: &mut AppState) {
        state.inbox = Some(crate::app::inbox::InboxState::default());
    }

    fn show_popup(state: &mut AppState) {
        state.popup_pane = Some(PopupPaneState {
            pane_id: PaneId::alloc(),
            terminal_id: crate::terminal::TerminalId::alloc(),
            width: None,
            height: None,
        });
    }

    fn show_dock_menu(state: &mut AppState) {
        state.dock_surface_menu = Some(DockSurfaceMenu { selected: 0 });
    }

    fn show_add_project(state: &mut AppState) {
        let mut home = crate::app::home::HomeState::default();
        home.add_project = Some(crate::app::home::AddProjectState::starting_at(
            &std::env::temp_dir(),
        ));
        state.home = Some(home);
    }

    fn show_navigator_picker(state: &mut AppState) {
        state.set_server_mode(Mode::Navigator);
    }

    fn show_work_link_picker(state: &mut AppState) {
        state.set_server_mode(Mode::WorkLinkPicker);
    }

    fn install_client_input_owner(state: &mut AppState, owner: ClientInputOwner) {
        let target = PaneFocusTarget {
            workspace_id: "workspace:test".into(),
            pane_id: PaneId::alloc(),
        };
        match owner {
            ClientInputOwner::Overlay(overlay) => state.open_client_overlay(overlay),
            ClientInputOwner::SnoozeMenu | ClientInputOwner::SnoozeTime => {
                state.sidebar_snooze = Some(SidebarSnoozeUiState {
                    target: SidebarPaneLifecycleTarget::Local(target),
                    anchor: (1, 1),
                    selected: SidebarSnoozeMenuAction::SetTime,
                    time_draft: (owner == ClientInputOwner::SnoozeTime).then(String::new),
                    error: None,
                });
            }
            ClientInputOwner::SettledMenu | ClientInputOwner::SettledDeleteConfirm => {
                state.sidebar_settled_menu_target = Some(target);
                state.sidebar_settled_menu_delete_armed =
                    owner == ClientInputOwner::SettledDeleteConfirm;
            }
            ClientInputOwner::AgentPicker => {
                state.agent_picker = Some(AgentPickerState {
                    candidates: Vec::new(),
                    text: String::new(),
                });
            }
            ClientInputOwner::SidebarGroupMenu => state.sidebar_group_menu_open = true,
            ClientInputOwner::SidebarFilterMenu => state.sidebar_filter_menu_open = true,
            ClientInputOwner::SidebarNewMenu => {
                state.sidebar_new_menu = Some(SidebarNewMenuState::default());
            }
            ClientInputOwner::SidebarNewThread => {
                state.sidebar_new_thread = Some(SidebarNewThreadState::default());
            }
            ClientInputOwner::SidebarProjectMenu => {
                state.sidebar_project_menu = Some(SidebarProjectMenuState::default());
            }
            ClientInputOwner::SidebarObjectMenu => {
                state.sidebar_object_menu = Some(SidebarObjectMenuState {
                    target: "SCA-1".into(),
                    anchor_row: None,
                    page: SidebarObjectMenuPage::Actions,
                    selected: 0,
                });
            }
            ClientInputOwner::SidebarSortMenu => {
                state.sidebar_sort_menu = Some(SidebarSortMenuState {
                    target: "repo:test".into(),
                    current: SidebarSortMode::Default,
                    anchor: (1, 1),
                    selected: 0,
                });
            }
            ClientInputOwner::SidebarSubgroupPicker => {
                state.sidebar_subgroup_picker = Some(SidebarSubgroupPickerState::default());
            }
            ClientInputOwner::SidebarPodPicker => {
                state.sidebar_pod_picker = Some(SidebarPodPickerState {
                    ws_idx: 0,
                    pane_id: target.pane_id,
                    anchor: (1, 1),
                    filter: crate::ui::dropdown::DropdownFilterState::default(),
                });
            }
            ClientInputOwner::PrActionConfirmation => {
                state.pr_action_confirmation = Some(PrActionConfirmation {
                    key: WorkItemKey {
                        repo: "owner/repo".into(),
                        pr_number: Some(1),
                        pr_url: None,
                        ticket_id: None,
                    },
                    action: crate::ui::work_list_detail::PrActionKind::MarkReady,
                });
            }
            ClientInputOwner::DockSurfaceMenu => {
                state.dock_surface_menu = Some(DockSurfaceMenu { selected: 0 });
            }
        }
    }

    fn replacing_surface_setups() -> [SurfaceSetup; 9] {
        [
            ("editor preview", show_editor_preview),
            ("symphony", show_symphony),
            ("loop history", show_loop_history),
            ("aloop run log", show_aloop_run_log),
            ("usage", show_usage),
            ("work", show_work),
            ("dock object preview", show_dock_object_preview),
            ("home", show_home),
            ("inbox", show_inbox),
        ]
    }

    #[test]
    fn tab_surface_replacement_matches_every_terminal_area_renderer_branch() {
        let empty = AppState::test_new();
        assert!(matches!(
            empty.terminal_area_surface(),
            TerminalAreaSurface::Empty
        ));
        assert!(empty.tab_surface_replaced());

        let mut normal = AppState::test_new();
        normal.workspaces = vec![crate::workspace::Workspace::test_new("tab")];
        normal.active = Some(0);
        assert!(matches!(
            normal.terminal_area_surface(),
            TerminalAreaSurface::Tab
        ));
        assert!(!normal.tab_surface_replaced());

        for (name, setup) in replacing_surface_setups() {
            let mut state = AppState::test_new();
            setup(&mut state);
            assert!(state.tab_surface_replaced(), "{name}");
            assert!(
                !matches!(state.terminal_area_surface(), TerminalAreaSurface::Tab),
                "{name}"
            );
        }
    }

    #[test]
    fn every_client_input_owner_wins_every_competing_surface() {
        let competitors: [SurfaceSetup; 10] = [
            ("popup", show_popup),
            ("symphony", show_symphony),
            ("work", show_work),
            ("usage", show_usage),
            ("dock menu", show_dock_menu),
            ("dock preview", show_dock_object_preview),
            ("home", show_home),
            ("add project", show_add_project),
            ("navigator picker", show_navigator_picker),
            ("work-link picker", show_work_link_picker),
        ];
        let client_owners = [
            ClientInputOwner::Overlay(ClientOverlay::RenameWorkspace),
            ClientInputOwner::Overlay(ClientOverlay::RenameTab),
            ClientInputOwner::Overlay(ClientOverlay::RenamePane),
            ClientInputOwner::Overlay(ClientOverlay::NewLinkedWorktree),
            ClientInputOwner::Overlay(ClientOverlay::OpenExistingWorktree),
            ClientInputOwner::Overlay(ClientOverlay::ConfirmRemoveWorktree),
            ClientInputOwner::Overlay(ClientOverlay::ConfirmClose),
            ClientInputOwner::Overlay(ClientOverlay::ContextMenu),
            ClientInputOwner::SnoozeMenu,
            ClientInputOwner::SnoozeTime,
            ClientInputOwner::SettledMenu,
            ClientInputOwner::SettledDeleteConfirm,
            ClientInputOwner::AgentPicker,
            ClientInputOwner::SidebarGroupMenu,
            ClientInputOwner::SidebarFilterMenu,
            ClientInputOwner::SidebarNewMenu,
            ClientInputOwner::SidebarNewThread,
            ClientInputOwner::SidebarProjectMenu,
            ClientInputOwner::SidebarObjectMenu,
            ClientInputOwner::SidebarSortMenu,
            ClientInputOwner::SidebarSubgroupPicker,
            ClientInputOwner::SidebarPodPicker,
            ClientInputOwner::PrActionConfirmation,
            ClientInputOwner::DockSurfaceMenu,
        ];

        for client_owner in client_owners {
            for (competitor, setup) in competitors {
                let mut state = AppState::test_new();
                state.set_server_mode(Mode::Terminal);
                setup(&mut state);
                install_client_input_owner(&mut state, client_owner);
                assert_eq!(
                    state.input_owner(),
                    InputOwner::Client(client_owner),
                    "{client_owner:?} must win {competitor}"
                );
            }

            let mut state = AppState::test_new();
            install_client_input_owner(&mut state, client_owner);
            let mut sidebar = SidebarPresentationState::default();
            let mut dock = DockPresentationState::default();
            state.swap_sidebar_presentation(&mut sidebar);
            state.swap_dock_presentation(&mut dock);
            let policy = ClientPresentationPolicy::from_presentations(
                Mode::Terminal,
                &sidebar,
                &dock,
                false,
                true,
                std::sync::Arc::new(Vec::new()),
            );
            assert_eq!(
                policy.owner,
                Some(client_owner),
                "saved presentation must preserve {client_owner:?}"
            );
            assert!(
                policy.mouse_motion_changes_view(),
                "{client_owner:?} hover must repaint"
            );
            assert!(
                !policy.pane_graphics_visible(),
                "{client_owner:?} must cover pane graphics"
            );
        }
    }

    #[test]
    fn local_presentation_policy_borrows_rendered_pane_geometry() {
        let mut state = AppState::test_new();
        state.view.pane_infos.push(crate::layout::PaneInfo {
            id: crate::layout::PaneId::from_raw(7),
            rect: Rect::new(1, 2, 30, 10),
            inner_rect: Rect::new(2, 3, 28, 8),
            scrollbar_rect: None,
            borders: ratatui::widgets::Borders::ALL,
            is_focused: true,
        });
        let expected = state.view.pane_infos.as_ptr();

        let policy = state.client_presentation_policy();

        assert_eq!(policy.tab_surface().pane_infos.as_ptr(), expected);
    }

    #[test]
    fn client_pane_focus_intent_wins_shared_mode_and_swaps_per_client() {
        let mut state = AppState::test_new();
        state.workspaces = vec![crate::workspace::Workspace::test_new("pane")];
        state.active = Some(0);
        state.set_server_mode(Mode::Settings);
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        state.swap_sidebar_presentation(&mut client_a);
        state.focus_client_on_pane();
        assert_eq!(state.input_owner(), InputOwner::Pane);
        assert_eq!(state.server_mode(), Mode::Settings);
        state.swap_sidebar_presentation(&mut client_a);

        state.swap_sidebar_presentation(&mut client_b);
        assert_eq!(
            state.input_owner(),
            InputOwner::Server(ServerInputOwner::Settings)
        );
        state.swap_sidebar_presentation(&mut client_b);

        state.swap_sidebar_presentation(&mut client_a);
        assert_eq!(state.input_owner(), InputOwner::Pane);
        state.swap_sidebar_presentation(&mut client_a);
    }

    #[test]
    fn test_state_projects_stalled_agent_nudge_defaults() {
        let state = AppState::test_new();
        assert!(!state.auto_nudge_stalled_agents);
        assert_eq!(
            state.agent_stale_after,
            std::time::Duration::from_secs(5 * 60)
        );
        assert_eq!(state.nudge_after, std::time::Duration::from_secs(5 * 60));
        assert_eq!(state.max_nudges, 3);
        assert_eq!(state.max_nudges_without_human, 5);
        assert_eq!(
            state.stall_nudge_message,
            "Re-verify what you are working on now; do not answer from memory. If you have subagents, poll them and restart any that are stalled. If everything is still progressing, reply with one line: Progressing, plus the count and names of running subagents if any (e.g. Progressing, 2 subagents: build, review). If it is done or something changed, say so and continue."
        );
    }

    fn linear_ownership_ticket(
        identifier: &str,
        assignee: Option<&str>,
        creator: Option<(&str, &str)>,
    ) -> crate::work_index::WorkTicket {
        crate::work_index::WorkTicket {
            identifier: identifier.into(),
            title: None,
            description: None,
            state: Some("In Progress".into()),
            assignee: assignee.map(str::to_string),
            creator: creator.map(|(id, name)| crate::work_index::WorkPerson {
                id: id.into(),
                name: name.into(),
            }),
            priority: None,
            cycle: None,
            group: crate::work_index::TicketGroup::Assigned,
            created_at: None,
            updated_at: None,
            branch: None,
            labels: Vec::new(),
            url: None,
            parent: None,
            relations: Vec::new(),
        }
    }

    #[test]
    fn linear_ownership_matches_assigned_authored_and_both_for_me() {
        let creator_only =
            linear_ownership_ticket("SCA-1", Some("Ada"), Some(("viewer-id", "Matthias")));
        let assignee_only = linear_ownership_ticket("SCA-2", Some("Matthias"), None);
        let foreign = linear_ownership_ticket("SCA-3", Some("Ada"), Some(("ada-id", "Ada")));
        let mut session = crate::work_index::WorkIndexSession::default();
        session.linear.viewer = Some("Matthias".into());
        session.set_linear_viewer_identity_for_test("viewer-id");
        let mut filter = SidebarWorkFilter::default();

        for (ownership, expected) in [
            (WorkOwnershipFilter::Assigned, [false, true, false]),
            (WorkOwnershipFilter::Authored, [true, false, false]),
            (WorkOwnershipFilter::Both, [true, true, false]),
        ] {
            filter.linear_ownership = ownership;
            assert_eq!(
                [
                    filter.matches_linear(&creator_only, &session),
                    filter.matches_linear(&assignee_only, &session),
                    filter.matches_linear(&foreign, &session),
                ],
                expected,
                "{} ownership",
                ownership.label()
            );
        }
    }

    #[test]
    fn sidebar_refresh_request_remains_server_owned_during_presentation_swaps() {
        let mut app = AppState::test_new();
        let mut presentation = SidebarPresentationState::default();
        app.sidebar_new_menu = Some(SidebarNewMenuState { selected: 2 });

        assert!(app.request_sidebar_refresh());
        app.swap_sidebar_presentation(&mut presentation);

        assert!(app.sidebar_refresh_requested);
        assert!(app.sidebar_refreshing);
        assert_eq!(app.sidebar_new_menu, None);
        assert_eq!(presentation.new_menu.map(|menu| menu.selected), Some(2));
    }

    #[test]
    fn subgroup_picker_is_isolated_between_sidebar_presentations() {
        let mut app = AppState::test_new();
        let mut first_client = SidebarPresentationState::default();
        let mut second_client = SidebarPresentationState::default();
        app.sidebar_subgroup_picker = Some(SidebarSubgroupPickerState {
            ws_idx: 0,
            tab_idx: 0,
            anchor: (7, 4),
            filter: crate::ui::dropdown::DropdownFilterState::default(),
        });

        app.swap_sidebar_presentation(&mut first_client);
        assert!(
            app.sidebar_subgroup_picker.is_none(),
            "the first client's picker must leave shared app state"
        );

        app.swap_sidebar_presentation(&mut second_client);
        assert!(!app.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::empty(),
        )));
        assert!(!app.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        )));
        app.swap_sidebar_presentation(&mut second_client);

        app.swap_sidebar_presentation(&mut first_client);
        assert!(app.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::empty(),
        )));
        assert_eq!(
            app.sidebar_subgroup_picker
                .as_ref()
                .map(|picker| picker.filter.query.as_str()),
            Some("a")
        );
        app.swap_sidebar_presentation(&mut first_client);

        app.swap_sidebar_presentation(&mut second_client);
        assert!(app.sidebar_subgroup_picker.is_none());
        assert!(!app.handle_sidebar_subgroup_picker_key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::empty(),
        )));
        app.swap_sidebar_presentation(&mut second_client);

        app.swap_sidebar_presentation(&mut first_client);
        assert_eq!(
            app.sidebar_subgroup_picker
                .as_ref()
                .map(|picker| picker.filter.query.as_str()),
            Some("a")
        );
    }

    #[test]
    fn collapsed_sidebar_groups_are_isolated_between_presentations() {
        let mut app = AppState::test_new();
        let mut first_client = SidebarPresentationState::default();
        let mut second_client = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut first_client);
        app.collapsed_sidebar_groups
            .insert("repo:Snoozed".to_string());
        app.workspace_scroll = 7;
        app.swap_sidebar_presentation(&mut first_client);

        app.swap_sidebar_presentation(&mut second_client);
        assert!(!app.collapsed_sidebar_groups.contains("repo:Snoozed"));
        assert_eq!(app.workspace_scroll, 0);
        app.swap_sidebar_presentation(&mut second_client);

        app.swap_sidebar_presentation(&mut first_client);
        assert!(app.collapsed_sidebar_groups.contains("repo:Snoozed"));
        assert_eq!(app.workspace_scroll, 7);
    }

    #[test]
    fn fresh_sidebar_presentation_seeds_every_view_and_honors_explicit_expansion() {
        let mut presentation = SidebarPresentationState::default();
        let overrides = std::collections::HashMap::from([("repo:Runs".to_string(), false)]);
        presentation.initialize_group_mode(SidebarGroupMode::Spaces, &overrides);

        assert_eq!(presentation.group_mode, SidebarGroupMode::Spaces);
        assert_eq!(
            presentation.group_menu_selected,
            SidebarGroupMode::Spaces.view_index()
        );
        for mode in SidebarGroupMode::ALL {
            let namespace = mode.collapse_namespace();
            if let Some(title) = crate::ui::sidebar::unassigned_section_title(mode) {
                assert!(
                    presentation
                        .collapsed_groups
                        .contains(&format!("{namespace}:{title}")),
                    "{namespace}:{title}"
                );
            }
            for title in ["Snoozed", "Settled", "Fleet", "Runs", "Aloops", "Symphony"] {
                let key = format!("{namespace}:{title}");
                if key == "repo:Runs" {
                    assert!(!presentation.collapsed_groups.contains(&key));
                } else {
                    assert!(presentation.collapsed_groups.contains(&key), "{key}");
                }
            }
        }
    }

    #[test]
    fn snooze_menu_selection_is_isolated_between_sidebar_presentations() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("snooze")];
        let target = PaneFocusTarget {
            workspace_id: app.workspaces[0].id.clone(),
            pane_id: app.workspaces[0].tabs[0].root_pane,
        };
        let mut first_client = SidebarPresentationState::default();
        let mut second_client = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut first_client);
        app.sidebar_focused = true;
        app.sidebar_snooze = Some(SidebarSnoozeUiState {
            target: SidebarPaneLifecycleTarget::Local(target.clone()),
            anchor: (9, 3),
            selected: SidebarSnoozeMenuAction::SetTime,
            time_draft: Some("14:30".to_string()),
            error: Some("example".to_string()),
        });
        app.swap_sidebar_presentation(&mut first_client);

        app.swap_sidebar_presentation(&mut second_client);
        assert!(!app.sidebar_focused);
        assert!(app.sidebar_snooze.is_none());
        app.swap_sidebar_presentation(&mut second_client);

        app.swap_sidebar_presentation(&mut first_client);
        assert_eq!(
            app.sidebar_snooze.as_ref().map(|snooze| (
                &snooze.target,
                snooze.anchor,
                snooze.selected,
                snooze.time_draft.as_deref(),
                snooze.error.as_deref(),
            )),
            Some((
                &SidebarPaneLifecycleTarget::Local(target),
                (9, 3),
                SidebarSnoozeMenuAction::SetTime,
                Some("14:30"),
                Some("example")
            ))
        );
        assert!(app.sidebar_focused);
    }

    #[test]
    fn context_menu_is_isolated_between_sidebar_presentations() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("context-menu")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let workspace_id = app.workspaces[0].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            app.workspaces[0].tabs[0].number,
        );
        let mut first_client = SidebarPresentationState::default();
        let mut second_client = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut first_client);
        app.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id: workspace_id.clone(),
                tab_id: tab_id.clone(),
                ws_idx: 0,
                tab_idx: 0,
                settle_pane_id: Some(pane_id),
                snooze_target: Some(pane_id),
                starred: false,
                has_subgroup: false,
            },
            x: 7,
            y: 3,
            selected: ContextMenuAction::Snooze,
        });
        app.open_client_overlay(ClientOverlay::ContextMenu);
        app.swap_sidebar_presentation(&mut first_client);

        app.swap_sidebar_presentation(&mut second_client);
        assert!(app.context_menu.is_none());
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert_eq!(app.client_overlay, ClientOverlay::None);
        app.swap_sidebar_presentation(&mut second_client);

        app.swap_sidebar_presentation(&mut first_client);
        assert_eq!(app.effective_interaction_mode(), Mode::ContextMenu);
        assert_eq!(app.server_mode(), Mode::Terminal);
        let menu = app.context_menu.as_ref().expect("first client's menu");
        assert_eq!(
            menu.kind,
            ContextMenuKind::Tab {
                workspace_id,
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                settle_pane_id: Some(pane_id),
                snooze_target: Some(pane_id),
                starred: false,
                has_subgroup: false,
            }
        );
        assert_eq!(menu.selected, ContextMenuAction::Snooze);
    }

    #[test]
    fn rename_and_context_menu_are_isolated_between_clients() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("client-modal")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        let workspace_id = app.workspaces[0].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            app.workspaces[0].tabs[0].number,
        );
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut client_a);
        app.open_client_overlay(ClientOverlay::RenameTab);
        app.name_input = "client a draft".into();
        app.rename_target = Some(RenameTarget::Tab {
            workspace_id: workspace_id.clone(),
            tab_id: tab_id.clone(),
        });
        app.swap_sidebar_presentation(&mut client_a);

        app.swap_sidebar_presentation(&mut client_b);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert!(app.rename_target.is_none());
        assert!(app.name_input.is_empty());
        app.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Tab {
                workspace_id: workspace_id.clone(),
                tab_id,
                ws_idx: 0,
                tab_idx: 0,
                settle_pane_id: None,
                snooze_target: None,
                starred: false,
                has_subgroup: false,
            },
            x: 2,
            y: 2,
            selected: ContextMenuAction::NewTab,
        });
        app.open_client_overlay(ClientOverlay::ContextMenu);
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
        assert_eq!(app.effective_interaction_mode(), Mode::ContextMenu);
        assert_eq!(app.server_mode(), Mode::Terminal);
        app.swap_sidebar_presentation(&mut client_b);

        app.swap_sidebar_presentation(&mut client_a);
        assert_eq!(app.effective_interaction_mode(), Mode::RenameTab);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert_eq!(app.name_input, "client a draft");
        assert!(matches!(
            app.rename_target,
            Some(RenameTarget::Tab { ref workspace_id, .. }) if *workspace_id == app.workspaces[0].id
        ));
        assert!(app.context_menu.is_none());
    }

    #[test]
    fn worktree_dialog_survives_another_clients_rename_and_render() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("client-overlay")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        let workspace_id = app.workspaces[0].id.clone();
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut client_b);
        app.open_client_overlay(ClientOverlay::NewLinkedWorktree);
        app.name_input = "feature/client-b-draft".into();
        app.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: workspace_id.clone(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "herdr".into(),
            repo_name: "herdr".into(),
            branch: "feature/client-b-draft".into(),
            checkout_path: "/worktrees/client-b-draft".into(),
            error: None,
            creating: false,
        });
        app.swap_sidebar_presentation(&mut client_b);

        app.swap_sidebar_presentation(&mut client_a);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert!(app.worktree_create.is_none());
        app.open_client_overlay(ClientOverlay::RenameWorkspace);
        app.rename_target = Some(RenameTarget::Workspace {
            workspace_id: workspace_id.clone(),
        });
        app.name_input = "client a rename".into();
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
        assert_eq!(app.effective_interaction_mode(), Mode::RenameWorkspace);
        assert_eq!(app.server_mode(), Mode::Terminal);
        app.swap_sidebar_presentation(&mut client_a);

        app.swap_sidebar_presentation(&mut client_b);
        assert_eq!(app.effective_interaction_mode(), Mode::NewLinkedWorktree);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert_eq!(app.name_input, "feature/client-b-draft");
        let draft = app
            .worktree_create
            .as_ref()
            .expect("client B worktree draft");
        assert_eq!(draft.branch, "feature/client-b-draft");
        assert_eq!(draft.source_workspace_id, workspace_id);
    }

    #[test]
    fn confirm_close_and_settled_delete_arm_are_isolated_between_clients() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("client-overlay")];
        app.active = Some(0);
        app.selected = 0;
        app.set_server_mode(Mode::Terminal);
        let workspace_id = app.workspaces[0].id.clone();
        let pane_id = app.workspaces[0].tabs[0].root_pane;
        let target = PaneFocusTarget {
            workspace_id: workspace_id.clone(),
            pane_id,
        };
        let mut client_a = SidebarPresentationState::default();
        let mut client_b = SidebarPresentationState::default();

        app.swap_sidebar_presentation(&mut client_a);
        app.open_client_overlay(ClientOverlay::ConfirmClose);
        app.confirm_close_workspace_id = Some(workspace_id.clone());
        app.sidebar_settled_menu_target = Some(target.clone());
        app.sidebar_settled_menu_selected = 3;
        app.sidebar_settled_menu_delete_armed = true;
        app.swap_sidebar_presentation(&mut client_a);

        app.swap_sidebar_presentation(&mut client_b);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert!(app.confirm_close_workspace_id.is_none());
        assert!(app.sidebar_settled_menu_target.is_none());
        assert!(!app.sidebar_settled_menu_delete_armed);
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
        app.swap_sidebar_presentation(&mut client_b);

        app.swap_sidebar_presentation(&mut client_a);
        assert_eq!(app.effective_interaction_mode(), Mode::ConfirmClose);
        assert_eq!(app.server_mode(), Mode::Terminal);
        assert_eq!(
            app.confirm_close_workspace_id.as_deref(),
            Some(workspace_id.as_str())
        );
        assert_eq!(app.sidebar_settled_menu_target, Some(target));
        assert_eq!(app.sidebar_settled_menu_selected, 3);
        assert!(app.sidebar_settled_menu_delete_armed);
    }

    #[test]
    fn server_owned_mode_survives_another_clients_overlay_swap() {
        for server_mode in [Mode::Settings, Mode::GlobalMenu, Mode::AddAction] {
            let mut app = AppState::test_new();
            app.workspaces = vec![crate::workspace::Workspace::test_new("server-mode")];
            app.active = Some(0);
            app.selected = 0;
            app.set_server_mode(server_mode);
            app.add_action = (server_mode == Mode::AddAction).then(|| AddActionState {
                name: "keep this draft".into(),
                command: "cargo test".into(),
                ..Default::default()
            });
            let expected_draft = app.add_action.clone();
            let workspace_id = app.workspaces[0].id.clone();
            let pane_id = app.workspaces[0].tabs[0].root_pane;
            let mut client_a = SidebarPresentationState::default();
            let mut client_b = SidebarPresentationState::default();

            app.swap_sidebar_presentation(&mut client_a);

            app.context_menu = Some(ContextMenuState {
                kind: ContextMenuKind::Workspace {
                    workspace_id: workspace_id.clone(),
                    ws_idx: 0,
                },
                x: 2,
                y: 2,
                selected: ContextMenuAction::RenameWorkspace,
            });
            app.open_client_overlay(ClientOverlay::ContextMenu);
            crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
            app.context_menu = None;
            app.close_client_overlay();

            app.rename_target = Some(RenameTarget::Workspace {
                workspace_id: workspace_id.clone(),
            });
            app.name_input = "client a rename".into();
            app.open_client_overlay(ClientOverlay::RenameWorkspace);
            crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
            app.rename_target = None;
            app.name_input.clear();
            app.close_client_overlay();

            app.sidebar_snooze = Some(SidebarSnoozeUiState {
                target: SidebarPaneLifecycleTarget::Local(PaneFocusTarget {
                    workspace_id,
                    pane_id,
                }),
                anchor: (3, 2),
                selected: SidebarSnoozeMenuAction::SetTime,
                time_draft: None,
                error: None,
            });
            crate::ui::compute_view(&mut app, Rect::new(0, 0, 100, 30));
            app.sidebar_snooze = None;
            app.swap_sidebar_presentation(&mut client_a);
            app.swap_sidebar_presentation(&mut client_b);

            assert_eq!(app.server_mode(), server_mode);
            assert_eq!(app.add_action, expected_draft);
            assert_eq!(app.client_overlay, ClientOverlay::None);
            assert!(app.context_menu.is_none());
            assert!(app.rename_target.is_none());
            assert!(app.sidebar_snooze.is_none());
        }
    }

    #[test]
    fn work_projection_rotation_includes_missive_in_both_directions() {
        assert_eq!(
            WorkProjection::Tickets.rotate_right(),
            WorkProjection::Missive
        );
        assert_eq!(
            WorkProjection::Missive.rotate_right(),
            WorkProjection::Agents
        );
        assert_eq!(
            WorkProjection::Agents.rotate_left(),
            WorkProjection::Missive
        );
        assert_eq!(
            WorkProjection::Missive.rotate_left(),
            WorkProjection::Tickets
        );
        assert_eq!(
            WorkProjection::ReviewQueue.rotate_left(),
            WorkProjection::Agents
        );
    }

    #[test]
    fn symphony_refresh_preserves_selection_by_workflow_identity() {
        let workflow = |workflow_id: &str, run_id: &str| crate::symphony::Workflow {
            workflow_id: workflow_id.to_string(),
            run_id: run_id.to_string(),
            name: workflow_id.to_string(),
            phase: "running".to_string(),
            wait: None,
            started_at: None,
            ticket: None,
            repo: None,
            pr: None,
            receipts: None,
        };
        let mut detail = SymphonyDetail {
            snapshot: crate::symphony::Snapshot {
                workflows: vec![workflow("first", "run-1"), workflow("selected", "run-2")],
                unavailable: None,
                polled: true,
            },
            selected: 1,
            observed_at: std::time::SystemTime::UNIX_EPOCH,
        };

        detail.replace_snapshot(crate::symphony::Snapshot {
            workflows: vec![workflow("selected", "run-2"), workflow("first", "run-1")],
            unavailable: None,
            polled: true,
        });

        assert_eq!(detail.selected, 0);
        assert_eq!(
            detail.snapshot.workflows[detail.selected].workflow_id,
            "selected"
        );
        assert_eq!(detail.snapshot.workflows[detail.selected].run_id, "run-2");

        detail.selected = 1;
        detail.replace_snapshot(crate::symphony::Snapshot {
            workflows: vec![workflow("replacement", "run-3")],
            unavailable: None,
            polled: true,
        });
        assert_eq!(detail.selected, 0);
    }

    #[test]
    fn agent_terminal_keeps_final_child_cursor_exposed() {
        let mut state = AppState::test_new();
        let ws = crate::workspace::Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        state.terminals.insert(
            ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
            crate::terminal::TerminalState::new(
                ws.tabs[0].panes[&pane_id].attached_terminal_id.clone(),
                std::path::PathBuf::from("/tmp"),
            ),
        );
        state
            .terminals
            .get_mut(&ws.tabs[0].panes[&pane_id].attached_terminal_id)
            .expect("terminal state")
            .launch_argv = Some(vec!["codex".to_string()]);
        state.workspaces = vec![ws];

        assert!(state.pane_exposes_host_cursor(0, pane_id));
    }

    #[test]
    fn adversarial_identity_state_satisfies_app_invariants_after_mutation() {
        let mut state = AppState::test_with_adversarial_identity_state();
        state.assert_invariants_for_test();

        let ws = &mut state.workspaces[0];
        let active_public = ws.tabs[ws.active_tab].number;
        assert_ne!(ws.active_tab + 1, active_public);
        let new_pane = ws.test_split(ratatui::layout::Direction::Horizontal);
        assert!(ws.public_pane_number(new_pane).is_some());
        state.ensure_test_terminals();

        state.assert_invariants_for_test();
    }

    fn navigator_row_for_display(is_workspace: bool) -> NavigatorRow {
        NavigatorRow {
            target: NavigatorTarget::Workspace { ws_idx: 0 },
            depth: if is_workspace { 0 } else { 1 },
            label: String::new(),
            meta: String::new(),
            status: crate::detect::AgentState::Idle,
            attention_tier: crate::terminal::state::AttentionTier::None,
            seen: true,
            stale: false,
            is_current: false,
            is_workspace,
            is_tab: false,
            expanded: true,
            search_text: String::new(),
            matched: true,
        }
    }

    #[test]
    fn navigator_display_lines_separate_workspace_groups() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![
                NavigatorDisplayLine::Row(0),
                NavigatorDisplayLine::Row(1),
                NavigatorDisplayLine::Spacer,
                NavigatorDisplayLine::Row(2),
                NavigatorDisplayLine::Row(3),
            ]
        );
    }

    #[test]
    fn navigator_display_lines_have_no_leading_spacer() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
        ];
        assert_eq!(
            navigator_display_lines(&rows),
            vec![NavigatorDisplayLine::Row(0), NavigatorDisplayLine::Row(1)]
        );
        assert!(navigator_display_lines(&[]).is_empty());
    }

    #[test]
    fn navigator_display_index_maps_row_to_line() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        assert_eq!(navigator_display_index_of_row(&lines, 2), Some(3));
        assert_eq!(navigator_display_index_of_row(&lines, 9), None);
    }

    #[test]
    fn navigator_first_row_skips_spacer_lines() {
        let rows = vec![
            navigator_row_for_display(true),
            navigator_row_for_display(false),
            navigator_row_for_display(true),
        ];
        let lines = navigator_display_lines(&rows);
        // Line 2 is the spacer before the second workspace.
        assert_eq!(navigator_first_row_at_or_after(&lines, 2), Some(2));
        assert_eq!(navigator_first_row_at_or_after(&lines, 4), None);
    }

    fn rgb_luminance(color: Color) -> f64 {
        let Color::Rgb(r, g, b) = color else {
            panic!("expected RGB color, got {color:?}");
        };
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast_ratio(a: Color, b: Color) -> f64 {
        let (lighter, darker) = {
            let a = rgb_luminance(a);
            let b = rgb_luminance(b);
            (a.max(b), a.min(b))
        };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn built_in_theme_names_resolve() {
        for name in THEME_NAMES {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn built_in_active_rows_remain_visible_with_matching_terminal_backgrounds() {
        for name in THEME_NAMES
            .iter()
            .copied()
            .filter(|name| *name != "terminal")
        {
            let palette = Palette::from_name(name).unwrap();
            let background_contrast = contrast_ratio(palette.panel_bg, palette.active_row_bg);
            assert!(
                background_contrast >= 1.05,
                "active row blends into the matching terminal background for {name}: {background_contrast:.2}:1"
            );

            let text_contrast = contrast_ratio(palette.text, palette.active_row_bg);
            assert!(
                text_contrast >= 3.0,
                "active row text loses contrast for {name}: {text_contrast:.2}:1"
            );
        }
    }

    #[test]
    fn built_in_selection_rows_stay_distinct_from_background_and_active_rows() {
        for name in THEME_NAMES
            .iter()
            .copied()
            .filter(|name| *name != "terminal")
        {
            let palette = Palette::from_name(name).unwrap();
            let background_contrast = contrast_ratio(palette.panel_bg, palette.selection_bg);
            assert!(
                background_contrast >= 1.05,
                "selection row blends into the matching terminal background for {name}: {background_contrast:.2}:1"
            );

            let text_contrast = contrast_ratio(palette.text, palette.selection_bg);
            assert!(
                text_contrast >= 3.0,
                "selection row text loses contrast for {name}: {text_contrast:.2}:1"
            );
            assert_ne!(
                palette.selection_bg, palette.active_row_bg,
                "selection row shares the active row color for {name}"
            );
        }
    }

    #[test]
    fn built_in_themes_leave_sidebar_background_unset() {
        for name in THEME_NAMES {
            let palette = Palette::from_name(name).unwrap();
            assert_eq!(
                palette.sidebar_bg, None,
                "built-in theme changed the sidebar background: {name}"
            );
        }
    }

    #[test]
    fn custom_sidebar_background_overrides_the_default() {
        let custom = crate::config::CustomThemeColors {
            sidebar_bg: Some("#181825".to_string()),
            ..Default::default()
        };

        assert_eq!(
            Palette::catppuccin().with_overrides(&custom).sidebar_bg,
            Some(Color::Rgb(24, 24, 37))
        );
    }

    /// `reset` is documented as a value for this token, and it means something
    /// an unset field cannot: hand the sidebar back to the terminal on a themed
    /// palette. Unset follows `panel_bg` instead, so the two cannot share a
    /// representation.
    #[test]
    fn an_explicit_reset_sidebar_background_still_inherits_the_terminal() {
        let custom = crate::config::CustomThemeColors {
            sidebar_bg: Some("reset".to_string()),
            ..Default::default()
        };
        let palette = Palette::catppuccin().with_overrides(&custom);

        assert_eq!(palette.sidebar_bg, Some(Color::Reset));
        assert_eq!(palette.sidebar_background(), Color::Reset);
        assert_eq!(
            Palette::catppuccin().sidebar_background(),
            Color::Rgb(24, 24, 37)
        );
    }

    /// Named and indexed colors are legal everywhere a hex color is, so a
    /// mismatch must still be classified when a palette carries them.
    #[test]
    fn appearance_classifies_named_and_indexed_panel_backgrounds() {
        use crate::terminal_theme::HostAppearance;

        let mut light = Palette::catppuccin();
        light.panel_bg = Color::White;
        assert_eq!(light.appearance(), Some(HostAppearance::Light));

        let mut dark = Palette::catppuccin_latte();
        dark.panel_bg = Color::Indexed(16);
        assert_eq!(dark.appearance(), Some(HostAppearance::Dark));

        let mut inherited = Palette::catppuccin();
        inherited.panel_bg = Color::Reset;
        assert_eq!(inherited.appearance(), None);
    }

    #[test]
    fn light_theme_aliases_resolve() {
        for name in ["light", "latte", "tokyo-day", "onelight", "lotus", "dawn"] {
            assert!(
                Palette::from_name(name).is_some(),
                "theme should resolve: {name}"
            );
        }
    }

    #[test]
    fn key_matches_requires_exact_modifiers() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        assert!(!key_matches(
            &KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
    }

    #[test]
    fn key_matches_letters_case_insensitively() {
        assert!(key_matches(
            &KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT),
            KeyCode::Char('b'),
            KeyModifiers::SHIFT,
        ));
    }

    fn pane_menu_sending(text: &str, has_agent_targets: bool) -> ContextMenuState {
        let mut menu = pane_menu(None, None);
        if let ContextMenuKind::Pane {
            send_text,
            has_agent_targets: targets,
            ..
        } = &mut menu.kind
        {
            *send_text = Some(text.to_string());
            *targets = has_agent_targets;
        }
        menu
    }

    /// A pane with nothing to send offers neither agent entry, so the menu
    /// never promises to hand an agent an empty prompt.
    #[test]
    fn a_pane_with_no_clicked_text_offers_no_agent_entries() {
        let items = pane_menu(None, None).items();
        assert!(!items.contains(&SEND_TO_NEW_AGENT_ITEM));
        assert!(!items.contains(&SEND_TO_EXISTING_AGENT_ITEM));
    }

    /// A new agent can always be started; an existing one needs to exist.
    #[test]
    fn clicked_text_always_offers_a_new_agent_and_only_sometimes_a_running_one() {
        let alone = pane_menu_sending("cargo test", false).items();
        assert!(alone.contains(&SEND_TO_NEW_AGENT_ITEM));
        assert!(!alone.contains(&SEND_TO_EXISTING_AGENT_ITEM));

        let with_peers = pane_menu_sending("cargo test", true).items();
        assert!(with_peers.contains(&SEND_TO_NEW_AGENT_ITEM));
        assert!(with_peers.contains(&SEND_TO_EXISTING_AGENT_ITEM));
    }

    fn pane_menu(link: Option<&str>, path: Option<PaneMenuPath>) -> ContextMenuState {
        let open_with = path
            .as_ref()
            .map(|_| PaneOpenWith::ALL.to_vec())
            .unwrap_or_default();
        ContextMenuState {
            kind: ContextMenuKind::Pane {
                workspace_id: "workspace".into(),
                tab_id: "workspace:t1".into(),
                ws_idx: 0,
                tab_idx: 0,
                pane_id: crate::layout::PaneId::alloc(),
                source_pane_id: None,
                has_manual_label: false,
                right_click_passthrough: false,
                linkable_work_link: None,
                link: link.map(str::to_string),
                path,
                open_with,
                send_text: None,
                has_agent_targets: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RenamePane,
        }
    }

    #[test]
    fn a_clicked_file_offers_both_the_file_and_its_folder_to_every_editor() {
        let menu = pane_menu(
            None,
            Some(PaneMenuPath {
                path: std::path::PathBuf::from("/repo/src/ui.rs"),
                line: Some(12),
                is_dir: false,
            }),
        );

        let items = menu.items();
        for target in PaneOpenWith::ALL {
            assert!(items.contains(&target.file_item()), "{items:?}");
            assert!(items.contains(&target.directory_item()), "{items:?}");
        }
        assert!(!items.contains(&COPY_LINK_ITEM), "{items:?}");
    }

    #[test]
    fn a_clicked_folder_offers_no_file_entry() {
        let menu = pane_menu(
            None,
            Some(PaneMenuPath {
                path: std::path::PathBuf::from("/repo/src"),
                line: None,
                is_dir: true,
            }),
        );

        let items = menu.items();
        for target in PaneOpenWith::ALL {
            assert!(!items.contains(&target.file_item()), "{items:?}");
            assert!(items.contains(&target.directory_item()), "{items:?}");
        }
    }

    /// A folder is what you open, not what you jump into: the file entry only
    /// exists when there is a file, and the folder entry only when the path has
    /// one to name.
    #[test]
    fn a_root_directory_offers_nothing_to_open() {
        let menu = pane_menu(
            None,
            Some(PaneMenuPath {
                path: std::path::PathBuf::from("/repo/src/ui.rs"),
                line: None,
                is_dir: false,
            }),
        );
        assert!(menu.items().contains(&PaneOpenWith::Nvim.directory_item()));

        let orphan = pane_menu(
            None,
            Some(PaneMenuPath {
                path: std::path::PathBuf::from("ui.rs"),
                line: None,
                is_dir: false,
            }),
        );
        let items = orphan.items();
        assert!(items.contains(&PaneOpenWith::Nvim.file_item()), "{items:?}");
        assert!(
            !items.contains(&PaneOpenWith::Nvim.directory_item()),
            "{items:?}"
        );
    }

    #[test]
    fn only_a_web_link_is_worth_opening_but_any_link_is_worth_copying() {
        let web = pane_menu(Some("https://example.com/a"), None);
        assert!(web.items().contains(&OPEN_LINK_ITEM));
        assert!(web.items().contains(&COPY_LINK_ITEM));

        let other = pane_menu(Some("ftp://example.com/a"), None);
        assert!(!other.items().contains(&OPEN_LINK_ITEM));
        assert!(other.items().contains(&COPY_LINK_ITEM));
    }

    #[test]
    fn linked_worktree_context_menu_keeps_safe_close_and_explicit_remove() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                workspace_id: "workspace".into(),
                ws_idx: 0,
                is_linked_worktree: true,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RenameWorkspace,
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "Delete worktree checkout..."]
        );
    }

    #[test]
    fn git_workspace_context_menu_keeps_remove_for_managed_worktrees_only() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                workspace_id: "workspace".into(),
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: false,
                collapsed: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RenameWorkspace,
        };

        assert_eq!(
            menu.items(),
            &["Rename", "Close", "New worktree", "Open worktree..."]
        );
    }

    #[test]
    fn parent_worktree_context_menu_uses_repo_actions() {
        let menu = ContextMenuState {
            kind: ContextMenuKind::GitWorkspace {
                workspace_id: "workspace".into(),
                ws_idx: 0,
                is_linked_worktree: false,
                has_worktree_children: true,
                collapsed: false,
            },
            x: 0,
            y: 0,
            selected: ContextMenuAction::RenameWorkspace,
        };

        assert_eq!(
            menu.items(),
            &[
                "Rename",
                "Close group",
                "New worktree",
                "Open worktree...",
                "Collapse"
            ]
        );
    }

    #[test]
    fn dock_file_state_and_preview_are_swapped_with_client_presentation() {
        let mut state = AppState::test_new();
        let mut client = DockPresentationState {
            files_sort: crate::files::FileSort::GitStatus,
            files_search_active: true,
            editor_preview: Some(DockEditorPreview {
                path: "/repo/src/lib.rs".into(),
                content: "fn lib() {}".into(),
                notice: None,
            }),
            ..DockPresentationState::default()
        };

        state.swap_dock_presentation(&mut client);

        assert_eq!(state.dock_files_sort, crate::files::FileSort::GitStatus);
        assert!(state.dock_files_search_active);
        assert_eq!(
            state
                .dock_editor_preview
                .as_ref()
                .map(|preview| preview.path.as_path()),
            Some(std::path::Path::new("/repo/src/lib.rs"))
        );
        assert_eq!(client.files_sort, crate::files::FileSort::Name);
        assert!(!client.files_search_active);
        assert!(client.editor_preview.is_none());
    }

    #[test]
    fn dock_symphony_selection_is_swapped_with_client_presentation() {
        let mut state = AppState::test_new();
        let selection = SymphonyDockSelection {
            workflow_id: "symphony-MAT-138".to_string(),
            run_id: "019a".to_string(),
        };
        let mut client = DockPresentationState {
            symphony: Some(selection.clone()),
            ..DockPresentationState::default()
        };

        state.swap_dock_presentation(&mut client);
        assert_eq!(state.dock_symphony, Some(selection.clone()));
        assert_eq!(client.symphony, None);

        // Detaching hands the selection back to the client, so another client
        // attached to the same state never sees this one's job.
        state.swap_dock_presentation(&mut client);
        assert_eq!(state.dock_symphony, None);
        assert_eq!(client.symphony, Some(selection));
    }

    fn app_with_object_and_bare_panes() -> (AppState, PaneId, PaneId) {
        let mut state = AppState::test_new();
        // Context tabs are the configured behaviour these cases describe; the
        // default path is covered by `work_links_do_not_open_the_dock_by_default`.
        state.open_dock_on_work_link = true;
        let mut workspace = crate::workspace::Workspace::test_new("objects");
        let object_pane = workspace.tabs[0].root_pane;
        workspace.test_add_tab(Some("bare"));
        let bare_pane = workspace.tabs[1].root_pane;
        state.workspaces = vec![workspace];
        state.active = Some(0);
        state.selected = 0;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0]
            .terminal_id(object_pane)
            .expect("object terminal")
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("object terminal state")
            .replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/scalable-so/herdr/pull/206".into()],
                ticket_ids: vec!["SCA-3165".into()],
                ..Default::default()
            });
        (state, object_pane, bare_pane)
    }

    #[test]
    fn symphony_dock_binds_to_the_newly_focused_pane_not_the_one_left() {
        let (mut state, object_pane, bare_pane) = app_with_object_and_bare_panes();
        // The object pane already has focus; `false` only says nothing moved.
        let _ = state.focus_pane_in_workspace(0, object_pane);
        state.reconcile_dock_context_tabs();
        let workflow = crate::symphony::Workflow {
            workflow_id: "symphony-MAT-138".to_string(),
            run_id: "019a".to_string(),
            name: "blocker dashboard".to_string(),
            phase: "runFlowStep".to_string(),
            wait: None,
            started_at: None,
            ticket: None,
            repo: None,
            pr: None,
            receipts: None,
        };

        // Opening a job creates and focuses its checkout tab, then binds.
        assert!(state.focus_pane_in_workspace(0, bare_pane));
        state.bind_symphony_dock_to_focused_pane(&workflow);
        state.reconcile_dock_context_tabs();
        assert_eq!(state.dock_tab, Some(DockSurface::Symphony));
        assert_eq!(
            state.dock_followed_pane,
            state.current_pane_focus_target(),
            "the dock follows the checkout pane"
        );

        // The pane the click came from never received the surface.
        assert!(state.focus_pane_in_workspace(0, object_pane));
        state.reconcile_dock_context_tabs();
        assert!(!state.dock_open_surfaces.contains(&DockSurface::Symphony));

        // A plain bind after the focus change reproduces the lost surface.
        assert!(state.focus_pane_in_workspace(0, bare_pane));
        state.reconcile_dock_context_tabs();
        assert!(state.focus_pane_in_workspace(0, object_pane));
        state.bind_symphony_dock(&workflow);
        state.reconcile_dock_context_tabs();
        assert_ne!(state.dock_tab, Some(DockSurface::Symphony));
    }

    #[test]
    fn work_links_do_not_open_the_dock_by_default() {
        let (mut state, object_pane, _) = app_with_object_and_bare_panes();
        state.open_dock_on_work_link = false;
        state.dock_collapsed = true;
        let _ = state.focus_pane_in_workspace(0, object_pane);
        state.reconcile_dock_context_tabs();

        assert!(
            state.dock_open_surfaces.is_empty(),
            "a linked pane opens no dock tab on its own"
        );
        assert!(
            state.dock_collapsed,
            "the dock stays where the human left it"
        );
        assert_eq!(
            state
                .dock_context_objects
                .iter()
                .map(|object| state.dock_object_label(object))
                .collect::<Vec<_>>(),
            ["#206", "SCA-3165"],
            "the links are still tracked, for the status row to name"
        );

        // The click on a named link is what opens it, and it opens the dock.
        let object = state.dock_context_objects[1].clone();
        state.dock_collapsed = false;
        state.open_dock_object(object.clone(), DockTabOrigin::User);
        assert_eq!(state.dock_tab, Some(DockSurface::Linear));
        assert_eq!(state.dock_tab_label(0), "SCA-3165");
        assert!(!state.dock_collapsed);
    }

    #[test]
    fn f20_context_tabs_follow_objects_and_bare_panes_stay_empty() {
        let (mut state, object_pane, bare_pane) = app_with_object_and_bare_panes();
        // Adopting the context tabs is unconditional; revealing the dock is the
        // opt-in half, so this case turns it on and the next one leaves it off.
        state.open_dock_on_work_link = true;
        state.reconcile_dock_context_tabs();
        assert_eq!(
            (0..state.dock_open_surfaces.len())
                .map(|index| state.dock_tab_label(index))
                .collect::<Vec<_>>(),
            ["#206", "SCA-3165"]
        );
        assert!(state.dock_tab_bindings.iter().all(|binding| {
            binding
                .as_ref()
                .is_some_and(|binding| binding.origin == DockTabOrigin::Context)
        }));
        assert!(!state.dock_collapsed);
        state.open_dock_surface(DockSurface::Files);

        assert!(state.focus_pane_in_workspace(0, bare_pane));
        state.reconcile_dock_context_tabs();
        assert!(state.dock_open_surfaces.is_empty());
        assert_eq!(state.dock_tab, None);

        assert!(state.focus_pane_in_workspace(0, object_pane));
        state.reconcile_dock_context_tabs();
        assert_eq!(state.dock_open_surfaces.len(), 3);
        assert!(state.dock_open_surfaces.contains(&DockSurface::Files));
    }

    #[test]
    fn f20_closed_context_tab_waits_for_an_object_set_change() {
        let (mut state, object_pane, _) = app_with_object_and_bare_panes();
        state.reconcile_dock_context_tabs();
        state.close_dock_surface(DockSurface::Pr);
        assert_eq!(state.dock_tab_label(0), "SCA-3165");

        state.reconcile_dock_context_tabs();
        assert_eq!(
            (0..state.dock_open_surfaces.len())
                .map(|index| state.dock_tab_label(index))
                .collect::<Vec<_>>(),
            ["SCA-3165"]
        );

        let terminal_id = state.workspaces[0]
            .terminal_id(object_pane)
            .expect("object terminal")
            .clone();
        let mut context = state.terminals[&terminal_id]
            .effective_work_context()
            .clone();
        context.ticket_ids.push("SCA-4000".into());
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("object terminal state")
            .replace_prevalidated_manual_work_context(context);
        state.reconcile_dock_context_tabs();
        assert_eq!(
            (0..state.dock_open_surfaces.len())
                .map(|index| state.dock_tab_label(index))
                .collect::<Vec<_>>(),
            ["#206", "SCA-4000"]
        );
    }

    #[test]
    fn f20_user_opened_object_tab_persists_when_context_changes() {
        let (mut state, object_pane, _) = app_with_object_and_bare_panes();
        state.reconcile_dock_context_tabs();
        let first = state.dock_tab_bindings[0]
            .as_ref()
            .expect("context binding")
            .object
            .clone();
        state.open_dock_object(first, DockTabOrigin::User);

        let terminal_id = state.workspaces[0]
            .terminal_id(object_pane)
            .expect("object terminal")
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("object terminal state")
            .replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
                pr_urls: vec!["https://github.com/scalable-so/herdr/pull/206".into()],
                ticket_ids: vec!["SCA-4000".into()],
                ..Default::default()
            });
        state.reconcile_dock_context_tabs();

        assert!(state.dock_tab_bindings.iter().any(|binding| binding
            .as_ref()
            .is_some_and(|binding| binding.object.key.ends_with("/206")
                && binding.origin == DockTabOrigin::User)));
    }

    #[test]
    fn f20_view_switch_focuses_an_existing_object_without_creating_a_list_tab() {
        let (mut state, _, _) = app_with_object_and_bare_panes();
        state.reconcile_dock_context_tabs();
        state.open_dock_surface(DockSurface::Files);
        let before = state.dock_open_surfaces.len();

        state.set_sidebar_group_mode(SidebarGroupMode::LinearTeam);

        assert_eq!(state.dock_tab, Some(DockSurface::Linear));
        assert_eq!(state.dock_open_surfaces.len(), before);
        assert!(state.active_dock_object(DockSurface::Linear).is_some());
    }

    #[test]
    fn f20_object_labels_include_missive_cell_truncation_and_full_title() {
        let mut state = AppState::test_new();
        let object = DockObjectRef {
            surface: DockSurface::Missive,
            key: "https://mail.missiveapp.com/#inbox/conversations/abc".into(),
        };
        state.dock_open_surfaces = vec![DockSurface::Missive];
        state.dock_tab_bindings = vec![Some(DockTabBinding {
            object,
            origin: DockTabOrigin::Context,
        })];
        state.dock_tab = Some(DockSurface::Missive);
        state.dock_active_tab_index = Some(0);
        state.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: vec![crate::work_index::MissiveConversation {
                id: "abc".into(),
                subject: "提交 attachment review".into(),
                app_url: "https://mail.missiveapp.com/#inbox/conversations/abc".into(),
                web_url: String::new(),
                team: None,
                assignees: Vec::new(),
                last_activity_at: None,
                closed: false,
                labels: Vec::new(),
                pane_bound: true,
                messages: Vec::new(),
                notes: Vec::new(),
                drafts: Vec::new(),
                posts: Vec::new(),
            }],
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        });

        assert!(crate::ui::text::display_width(&state.dock_tab_label(0)) <= 12);
        assert_eq!(state.dock_tab_title(0), "提交 attachment review");
    }

    #[test]
    fn rich_object_tab_and_scroll_persist_per_object_and_per_client() {
        let pr = WorkItemKey {
            repo: "owner/repo".into(),
            pr_number: Some(206),
            pr_url: Some("https://github.com/owner/repo/pull/206".into()),
            ticket_id: None,
        };
        let ticket = WorkItemKey {
            repo: String::new(),
            pr_number: None,
            pr_url: None,
            ticket_id: Some("SCA-3165".into()),
        };
        let mut state = AppState::test_new();
        state.dock_object_preview = Some(DockObjectRef {
            surface: DockSurface::Linear,
            key: "SCA-3165".into(),
        });
        state.dock_object_views.insert(
            pr.clone(),
            ObjectViewState {
                tab: PrDetailTab::Files,
                scroll: 11,
                ..Default::default()
            },
        );
        state.dock_object_views.insert(
            ticket.clone(),
            ObjectViewState {
                scroll: 7,
                ..Default::default()
            },
        );

        assert_eq!(state.dock_object_views[&pr].tab, PrDetailTab::Files);
        assert_eq!(state.dock_object_views[&pr].scroll, 11);
        assert_eq!(state.dock_object_views[&ticket].scroll, 7);

        let mut client = DockPresentationState::default();
        state.swap_dock_presentation(&mut client);
        assert!(state.dock_object_preview.is_none());
        assert!(state.dock_object_views.is_empty());
        assert_eq!(
            client.object_preview,
            Some(DockObjectRef {
                surface: DockSurface::Linear,
                key: "SCA-3165".into(),
            })
        );
        assert_eq!(client.object_views[&pr].tab, PrDetailTab::Files);
        assert_eq!(client.object_views[&ticket].scroll, 7);
    }
}
