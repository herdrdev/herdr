//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

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
pub(crate) use unix_common::{begin_cli_output, end_cli_output};

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
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

/// The activation tool a pane's interpreter environment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualEnvKind {
    Conda,
    Venv,
}

impl VirtualEnvKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Conda => "conda",
            Self::Venv => "venv",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        match value {
            "conda" => Some(Self::Conda),
            "venv" => Some(Self::Venv),
            _ => None,
        }
    }
}

/// An activated interpreter environment observed on a live process.
///
/// `conda activate` and the `venv`/`virtualenv` activate scripts both work the
/// same way: export a prefix variable and put that prefix's binary directory
/// first on `PATH`. The prefix is therefore the whole activation — everything
/// else is derived from it, which is why only the prefix and its display name
/// are recorded here instead of a snapshot of the process `PATH`. A `PATH`
/// captured on one run goes stale as soon as the user installs, removes, or
/// upgrades anything outside the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualEnvActivation {
    pub kind: VirtualEnvKind,
    pub prefix: std::path::PathBuf,
    /// `CONDA_DEFAULT_ENV`, or the prompt label a venv was activated with.
    pub name: Option<String>,
}

impl VirtualEnvActivation {
    /// Directories the activation puts in front of `PATH`, nearest first.
    ///
    /// Conda spreads a Windows environment over several directories and its
    /// activate script adds all of them, so a single `Scripts` entry is not
    /// enough to make the environment usable there.
    pub(crate) fn path_entries(&self) -> Vec<std::path::PathBuf> {
        let prefix = &self.prefix;
        match (self.kind, cfg!(windows)) {
            (VirtualEnvKind::Conda, true) => vec![
                prefix.clone(),
                prefix.join("Library").join("mingw-w64").join("bin"),
                prefix.join("Library").join("usr").join("bin"),
                prefix.join("Library").join("bin"),
                prefix.join("Scripts"),
                prefix.join("bin"),
            ],
            (VirtualEnvKind::Venv, true) => vec![prefix.join("Scripts")],
            (_, false) => vec![prefix.join("bin")],
        }
    }

    /// Environment overrides that re-enter this environment in a fresh process.
    ///
    /// `base_path` is the `PATH` the new process would otherwise inherit. The
    /// activation entries are prepended to it, skipping any that survived, so
    /// repeated restores cannot grow `PATH` without bound.
    ///
    /// `CONDA_SHLVL` is pinned to 1 rather than restored from the observed
    /// process: nesting depth belongs to the shell that stacked the
    /// activations, and a restored pane starts from an unactivated shell.
    ///
    /// Conda's automatic base activation is turned off for the restored shell.
    /// A pane comes back as an interactive login shell, so it re-runs the
    /// user's rc files, and conda's init hook activates base by default. That
    /// runs after this environment is handed in and replaces it — it shadows a
    /// restored venv's interpreter too, because base lands ahead of the venv on
    /// `PATH`. Both spellings are set because conda renamed the setting in 25.x
    /// and older releases only understand the previous one. Panes without a
    /// recorded environment are untouched and still auto-activate base.
    pub(crate) fn launch_env(&self, base_path: Option<&str>) -> Vec<(String, String)> {
        let mut env = match self.kind {
            VirtualEnvKind::Conda => {
                let mut vars = vec![(
                    "CONDA_PREFIX".to_string(),
                    self.prefix.to_string_lossy().into_owned(),
                )];
                if let Some(name) = &self.name {
                    vars.push(("CONDA_DEFAULT_ENV".to_string(), name.clone()));
                }
                vars.push(("CONDA_SHLVL".to_string(), "1".to_string()));
                vars
            }
            VirtualEnvKind::Venv => {
                let mut vars = vec![(
                    "VIRTUAL_ENV".to_string(),
                    self.prefix.to_string_lossy().into_owned(),
                )];
                if let Some(name) = &self.name {
                    vars.push(("VIRTUAL_ENV_PROMPT".to_string(), name.clone()));
                }
                vars
            }
        };

        env.push(("CONDA_AUTO_ACTIVATE".to_string(), "false".to_string()));
        env.push(("CONDA_AUTO_ACTIVATE_BASE".to_string(), "false".to_string()));

        let entries = self.path_entries();
        let inherited = base_path.unwrap_or_default();
        let kept = std::env::split_paths(inherited)
            .filter(|dir| !entries.iter().any(|entry| entry == dir))
            .collect::<Vec<_>>();
        if let Ok(path) = std::env::join_paths(entries.into_iter().chain(kept)) {
            env.push(("PATH".to_string(), path.to_string_lossy().into_owned()));
        }
        env
    }
}

