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
use super::terminal_geometry::should_query_host_terminal_theme;

// ---------------------------------------------------------------------------
// Terminal setup / restore
// ---------------------------------------------------------------------------

/// Sets up the terminal for client mode (raw mode, optional mouse, keyboard enhancements).
///
/// Returns a guard that restores the terminal when dropped.
pub(super) fn setup_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(true, mouse_capture)
}

/// Sets up a direct attach terminal.
///
/// Direct attach forwards stdin to the attached PTY. When configured, mouse
/// capture lets wheel events drive the attached viewport or reach child
/// programs that requested mouse input.
pub(super) fn setup_direct_attach_terminal(mouse_capture: bool) -> io::Result<TerminalGuard> {
    setup_terminal_with_capabilities(false, mouse_capture)
}

pub(super) fn setup_terminal_with_capabilities(
    enable_client_protocols: bool,
    mouse_capture: bool,
) -> io::Result<TerminalGuard> {
    ratatui::init();
    let mut terminal_guard = TerminalGuard {
        host_escape_disambiguation_active: false,
        buffered_host_input: Vec::new(),
        reset_keyboard_enhancements: false,
        reset_modify_other_keys: false,
        reset_host_color_scheme_reports: false,
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
    let mut windows_virtual_terminal_input =
        if windows_vti_input_backend_enabled() && windows_ssh_session {
            enable_windows_virtual_terminal_input(
                &terminal_guard.restore_claimed,
                &terminal_guard.restore_windows_input_mode,
            )
        } else {
            WindowsVirtualTerminalInputSetup::default()
        };

    let (host_escape_disambiguation_active, buffered_host_input) = if enable_client_protocols {
        terminal_guard.reset_keyboard_enhancements = true;
        push_keyboard_enhancement_flags()?;
        let (active, buffered_input) = query_host_escape_disambiguation();
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste, EnableFocusChange)?;
        if host_color_scheme_reports {
            terminal_guard.reset_host_color_scheme_reports = true;
            write_host_color_scheme_report_mode(&mut io::stdout(), true)?;
        }
        (active, buffered_input)
    } else {
        if should_query_host_terminal_theme() {
            write_host_color_scheme_report_mode(&mut io::stdout(), false)?;
        }
        set_mouse_capture(mouse_capture, false)?;
        execute!(io::stdout(), EnableBracketedPaste)?;
        (false, Vec::new())
    };

    #[cfg(windows)]
    if enable_client_protocols && windows_vti_input_backend_enabled() && !windows_ssh_session {
        windows_virtual_terminal_input = enable_windows_virtual_terminal_input(
            &terminal_guard.restore_claimed,
            &terminal_guard.restore_windows_input_mode,
        );
    }

    #[cfg(windows)]
    if enable_client_protocols
        && windows_vti_input_backend_enabled()
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
        io::stdout().write_all(mode.set_sequence())?;
        io::stdout().flush()?;
    }

    execute!(io::stdout(), DisableLineWrap)?;

    terminal_guard.host_escape_disambiguation_active = host_escape_disambiguation_active;
    terminal_guard.buffered_host_input = buffered_host_input;
    Ok(terminal_guard)
}

pub(super) fn should_enable_host_color_scheme_reports(enable_client_protocols: bool) -> bool {
    enable_client_protocols && should_query_host_terminal_theme()
}

/// Guard that restores the terminal when dropped.
pub(super) struct TerminalGuard {
    host_escape_disambiguation_active: bool,
    buffered_host_input: Vec<u8>,
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
    restore_claimed: Arc<AtomicBool>,
    restored: bool,
    #[cfg(windows)]
    restore_windows_input_mode: Arc<WindowsInputModeRestore>,
}

#[cfg(not(windows))]
const HOST_KEYBOARD_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
#[cfg(not(windows))]
const MAX_BUFFERED_HOST_INPUT: usize = 64 * 1024;

#[cfg(not(windows))]
#[derive(Default)]
struct HostKeyboardProbeResponses {
    flags: Option<u16>,
    primary_device_attributes: bool,
}

