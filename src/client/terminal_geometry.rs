use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[cfg(any(unix, test))]
use tracing::debug;

use super::ClientLoopEvent;

const DEFAULT_CELL_WIDTH_PX: u32 = 8;
const DEFAULT_CELL_HEIGHT_PX: u32 = 16;

/// Average cell size derived from a terminal ioctl pixel extent.
///
/// The extent need not divide evenly by the grid: terminals may include padding,
/// and pixel mouse coordinates retain the raw extent for proportional mapping.
pub(super) fn ioctl_cell_size(
    columns: u16,
    rows: u16,
    width_px: u32,
    height_px: u32,
) -> Option<(u32, u32)> {
    if columns == 0 || rows == 0 || width_px == 0 || height_px == 0 {
        return None;
    }
    Some((
        (width_px / u32::from(columns)).max(1),
        (height_px / u32::from(rows)).max(1),
    ))
}

fn ioctl_terminal_geometry() -> Option<(u16, u16, u32, u32)> {
    let size = crossterm::terminal::window_size().ok()?;
    let (cell_width_px, cell_height_px) = ioctl_cell_size(
        size.columns,
        size.rows,
        u32::from(size.width),
        u32::from(size.height),
    )?;
    Some((size.columns, size.rows, cell_width_px, cell_height_px))
}

pub(super) fn cell_size_fallback(reported: u64, last: Option<(u32, u32)>) -> (u32, u32) {
    unpack_cell_size(reported)
        .or(last.filter(|(width, height)| *width > 0 && *height > 0))
        .unwrap_or((DEFAULT_CELL_WIDTH_PX, DEFAULT_CELL_HEIGHT_PX))
}

#[cfg(any(unix, test))]
pub(super) fn pack_cell_size(width_px: u32, height_px: u32) -> u64 {
    (u64::from(width_px) << 32) | u64::from(height_px)
}

fn unpack_cell_size(packed: u64) -> Option<(u32, u32)> {
    let width_px = (packed >> 32) as u32;
    let height_px = (packed & u64::from(u32::MAX)) as u32;
    (width_px > 0 && height_px > 0).then_some((width_px, height_px))
}

fn current_terminal_geometry(
    pixel_geometry_enabled: bool,
    pixel_geometry_fallback: bool,
    reported_cell_size: &AtomicU64,
    last_cell_size: Option<(u32, u32)>,
) -> (u16, u16, u32, u32, bool) {
    if !pixel_geometry_enabled {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        return (cols, rows, 0, 0, false);
    }
    if let Some((cols, rows, cell_width_px, cell_height_px)) = ioctl_terminal_geometry() {
        return (cols, rows, cell_width_px, cell_height_px, true);
    }
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if !pixel_geometry_fallback {
        return (cols, rows, 0, 0, false);
    }
    let (cell_width_px, cell_height_px) =
        cell_size_fallback(reported_cell_size.load(Ordering::Acquire), last_cell_size);
    (cols, rows, cell_width_px, cell_height_px, false)
}

/// Reads terminal geometry before the handshake. Pixel input and direct graphics
/// are eligible only when one ioctl supplied a coherent exact geometry snapshot.
pub(super) fn initial_terminal_geometry(
    pixel_geometry_enabled: bool,
    pixel_geometry_fallback: bool,
) -> (u16, u16, u32, u32, bool) {
    if !pixel_geometry_enabled {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        return (cols, rows, 0, 0, false);
    }
    match ioctl_terminal_geometry() {
        Some((cols, rows, width, height)) => (cols, rows, width, height, true),
        None => {
            let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
            if pixel_geometry_fallback {
                (
                    cols,
                    rows,
                    DEFAULT_CELL_WIDTH_PX,
                    DEFAULT_CELL_HEIGHT_PX,
                    false,
                )
            } else {
                (cols, rows, 0, 0, false)
            }
        }
    }
}

pub(super) fn resize_report_required(
    signalled: bool,
    new_size: (u16, u16, u32, u32, bool),
    last_size: (u16, u16, u32, u32, bool),
) -> bool {
    signalled || new_size != last_size
}

