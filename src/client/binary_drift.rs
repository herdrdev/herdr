use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::protocol::{self, FrameData};
use tracing::warn;

const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) type DriftNotice = Arc<Mutex<Option<String>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExeStamp {
    dev: u64,
    inode: u64,
    size: u64,
    mtime: SystemTime,
}

pub(crate) fn drift_badge(
    running: &str,
    replaced: bool,
    installed: Option<&str>,
) -> Option<String> {
    if !replaced {
        return None;
    }

    let Some(installed) = installed else {
        return Some("⚠ restart Herdr: installed build unknown".to_owned());
    };
    if !running.contains("-dirty") && !installed.contains("-dirty") && running == installed {
        return None;
    }

    Some(format!(
        "⚠ restart Herdr: installed {} ≠ running {}",
        build_id(installed),
        build_id(running)
    ))
}

pub(crate) fn exe_replaced(
    proc_deleted: bool,
    startup: Option<ExeStamp>,
    current: Option<ExeStamp>,
) -> bool {
    proc_deleted
        || matches!((startup, current), (Some(startup), Some(current)) if startup != current)
}

fn monitor_step(
    running: &str,
    proc_deleted: bool,
    baseline: Option<ExeStamp>,
    current: Option<ExeStamp>,
    installed: Option<&str>,
) -> (Option<ExeStamp>, Option<String>) {
    if !exe_replaced(proc_deleted, baseline, current) {
        return (baseline, None);
    }

    match drift_badge(running, true, installed) {
        Some(badge) => (baseline, Some(badge)),
        None => (current, None),
    }
}

fn build_id(version: &str) -> &str {
    version
        .split_once("+fork.")
        .map(|(_, id)| id)
        .unwrap_or(version)
}

fn exe_stamp(path: &Path) -> Option<ExeStamp> {
    let metadata = fs::metadata(path).ok()?;
    let (dev, inode) = crate::platform::binary_file_identity(path).ok()?;
    Some(ExeStamp {
        dev,
        inode,
        size: metadata.len(),
        mtime: metadata.modified().ok()?,
    })
}

pub(crate) fn start_monitor(running: String, notice: DriftNotice) {
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let startup = exe_stamp(&executable);
    let proc_deleted = crate::platform::process_executable_deleted();
    let installed_path = crate::platform::installed_executable_path(&executable, proc_deleted);

    let _ = thread::Builder::new()
        .name("herdr-binary-drift".to_owned())
        .spawn(move || {
            let mut startup = startup;
            loop {
                thread::sleep(CHECK_INTERVAL);
                let current = exe_stamp(&executable);
                if !exe_replaced(proc_deleted, startup, current) {
                    continue;
                }

                let installed = installed_version(&installed_path);
                let (next_startup, badge) = monitor_step(
                    &running,
                    proc_deleted,
                    startup,
                    current,
                    installed.as_deref(),
                );
                startup = next_startup;
                if let Some(badge) = badge {
                    warn_binary_replaced(&running, installed.as_deref());
                    if let Ok(mut shared) = notice.lock() {
                        *shared = Some(badge);
                    }
                    break;
                }
            }
        });
}

fn installed_version(path: &Path) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
        }
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    version.trim().strip_prefix("herdr ").map(str::to_owned)
}

fn warn_binary_replaced(running: &str, installed: Option<&str>) {
    warn!(
        binary_replaced = true,
        running,
        installed = installed.unwrap_or("unknown"),
        "Herdr executable was replaced on disk"
    );
}

pub(crate) fn overlay_badge(frame: &mut FrameData, badge: &str) {
    if frame.width == 0 || frame.height == 0 {
        return;
    }
    let row_start = usize::from(frame.height - 1) * usize::from(frame.width);
    let row_end = row_start + usize::from(frame.width);
    if frame.cells.len() < row_end {
        return;
    }

    let badge_width = crate::protocol::rendered_text_width(badge);
    let start = usize::from(frame.width).saturating_sub(badge_width);
    let mut column = start;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(badge, true) {
        let width = unicode_width::UnicodeWidthStr::width(grapheme).max(1);
        if column + width > usize::from(frame.width) {
            break;
        }
        let cell = &mut frame.cells[row_start + column];
        cell.symbol = grapheme.to_owned();
        cell.fg = protocol::color_to_u32(ratatui::style::Color::LightYellow);
        cell.bg = protocol::color_to_u32(ratatui::style::Color::Reset);
        cell.modifier = protocol::modifier_to_u16(ratatui::style::Modifier::BOLD);
        cell.skip = false;
        cell.hyperlink = None;
        for continuation in 1..width {
            let cell = &mut frame.cells[row_start + column + continuation];
            cell.symbol.clear();
            cell.fg = protocol::color_to_u32(ratatui::style::Color::LightYellow);
            cell.bg = protocol::color_to_u32(ratatui::style::Color::Reset);
            cell.modifier = protocol::modifier_to_u16(ratatui::style::Modifier::BOLD);
            cell.skip = false;
            cell.hyperlink = None;
        }
        column += width;
    }
}

