use serde::Deserialize;
use tracing::warn;

pub const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

pub(crate) fn canonical_theme_name(name: &str) -> Option<&'static str> {
    match name.to_lowercase().replace([' ', '_'], "-").as_str() {
        "catppuccin" | "catppuccin-mocha" => Some("catppuccin"),
        "catppuccin-latte" | "latte" | "light" => Some("catppuccin-latte"),
        "terminal" => Some("terminal"),
        "tokyo-night" | "tokyonight" => Some("tokyo-night"),
        "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some("tokyo-night-day"),
        "dracula" => Some("dracula"),
        "nord" => Some("nord"),
        "gruvbox" | "gruvbox-dark" => Some("gruvbox"),
        "gruvbox-light" => Some("gruvbox-light"),
        "one-dark" | "onedark" => Some("one-dark"),
        "one-light" | "onelight" => Some("one-light"),
        "solarized" | "solarized-dark" => Some("solarized"),
        "solarized-light" => Some("solarized-light"),
        "kanagawa" => Some("kanagawa"),
        "kanagawa-lotus" | "lotus" => Some("kanagawa-lotus"),
        "rose-pine" | "rosepine" => Some("rose-pine"),
        "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some("rose-pine-dawn"),
        "vesper" => Some("vesper"),
        _ => None,
    }
}

/// Theme configuration: pick a built-in or override individual tokens.
///
/// ```toml
/// [theme]
/// name = "tokyo-night"  # built-in: catppuccin, terminal, dracula, nord, etc.
///
/// [theme.custom]        # override individual tokens on top of the base
/// accent = "#f5c2e7"
/// red = "#ff6188"
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Built-in theme name. Default: "catppuccin".
    pub name: Option<String>,
    /// Follow host terminal light/dark appearance and switch between theme names.
    pub auto_switch: bool,
    /// Theme name used when `auto_switch` selects a dark appearance.
    pub dark_name: Option<String>,
    /// Theme name used when `auto_switch` selects a light appearance.
    pub light_name: Option<String>,
    /// Custom overrides — applied on top of the selected base theme.
    pub custom: Option<CustomThemeColors>,
}

impl ThemeConfig {
    pub(crate) fn diagnostics(&self) -> Vec<String> {
        let valid = THEME_NAMES.join(", ");
        [
            ("theme.name", self.name.as_deref(), "catppuccin"),
            ("theme.dark_name", self.dark_name.as_deref(), "catppuccin"),
            (
                "theme.light_name",
                self.light_name.as_deref(),
                "catppuccin-latte",
            ),
        ]
        .into_iter()
        .filter_map(|(field, value, fallback)| {
            let value = value?;
            canonical_theme_name(value).is_none().then(|| {
                format!(
                    "unknown theme name {field} = {value:?}; using {fallback:?}; valid themes: {valid}"
                )
            })
        })
        .collect()
    }
}

/// Per-token color overrides. All fields optional — only set what you want to change.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct CustomThemeColors {
    pub accent: Option<String>,
    pub panel_bg: Option<String>,
    pub sidebar_bg: Option<String>,
    pub active_row_bg: Option<String>,
    pub selection_bg: Option<String>,
    pub surface0: Option<String>,
    pub surface1: Option<String>,
    pub surface_dim: Option<String>,
    pub overlay0: Option<String>,
    pub overlay1: Option<String>,
    pub text: Option<String>,
    pub subtext0: Option<String>,
    pub mauve: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub red: Option<String>,
    pub blue: Option<String>,
    pub teal: Option<String>,
    pub peach: Option<String>,
}

