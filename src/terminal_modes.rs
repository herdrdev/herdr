use std::io::{self, Write};

#[cfg(any(not(windows), test))]
const DISABLE_HOST_MOUSE_REPORTING_SEQUENCE: &[u8] =
    b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(any(windows, test))]
const WINDOWS_SSH_MOUSE_REPORTING_ENABLE_SEQUENCE: &[u8] =
    b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";
#[cfg(any(windows, test))]
const WINDOWS_SSH_MOUSE_REPORTING_DISABLE_SEQUENCE: &[u8] =
    b"\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(not(windows))]
pub(crate) fn clear_host_mouse_reporting<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE)?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn clear_host_mouse_reporting<W: Write>(_writer: &mut W) -> io::Result<()> {
    Ok(())
}

#[cfg(any(windows, test))]
pub(crate) fn set_windows_ssh_mouse_reporting<W: Write>(
    writer: &mut W,
    enabled: bool,
    sgr_pixels: bool,
) -> io::Result<()> {
    writer.write_all(if enabled {
        WINDOWS_SSH_MOUSE_REPORTING_ENABLE_SEQUENCE
    } else {
        WINDOWS_SSH_MOUSE_REPORTING_DISABLE_SEQUENCE
    })?;
    if enabled {
        writer.write_all(if sgr_pixels {
            b"\x1b[?1016h"
        } else {
            b"\x1b[?1016l"
        })?;
    }
    writer.flush()
}

#[cfg(not(windows))]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    writer: &mut W,
    report_all_keys: bool,
) -> io::Result<()> {
    let mut flags = crate::input::ime_compatible_keyboard_enhancement_flags();
    if report_all_keys {
        flags |= crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
        flags = crossterm::event::KeyboardEnhancementFlags::from_bits_retain(
            flags.bits() | 0b0001_0000,
        );
    }
    crossterm::execute!(
        writer,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::PushKeyboardEnhancementFlags(flags)
    )
}

#[cfg(windows)]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    _writer: &mut W,
    _report_all_keys: bool,
) -> io::Result<()> {
    Ok(())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DirectHostKeyboardState {
    kitty_flags: Option<u16>,
    modify_other_keys_level: u8,
}

#[cfg(not(windows))]
pub(crate) fn set_direct_host_keyboard_protocol<W: Write>(
    writer: &mut W,
    active: &mut DirectHostKeyboardState,
    next_flags: u16,
    next_modify_other_keys_level: u8,
) -> io::Result<()> {
    let next_kitty_flags = (next_flags != 0).then_some(next_flags);
    if active.kitty_flags == next_kitty_flags
        && active.modify_other_keys_level == next_modify_other_keys_level
    {
        return Ok(());
    }

    if active.kitty_flags != next_kitty_flags {
        if active.kitty_flags.is_some() {
            writer.write_all(b"\x1b[<1u")?;
        }
        if next_flags != 0 {
            write!(writer, "\x1b[>{next_flags}u")?;
        }
    }
    if active.modify_other_keys_level != next_modify_other_keys_level {
        write!(writer, "\x1b[>4;{next_modify_other_keys_level}m")?;
    }
    writer.flush()?;
    *active = DirectHostKeyboardState {
        kitty_flags: next_kitty_flags,
        modify_other_keys_level: next_modify_other_keys_level,
    };
    Ok(())
}

#[cfg(windows)]
pub(crate) fn set_direct_host_keyboard_protocol<W: Write>(
    _writer: &mut W,
    active: &mut DirectHostKeyboardState,
    next_flags: u16,
    next_modify_other_keys_level: u8,
) -> io::Result<()> {
    *active = DirectHostKeyboardState {
        kitty_flags: (next_flags != 0).then_some(next_flags),
        modify_other_keys_level: next_modify_other_keys_level,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_report_all_replaces_the_current_herdr_stack_entry() {
        let mut output = Vec::new();

        set_host_kitty_keyboard_report_all(&mut output, true).unwrap();
        set_host_kitty_keyboard_report_all(&mut output, false).unwrap();

        assert_eq!(output, b"\x1b[<1u\x1b[>31u\x1b[<1u\x1b[>7u");
    }

    #[cfg(not(windows))]
    #[test]
    fn direct_keyboard_protocol_owns_exactly_one_stack_entry_and_modify_other_keys() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 3, 0).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 15, 2).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert_eq!(
            output,
            b"\x1b[>3u\x1b[<1u\x1b[>15u\x1b[>4;2m\x1b[<1u\x1b[>4;0m"
        );
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[cfg(not(windows))]
    #[test]
    fn direct_modify_other_keys_works_without_kitty_flags() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 1).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 2).unwrap();
        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert_eq!(output, b"\x1b[>4;1m\x1b[>4;2m\x1b[>4;0m");
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[test]
    fn direct_legacy_keyboard_mode_does_not_pop_the_host_stack() {
        let mut output = Vec::new();
        let mut active = DirectHostKeyboardState::default();

        set_direct_host_keyboard_protocol(&mut output, &mut active, 0, 0).unwrap();

        assert!(output.is_empty());
        assert_eq!(active, DirectHostKeyboardState::default());
    }

    #[test]
    fn clears_all_known_host_mouse_modes() {
        let sequence = std::str::from_utf8(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE).unwrap();

        for mode in ["1000", "1002", "1003", "1005", "1006", "1015", "1016"] {
            assert!(
                sequence.contains(&format!("\x1b[?{mode}l")),
                "missing mouse mode {mode}"
            );
        }
    }

    #[test]
    fn windows_ssh_mouse_reporting_setup_and_teardown_request_required_modes() {
        let mut output = Vec::new();

        set_windows_ssh_mouse_reporting(&mut output, true, true).unwrap();
        set_windows_ssh_mouse_reporting(&mut output, true, false).unwrap();
        set_windows_ssh_mouse_reporting(&mut output, false, false).unwrap();

        assert_eq!(
            output,
            b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1016l\x1b[?1016l\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l"
        );
    }
}
