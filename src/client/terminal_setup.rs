//! Terminal setup and restoration for the rendered client.

use std::io::{self, Write as _};
#[cfg(not(windows))]
use std::os::fd::AsRawFd as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(windows)]
use std::sync::{Mutex, MutexGuard};
#[cfg(not(windows))]
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture,
};
#[cfg(not(windows))]
use crossterm::event::{PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
use crossterm::execute;
use crossterm::terminal::{DisableLineWrap, EnableLineWrap};

use super::frame_output::clear_received_kitty_graphics;

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// `bidi` says whether this client lays out pane rows in BiDi visual order
/// itself; a host that reorders text too is then switched to explicit mode.
///
/// Returns a guard that restores the terminal when dropped.
pub(super) fn setup_terminal(mouse_capture: bool, bidi: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture, bidi)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. When configured, mouse
/// capture lets wheel events drive the attached viewport or reach child
/// programs that requested mouse input.
pub(super) fn setup_direct_attach_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    // The attached pane's own bytes reach the host, so the host keeps its BiDi mode.
    setup_terminal_with_capabilities(false, mouse_capture, false)
}

pub(super) fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
    bidi: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();
    let mut terminal_guard = TerminalGuard {
        host_escape_disambiguation_active: false,
        host_sgr_pixel_mouse: None,
        buffered_host_input: Vec::new(),
        reset_keyboard_enhancements: false,
        reset_modify_other_keys: false,
        modify_other_keys_sequence: None,
        reset_host_color_scheme_reports: false,
        host_bidi: None,
        host_bidi_explicit: Arc::new(AtomicBool::new(false)),
        restore_claimed: Arc::new(AtomicBool::new(false)),
        restored: false,
        #[cfg(windows)]
        restore_windows_input_mode: Arc::new(WindowsInputModeRestore::default()),
    };
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    let host_color_scheme_reports =
        should_enable_host_color_scheme_reports(enable_client_protocols);
    #[cfg(windows)]
    let windows_ssh_session = is_ssh_session();
    #[cfg(windows)]
    let mut windows_virtual_terminal_input = if windows_ssh_session {
        enable_windows_virtual_terminal_input(
            &terminal_guard.restore_claimed,
            &terminal_guard.restore_windows_input_mode,
        )
    } else {
        WindowsVirtualTerminalInputSetup::default()
    };

    let (host_probe, buffered_host_input) = if enable_client_protocols {
        terminal_guard.reset_keyboard_enhancements = true;
        push_keyboard_enhancement_flags()?;
        let (probe, buffered_input) = query_host_capabilities(true);
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        if host_color_scheme_reports {
            terminal_guard.reset_host_color_scheme_reports = true;
            write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        }
        (probe, buffered_input)
    } else {
        if should_enable_host_color_scheme_reports(true) {
            write_host_color_scheme_report_mode(&mut io::stdout(), false)?;
        }
        let (probe, buffered_input) = query_host_capabilities(false);
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        (probe, buffered_input)
    };

    #[cfg(windows)]
    if enable_client_protocols && !windows_ssh_session {
        windows_virtual_terminal_input = enable_windows_virtual_terminal_input(
            &terminal_guard.restore_claimed,
            &terminal_guard.restore_windows_input_mode,
        );
    }

    #[cfg(windows)]
    if enable_client_protocols
        && windows_virtual_terminal_input.active
        && windows_win32_input_mode_enabled()
    {
        enable_windows_win32_input_mode(&mut io::stdout())?;
    }

    let modify_other_keys_mode = enable_client_protocols
        .then(crate::input::host_modify_other_keys_mode)
        .flatten();
    if let Some(mode) = modify_other_keys_mode {
        terminal_guard.reset_modify_other_keys = true;
        terminal_guard.modify_other_keys_sequence = Some(mode.set_sequence());
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    execute!(io::stdout(), DisableLineWrap)?;

    terminal_guard.host_escape_disambiguation_active = host_probe.escape_disambiguation;
    terminal_guard.host_sgr_pixel_mouse = host_probe.sgr_pixel_mouse;
    terminal_guard.buffered_host_input = buffered_host_input;
    if enable_client_protocols {
        terminal_guard.host_bidi = host_probe.host_bidi;
        terminal_guard.set_host_bidi_explicit(&mut io::stdout(), bidi)?;
    }
    Ok(terminal_guard)
}

pub(super) fn should_enable_host_color_scheme_reports(enable_client_protocols: bool) -> bool {
    enable_client_protocols && !cfg!(windows)
}

/// Guard that restores the terminal when dropped.
pub(super) struct TerminalGuard {
    host_escape_disambiguation_active: bool,
    host_sgr_pixel_mouse: Option<bool>,
    buffered_host_input: Vec<u8>,
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    /// The modifyOtherKeys sequence written at setup, re-sent on refresh.
    modify_other_keys_sequence: Option<&'static [u8]>,
    reset_host_color_scheme_reports: bool,
    /// The host's ECMA-48 BiDi mode (BDSM) reported at startup.
    host_bidi: Option<HostBidiMode>,
    /// Set while Herdr holds the host in explicit BiDi mode and owes `CSI 8 h`.
    host_bidi_explicit: Arc<AtomicBool>,
    restore_claimed: Arc<AtomicBool>,
    restored: bool,
    #[cfg(windows)]
    restore_windows_input_mode: Arc<WindowsInputModeRestore>,
}

#[cfg(not(windows))]
const HOST_CAPABILITY_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(not(windows))]
const MAX_BUFFERED_HOST_INPUT: usize = 64 * 1024;

#[cfg(not(windows))]
#[derive(Default)]
struct HostKeyboardProbeResponses {
    flags: Option<u16>,
    sgr_pixel_mouse_status: Option<u8>,
    bidi_status: Option<u8>,
    primary_device_attributes: bool,
}

/// What the startup probe learned about the host terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostTerminalProbe {
    pub(super) escape_disambiguation: bool,
    /// None when the host did not answer; exact pixel input stays disabled.
    pub(super) sgr_pixel_mouse: Option<bool>,
    /// The host's ECMA-48 BiDi mode (BDSM). None when the host does not
    /// recognize the mode or did not answer.
    pub(super) host_bidi: Option<HostBidiMode>,
}