/// Read an activation out of a NUL-separated environment block.
///
/// A venv nested inside a conda environment leaves both prefixes exported, and
/// `VIRTUAL_ENV` is the inner one, so it wins when both are present.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_virtual_env_activation(environ: &[u8]) -> Option<VirtualEnvActivation> {
    let mut conda_prefix = None;
    let mut conda_name = None;
    let mut venv_prefix = None;
    let mut venv_name = None;

    for record in environ.split(|&byte| byte == 0) {
        let Some(index) = record.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let (key, value) = record.split_at(index);
        let Ok(value) = std::str::from_utf8(&value[1..]) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        match key {
            b"CONDA_PREFIX" => conda_prefix = Some(value.to_string()),
            b"CONDA_DEFAULT_ENV" => conda_name = Some(value.to_string()),
            b"VIRTUAL_ENV" => venv_prefix = Some(value.to_string()),
            b"VIRTUAL_ENV_PROMPT" => venv_name = Some(value.to_string()),
            _ => {}
        }
    }

    if let Some(prefix) = venv_prefix {
        return Some(VirtualEnvActivation {
            kind: VirtualEnvKind::Venv,
            prefix: prefix.into(),
            name: venv_name,
        });
    }
    conda_prefix.map(|prefix| VirtualEnvActivation {
        kind: VirtualEnvKind::Conda,
        prefix: prefix.into(),
        name: conda_name,
    })
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn environ(records: &[&str]) -> Vec<u8> {
        let mut block = Vec::new();
        for record in records {
            block.extend_from_slice(record.as_bytes());
            block.push(0);
        }
        block
    }

    fn path_value(env: &[(String, String)]) -> Vec<std::path::PathBuf> {
        let path = env
            .iter()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value.clone())
            .expect("expected PATH");
        std::env::split_paths(&path).collect()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_virtual_env_activation_reads_conda_prefix_and_name() {
        let activation = parse_virtual_env_activation(&environ(&[
            "PATH=/opt/conda/envs/web/bin:/usr/bin",
            "CONDA_PREFIX=/opt/conda/envs/web",
            "CONDA_DEFAULT_ENV=web",
            "TERM=xterm-256color",
        ]))
        .expect("expected an activation");

        assert_eq!(activation.kind, VirtualEnvKind::Conda);
        assert_eq!(
            activation.prefix,
            std::path::PathBuf::from("/opt/conda/envs/web")
        );
        assert_eq!(activation.name.as_deref(), Some("web"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_virtual_env_activation_prefers_venv_nested_in_conda() {
        let activation = parse_virtual_env_activation(&environ(&[
            "CONDA_PREFIX=/opt/conda",
            "CONDA_DEFAULT_ENV=base",
            "VIRTUAL_ENV=/work/api/.venv",
            "VIRTUAL_ENV_PROMPT=api",
        ]))
        .expect("expected an activation");

        assert_eq!(activation.kind, VirtualEnvKind::Venv);
        assert_eq!(
            activation.prefix,
            std::path::PathBuf::from("/work/api/.venv")
        );
        assert_eq!(activation.name.as_deref(), Some("api"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_virtual_env_activation_ignores_unactivated_and_empty_environments() {
        assert!(parse_virtual_env_activation(&environ(&["PATH=/usr/bin", "TERM=xterm"])).is_none());
        // conda exports an empty CONDA_PREFIX after the last `conda deactivate`.
        assert!(
            parse_virtual_env_activation(&environ(&["CONDA_PREFIX=", "VIRTUAL_ENV="])).is_none()
        );
    }

    #[test]
    fn launch_env_puts_the_environment_first_on_the_inherited_path() {
        let activation = VirtualEnvActivation {
            kind: VirtualEnvKind::Venv,
            prefix: "/work/api/.venv".into(),
            name: None,
        };

        let env = activation.launch_env(Some("/usr/local/bin:/usr/bin"));

        assert!(env.contains(&("VIRTUAL_ENV".to_string(), "/work/api/.venv".to_string())));
        assert_eq!(
            path_value(&env),
            [
                std::path::PathBuf::from("/work/api/.venv/bin"),
                std::path::PathBuf::from("/usr/local/bin"),
                std::path::PathBuf::from("/usr/bin"),
            ]
        );
    }

    #[test]
    fn launch_env_does_not_repeat_entries_already_on_the_inherited_path() {
        let activation = VirtualEnvActivation {
            kind: VirtualEnvKind::Conda,
            prefix: "/opt/conda/envs/web".into(),
            name: Some("web".to_string()),
        };

        let env = activation.launch_env(Some("/opt/conda/envs/web/bin:/usr/bin"));

        assert_eq!(
            path_value(&env),
            [
                std::path::PathBuf::from("/opt/conda/envs/web/bin"),
                std::path::PathBuf::from("/usr/bin"),
            ]
        );
    }

    #[test]
    fn launch_env_stops_conda_from_auto_activating_base_over_the_restored_env() {
        // The restored shell re-runs the user's rc files, and conda's init hook
        // activates base by default. Without this the pane comes back on base
        // no matter what was handed in — including for a venv, which base
        // shadows on PATH.
        for kind in [VirtualEnvKind::Conda, VirtualEnvKind::Venv] {
            let activation = VirtualEnvActivation {
                kind,
                prefix: "/work/api/.venv".into(),
                name: None,
            };

            let env = activation.launch_env(Some("/usr/bin"));

            assert!(env.contains(&("CONDA_AUTO_ACTIVATE".to_string(), "false".to_string())));
            assert!(env.contains(&("CONDA_AUTO_ACTIVATE_BASE".to_string(), "false".to_string())));
        }
    }

    #[test]
    fn launch_env_pins_conda_nesting_depth_to_a_single_activation() {
        let activation = VirtualEnvActivation {
            kind: VirtualEnvKind::Conda,
            prefix: "/opt/conda/envs/web".into(),
            name: Some("web".to_string()),
        };

        let env = activation.launch_env(None);

        assert!(env.contains(&("CONDA_SHLVL".to_string(), "1".to_string())));
        assert!(env.contains(&("CONDA_DEFAULT_ENV".to_string(), "web".to_string())));
    }

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
