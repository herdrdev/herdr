use crate::terminal::TerminalId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidebarRowColor {
    pub(crate) r: u8,
    pub(crate) g: u8,
    pub(crate) b: u8,
}

impl SidebarRowColor {
    pub(crate) const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub(crate) fn ratatui(self) -> ratatui::style::Color {
        ratatui::style::Color::Rgb(self.r, self.g, self.b)
    }

    pub(crate) fn contrast(self) -> ratatui::style::Color {
        let luminance = u32::from(self.r) * 299 + u32::from(self.g) * 587 + u32::from(self.b) * 114;
        if luminance >= 128_000 {
            ratatui::style::Color::Black
        } else {
            ratatui::style::Color::White
        }
    }

    pub(crate) fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }

    pub(crate) fn parse_hex(value: &str) -> Option<Self> {
        let hex = value.trim().strip_prefix('#')?;
        if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        match hex.len() {
            3 => {
                let mut digits = hex
                    .bytes()
                    .map(|byte| char::from(byte).to_digit(16).map(|digit| digit as u8 * 17));
                Some(Self::new(digits.next()??, digits.next()??, digits.next()??))
            }
            6 => Some(Self::new(
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            )),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SidebarColorTarget {
    Workspace { workspace_id: String },
    Tab { tab_id: String },
    Agent { terminal_id: TerminalId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SidebarColorPickerState {
    pub(crate) target: SidebarColorTarget,
    pub(crate) target_label: String,
    pub(crate) input: String,
    pub(crate) replace_on_type: bool,
    pub(crate) selected_preset: usize,
    pub(crate) error: Option<String>,
}

pub(crate) const SIDEBAR_COLOR_PRESETS: [(&str, SidebarRowColor); 12] = [
    ("red", SidebarRowColor::new(0xF3, 0x8B, 0xA8)),
    ("orange", SidebarRowColor::new(0xFA, 0xB3, 0x87)),
    ("yellow", SidebarRowColor::new(0xF9, 0xE2, 0xAF)),
    ("lime", SidebarRowColor::new(0xC6, 0xE3, 0x77)),
    ("green", SidebarRowColor::new(0xA6, 0xE3, 0xA1)),
    ("teal", SidebarRowColor::new(0x94, 0xE2, 0xD5)),
    ("cyan", SidebarRowColor::new(0x89, 0xDC, 0xEB)),
    ("blue", SidebarRowColor::new(0x89, 0xB4, 0xFA)),
    ("indigo", SidebarRowColor::new(0x8C, 0x8F, 0xF0)),
    ("purple", SidebarRowColor::new(0xCB, 0xA6, 0xF7)),
    ("pink", SidebarRowColor::new(0xF5, 0xC2, 0xE7)),
    ("gray", SidebarRowColor::new(0xBA, 0xC2, 0xDE)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_and_long_hex_colors_strictly() {
        assert_eq!(
            SidebarRowColor::parse_hex("#1aF"),
            Some(SidebarRowColor::new(0x11, 0xAA, 0xFF))
        );
        assert_eq!(
            SidebarRowColor::parse_hex("#12aBcF"),
            Some(SidebarRowColor::new(0x12, 0xAB, 0xCF))
        );
        assert_eq!(SidebarRowColor::parse_hex("12ABCF"), None);
        assert_eq!(SidebarRowColor::parse_hex("#12ABCG"), None);
        assert_eq!(SidebarRowColor::parse_hex("#1234"), None);
    }

    #[test]
    fn contrast_and_hex_are_deterministic() {
        assert_eq!(
            SidebarRowColor::new(0, 0, 0).contrast(),
            ratatui::style::Color::White
        );
        assert_eq!(
            SidebarRowColor::new(255, 255, 255).contrast(),
            ratatui::style::Color::Black
        );
        assert_eq!(SidebarRowColor::new(1, 2, 15).hex(), "#01020F");
    }
}