#[cfg(test)]
mod tests {
    use super::{drift_badge, exe_replaced, monitor_step, overlay_badge, ExeStamp};
    use crate::protocol::{CellData, FrameData};
    use std::time::UNIX_EPOCH;

    fn stamp(inode: u64) -> ExeStamp {
        ExeStamp {
            dev: 1,
            inode,
            size: 10,
            mtime: UNIX_EPOCH,
        }
    }

    #[test]
    fn drift_badge_cases() {
        let cases = [
            (
                "same clean build",
                "0.9.1+fork.abc123",
                false,
                Some("0.9.1+fork.abc123"),
                None,
            ),
            (
                "same clean build was replaced",
                "0.9.1+fork.abc123",
                true,
                Some("0.9.1+fork.abc123"),
                None,
            ),
            (
                "different build",
                "0.9.1+fork.old123",
                true,
                Some("0.9.1+fork.new456"),
                Some("⚠ restart Herdr: installed new456 ≠ running old123"),
            ),
            (
                "dirty running build",
                "0.9.1+fork.abc123-dirty",
                true,
                Some("0.9.1+fork.abc123"),
                Some("⚠ restart Herdr: installed abc123 ≠ running abc123-dirty"),
            ),
            (
                "dirty installed build",
                "0.9.1+fork.abc123",
                true,
                Some("0.9.1+fork.abc123-dirty"),
                Some("⚠ restart Herdr: installed abc123-dirty ≠ running abc123"),
            ),
            (
                "installed build is unknown",
                "0.9.1+fork.abc123",
                true,
                None,
                Some("⚠ restart Herdr: installed build unknown"),
            ),
            (
                "unchanged executable",
                "0.9.1+fork.old123",
                false,
                Some("0.9.1+fork.new456"),
                None,
            ),
        ];

        for (name, running, replaced, installed, expected) in cases {
            assert_eq!(
                drift_badge(running, replaced, installed),
                expected.map(str::to_owned),
                "case: {name}"
            );
        }
    }

    #[test]
    fn same_build_reinstall_rebaselines_then_detects_different_build() {
        let original = stamp(1);
        let identical_reinstall = stamp(2);
        let different_reinstall = stamp(3);

        let (baseline, badge) = monitor_step(
            "0.9.1+fork.running",
            false,
            Some(original),
            Some(identical_reinstall),
            Some("0.9.1+fork.running"),
        );
        assert_eq!(baseline, Some(identical_reinstall));
        assert_eq!(badge, None);

        let (baseline, badge) = monitor_step(
            "0.9.1+fork.running",
            false,
            baseline,
            Some(different_reinstall),
            Some("0.9.1+fork.newbuild"),
        );
        assert_eq!(baseline, Some(identical_reinstall));
        assert_eq!(
            badge.as_deref(),
            Some("⚠ restart Herdr: installed newbuild ≠ running running")
        );
    }

    #[test]
    fn exe_replaced_cases() {
        let cases = [
            (
                "proc exe is deleted",
                true,
                Some(stamp(1)),
                Some(stamp(1)),
                true,
            ),
            ("inode changed", false, Some(stamp(1)), Some(stamp(2)), true),
            (
                "current stamp is missing",
                false,
                Some(stamp(1)),
                None,
                false,
            ),
            (
                "startup stamp is missing",
                false,
                None,
                Some(stamp(2)),
                false,
            ),
            ("same stamp", false, Some(stamp(1)), Some(stamp(1)), false),
        ];

        for (name, proc_deleted, startup, current, expected) in cases {
            assert_eq!(
                exe_replaced(proc_deleted, startup, current),
                expected,
                "case: {name}"
            );
        }
    }

    #[test]
    fn overlay_badge_is_right_aligned_on_the_bottom_row() {
        let mut frame = FrameData {
            cells: (0..20)
                .map(|_| CellData {
                    symbol: ".".to_owned(),
                    fg: 0,
                    bg: 0,
                    modifier: 0,
                    skip: false,
                    hyperlink: None,
                })
                .collect(),
            width: 10,
            height: 2,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };

        overlay_badge(&mut frame, "⚠ go");

        assert_eq!(
            frame.cells[16..20]
                .iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>(),
            "⚠ go"
        );
        assert!(frame.cells[16..20].iter().all(
            |cell| cell.fg == crate::protocol::color_to_u32(ratatui::style::Color::LightYellow)
        ));
        assert_eq!(frame.cells[15].symbol, ".");
    }
}
