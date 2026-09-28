use std::path::{Path, PathBuf};
use std::process::Command;

use super::{ClipboardImage, ForegroundJob, Signal};

pub(crate) fn effective_user_name() -> Option<String> {
    None
}

fn sync_parent_dir(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

pub(crate) fn replace_file_durably(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::rename(source, target)?;
    sync_parent_dir(
        target.parent().ok_or_else(|| {
            std::io::Error::other("persisted session path has no parent directory")
        })?,
    )
}

pub(crate) fn remove_file_durably(path: &Path, _tombstone: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_parent_dir(path.parent().ok_or_else(|| {
            std::io::Error::other("persisted session path has no parent directory")
        })?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
pub(crate) use super::unix_common::set_default_plugin_pane_pwd;

#[cfg(not(unix))]
pub(crate) fn set_default_plugin_pane_pwd(
    _env: &mut Vec<(String, String)>,
    _cwd: &std::path::Path,
) {
}

#[cfg(unix)]
pub(super) const REMOTE_BRIDGE_CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

pub(crate) fn forward_remote_bridge_stdio(
    stream: crate::ipc::LocalStream,
    _idle_timeout: bool,
) -> std::io::Result<()> {
    use interprocess::TryClone as _;

    let mut stdout = std::io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;
    let _upload = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let _ = std::io::copy(&mut stdin, &mut stdin_to_socket);
    });
    std::io::copy(&mut socket_to_stdout, &mut stdout).map(|_| ())
}

#[cfg(not(unix))]
pub(super) fn read_terminal_grid_size() -> std::io::Result<(u16, u16)> {
    crossterm::terminal::size()
}

pub(crate) fn remote_ssh_config_paths() -> super::RemoteSshConfigPaths {
    super::RemoteSshConfigPaths {
        user_config: std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".ssh").join("config")),
        system_config: None,
        multiplexing: false,
    }
}

pub(crate) fn create_remote_ssh_config_dir(_control_socket_name: &str) -> std::io::Result<PathBuf> {
    for attempt in 0..100 {
        let dir = std::env::temp_dir().join(format!("herdr-ssh-{}-{attempt}", std::process::id()));
        match create_remote_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "failed to create private herdr ssh config directory",
    ))
}

pub(crate) fn create_remote_ssh_config_file(
    path: &std::path::Path,
) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub(crate) fn create_remote_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

pub(crate) fn remote_private_temp_base() -> PathBuf {
    std::env::temp_dir()
}

pub(crate) fn remote_bridge_endpoint_path(readable_name: &str, _short_name: &str) -> PathBuf {
    std::env::temp_dir().join(readable_name)
}

pub(crate) fn remote_reattach_program(program: &str) -> String {
    shell_quote(program)
}

pub(crate) fn remote_reattach_argument(value: &str) -> String {
    shell_quote(value)
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}
/// Unsupported platform stub.
pub fn raise_server_nofile_limit() {}

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    false
}

pub(crate) fn should_query_host_terminal_palette() -> bool {
    false
}

pub(crate) fn hostname() -> Option<String> {
    None
}

pub(crate) fn local_datetime() -> Option<time::PrimitiveDateTime> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    local_datetime_at(now)
}

pub(crate) fn local_datetime_at(unix_seconds: u64) -> Option<time::PrimitiveDateTime> {
    let timestamp = i64::try_from(unix_seconds).ok()?;
    let datetime = time::OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    Some(time::PrimitiveDateTime::new(
        datetime.date(),
        datetime.time(),
    ))
}

pub(crate) fn local_time_today_unix(hour: u8, minute: u8) -> Option<u64> {
    let now = time::OffsetDateTime::now_utc();
    let time = time::Time::from_hms(hour, minute, 0).ok()?;
    let deadline = time::PrimitiveDateTime::new(now.date(), time).assume_utc();
    u64::try_from(deadline.unix_timestamp()).ok()
}

pub(crate) fn tomorrow_morning_unix() -> Option<u64> {
    None
}

pub(crate) fn status_commands_supported() -> bool {
    false
}

pub(crate) fn configure_status_command(_process: &mut std::process::Command) {}

pub(crate) struct StatusCommandGuard;

impl StatusCommandGuard {
    pub(crate) fn new(_child: &tokio::process::Child) -> std::io::Result<Self> {
        Ok(Self)
    }

    pub(crate) fn terminate(&mut self) {}
}

fn raw_command_argv(command: &str, flag: &str) -> Vec<std::ffi::OsString> {
    vec!["/bin/sh".into(), flag.into(), command.into()]
}

pub(crate) fn detached_custom_command_process_platform(command: &str) -> std::process::Command {
    let argv = raw_command_argv(command, "-lc");
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

pub(crate) fn pane_custom_command_pty_builder_platform(
    command: &str,
) -> portable_pty::CommandBuilder {
    portable_pty::CommandBuilder::from_argv(raw_command_argv(command, "-c"))
}

pub(crate) fn interactive_shell_command(_argv: &[String], _shell_name: &str) -> Option<String> {
    None
}

/// Unsupported platform stub.
pub(crate) fn scrollback_editor_argv(_path: &std::path::Path) -> std::io::Result<Vec<String>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening scrollback in an editor is not supported on this platform",
    ))
}

/// Unsupported platform stub.
pub fn detach_server_daemon_command(_command: &mut Command) {}

/// Unsupported platform stub.
pub fn current_process_is_detached_server_daemon() -> bool {
    false
}

pub(crate) fn available_pane_shell(_child_pid: u32) -> Option<String> {
    None
}

/// Unsupported platform stub.
pub fn foreground_job(_child_pid: u32) -> Option<ForegroundJob> {
    None
}

/// Unsupported platform stub.
pub fn descendant_processes(_root_pid: u32) -> Vec<crate::platform::ForegroundProcess> {
    Vec::new()
}

/// Unsupported platform stub.
pub fn foreground_group_leader_job(_process_group_id: u32) -> Option<ForegroundJob> {
    None
}

/// Unsupported platform stub.
pub fn foreground_process_group_id(_child_pid: u32) -> Option<u32> {
    None
}

/// Unsupported platform stub.
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// Unsupported platform stub.
/// Platform parity with the Linux snapshot path. Only Linux pays a
/// full-`/proc`-scan per call, so elsewhere this is the uncached function.
pub fn session_processes_cached(_child_pid: u32) -> Vec<u32> {
    session_processes(_child_pid)
}

pub fn session_processes(_child_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Unsupported platform stub.
pub fn signal_processes(_pids: &[u32], _signal: Signal) {}

/// Unsupported platform stub.
pub fn process_exists(_pid: u32) -> bool {
    false
}

/// Unsupported platform stub.
pub fn write_clipboard(_bytes: &[u8]) -> bool {
    false
}

/// Unsupported platform stub.
pub fn read_clipboard_text() -> Option<String> {
    None
}

/// Unsupported platform stub.
pub fn open_url(_url: &str) -> std::io::Result<Option<std::process::Child>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening URLs is not supported on this platform",
    ))
}

/// Unsupported platform stub.
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

/// Unsupported platform stub.
pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}

pub(crate) fn sample_status_metrics(
    _sampler: &mut super::status_metrics::StatusMetricSampler,
) -> super::status_metrics::StatusMetrics {
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|value| super::status_metrics::short_hostname(&value))
        .unwrap_or_else(|| "localhost".into());
    super::status_metrics::StatusMetrics {
        hostname,
        ..super::status_metrics::StatusMetrics::default()
    }
}