/// The outer terminal's ECMA-48 BiDi mode (BDSM, ANSI mode 8) from DECRPM.
// Windows clients do not probe the host, so they never construct a mode.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostBidiMode {
    /// Status 1: the host reorders text itself and can be switched to
    /// explicit mode.
    Implicit,
    /// Status 3: the host always reorders text itself; `CSI 8 l` cannot stop
    /// it.
    PermanentlyImplicit,
    /// Status 2 or 4: the host draws text in the order it receives it.
    Explicit,
}

#[cfg(not(windows))]
fn query_host_capabilities(query_keyboard: bool) -> (HostTerminalProbe, Vec<u8>) {
    const FULL_QUERY: &[u8] = b"\x1b[?1016$p\x1b[?u\x1b[8$p\x1b[c";
    const PIXEL_MOUSE_QUERY: &[u8] = b"\x1b[?1016$p\x1b[8$p\x1b[c";

    let mut buffered_input = Vec::new();
    let query = if query_keyboard {
        FULL_QUERY
    } else {
        PIXEL_MOUSE_QUERY
    };
    if let Err(err) = io::stdout()
        .write_all(query)
        .and_then(|()| io::stdout().flush())
    {
        tracing::debug!(%err, "host capability query unavailable");
        return (HostTerminalProbe::default(), buffered_input);
    }

    // Bypass StdinLock's shared buffer so poll and read observe the same bytes.
    let stdin = io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let deadline = Instant::now() + HOST_CAPABILITY_QUERY_TIMEOUT;
    let mut responses = HostKeyboardProbeResponses::default();
    while !responses.primary_device_attributes && buffered_input.len() < MAX_BUFFERED_HOST_INPUT {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        match crate::platform::poll_fd_readable(stdin_fd, timeout_ms) {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::debug!(%err, "host keyboard enhancement query read unavailable");
                break;
            }
        }

        let mut scratch = [0u8; 4096];
        let capacity = MAX_BUFFERED_HOST_INPUT - buffered_input.len();
        let read_limit = capacity.min(scratch.len());
        match crate::platform::read_fd(stdin_fd, &mut scratch[..read_limit]) {
            Ok(0) => break,
            Ok(read) => {
                buffered_input.extend_from_slice(&scratch[..read]);
                consume_host_keyboard_probe_responses(&mut buffered_input, &mut responses);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::debug!(%err, "host keyboard enhancement query read failed");
                break;
            }
        }
    }

    (host_terminal_probe(&responses), buffered_input)
}

#[cfg(not(windows))]
fn host_terminal_probe(responses: &HostKeyboardProbeResponses) -> HostTerminalProbe {
    let sgr_pixel_mouse = match responses.sgr_pixel_mouse_status {
        // The query follows an explicit reset, so a host still reporting SET may
        // send pixels where Herdr expects cells. Wrong mouse units beat refusing
        // to start, so keep pixel mouse off and continue.
        Some(status @ (1 | 3)) => {
            tracing::warn!(status, "host SGR pixel mouse mode stays set after reset");
            Some(false)
        }
        Some(2) => Some(true),
        Some(0 | 4) => Some(false),
        _ => None,
    };
    let host_bidi = match responses.bidi_status {
        Some(1) => Some(HostBidiMode::Implicit),
        Some(3) => Some(HostBidiMode::PermanentlyImplicit),
        Some(2 | 4) => Some(HostBidiMode::Explicit),
        _ => None,
    };
    HostTerminalProbe {
        escape_disambiguation: host_escape_disambiguation_confirmed(responses),
        sgr_pixel_mouse,
        host_bidi,
    }
}

#[cfg(not(windows))]
fn host_escape_disambiguation_confirmed(responses: &HostKeyboardProbeResponses) -> bool {
    responses.primary_device_attributes
        && responses
            .flags
            .is_some_and(|flags| flags & 0b0000_0001 != 0)
}

#[cfg(not(windows))]
fn consume_host_keyboard_probe_responses(
    buffered_input: &mut Vec<u8>,
    responses: &mut HostKeyboardProbeResponses,
) {
    const PASTE_START: &[u8] = b"\x1b[200~";
    const PASTE_END: &[u8] = b"\x1b[201~";

    let mut offset = 0;
    while offset < buffered_input.len() {
        if buffered_input[offset..].starts_with(PASTE_START) {
            let payload_start = offset + PASTE_START.len();
            let Some(relative_end) = buffered_input[payload_start..]
                .windows(PASTE_END.len())
                .position(|bytes| bytes == PASTE_END)
            else {
                break;
            };
            offset = payload_start + relative_end + PASTE_END.len();
            continue;
        }
        if let Some(control_string_end) = host_control_string_end(&buffered_input[offset..]) {
            let Some(control_string_end) = control_string_end else {
                break;
            };
            offset += control_string_end;
            continue;
        }
        // DECRPM for an ANSI mode: CSI 8 ; Ps $ y.
        match host_bidi_mode_report(&buffered_input[offset..]) {
            Some(Some((status, len))) => {
                responses.bidi_status = Some(status);
                buffered_input.drain(offset..offset + len);
                continue;
            }
            Some(None) => break,
            None => {}
        }
        if !buffered_input[offset..].starts_with(b"\x1b[?") {
            offset += 1;
            continue;
        }

        let start = offset;
        let mut end = start + 3;
        while end < buffered_input.len()
            && (buffered_input[end].is_ascii_digit() || buffered_input[end] == b';')
        {
            end += 1;
        }
        if end == buffered_input.len() {
            break;
        }
        let body = &buffered_input[start + 3..end];
        // DECRPM: CSI ? 1016 ; Ps $ y.
        if buffered_input[end] == b'$' {
            let Some(&final_byte) = buffered_input.get(end + 1) else {
                break;
            };
            if final_byte == b'y' {
                if let Some(status) = parse_sgr_pixel_mouse_status(body) {
                    responses.sgr_pixel_mouse_status = Some(status);
                    buffered_input.drain(start..=end + 1);
                    continue;
                }
            }
            offset += 1;
            continue;
        }
        let recognized = match buffered_input[end] {
            b'u' if !body.is_empty() && body.iter().all(u8::is_ascii_digit) => {
                std::str::from_utf8(body)
                    .ok()
                    .and_then(|flags| flags.parse().ok())
                    .map(|flags| {
                        if !responses.primary_device_attributes {
                            responses.flags = Some(flags);
                        }
                    })
                    .is_some()
            }
            b'c' if !body.is_empty()
                && body
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || *byte == b';') =>
            {
                responses.primary_device_attributes = true;
                true
            }
            _ => false,
        };
        if recognized {
            buffered_input.drain(start..=end);
        } else {
            offset += 1;
        }
    }
}

