//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

/// A process lifetime, independent of mutable names, argv and terminal titles.
/// The birth token is opaque and host/boot-local, not a persisted session ID.
/// Compare the complete value: equal PIDs with different tokens are different
/// processes. Failure to read an identity is not evidence of a retained binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) birth_token: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

/// Why a pane runtime ended, before application persistence policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitReason {
    Exited,
    Interrupted,
    /// Imported runtimes have no child wait handle in the replacement server.
    #[cfg(unix)]
    Handoff,
    WaitFailed,
}

impl ChildExitReason {
    pub(crate) fn requires_session_checkpoint(self) -> bool {
        match self {
            Self::Interrupted => true,
            #[cfg(unix)]
            Self::Handoff => true,
            _ => false,
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_common::classify_child_exit;

#[cfg(not(any(unix, windows)))]
pub(crate) fn classify_child_exit(_status: &portable_pty::ExitStatus) -> ChildExitReason {
    ChildExitReason::Exited
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

pub(crate) fn prepare_paste_text_for_pty(text: String) -> String {
    prepare_paste_text_for_pty_platform(text)
}

pub(crate) fn plugin_runtime_path(path: &std::path::Path) -> std::path::PathBuf {
    plugin_runtime_path_platform(path)
}

#[cfg(not(windows))]
fn plugin_runtime_path_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn prepare_paste_text_for_pty_platform(text: String) -> String {
    text
}

#[cfg(not(windows))]
pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

#[cfg(not(windows))]
fn configure_background_command_platform(_command: &mut std::process::Command) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: cfg!(unix),
        direct_terminal_attach: cfg!(unix),
        preserve_legacy_doubled_escape_input: cfg!(target_os = "macos"),
    }
}

pub(crate) fn terminal_grid_size() -> std::io::Result<(u16, u16)> {
    #[cfg(unix)]
    let (cols, rows) = unix_common::read_terminal_grid_size()?;
    #[cfg(windows)]
    let (cols, rows) = windows::read_terminal_grid_size()?;
    #[cfg(not(any(unix, windows)))]
    let (cols, rows) = fallback::read_terminal_grid_size()?;

    if cols == 0 || rows == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "terminal reported a zero-sized grid",
        ));
    }
    Ok((cols, rows))
}

#[cfg(not(windows))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the SIGWINCH handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records SIGWINCH events that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

#[cfg(unix)]
mod unix_common;
#[cfg(unix)]
pub(crate) use unix_common::{
    begin_cli_output, end_cli_output, forward_remote_bridge_stdio, sync_directory_after_replace,
    RemoteBridgeWake,
};

mod client_state;
pub(crate) use client_state::{create_private_state_file, replace_file, sync_parent_directory};

/// Preserve the existing non-Unix post-replacement durability policy.
#[cfg(not(unix))]
pub(crate) fn sync_directory_after_replace(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn quote_windows_command_line_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_agent_hint_with_registry(
    _registry: &crate::agents::AgentRegistry,
    _pid: u32,
) -> Option<crate::detect::Agent> {
    None
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    parse_agent_env_hint_with_registry(&crate::agents::registry(), environ)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint_with_registry(
    registry: &crate::agents::AgentRegistry,
    environ: &[u8],
) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        // Preserve the label parser's alias, executable suffix, and basename
        // compatibility without reacquiring the registry for each job member.
        let mut label = std::str::from_utf8(value).ok()?.trim().to_lowercase();
        for suffix in [".exe", ".cmd", ".bat", ".ps1", ".js"] {
            if label.ends_with(suffix) {
                label.truncate(label.len() - suffix.len());
                break;
            }
        }
        let name = label
            .rsplit(['/', '\\'])
            .find(|component| !component.is_empty())
            .unwrap_or(&label);
        return registry
            .profile_by_normalized_alias(name)
            .or_else(|| registry.profile_by_versioned_process_name(name))
            .map(|profile| profile.legacy_agent());
    }
    None
}

/// Unix foreground observation is registry-independent; Windows must select
/// candidates with the same snapshot as its caller's detection cycle. A bound
/// process is only a PID locator; callers must validate its saved birth token
/// before retaining identity from the returned observation.
#[cfg(not(windows))]
pub(crate) fn foreground_job_with_registry(
    _registry: &crate::agents::RegistrySnapshot,
    pid: u32,
    _bound: Option<&ForegroundProcess>,
) -> Option<ForegroundJob> {
    foreground_job(pid)
}

