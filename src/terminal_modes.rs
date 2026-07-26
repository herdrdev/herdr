use std::io::{self, Write};

#[cfg(any(not(windows), test))]
const DISABLE_HOST_MOUSE_REPORTING_SEQUENCE: &[u8] =
    b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l";

#[cfg(not(windows))]
pub(crate) fn clear_host_mouse_reporting<W: Write>(writer: &mut W) -> io::Result<()> {
    writer.write_all(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE)?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn clear_host_mouse_reporting<W: Write>(_writer: &mut W) -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    writer: &mut W,
    report_all_keys: bool,
) -> io::Result<()> {
    let mut flags = crate::input::ime_compatible_keyboard_enhancement_flags();
    if report_all_keys {
        flags |= crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
    }
    write!(writer, "\x1b[={}u", flags.bits())?;
    writer.flush()
}

#[cfg(windows)]
pub(crate) fn set_host_kitty_keyboard_report_all<W: Write>(
    _writer: &mut W,
    _report_all_keys: bool,
) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_keyboard_report_all_only_changes_the_current_herdr_stack_entry() {
        let mut output = Vec::new();

        set_host_kitty_keyboard_report_all(&mut output, true).unwrap();
        set_host_kitty_keyboard_report_all(&mut output, false).unwrap();

        assert_eq!(output, b"\x1b[=15u\x1b[=7u");
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

    // Regression test for issue #1713: after leaving the alternate screen,
    // teardown clears host mouse reporting a *second* time. Terminals that
    // track DEC private modes per screen buffer restore the primary screen's
    // saved mouse-tracking state on LeaveAlternateScreen, so a single
    // pre-restore clear is not enough. This asserts that calling
    // `clear_host_mouse_reporting` twice (mirroring the pre-restore and
    // post-restore calls in the teardown paths) is idempotent and always
    // re-emits the full disable sequence — including SGR mode 1006, the
    // source of the stray `35;22;52M`-style reports in the bug report.
    #[test]
    fn second_clear_after_restore_reemits_full_disable_sequence() {
        let mut output = Vec::new();

        // Pre-restore clear (before ratatui::restore leaves the alt screen).
        clear_host_mouse_reporting(&mut output).unwrap();
        // Post-restore clear (the fix): must re-emit the disable sequence so
        // the primary screen's restored mouse-tracking state is cleared.
        clear_host_mouse_reporting(&mut output).unwrap();

        let expected: Vec<u8> = if cfg!(windows) {
            Vec::new()
        } else {
            let mut e = DISABLE_HOST_MOUSE_REPORTING_SEQUENCE.to_vec();
            e.extend_from_slice(DISABLE_HOST_MOUSE_REPORTING_SEQUENCE);
            e
        };
        assert_eq!(output, expected);

        // On non-Windows, the SGR mouse disable (1006) — the mode responsible
        // for the stray reports in issue #1713 — must appear twice after the
        // pre- and post-restore clears.
        if !cfg!(windows) {
            let text = std::str::from_utf8(&output).unwrap();
            assert_eq!(text.matches("\x1b[?1006l").count(), 2);
        }
    }
}