#[cfg(not(windows))]
fn query_host_escape_disambiguation() -> (bool, Vec<u8>) {
    const QUERY: &[u8] = b"\x1b[?u\x1b[c";

    let mut buffered_input = Vec::new();
    if let Err(err) = io::stdout()
        .write_all(QUERY)
        .and_then(|()| io::stdout().flush())
    {
        tracing::debug!(%err, "host keyboard enhancement query unavailable");
        return (false, buffered_input);
    }

    // Bypass StdinLock's shared buffer so poll and read observe the same bytes.
    let stdin = io::stdin();
    let stdin_fd = stdin.as_raw_fd();
    let deadline = Instant::now() + HOST_KEYBOARD_QUERY_TIMEOUT;
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

    (
        host_escape_disambiguation_confirmed(&responses),
        buffered_input,
    )
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
fn query_host_escape_disambiguation() -> (bool, Vec<u8>) {
    (false, Vec::new())
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
) -> io::Result<()> {
    if reset_host_color_scheme_reports {
        writer.write_all(
            crate::terminal_theme::HOST_COLOR_SCHEME_REPORT_DISABLE_SEQUENCE.as_bytes(),
        )?;
    }
    // Restore a visible cursor and reset DECSCUSR back to the terminal default.
    writer.write_all(b"\x1b[?25h\x1b[0 q")?;
    writer.flush()
}

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

#[cfg(windows)]
pub(super) fn windows_vti_input_backend_enabled() -> bool {
    std::env::var("HERDR_WINDOWS_INPUT_BACKEND")
        .map(|backend| !backend.eq_ignore_ascii_case("crossterm"))
        .unwrap_or(true)
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
    exact_geometry: bool,
) -> bool {
    enabled && requested && exact_geometry
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
    windows_vti_input_backend_enabled()
        && (is_ssh_session() || crate::platform::windows_virtual_terminal_input_active())
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
        #[cfg(windows)]
        restore_windows_input_mode,
    )
}

fn restore_terminal_state(
    reset_keyboard_enhancements: bool,
    reset_modify_other_keys: bool,
    reset_host_color_scheme_reports: bool,
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
    let postlude_result =
        write_terminal_restore_postlude(&mut io::stdout(), reset_host_color_scheme_reports);

    #[cfg(windows)]
    if windows_vti_input_backend_enabled() && windows_win32_input_mode_enabled() {
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

impl TerminalGuard {
    pub(super) fn host_escape_disambiguation_active(&self) -> bool {
        self.host_escape_disambiguation_active
    }

    pub(super) fn take_buffered_host_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buffered_host_input)
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
        #[cfg(windows)]
        let restore_windows_input_mode = self.restore_windows_input_mode.clone();
        move || {
            let _ = restore_terminal_state_once(
                &restore_claimed,
                reset_keyboard_enhancements,
                reset_modify_other_keys,
                reset_host_color_scheme_reports,
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
    #[test]
    fn host_keyboard_probe_consumes_fragmented_responses_and_preserves_input() {
        let stream = b"before\x1b[?7u-middle-\x1b[?1;2cafter";

        for split in 1..stream.len() {
            let mut buffered = Vec::new();
            let mut responses = HostKeyboardProbeResponses::default();
            buffered.extend_from_slice(&stream[..split]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);
            buffered.extend_from_slice(&stream[split..]);
            consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

            assert_eq!(responses.flags, Some(7), "split {split}");
            assert!(responses.primary_device_attributes, "split {split}");
            assert_eq!(buffered, b"before-middle-after", "split {split}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_typed_input_before_responses() {
        let mut buffered = b"aPtyped\x1b[?7u\x1b[?1;2c".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
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
        let opaque = b"\x1b[200~paste \x1b[?1u \x1b[?1;2c\x1b[201~-\x1bPdata \x1b[?7u\x1b\\";
        let mut buffered = [opaque.as_slice(), b"\x1b[?7u\x1b[?1;2c"].concat();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert!(host_escape_disambiguation_confirmed(&responses));
        assert_eq!(buffered, opaque);
    }

    #[cfg(not(windows))]
    #[test]
    fn host_keyboard_probe_preserves_malformed_responses() {
        let mut buffered = b"a\x1b[?7;1ub\x1b[?65536uc".to_vec();
        let mut responses = HostKeyboardProbeResponses::default();

        consume_host_keyboard_probe_responses(&mut buffered, &mut responses);

        assert_eq!(responses.flags, None);
        assert!(!responses.primary_device_attributes);
        assert_eq!(buffered, b"a\x1b[?7;1ub\x1b[?65536uc");
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