#[cfg(not(windows))]
pub(crate) fn foreground_process_group_id_with_registry(
    _registry: &crate::agents::RegistrySnapshot,
    pid: u32,
    _bound: Option<&ForegroundProcess>,
) -> Option<u32> {
    foreground_process_group_id(pid)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub(crate) struct InputSourceRestore;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn pump_input_source_runloop() {}

/// Switches the host keyboard input source while prefix mode is active.
///
/// `App` drives this through a trait so the prefix-mode transitions can be
/// tested with a fake, without touching the real macOS APIs or leaking a
/// platform-specific restore type into `App`.
pub(crate) trait PrefixInputSource {
    /// Switch to an ASCII-capable input source for prefix commands. No-op if
    /// the current source is already ASCII-capable, the platform is
    /// unsupported, or the switch fails. Calling it again before `restore`
    /// keeps the source saved by the first call.
    fn switch_to_ascii(&mut self);

    /// Restore whatever `switch_to_ascii` saved. No-op if nothing was switched.
    fn restore(&mut self);
}

/// Production [`PrefixInputSource`] backed by the per-platform API.
#[derive(Default)]
pub(crate) struct RealPrefixInputSource {
    restore: Option<InputSourceRestore>,
}

impl PrefixInputSource for RealPrefixInputSource {
    fn switch_to_ascii(&mut self) {
        if self.restore.is_none() {
            // Drain pending input-source-change notifications so the read below is fresh (see
            // `pump_input_source_runloop`); a no-op on non-macOS.
            pump_input_source_runloop();
            self.restore = switch_to_ascii_input_source();
        }
    }

    fn restore(&mut self) {
        let _ = self.restore.take();
    }
}

#[cfg(all(test, any(unix, windows)))]
#[test]
fn child_exit_classification_only_checkpoints_interruptions() {
    for code in [0, 1, 130, 255, 0xC0000005] {
        let reason = classify_child_exit(&portable_pty::ExitStatus::with_exit_code(code));
        assert_eq!(reason, ChildExitReason::Exited, "exit code {code:#x}");
        assert!(!reason.requires_session_checkpoint());
    }
    #[cfg(windows)]
    let status = portable_pty::ExitStatus::with_exit_code(0xC000013A);
    #[cfg(not(windows))]
    let status = portable_pty::ExitStatus::with_signal("Terminated: 15");
    assert_eq!(classify_child_exit(&status), ChildExitReason::Interrupted);
    assert!(classify_child_exit(&status).requires_session_checkpoint());
    #[cfg(unix)]
    assert!(ChildExitReason::Handoff.requires_session_checkpoint());
    assert!(!ChildExitReason::WaitFailed.requires_session_checkpoint());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_uses_the_pinned_registry_and_preserves_label_compatibility() {
        let registry = crate::agents::store::snapshot_for_test(vec![(
            "agents/pane-env-agent/agent.toml".into(),
            "schema = 1\nid = 'pane-env-agent'\nname = 'Pane env'\naliases = ['pane-env-alias']\nstartable = true\n[launch]\nunix = 'pane-env-agent'\nwindows = 'pane-env-agent'\n".into(),
        )], 99).unwrap();
        let agent = crate::detect::Agent::parse("pane-env-agent").unwrap();
        for label in [
            "pane-env-agent",
            " PANE-ENV-ALIAS ",
            r"C:\bin\PANE-ENV-ALIAS.EXE",
            "/opt/bin/pane-env-alias.js",
        ] {
            let environ = format!("PATH=/bin\0HERDR_AGENT={label}\0");
            assert_eq!(
                parse_agent_env_hint_with_registry(&registry, environ.as_bytes()),
                Some(agent)
            );
        }
        assert_eq!(
            parse_agent_env_hint_with_registry(
                &crate::agents::AgentRegistry::default(),
                b"HERDR_AGENT=pane-env-agent\0"
            ),
            None
        );
        assert_eq!(
            parse_agent_env_hint_with_registry(&registry, b"HERDR_AGENT=\xff\0"),
            None
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod process_identity_tests {
    use super::process_identity;
    use std::process::{Child, Command, Stdio};

    struct HarmlessChild(Child);

    impl HarmlessChild {
        fn spawn() -> Self {
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "read -r line"]);
                command
            };
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("cmd.exe");
                command.args(["/D", "/Q", "/C", "set /p line="]);
                command
            };
            Self(
                command
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("spawn harmless stdin-waiting child"),
            )
        }
    }

    impl Drop for HarmlessChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn process_identity_is_stable_for_the_current_native_process() {
        let pid = std::process::id();
        let identity = process_identity(pid).expect("current process birth token");
        assert_eq!(identity.pid, pid);
        assert_eq!(process_identity(pid), Some(identity));
        assert_eq!(process_identity(0), None);
        assert_eq!(process_identity(u32::MAX), None);
    }

    #[test]
    fn process_identity_does_not_retain_a_reaped_native_child() {
        let mut child = HarmlessChild::spawn();
        let pid = child.0.id();
        let identity = process_identity(pid).expect("live child birth token");
        assert_eq!(process_identity(pid), Some(identity));
        child.0.kill().expect("terminate harmless child");
        child.0.wait().expect("reap harmless child");
        assert_ne!(process_identity(pid), Some(identity));
    }

    #[cfg(windows)]
    #[test]
    fn process_identity_rejects_a_child_that_exited_with_still_active_code() {
        let mut child = Command::new("cmd.exe")
            .args(["/D", "/Q", "/C", "exit 259"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn harmless exiting child");
        let pid = child.id();
        assert_eq!(child.wait().expect("reap child").code(), Some(259));
        // Child still owns its handle here, keeping the exited process object
        // queryable. The exit timestamp must reject it despite numeric code 259.
        assert_eq!(process_identity(pid), None);
    }
}