#[cfg(not(windows))]
fn parse_sgr_pixel_mouse_status(body: &[u8]) -> Option<u8> {
    let mut fields = body.split(|byte| *byte == b';');
    (fields.next()? == b"1016").then_some(())?;
    let status = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    (fields.next().is_none() && status <= 4).then_some(status)
}

/// Matches a BDSM report at the start of `bytes`: Some(Some((status, len)))
/// for a whole report, Some(None) when the bytes so far could still become
/// one, and None otherwise.
#[cfg(not(windows))]
fn host_bidi_mode_report(bytes: &[u8]) -> Option<Option<(u8, usize)>> {
    const PREFIX: &[u8] = b"\x1b[8;";
    if bytes.len() < PREFIX.len() {
        return PREFIX.starts_with(bytes).then_some(None);
    }
    if !bytes.starts_with(PREFIX) {
        return None;
    }
    let rest = &bytes[PREFIX.len()..];
    let Some(&status) = rest.first() else {
        return Some(None);
    };
    if !(b'0'..=b'4').contains(&status) {
        return None;
    }
    match &rest[1..] {
        [] | [b'$'] => Some(None),
        [b'$', b'y', ..] => Some(Some((status - b'0', PREFIX.len() + 3))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn host_control_string_end(bytes: &[u8]) -> Option<Option<usize>> {
    if bytes.first() != Some(&0x1b) {
        return None;
    }

    let allow_bel = if bytes.starts_with(b"\x1b]") {
        true
    } else if bytes
        .get(1)
        .is_some_and(|byte| matches!(*byte, b'P' | b'_' | b'^' | b'X'))
    {
        false
    } else {
        return None;
    };

    for offset in 2..bytes.len() {
        if allow_bel && bytes[offset] == 0x07 {
            return Some(Some(offset + 1));
        }
        if bytes[offset..].starts_with(b"\x1b\\") {
            return Some(Some(offset + 2));
        }
    }
    Some(None)
}

#[cfg(windows)]
fn query_host_capabilities(_query_keyboard: bool) -> (HostTerminalProbe, Vec<u8>) {
    (HostTerminalProbe::default(), Vec::new())
}

pub(super) fn write_host_color_scheme_report_mode(
    writer: &mut impl io::Write,
    enabled: bool,
) -> io::Result<()> {
    let sequence = if enabled {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_ENABLE_SEQUENCE
    } else {
        crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE
    };
    writer.write_all(sequence.as_bytes())?;
    writer.flush()
}

pub(super) fn write_terminal_restore_postlude(
    writer: &mut impl io::Write,
    reset_host_color_scheme_reports: bool,
    restore_host_bidi_implicit: bool,
) -> io::Result<()> {
    if restore_host_bidi_implicit {
        writer.write_all(HOST_BIDI_IMPLICIT)?;
    }
    if reset_host_color_scheme_reports {
        writer.write_all(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        )?;
    }
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

/// ECMA-48 BDSM: the host reorders text itself.
const HOST_BIDI_IMPLICIT: &[u8] = b"\x1b[8h";
/// ECMA-48 BDSM: the application already laid out text in visual order.
const HOST_BIDI_EXPLICIT: &[u8] = b"\x1b[8l";

pub(super) fn should_draw_host_cursor(mode: crate::config::HostCursorModeConfig) -> bool {
    match mode {
        crate::config::HostCursorModeConfig::Auto => {
            crate::platform::should_draw_host_cursor_by_default()
        }
        crate::config::HostCursorModeConfig::Native => false,
        crate::config::HostCursorModeConfig::Drawn => true,
    }
}

#[cfg(windows)]
#[derive(Default)]
pub(super) struct WindowsVirtualTerminalInputSetup {
    active: bool,
    restore_mode: Option<u32>,
    warning: Option<&'static str>,
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsInputModeRestore {
    mode: Mutex<Option<u32>>,
}

#[cfg(windows)]
impl WindowsInputModeRestore {
    fn lock(&self) -> MutexGuard<'_, Option<u32>> {
        match self.mode.lock() {
            Ok(mode) => mode,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn activate(
        &self,
        restore_claimed: &AtomicBool,
        operation: impl FnOnce() -> WindowsVirtualTerminalInputSetup,
    ) -> WindowsVirtualTerminalInputSetup {
        let mut restore_mode = self.lock();
        if restore_claimed.load(Ordering::Acquire) {
            return WindowsVirtualTerminalInputSetup::default();
        }
        let setup = operation();
        if restore_mode.is_none() {
            *restore_mode = setup.restore_mode;
        }
        setup
    }

    fn take(&self) -> Option<u32> {
        self.lock().take()
    }
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input(
    restore_claimed: &AtomicBool,
    restore_mode: &WindowsInputModeRestore,
) -> WindowsVirtualTerminalInputSetup {
    let setup = restore_mode.activate(restore_claimed, enable_windows_virtual_terminal_input_inner);
    if let Some(warning) = setup.warning {
        tracing::warn!("{warning}");
    }
    setup
}

#[cfg(windows)]
fn enable_windows_virtual_terminal_input_inner() -> WindowsVirtualTerminalInputSetup {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to get Windows console input handle for VT input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to read Windows console input mode for VT input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let desired = windows_virtual_terminal_input_mode(mode);
    if desired == mode {
        return WindowsVirtualTerminalInputSetup {
            active: true,
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    if unsafe { SetConsoleMode(handle, desired) } == 0 {
        return WindowsVirtualTerminalInputSetup {
            warning: Some("failed to enable Windows virtual terminal input"),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    let mut applied = 0;
    if unsafe { GetConsoleMode(handle, &mut applied) } == 0 {
        let rollback_failed = unsafe { SetConsoleMode(handle, mode) } == 0;
        return WindowsVirtualTerminalInputSetup {
            restore_mode: rollback_failed.then_some(mode),
            warning: Some(if rollback_failed {
                "failed to verify or restore Windows virtual terminal input mode"
            } else {
                "failed to verify Windows virtual terminal input mode"
            }),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }
    if applied & ENABLE_VIRTUAL_TERMINAL_INPUT == 0 {
        let rollback_failed = unsafe { SetConsoleMode(handle, mode) } == 0;
        return WindowsVirtualTerminalInputSetup {
            restore_mode: rollback_failed.then_some(mode),
            warning: Some(if rollback_failed {
                "Windows virtual terminal input bit did not stick and the prior mode could not be restored"
            } else {
                "Windows virtual terminal input bit did not stick"
            }),
            ..WindowsVirtualTerminalInputSetup::default()
        };
    }

    WindowsVirtualTerminalInputSetup {
        active: true,
        restore_mode: Some(mode),
        warning: None,
    }
}

pub(super) fn is_ssh_session() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

#[cfg(any(windows, test))]
pub(super) fn windows_virtual_terminal_input_mode(mode: u32) -> u32 {
    mode | 0x0200
}

#[cfg(windows)]
fn restore_windows_input_mode_value(mode: u32) {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Console::{GetStdHandle, SetConsoleMode, STD_INPUT_HANDLE};

    let handle: HANDLE = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }
    if unsafe { SetConsoleMode(handle, mode) } == 0 {
        tracing::warn!("failed to restore Windows console input mode");
    }
}

pub(super) fn effective_mouse_capture(
    server_enabled: bool,
    direct_attach_preference: bool,
) -> bool {
    server_enabled || direct_attach_preference
}

pub(super) fn effective_sgr_pixel_mouse(
    enabled: bool,
    requested: bool,
    pixel_mouse_available: bool,
) -> bool {
    enabled && requested && pixel_mouse_available
}

#[cfg(any(windows, test))]
fn set_windows_native_mouse_capture<W: io::Write>(
    writer: &mut W,
    enabled: bool,
    sgr_pixels: bool,
    set_console_capture: impl FnOnce(bool) -> io::Result<()>,
) -> io::Result<()> {
    if !enabled {
        crate::terminal_modes::clear_host_mouse_reporting(writer)?;
    }
    set_console_capture(enabled)?;
    if enabled {
        crate::terminal_modes::set_windows_mouse_reporting(writer, true, sgr_pixels)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_uses_vt_mouse_reporting() -> bool {
    is_ssh_session() || crate::platform::windows_virtual_terminal_input_active()
}

pub(super) fn set_mouse_capture(enabled: bool, sgr_pixels: bool) -> io::Result<()> {
    #[cfg(windows)]
    if windows_uses_vt_mouse_reporting() {
        if !enabled {
            crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
        }
        return crate::terminal_modes::set_windows_mouse_reporting(
            &mut io::stdout(),
            enabled,
            sgr_pixels,
        );
    }
    #[cfg(windows)]
    return set_windows_native_mouse_capture(&mut io::stdout(), enabled, sgr_pixels, |enabled| {
        if enabled {
            execute!(io::stdout(), EnableMouseCapture)
        } else {
            disable_windows_native_mouse_capture()
        }
    });
    #[cfg(not(windows))]
    crate::terminal_modes::clear_host_mouse_reporting(&mut io::stdout())?;
    #[cfg(not(windows))]
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)?;
        if sgr_pixels {
            io::stdout().write_all(b"\x1b[?1016h")?;
            io::stdout().flush()?;
        }
        Ok(())
    } else {
        match execute!(io::stdout(), DisableMouseCapture) {
            Ok(()) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

#[cfg(windows)]
fn disable_windows_native_mouse_capture() -> io::Result<()> {
    match execute!(io::stdout(), DisableMouseCapture) {
        Ok(()) => Ok(()),
        Err(err) if err.to_string() == "Initial console modes not set" => Ok(()),
        Err(err) => Err(err),
    }
}

fn restore_terminal_state_once(
    restore_claimed: &AtomicBool,
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    host_bidi_explicit: &AtomicBool,
    #[cfg(windows)] restore_windows_input_mode: &WindowsInputModeRestore,
) -> io::Result<()> {
    if restore_claimed.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    #[cfg(windows)]
    let restore_windows_input_mode = restore_windows_input_mode.take();
    restore_terminal_state(
        reset_keyboard_enhancements,
        reset_modify_other_keys,
        reset_host_color_scheme_reports,
        host_bidi_explicit.swap(false, Ordering::AcqRel),
        #[cfg(windows)]
        restore_windows_input_mode,
    )
}

fn restore_terminal_state(
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    restore_host_bidi_implicit: bool,
    #[cfg(windows)] restore_windows_input_mode: Option<u32>,
) -> io::Result<()> {
    let _ = clear_received_kitty_graphics(&mut io::stdout());

    // Reset modifyOtherKeys if we enabled it.
    if reset_modify_other_keys {
        let _ = io::stdout().write_all(b"\x1b[>4;0m");
        let _ = io::stdout().flush();
    }

    if reset_keyboard_enhancements {
        let _ = pop_keyboard_enhancement_flags();
    }

    let _ = execute!(
        io::stdout(),
        EnableLineWrap,
        DisableFocusChange,
        DisableBracketedPaste
    );
    let _ = set_mouse_capture(false, false);
    #[cfg(windows)]
    if let Some(mode) = restore_windows_input_mode {
        restore_windows_input_mode_value(mode);
    }
    #[cfg(windows)]
    if !is_ssh_session() {
        let _ = disable_windows_native_mouse_capture();
    }

    let restore_result = ratatui::try_restore();
    let postlude_result = write_terminal_restore_postlude(
        &mut io::stdout(),
        reset_host_color_scheme_reports,
        restore_host_bidi_implicit,
    );

    #[cfg(windows)]
    if windows_win32_input_mode_enabled() {
        let _ = disable_windows_win32_input_mode(&mut io::stdout());
    }

    restore_result.and(postlude_result)
}

#[cfg(not(windows))]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(crate::input::ime_compatible_keyboard_enhancement_flags())
    )
}

#[cfg(windows)]
fn push_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    execute!(io::stdout(), PopKeyboardEnhancementFlags)
}

#[cfg(windows)]
fn pop_keyboard_enhancement_flags() -> io::Result<()> {
    Ok(())
}

#[cfg(any(windows, test))]
pub(super) fn windows_win32_input_mode_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_PROBE")
        .map(|probe| probe.eq_ignore_ascii_case("win32"))
        .unwrap_or(true)
}

#[cfg(windows)]
fn enable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001h")?;
    writer.flush()
}

#[cfg(windows)]
fn disable_windows_win32_input_mode(writer: &mut impl std::io::Write) -> io::Result<()> {
    writer.write_all(b"\x1b[?9001l")?;
    writer.flush()
}

/// Host input modes to re-assert after the host may have reset them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct HostKeyboardProtocolRefresh {
    /// Some when the host confirmed the kitty keyboard protocol at startup;
    /// the value says whether all keys are currently reported as escapes.
    pub(super) kitty_report_all_keys: Option<bool>,
    pub(super) modify_other_keys_sequence: Option<&'static [u8]>,
    /// Re-enable focus reporting (1004). Callers reacting to a focus-in skip
    /// it: that host still reports focus, and re-enabling from a focus report
    /// could loop on a host that answers the enable with another report.
    pub(super) focus_reporting: bool,
}

pub(super) fn write_host_keyboard_protocol_refresh(
    writer: &mut impl io::Write,
    refresh: HostKeyboardProtocolRefresh,
) -> io::Result<()> {
    if let Some(report_all_keys) = refresh.kitty_report_all_keys {
        // Replace Herdr's stack entry rather than pushing, so repeated
        // refreshes never grow the host's keyboard stack. On a host that
        // reset its stack, the pop is a no-op and the push restores the flags.
        crate::terminal_modes::set_host_kitty_keyboard_report_all(writer, report_all_keys)?;
    }
    if let Some(sequence) = refresh.modify_other_keys_sequence {
        writer.write_all(sequence)?;
    }
    writer.write_all(b"\x1b[?2004h")?;
    if refresh.focus_reporting {
        writer.write_all(b"\x1b[?1004h")?;
    }
    writer.flush()
}

impl TerminalGuard {
    pub(super) fn host_escape_disambiguation_active(&self) -> bool {
        self.host_escape_disambiguation_active
    }

    /// Whether the host answered that it supports SGR pixel mouse reports.
    pub(super) fn host_sgr_pixel_mouse(&self) -> Option<bool> {
        self.host_sgr_pixel_mouse
    }

    pub(super) fn take_buffered_host_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffered_host_input)
    }

    /// Whether the host reorders text itself and cannot be switched to
    /// explicit mode. The client then leaves pane rows in logical order,
    /// because laying them out too would reorder text twice.
    pub(super) fn host_reorders_text_permanently(&self) -> bool {
        self.host_bidi == Some(HostBidiMode::PermanentlyImplicit)
    }

    /// Holds a host that reorders text itself in explicit BiDi mode while this
    /// client reorders pane rows, so text is not reordered twice. Herdr only
    /// ever returns the host to implicit mode after switching it away, and
    /// never writes to a host whose mode is permanent.
    /// Returns whether the host mode changed.
    pub(super) fn set_host_bidi_explicit(
        &self,
        writer: &mut impl io::Write,
        explicit: bool,
    ) -> io::Result<bool> {
        if self.host_bidi != Some(HostBidiMode::Implicit)
            || self.restore_claimed.load(Ordering::Acquire)
        {
            return Ok(false);
        }
        if self.host_bidi_explicit.load(Ordering::Acquire) == explicit {
            return Ok(false);
        }
        writer.write_all(if explicit {
            HOST_BIDI_EXPLICIT
        } else {
            HOST_BIDI_IMPLICIT
        })?;
        writer.flush()?;
        // Record the mode only once the host has it, so a failed switch is
        // retried and restore does not undo a switch that never happened.
        self.host_bidi_explicit.store(explicit, Ordering::Release);
        Ok(true)
    }

    /// Re-sends the keyboard and input modes enabled at setup. A host that
    /// reset its terminal state (a reconnect or window reload) forgets them,
    /// which turns Shift+Enter and Ctrl+Enter into plain Enter.
    pub(super) fn refresh_host_keyboard_protocol(
        &self,
        writer: &mut impl io::Write,
        report_all_keys: bool,
        focus_reporting: bool,
    ) -> io::Result<()> {
        // Direct attach leaves these modes to the attached pane.
        if !self.reset_keyboard_enhancements || self.restore_claimed.load(Ordering::Acquire) {
            return Ok(());
        }
        // A host reset also returns BDSM to its implicit default.
        if self.host_bidi_explicit.load(Ordering::Acquire) {
            writer.write_all(HOST_BIDI_EXPLICIT)?;
        }
        write_host_keyboard_protocol_refresh(
            writer,
            HostKeyboardProtocolRefresh {
                kitty_report_all_keys: self
                    .host_escape_disambiguation_active
                    .then_some(report_all_keys),
                modify_other_keys_sequence: self.modify_other_keys_sequence,
                focus_reporting,
            },
        )
    }

    #[cfg(windows)]
    pub(super) fn recover_windows_virtual_terminal_input(&self) -> io::Result<()> {
        let active = enable_windows_virtual_terminal_input(
            &self.restore_claimed,
            &self.restore_windows_input_mode,
        )
        .active;
        if active && windows_win32_input_mode_enabled() {
            enable_windows_win32_input_mode(&mut io::stdout())?;
        }
        Ok(())
    }

    /// Captures the restoration state for use by the process panic hook.
    pub(super) fn panic_restore(&self) -> impl Fn() + Send + Sync + 'static {
        let restore_claimed = self.restore_claimed.clone();
        let reset_keyboard_enhancements = self.reset_keyboard_enhancements;
        let reset_modify_other_keys = self.reset_modify_other_keys;
        let reset_host_color_scheme_reports = self.reset_host_color_scheme_reports;
        let host_bidi_explicit = self.host_bidi_explicit.clone();
        #[cfg(windows)]
        let restore_windows_input_mode = self.restore_windows_input_mode.clone();
        move || {
            let _ = restore_terminal_state_once(
                &restore_claimed,
                reset_keyboard_enhancements,
                reset_modify_other_keys,
                reset_host_color_scheme_reports,
                &host_bidi_explicit,
                #[cfg(windows)]
                &restore_windows_input_mode,
            );
        }
    }

    pub(super) fn restore(mut self) -> io::Result<()> {
        self.restored = true;
        restore_terminal_state_once(
            &self.restore_claimed,
            self.reset_keyboard_enhancements,
            self.reset_modify_other_keys,
            self.reset_host_color_scheme_reports,
            &self.host_bidi_explicit,
            #[cfg(windows)]
            &self.restore_windows_input_mode,
        )
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if !self.restored {
            let _ = restore_terminal_state_once(
                &self.restore_claimed,
                self.reset_keyboard_enhancements,
                self.reset_modify_other_keys,
                self.reset_host_color_scheme_reports,
                &self.host_bidi_explicit,
                #[cfg(windows)]
                &self.restore_windows_input_mode,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_input_mode_restore_tracks_recovery_after_cleanup_callback_creation() {
        let restore_claimed = Arc::new(AtomicBool::new(false));
        let restore_mode = Arc::new(WindowsInputModeRestore::default());
        let callback_claimed = restore_claimed.clone();
        let callback_mode = restore_mode.clone();
        let cleanup = move || {
            if callback_claimed.swap(true, Ordering::AcqRel) {
                None
            } else {
                callback_mode.take()
            }
        };

        let failed =
            restore_mode.activate(&restore_claimed, WindowsVirtualTerminalInputSetup::default);
        assert!(!failed.active);
        let already_active =
            restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
                active: true,
                ..WindowsVirtualTerminalInputSetup::default()
            });
        assert!(already_active.active);
        restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: Some(152),
            warning: None,
        });
        restore_mode.activate(&restore_claimed, || WindowsVirtualTerminalInputSetup {
            active: true,
            restore_mode: Some(999),
            warning: None,
        });

        assert_eq!(cleanup(), Some(152));
        assert_eq!(cleanup(), None);
        let called = AtomicBool::new(false);
        restore_mode.activate(&restore_claimed, || {
            called.store(true, Ordering::Release);
            WindowsVirtualTerminalInputSetup::default()
        });
        assert!(!called.load(Ordering::Acquire));
    }

    #[derive(Clone, Default)]
    struct SharedOutput(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    impl io::Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    fn refresh_test_guard(
        kitty_confirmed: bool,
        client_protocols: bool,
        modify_other_keys_sequence: Option<&'static [u8]>,
    ) -> TerminalGuard {
        TerminalGuard {
            host_escape_disambiguation_active: kitty_confirmed,
            host_sgr_pixel_mouse: None,
            buffered_host_input: Vec::new(),
            reset_keyboard_enhancements: client_protocols,
            reset_modify_other_keys: modify_other_keys_sequence.is_some(),
            modify_other_keys_sequence,
            reset_host_color_scheme_reports: false,
            host_bidi: None,
            host_bidi_explicit: Arc::new(AtomicBool::new(false)),
            restore_claimed: Arc::new(AtomicBool::new(false)),
            // Keep Drop from writing restore sequences to the test's stdout.
            restored: true,
        }
    }

    #[cfg(not(windows))]
    fn refresh_bytes(guard: &TerminalGuard, report_all_keys: bool, focus: bool) -> Vec<u8> {
        let mut output = Vec::new();
        guard
            .refresh_host_keyboard_protocol(&mut output, report_all_keys, focus)
            .unwrap();
        output
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_protocol_refresh_resends_the_setup_modes() {
        // Focus-in on a kitty host: replace Herdr's stack entry, keep paste on.
        let kitty = refresh_test_guard(true, true, None);
        assert_eq!(
            refresh_bytes(&kitty, false, false),
            b"\x1b[<1u\x1b[>7u\x1b[?2004h"
        );
        // Resize while a pane wants all keys, on a modifyOtherKeys host too.
        let both = refresh_test_guard(true, true, Some(b"\x1b[>4;1m"));
        assert_eq!(
            refresh_bytes(&both, true, true),
            b"\x1b[<1u\x1b[>31u\x1b[>4;1m\x1b[?2004h\x1b[?1004h"
        );
        // A host that never confirmed the kitty protocol gets no kitty flags.
        let legacy = refresh_test_guard(false, true, None);
        assert_eq!(
            refresh_bytes(&legacy, false, true),
            b"\x1b[?2004h\x1b[?1004h"
        );
        // Direct attach leaves keyboard modes to the attached pane.
        let direct = refresh_test_guard(true, false, None);
        assert!(refresh_bytes(&direct, false, true).is_empty());
        // Nothing is re-enabled once the terminal is being restored.
        kitty.restore_claimed.store(true, Ordering::Release);
        assert!(refresh_bytes(&kitty, false, true).is_empty());
    }

    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn host_keyboard_protocol_refresh_restores_shift_enter_after_host_reset() {
        use crate::input::{KeyboardProtocol, TerminalKey};
        use crossterm::event::{KeyCode, KeyModifiers};

        let guard = refresh_test_guard(true, true, None);
        let shift_enter = TerminalKey::new(KeyCode::Enter, KeyModifiers::SHIFT);
        // libghostty stands in for the host terminal. `reset` models a host
        // that lost its modes, like a reloaded VS Code window (#4985).
        for reset in [false, true] {
            let (host, _rx) =
                crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                    80, 24, 0, b"", 4096,
                );
            // Herdr's setup push.
            host.test_process_pty_bytes(b"\x1b[>7u");
            if reset {
                host.test_process_pty_bytes(b"\x1bc");
                assert_eq!(host.keyboard_protocol(), KeyboardProtocol::Legacy);
                assert_eq!(host.encode_terminal_key(shift_enter.clone()), b"\r");
            }
            for _ in 0..3 {
                host.test_process_pty_bytes(&refresh_bytes(&guard, false, true));
            }
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Kitty { flags: 7 },
                "reset={reset}"
            );
            assert_eq!(
                host.encode_terminal_key(shift_enter.clone()),
                b"\x1b[13;2u",
                "reset={reset}"
            );
            // Refreshes never grow the stack: Herdr's one exit pop restores
            // the host's own keyboard mode.
            host.test_process_pty_bytes(b"\x1b[<1u");
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Legacy,
                "reset={reset}"
            );
        }
    }

    #[cfg(not(windows))]
    #[tokio::test(flavor = "multi_thread")]
    async fn host_keyboard_protocol_refresh_keeps_the_outer_mode_across_report_all_toggles() {
        use crate::input::KeyboardProtocol;

        let guard = refresh_test_guard(true, true, None);
        let (host, _rx) = crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
            80, 24, 0, b"", 4096,
        );
        // Something outside Herdr (a shell or wrapper) already set its own mode.
        host.test_process_pty_bytes(b"\x1b[>1u");
        // Herdr's setup push.
        host.test_process_pty_bytes(b"\x1b[>7u");
        for round in 0..6 {
            let report_all = round % 2 == 0;
            let mut toggle = Vec::new();
            crate::terminal_modes::set_host_kitty_keyboard_report_all(&mut toggle, report_all)
                .unwrap();
            host.test_process_pty_bytes(&toggle);
            for focus in [false, true, false] {
                host.test_process_pty_bytes(&refresh_bytes(&guard, report_all, focus));
            }
            let flags = if report_all { 31 } else { 7 };
            assert_eq!(
                host.keyboard_protocol(),
                KeyboardProtocol::Kitty { flags },
                "round {round}"
            );
        }
        // Herdr's one exit pop restores exactly the outer mode.
        host.test_process_pty_bytes(b"\x1b[<1u");
        assert_eq!(
            host.keyboard_protocol(),
            KeyboardProtocol::Kitty { flags: 1 }
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_consumes_fragmented_responses_and_preserves_input() {
        let stream = b"before\x1b[?1016;2$y-middle-\x1b[?7u-\x1b[?1;2cafter";

        for split in 1..stream.len() {
            let mut buffered = Vec::new();
            let mut responses = HostKeyboardProbeResponses::default();
            buffered.extend_from_slice(&stream[..split]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
            buffered.extend_from_slice(&stream[split..]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(responses.sgr_pixel_mouse_status, Some(2), "split {split}");
            assert_eq!(responses.flags, Some(7), "split {split}");
            assert!(responses.primary_device_attributes, "split {split}");
            assert_eq!(buffered, b"before-middle--after", "split {split}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_accepts_only_usable_sgr_pixel_mouse_states() {
        for status in 0..=4 {
            let mut buffered = format!("\x1b[?1016;{status}$y").into_bytes();
            let mut responses = HostKeyboardProbeResponses::default();

            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(
                host_terminal_probe(&responses).sgr_pixel_mouse,
                Some(status == 2),
                "status {status}"
            );
            assert!(buffered.is_empty());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_probe_keeps_a_pixel_mouse_answer_without_device_attributes() {
        let mut buffered = b"\x1b[?1016;0$y".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();
        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(!responses.primary_device_attributes);
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(false));
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_typed_input_before_responses() {
        let mut buffered = b"aPtyped\x1b[?1016;2$y\x1b[?7u\x1b[?1;2c".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(true));
        assert_eq!(buffered, b"aPtyped");
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_requires_disambiguation_bit_and_device_attributes() {
        for (flags, expected) in [(0, false), (2, false), (7, true)] {
            let mut buffered = format!("\x1b[?{flags}u\x1b[?1;2c").into_bytes();
            let mut responses = HostKeyboardProbeResponses::default();

            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(host_escape_disambiguation_confirmed(&responses), expected);
            assert!(buffered.is_empty());
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_requires_flags_before_device_attributes() {
        let mut buffered = b"\x1b[?1;2c\x1b[?7uinput".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(responses.flags, None);
        assert!(responses.primary_device_attributes);
        assert_eq!(buffered, b"input");
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_response_shaped_payloads() {
        let opaque =
            b"\x1b[200~paste \x1b[?1016;2$y \x1b[?1u \x1b[?1;2c\x1b[201~-\x1bPdata \x1b[?7u\x1b\\";
        let mut buffered = [opaque.as_slice(), b"\x1b[?1016;2$y\x1b[?7u\x1b[?1;2c"].concat();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
        assert_eq!(host_terminal_probe(&responses).sgr_pixel_mouse, Some(true));
        assert_eq!(buffered, opaque);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_malformed_responses() {
        let mut buffered = b"a\x1b[?7;1ub\x1b[?65536uc\x1b[?1016;5$y\x1b[?1006;2$y".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(responses.flags, None);
        assert_eq!(responses.sgr_pixel_mouse_status, None);
        assert!(!responses.primary_device_attributes);
        assert_eq!(
            buffered,
            b"a\x1b[?7;1ub\x1b[?65536uc\x1b[?1016;5$y\x1b[?1006;2$y"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_consumes_fragmented_bidi_mode_reports() {
        let stream = b"be\x1b[?1016;2$yfore\x1b[8;1$y-mid\x1b[?7u-\x1b[?1;2cafter";

        for split in 1..stream.len() {
            let mut buffered = Vec::new();
            let mut responses = HostKeyboardProbeResponses::default();
            buffered.extend_from_slice(&stream[..split]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
            buffered.extend_from_slice(&stream[split..]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(responses.bidi_status, Some(1), "split {split}");
            assert_eq!(responses.sgr_pixel_mouse_status, Some(2), "split {split}");
            assert_eq!(responses.flags, Some(7), "split {split}");
            assert!(responses.primary_device_attributes, "split {split}");
            assert_eq!(buffered, b"before-mid-after", "split {split}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_capability_probe_maps_bidi_mode_states() {
        for (status, expected) in [
            (0, None),
            (1, Some(HostBidiMode::Implicit)),
            (2, Some(HostBidiMode::Explicit)),
            (3, Some(HostBidiMode::PermanentlyImplicit)),
            (4, Some(HostBidiMode::Explicit)),
        ] {
            let mut buffered = format!("a\x1b[?1016;2$y\x1b[8;{status}$yb\x1b[?1;2c").into_bytes();
            let mut responses = HostKeyboardProbeResponses::default();

            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            let probe = host_terminal_probe(&responses);
            assert_eq!(probe.host_bidi, expected, "status {status}");
            assert_eq!(probe.sgr_pixel_mouse, Some(true), "status {status}");
            assert_eq!(buffered, b"ab", "status {status}");
        }
        // No reply at all leaves the host mode unknown.
        let mut buffered = b"\x1b[?1;2c".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();
        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
        assert_eq!(host_terminal_probe(&responses).host_bidi, None);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_bidi_probe_preserves_typed_input_and_malformed_reports() {
        // Typed bytes, a pasted report, and malformed or other-mode reports stay input.
        let kept = b"8;1$y\x1b[8;5$y\x1b[8;1$x\x1b[18;1$y\x1b[8;1y\x1b[200~\x1b[8;1$y\x1b[201~";
        let mut buffered = [kept.as_slice(), b"\x1b[8;2$y\x1b[?1;2c"].concat();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(
            host_terminal_probe(&responses).host_bidi,
            Some(HostBidiMode::Explicit)
        );
        assert_eq!(buffered, kept);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_bidi_explicit_mode_follows_the_bidi_setting_and_survives_refresh() {
        let mut guard = refresh_test_guard(true, true, None);
        guard.host_bidi = Some(HostBidiMode::Implicit);
        let mut output = Vec::new();
        // The result says whether the host mode changed, so a live reload
        // knows to repaint rows drawn under the previous mode.
        assert!(guard.set_host_bidi_explicit(&mut output, true).unwrap());
        assert!(!guard.set_host_bidi_explicit(&mut output, true).unwrap());
        assert_eq!(output, b"\x1b[8l");
        assert_eq!(
            refresh_bytes(&guard, false, false),
            b"\x1b[8l\x1b[<1u\x1b[>7u\x1b[?2004h"
        );
        // A live reload that turns BiDi layout off hands reordering back.
        output.clear();
        guard.set_host_bidi_explicit(&mut output, false).unwrap();
        guard.set_host_bidi_explicit(&mut output, false).unwrap();
        assert_eq!(output, b"\x1b[8h");
        output.clear();
        guard.set_host_bidi_explicit(&mut output, true).unwrap();
        assert_eq!(output, b"\x1b[8l");
        assert!(guard.host_bidi_explicit.load(Ordering::Acquire));

        // Hosts that do not reorder text, did not answer, or reorder text
        // permanently are never switched.
        for host_bidi in [
            None,
            Some(HostBidiMode::Explicit),
            Some(HostBidiMode::PermanentlyImplicit),
        ] {
            let mut guard = refresh_test_guard(true, true, None);
            guard.host_bidi = host_bidi;
            let mut output = Vec::new();
            assert!(!guard.set_host_bidi_explicit(&mut output, true).unwrap());
            assert!(!guard.set_host_bidi_explicit(&mut output, false).unwrap());
            assert!(output.is_empty(), "host_bidi {host_bidi:?}");
            assert!(!guard.host_bidi_explicit.load(Ordering::Acquire));
            assert_eq!(
                refresh_bytes(&guard, false, false),
                b"\x1b[<1u\x1b[>7u\x1b[?2004h",
                "host_bidi {host_bidi:?}"
            );
            assert_eq!(
                guard.host_reorders_text_permanently(),
                host_bidi == Some(HostBidiMode::PermanentlyImplicit)
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn permanently_implicit_host_bidi_is_left_alone_and_restore_writes_nothing() {
        let mut buffered = b"\x1b[8;3$y\x1b[?1;2c".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();
        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
        let mut guard = refresh_test_guard(true, true, None);
        guard.host_bidi = host_terminal_probe(&responses).host_bidi;
        assert!(guard.host_reorders_text_permanently());

        let mut output = Vec::new();
        assert!(!guard.set_host_bidi_explicit(&mut output, true).unwrap());
        assert!(output.is_empty(), "CSI 8 l cannot switch a permanent mode");
        // Nothing was switched, so restore owes the host no CSI 8 h.
        assert!(!guard.host_bidi_explicit.load(Ordering::Acquire));
    }

    #[cfg(not(windows))]
    #[test]
    fn host_bidi_explicit_mode_is_recorded_only_after_the_host_write_succeeds() {
        struct FailingWriter {
            fail_flush: bool,
        }
        impl io::Write for FailingWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.fail_flush {
                    Ok(buf.len())
                } else {
                    Err(io::Error::other("write failed"))
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("flush failed"))
            }
        }

        let mut guard = refresh_test_guard(true, true, None);
        guard.host_bidi = Some(HostBidiMode::Implicit);
        for fail_flush in [false, true] {
            assert!(guard
                .set_host_bidi_explicit(&mut FailingWriter { fail_flush }, true)
                .is_err());
            assert!(!guard.host_bidi_explicit.load(Ordering::Acquire));
        }
        // The failed switch is retried rather than treated as already applied,
        // and restore does not emit implicit mode for a switch that never landed.
        let mut output = Vec::new();
        assert!(guard.set_host_bidi_explicit(&mut output, true).unwrap());
        assert_eq!(output, b"\x1b[8l");
        assert!(guard.host_bidi_explicit.load(Ordering::Acquire));
    }

    #[test]
    fn terminal_restore_postlude_returns_host_bidi_only_when_switched() {
        let mut output = Vec::new();
        write_terminal_restore_postlude(&mut output, false, true).unwrap();
        assert_eq!(output, b"\x1b[8h\x1b[?25h\x1b[0 q");

        output.clear();
        write_terminal_restore_postlude(&mut output, false, false).unwrap();
        assert!(!output.windows(4).any(|bytes| bytes == b"\x1b[8h"));
    }

    #[test]
    fn windows_native_mouse_capture_reasserts_final_encoding_after_native_enable() {
        let mut output = SharedOutput::default();
        for sgr_pixels in [false, false, true, false] {
            let start = output.0.borrow().len();
            let native_output = output.clone();
            let native_boundary = std::cell::Cell::new(0);
            set_windows_native_mouse_capture(&mut output, true, sgr_pixels, |enabled| {
                assert!(enabled);
                // ConPTY enables host SGR during native capture, before our VT requests.
                native_output
                    .0
                    .borrow_mut()
                    .extend_from_slice(b"\x1b[?1003h\x1b[?1006h");
                native_boundary.set(native_output.0.borrow().len());
                Ok(())
            })
            .unwrap();

            let bytes = output.0.borrow();
            let before = std::str::from_utf8(&bytes[start..native_boundary.get()]).unwrap();
            let after = std::str::from_utf8(&bytes[native_boundary.get()..]).unwrap();
            assert!(!before.contains("\x1b[?1016l"));
            for reset in ["\x1b[?1005l", "\x1b[?1006l"] {
                assert!(
                    !after.contains(reset),
                    "mouse format reset after native capture (sgr_pixels={sgr_pixels}): {after:?}"
                );
            }
            assert!(after.contains("\x1b[?1003h\x1b[?1006h"));
            assert_eq!(after.contains("\x1b[?1016h"), sgr_pixels);
            if !sgr_pixels {
                assert!(after.starts_with("\x1b[?1016l"));
            }
        }
    }

    #[test]
    fn windows_native_mouse_capture_disable_clears_reporting() {
        let mut output = Vec::new();

        set_windows_native_mouse_capture(&mut output, false, false, |enabled| {
            assert!(!enabled);
            Ok(())
        })
        .unwrap();

        assert_eq!(
            output,
            b"\x1b[?1006l\x1b[?1016l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?9l"
        );
    }
}