/// Watches the terminal size and sends resize events when it changes.
///
/// The baseline cell size must match what the handshake sent to the server:
/// reading a fresh one here would race the host cell size reply and could
/// swallow the first change.
#[allow(clippy::too_many_arguments)] // The arguments are one immutable launch snapshot, not shared state.
pub(super) fn resize_poll_loop(
    resize_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    initial_cols: u16,
    initial_rows: u16,
    initial_cell_width: u32,
    initial_cell_height: u32,
    initial_pixel_geometry_exact: bool,
    pixel_geometry_enabled: bool,
    pixel_geometry_fallback: bool,
    reported_cell_size: &AtomicU64,
    should_quit: &Arc<AtomicBool>,
) {
    crate::platform::watch_terminal_resize_signal();
    let mut last_size = (
        initial_cols,
        initial_rows,
        initial_cell_width,
        initial_cell_height,
        initial_pixel_geometry_exact,
    );
    while !should_quit.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(100));
        let signalled = crate::platform::take_terminal_resize_signal();
        let new_size = current_terminal_geometry(
            pixel_geometry_enabled,
            pixel_geometry_fallback,
            reported_cell_size,
            Some((last_size.2, last_size.3)),
        );
        if resize_report_required(signalled, new_size, last_size) {
            last_size = new_size;
            if resize_tx
                .blocking_send(ClientLoopEvent::Resize(
                    new_size.0, new_size.1, new_size.2, new_size.3, new_size.4,
                ))
                .is_err()
            {
                break;
            }
        }
    }
}

#[cfg(any(not(windows), test))]
pub(super) fn query_host_terminal_appearance() {
    let _ = write_host_terminal_appearance_query(io::stdout());
}

#[cfg(any(not(windows), test))]
pub(super) fn write_host_terminal_appearance_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(crate::terminal_theme::HOST_COLOR_SCHEME_QUERY_SEQUENCE.as_bytes())?;
    writer.flush()
}

pub(super) fn query_host_terminal_theme() {
    let _ = write_host_terminal_theme_query(io::stdout());
}

pub(super) fn should_query_host_terminal_theme() -> bool {
    !cfg!(windows)
}

pub(super) fn write_host_terminal_theme_query(mut writer: impl io::Write) -> io::Result<()> {
    let query = crate::terminal_theme::host_terminal_theme_query_sequence(
        crate::platform::should_query_host_terminal_palette(),
    );
    writer.write_all(query.as_bytes())?;
    writer.flush()
}

const HOST_CELL_SIZE_QUERY: &[u8] = b"\x1b[16t";

pub(super) fn query_host_cell_size() {
    let _ = write_host_cell_size_query(io::stdout());
}

pub(super) fn should_query_host_cell_size() -> bool {
    !cfg!(windows)
}

pub(super) fn host_cell_size_query_required(kitty_graphics_enabled: bool) -> bool {
    kitty_graphics_enabled && should_query_host_cell_size() && ioctl_terminal_geometry().is_none()
}

pub(super) fn write_host_cell_size_query(mut writer: impl io::Write) -> io::Result<()> {
    writer.write_all(HOST_CELL_SIZE_QUERY)?;
    writer.flush()
}

#[cfg(any(unix, test))]
pub(super) fn store_reported_cell_size(
    reported_cell_size: &AtomicU64,
    width_px: u32,
    height_px: u32,
) {
    let packed = pack_cell_size(width_px, height_px);
    if reported_cell_size.swap(packed, Ordering::AcqRel) != packed {
        debug!(width_px, height_px, "host terminal reported cell size");
    }
}

#[cfg(any(unix, test))]
pub(super) fn reported_cell_size_from_events(
    events: &[crate::raw_input::RawInputEvent],
) -> Option<(u32, u32)> {
    events.iter().rev().find_map(|event| match event {
        crate::raw_input::RawInputEvent::HostCellSizeReport {
            width_px,
            height_px,
        } => Some((*width_px, *height_px)),
        _ => None,
    })
}
