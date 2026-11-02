use super::*;

/// How this client shows pane images on its host terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GraphicsMode {
    /// `[terminal] kitty_graphics = false`, or a client that does not render the
    /// shell: no image scene is requested.
    Off,
    /// The host terminal answered the Kitty graphics query; emit Kitty escapes.
    Kitty,
    /// The host terminal answered DA1 without the Kitty reply; paint each image
    /// as half-block cells in the text frame.
    Cells,
}

impl GraphicsMode {
    /// A host that was not asked, or never answered, keeps Kitty output, as
    /// before the probe existed; only a host known to lack it gets cells.
    pub(super) fn new(configured: bool, host_kitty_graphics: Option<bool>) -> Self {
        match (configured, host_kitty_graphics) {
            (false, _) => Self::Off,
            (true, Some(false)) => Self::Cells,
            (true, _) => Self::Kitty,
        }
    }

    /// Whether this client asks for image scenes and reports a cell size.
    pub(super) fn active(self) -> bool {
        self != Self::Off
    }

    /// Whether Kitty graphics escapes may reach the host terminal.
    pub(super) fn kitty(self) -> bool {
        self == Self::Kitty
    }
}

pub(super) struct ClientLoopConfig {
    pub(super) sound_config: crate::config::SoundConfig,
    pub(super) mouse_scroll_lines: usize,
    pub(super) redraw_on_focus_gained: bool,
    pub(super) host_cursor: crate::config::HostCursorModeConfig,
    pub(super) graphics: GraphicsMode,
    pub(super) pixel_geometry_enabled: bool,
    pub(super) pixel_geometry_fallback: bool,
    pub(super) mouse_capture_active: bool,
    pub(super) host_escape_disambiguation_active: bool,
    pub(super) host_sgr_pixel_mouse: Option<bool>,
    pub(super) initial_host_input: Vec<u8>,
    pub(super) endpoint_keybindings: bool,
    pub(super) remote_image_paste_key:
        Option<(crossterm::event::KeyCode, crossterm::event::KeyModifiers)>,
    pub(super) shell_config: Option<shell::ClientShellConfig>,
}

#[cfg(test)]
mod tests {
    use super::GraphicsMode;

    #[test]
    fn only_hosts_known_to_lack_kitty_graphics_get_cells() {
        assert_eq!(GraphicsMode::new(true, Some(true)), GraphicsMode::Kitty);
        assert_eq!(GraphicsMode::new(true, Some(false)), GraphicsMode::Cells);
        // Native Windows sends no probe; it keeps the Kitty output it had.
        assert_eq!(GraphicsMode::new(true, None), GraphicsMode::Kitty);
        assert_eq!(GraphicsMode::new(false, Some(true)), GraphicsMode::Off);
    }
}