/// Parse a color string into a ratatui Color.
/// Supports: hex (#rrggbb, #rgb), named colors, rgb(r,g,b), and reset aliases.
pub fn parse_color(s: &str) -> ratatui::style::Color {
    use ratatui::style::Color;
    let s = s.trim().to_lowercase();

    match s.as_str() {
        "reset" | "default" | "none" | "transparent" => return Color::Reset,
        _ => {}
    }

    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                u8::from_str_radix(&hex[0..2], 16),
                u8::from_str_radix(&hex[2..4], 16),
                u8::from_str_radix(&hex[4..6], 16),
            ) {
                return Color::Rgb(r, g, b);
            }
        } else if hex.len() == 3 {
            let chars: Vec<u8> = hex
                .chars()
                .filter_map(|c| u8::from_str_radix(&c.to_string(), 16).ok())
                .collect();
            if chars.len() == 3 {
                return Color::Rgb(chars[0] * 17, chars[1] * 17, chars[2] * 17);
            }
        }
    }

    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            if let (Ok(r), Ok(g), Ok(b)) = (
                parts[0].trim().parse::<u8>(),
                parts[1].trim().parse::<u8>(),
                parts[2].trim().parse::<u8>(),
            ) {
                return Color::Rgb(r, g, b);
            }
        }
    }

    match s.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        _ => {
            warn!(color = s, "unknown color, defaulting to cyan");
            Color::Cyan
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn theme_name_parses() {
        let toml = r#"
[theme]
name = "dracula"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.theme.name.as_deref(), Some("dracula"));
    }

    #[test]
    fn unknown_theme_names_are_diagnosed() {
        let config: Config = toml::from_str(
            r#"
[theme]
name = "catppucin"
dark_name = "tokio-night"
light_name = "lattee"
"#,
        )
        .unwrap();

        let diagnostics = config.theme.diagnostics();
        assert_eq!(diagnostics.len(), 3);
        assert!(diagnostics[0].contains("theme.name = \"catppucin\""));
        assert!(diagnostics[0].contains("using \"catppuccin\""));
        assert!(diagnostics[1].contains("theme.dark_name = \"tokio-night\""));
        assert!(diagnostics[2].contains("theme.light_name = \"lattee\""));
        assert!(diagnostics[2].contains("using \"catppuccin-latte\""));
    }

    #[test]
    fn theme_name_aliases_are_valid() {
        for name in ["catppuccin-mocha", "tokyonight", "gruvbox-dark", "dawn"] {
            assert!(canonical_theme_name(name).is_some(), "alias: {name}");
        }
    }

    #[test]
    fn parse_color_accepts_reset_aliases() {
        use ratatui::style::Color;

        for value in ["reset", "default", "none", "transparent"] {
            assert_eq!(parse_color(value), Color::Reset, "value: {value}");
        }
    }

    #[test]
    fn theme_auto_switch_fields_parse() {
        let toml = r#"
[theme]
name = "catppuccin"
auto_switch = true
dark_name = "tokyo-night"
light_name = "catppuccin-latte"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.theme.name.as_deref(), Some("catppuccin"));
        assert!(config.theme.auto_switch);
        assert_eq!(config.theme.dark_name.as_deref(), Some("tokyo-night"));
        assert_eq!(config.theme.light_name.as_deref(), Some("catppuccin-latte"));
    }

    #[test]
    fn theme_custom_overrides_parse() {
        let toml = r##"
[theme]
name = "nord"

[theme.custom]
panel_bg = "#1e1e2e"
sidebar_bg = "#181825"
active_row_bg = "#313244"
selection_bg = "#45475a"
accent = "#ff79c6"
red = "rgb(255, 85, 85)"
"##;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.theme.name.as_deref(), Some("nord"));
        let custom = config.theme.custom.as_ref().unwrap();
        assert_eq!(custom.panel_bg.as_deref(), Some("#1e1e2e"));
        assert_eq!(custom.sidebar_bg.as_deref(), Some("#181825"));
        assert_eq!(custom.active_row_bg.as_deref(), Some("#313244"));
        assert_eq!(custom.selection_bg.as_deref(), Some("#45475a"));
        assert_eq!(custom.accent.as_deref(), Some("#ff79c6"));
        assert_eq!(custom.red.as_deref(), Some("rgb(255, 85, 85)"));
        assert!(custom.green.is_none());
    }

    #[test]
    fn theme_defaults_when_missing() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.theme.name.is_none());
        assert!(!config.theme.auto_switch);
        assert!(config.theme.dark_name.is_none());
        assert!(config.theme.light_name.is_none());
        assert!(config.theme.custom.is_none());
    }
}
