use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    ffi::{c_void, OsStr},
    mem::{size_of, MaybeUninit},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    ptr::{copy_nonoverlapping, null_mut},
    sync::{
        atomic::{AtomicU64, Ordering as AtomicOrdering},
        Arc, LazyLock, Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod clipboard_image;

use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
    Win32::{
        Foundation::{
            CloseHandle, GlobalFree, LocalFree, FILETIME, HANDLE, HWND, INVALID_HANDLE_VALUE,
            NTSTATUS, STATUS_SUCCESS, UNICODE_STRING,
        },
        Globalization::{CompareStringOrdinal, CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::CreateDirectoryW,
        System::{
            Console::GetConsoleWindow,
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard,
                RegisterClipboardFormatW, SetClipboardData,
            },
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First,
                    Thread32Next, PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD,
                    THREADENTRY32,
                },
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Memory::{
                GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, VirtualQueryEx, GMEM_MOVEABLE,
                MEMORY_BASIC_INFORMATION,
            },
            Ole::{CF_DIB, CF_DIBV5, CF_UNICODETEXT},
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess, OpenThread,
                QueryFullProcessImageNameW, ResumeThread, TerminateProcess, CREATE_NO_WINDOW,
                CREATE_SUSPENDED, DETACHED_PROCESS, PROCESS_BASIC_INFORMATION,
                PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
                THREAD_SUSPEND_RESUME,
            },
        },
        UI::{
            Input::{
                Ime::ImmGetDefaultIMEWnd,
                KeyboardAndMouse::{
                    GetKeyboardLayout, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                    KEYEVENTF_KEYUP,
                },
            },
            Shell::{
                CommandLineToArgvW, ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_TIP,
                NIIF_INFO, NIIF_NOSOUND, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, GetForegroundWindow, GetWindowThreadProcessId,
                LoadIconW, SendMessageTimeoutW, IDI_APPLICATION, SMTO_ABORTIFHUNG, WM_IME_CONTROL,
            },
        },
    },
};

use super::{ClipboardImage, ForegroundJob, Signal};

const STILL_ACTIVE: u32 = 259;
const FOREGROUND_PROCESS_SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(250);
const FOREGROUND_SELECTION_RECHECK: Duration = Duration::from_secs(5);
const FOREGROUND_SELECTION_CACHE_CAPACITY: usize = 1_024;
const FOREGROUND_SELECTION_CACHE_RETENTION: Duration = Duration::from_secs(60);
const PANE_RUNTIME_MARKER_ENV_VAR: &str = "HERDR_PANE_RUNTIME_ID";

pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title.strip_prefix("Administrator: ").unwrap_or(title)
}

const MAX_PROCESS_ENVIRONMENT_BYTES: usize = 256 * 1024;
const PROCESS_ENVIRONMENT_READ_CHUNK_BYTES: usize = 16 * 1024;
const PROCESS_RUNTIME_MARKER_CACHE_CAPACITY: usize = 1_024;
const PROCESS_RUNTIME_MARKER_CACHE_RETENTION: Duration = Duration::from_secs(60);
const PROCESS_RUNTIME_MARKER_NEGATIVE_TTL: Duration = Duration::from_secs(1);

static NEXT_PANE_RUNTIME_MARKER: AtomicU64 = AtomicU64::new(1);
static PROCESS_RUNTIME_MARKER_CACHE: LazyLock<Mutex<HashMap<u32, CachedProcessRuntimeMarker>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static GIT_BASH_PROCESS_CACHE: LazyLock<Mutex<HashMap<u32, CachedGitBashProcess>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn remote_ssh_config_paths() -> super::RemoteSshConfigPaths {
    super::RemoteSshConfigPaths {
        user_config: std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .map(|home| home.join(".ssh").join("config")),
        system_config: std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("ssh").join("ssh_config")),
        multiplexing: false,
    }
}

pub(crate) fn create_remote_ssh_config_dir(_control_socket_name: &str) -> std::io::Result<PathBuf> {
    let base = remote_private_temp_base();
    std::fs::create_dir_all(&base)?;
    for attempt in 0..100 {
        let dir = base.join(format!("ssh-{}-{attempt}", std::process::id()));
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
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

pub(crate) fn create_remote_private_dir(path: &std::path::Path) -> std::io::Result<()> {
    use interprocess::os::windows::security_descriptor::{
        AsSecurityDescriptorExt as _, SecurityDescriptor,
    };
    use widestring::U16CString;

    let sddl = U16CString::from_str("D:P(A;OICI;GA;;;SY)(A;OICI;GA;;;OW)")
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err))?;
    let security_descriptor = SecurityDescriptor::deserialize(&sddl)?;
    let mut security_attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 0,
    };
    security_descriptor.write_to_security_attributes(&mut security_attributes);
    let path = extended_length_path(path)?;
    if unsafe { CreateDirectoryW(path.as_ptr(), &security_attributes) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn extended_length_path(path: &std::path::Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt as _;

    let path = std::path::absolute(path)?;
    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut extended = if wide.starts_with(&[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16])
        || wide.starts_with(&[b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16])
    {
        wide
    } else if wide.starts_with(&[b'\\' as u16, b'\\' as u16]) {
        "\\\\?\\UNC\\"
            .encode_utf16()
            .chain(wide.into_iter().skip(2))
            .collect()
    } else {
        "\\\\?\\".encode_utf16().chain(wide).collect()
    };
    extended.push(0);
    Ok(extended)
}

pub(crate) fn remote_private_temp_base() -> PathBuf {
    crate::config::state_dir().join("remote")
}

pub(crate) fn remote_bridge_endpoint_path(_readable_name: &str, short_name: &str) -> PathBuf {
    remote_private_temp_base().join(short_name)
}

pub(crate) fn remote_reattach_program(program: &str) -> String {
    let path = std::env::current_exe()
        .ok()
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from(program));
    format!(
        "& {}",
        remote_reattach_argument(&path.display().to_string())
    )
}

pub(crate) fn remote_reattach_argument(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Encode native or targeted semantic Win32 input for a compatible ConPTY destination.
pub(crate) fn encode_windows_conpty_fallback(key: &crate::input::TerminalKey) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    let (virtual_key_code, virtual_scan_code, unicode, control_key_state) =
        if let Some(record) = key.windows_record() {
            (
                record.virtual_key_code,
                record.virtual_scan_code,
                record.unicode,
                record.control_key_state,
            )
        } else if key.code == KeyCode::Esc
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press
            && key.vt_bytes().is_none()
        {
            return Some(b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_".to_vec());
        } else if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT {
            (13, 28, 13, 16)
        } else {
            return None;
        };
    let key_down = key.kind != KeyEventKind::Release;
    let repeat_count = if key_down { key.repeat_count.max(1) } else { 1 };

    Some(
        format!(
            "\x1b[{virtual_key_code};{virtual_scan_code};{unicode};{};{control_key_state};{repeat_count}_",
            u8::from(key_down),
        )
        .into_bytes(),
    )
}

#[derive(Debug)]
struct CachedProcessSnapshot {
    built_at: Instant,
    snapshot: Arc<ProcessSnapshot>,
}

#[derive(Debug)]
struct ProcessSnapshotCache {
    cached: Option<CachedProcessSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessSignature {
    pid: u32,
    parent_pid: u32,
    name: String,
}

impl ProcessSignature {
    fn from_entry(entry: &WindowsProcessEntry) -> Self {
        Self {
            pid: entry.pid,
            parent_pid: entry.parent_pid,
            name: entry.name.clone(),
        }
    }

    fn matches(&self, entry: Option<&WindowsProcessEntry>) -> bool {
        entry.is_some_and(|entry| {
            self.pid == entry.pid && self.parent_pid == entry.parent_pid && self.name == entry.name
        })
    }
}

#[derive(Debug)]
enum ProcessIdentity {
    Handle(OwnedHandle),
    #[cfg(test)]
    Stub {
        running: bool,
        creation_time: Option<u64>,
    },
}

impl ProcessIdentity {
    fn open(pid: u32) -> Option<Self> {
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle.cast()) };
        Some(Self::Handle(handle))
    }

    fn running(&self) -> bool {
        match self {
            Self::Handle(handle) => {
                let mut exit_code = 0;
                let read = unsafe {
                    GetExitCodeProcess(handle.as_raw_handle().cast(), &mut exit_code) != 0
                };
                read && exit_code == STILL_ACTIVE
            }
            #[cfg(test)]
            Self::Stub { running, .. } => *running,
        }
    }

    fn creation_time(&self) -> Option<u64> {
        match self {
            Self::Handle(handle) => process_creation_time(handle.as_raw_handle().cast()),
            #[cfg(test)]
            Self::Stub { creation_time, .. } => *creation_time,
        }
    }
}

#[derive(Debug)]
struct CachedForegroundSelection {
    shell: ProcessSignature,
    selected: ProcessSignature,
    descendants: Vec<ProcessSignature>,
    descendant_identities: Vec<ProcessIdentity>,
    shell_identity: ProcessIdentity,
    selected_identity: ProcessIdentity,
    job: ForegroundJob,
    verified_at: Instant,
    last_used: Instant,
}

#[derive(Debug, Default)]
struct ForegroundSelectionCache {
    entries: HashMap<u32, CachedForegroundSelection>,
}

#[derive(Debug)]
struct CachedProcessRuntimeMarker {
    creation_time: u64,
    marker: Option<String>,
    cached_at: Instant,
    last_used: Instant,
}

#[derive(Debug)]
struct CachedGitBashProcess {
    creation_time: u64,
    is_git_bash: bool,
    last_used: Instant,
}

static FOREGROUND_PROCESS_SNAPSHOT_CACHE: Mutex<ProcessSnapshotCache> =
    Mutex::new(ProcessSnapshotCache { cached: None });
static FOREGROUND_SELECTION_CACHE: LazyLock<Mutex<ForegroundSelectionCache>> =
    LazyLock::new(|| Mutex::new(ForegroundSelectionCache::default()));

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    true
}

/// The machine's node name, as shown by tmux's `#h`.
pub(crate) fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.is_empty())
}

pub(crate) fn local_datetime() -> Option<time::PrimitiveDateTime> {
    let mut timestamp: libc::time_t = 0;
    if unsafe { libc::time(&mut timestamp) } == -1 {
        return None;
    }
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_s(&mut local, &timestamp) } != 0 {
        return None;
    }
    let month = time::Month::try_from(u8::try_from(local.tm_mon + 1).ok()?).ok()?;
    let date = time::Date::from_calendar_date(
        local.tm_year + 1900,
        month,
        u8::try_from(local.tm_mday).ok()?,
    )
    .ok()?;
    let time = time::Time::from_hms(
        u8::try_from(local.tm_hour).ok()?,
        u8::try_from(local.tm_min).ok()?,
        u8::try_from(local.tm_sec).ok()?,
    )
    .ok()?;
    Some(time::PrimitiveDateTime::new(date, time))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsProcessCommand {
    creation_time: Option<u64>,
    argv0: Option<String>,
    argv: Option<Vec<String>>,
    cmdline: Option<String>,
}

#[derive(Debug, Clone)]
struct WindowsProcessEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
    command: OnceLock<WindowsProcessCommand>,
}

impl WindowsProcessEntry {
    fn command(&self) -> &WindowsProcessCommand {
        self.command
            .get_or_init(|| read_process_command(self.pid, &self.name))
    }
}

#[derive(Debug)]
struct ProcessSnapshot {
    entries: Vec<WindowsProcessEntry>,
    entry_by_pid: HashMap<u32, usize>,
    children_by_parent: HashMap<u32, Vec<usize>>,
    agent_indices: OnceLock<Vec<usize>>,
}

impl ProcessSnapshot {
    fn new(entries: Vec<WindowsProcessEntry>) -> Self {
        let mut entry_by_pid = HashMap::with_capacity(entries.len());
        let mut children_by_parent = HashMap::<u32, Vec<usize>>::new();
        for (index, entry) in entries.iter().enumerate() {
            entry_by_pid.insert(entry.pid, index);
            children_by_parent
                .entry(entry.parent_pid)
                .or_default()
                .push(index);
        }
        Self {
            entries,
            entry_by_pid,
            children_by_parent,
            agent_indices: OnceLock::new(),
        }
    }

    fn entry(&self, pid: u32) -> Option<&WindowsProcessEntry> {
        self.entry_by_pid
            .get(&pid)
            .map(|&index| &self.entries[index])
    }

    fn descendant_signatures(&self, root_pid: u32) -> Vec<ProcessSignature> {
        let mut signatures = descendant_entries(root_pid, self)
            .into_iter()
            .map(ProcessSignature::from_entry)
            .collect::<Vec<_>>();
        signatures.sort_unstable_by_key(|entry| entry.pid);
        signatures
    }

    fn agent_indices(&self) -> &[usize] {
        self.agent_indices.get_or_init(|| {
            self.entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| process_entry_identifies_agent(entry).then_some(index))
                .collect()
        })
    }
}

pub fn raise_server_nofile_limit() {}

pub(crate) fn apply_pane_runtime_marker_platform(command: &mut portable_pty::CommandBuilder) {
    if command_uses_git_bash(command) {
        command.env(PANE_RUNTIME_MARKER_ENV_VAR, next_pane_runtime_marker());
    }
}

fn next_pane_runtime_marker() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = NEXT_PANE_RUNTIME_MARKER.fetch_add(1, AtomicOrdering::Relaxed);
    format!("{:x}-{timestamp:x}-{counter:x}", std::process::id())
}

fn raw_command_shell(comspec: Option<std::ffi::OsString>) -> std::ffi::OsString {
    comspec
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into())
}

pub(crate) fn interactive_shell_command(argv: &[String], shell_name: &str) -> Option<String> {
    let shell_name = shell_name.to_ascii_lowercase();
    let powershell = shell_name.contains("powershell") || shell_name.contains("pwsh");
    let script = powershell_agent_script(argv)?;
    if powershell {
        Some(script)
    } else {
        Some(cmd_encoded_powershell_command(&script))
    }
}

fn powershell_agent_script(argv: &[String]) -> Option<String> {
    let (program, args) = argv.split_first()?;
    if args.is_empty() {
        return Some(format!("& {}", super::quote_powershell_arg(program)));
    }

    let command_line = args
        .iter()
        .map(|arg| quote_windows_command_line_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!(
        "$p=Start-Process -FilePath {} -ArgumentList {} -NoNewWindow -Wait -PassThru",
        super::quote_powershell_arg(program),
        super::quote_powershell_arg(&command_line),
    ))
}

fn quote_windows_command_line_arg(value: &str) -> String {
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

fn cmd_encoded_powershell_command(script: &str) -> String {
    use base64::Engine as _;

    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    format!("powershell.exe -NoLogo -NoProfile -EncodedCommand {encoded}")
}

pub(crate) fn detached_custom_command_process_platform(command: &str) -> std::process::Command {
    detached_custom_command_process_with_comspec(command, std::env::var_os("ComSpec"))
}

pub(crate) fn status_commands_supported() -> bool {
    true
}

pub(crate) fn configure_status_command(process: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    // The process must not run before it is assigned to the kill-on-close job.
    process.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
}

pub(crate) struct StatusCommandGuard {
    job: usize,
}

impl StatusCommandGuard {
    pub(crate) fn new(child: &tokio::process::Child) -> std::io::Result<Self> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }

        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limits_size = match u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()) {
            Ok(size) => size,
            Err(_) => {
                unsafe {
                    CloseHandle(job);
                }
                return Err(std::io::Error::other("job limits size exceeds u32"));
            }
        };
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                limits_size,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }

        let Some(process) = child.raw_handle() else {
            unsafe {
                CloseHandle(job);
            }
            return Err(std::io::Error::other(
                "status command has no process handle",
            ));
        };
        if unsafe { AssignProcessToJobObject(job, process.cast()) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }
        if let Err(error) = resume_suspended_process(child.id()) {
            unsafe {
                CloseHandle(job);
            }
            return Err(error);
        }

        Ok(Self { job: job as usize })
    }
}

fn resume_suspended_process(process_id: Option<u32>) -> std::io::Result<()> {
    let process_id =
        process_id.ok_or_else(|| std::io::Error::other("status command has no process id"))?;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }

    let result = (|| {
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = u32::try_from(size_of::<THREADENTRY32>())
            .map_err(|_| std::io::Error::other("thread entry size exceeds u32"))?;
        if unsafe { Thread32First(snapshot, &mut entry) } == 0 {
            return Err(std::io::Error::last_os_error());
        }

        loop {
            if entry.th32OwnerProcessID == process_id {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let resume_result = unsafe { ResumeThread(thread) };
                let resume_error = (resume_result == u32::MAX).then(std::io::Error::last_os_error);
                unsafe {
                    CloseHandle(thread);
                }
                if let Some(error) = resume_error {
                    return Err(error);
                }
                return Ok(());
            }
            if unsafe { Thread32Next(snapshot, &mut entry) } == 0 {
                return Err(std::io::Error::other(
                    "status command primary thread was not found",
                ));
            }
        }
    })();

    unsafe {
        CloseHandle(snapshot);
    }
    result
}

impl StatusCommandGuard {
    pub(crate) fn terminate(&mut self) {
        if self.job != 0 {
            // KILL_ON_JOB_CLOSE terminates the shell and every descendant still in
            // the job, including on task cancellation and config reload.
            unsafe {
                CloseHandle(self.job as HANDLE);
            }
            self.job = 0;
        }
    }
}

impl Drop for StatusCommandGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn detached_custom_command_process_with_comspec(
    command: &str,
    comspec: Option<std::ffi::OsString>,
) -> std::process::Command {
    use std::os::windows::process::CommandExt;

    let mut process = std::process::Command::new(raw_command_shell(comspec));
    process.arg("/d").arg("/c").raw_arg(command);
    process
}

pub(crate) fn pane_custom_command_pty_builder_platform(
    command: &str,
) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_with_comspec(command, std::env::var_os("ComSpec"))
}

fn pane_custom_command_pty_builder_with_comspec(
    command: &str,
    comspec: Option<std::ffi::OsString>,
) -> portable_pty::CommandBuilder {
    let mut builder = portable_pty::CommandBuilder::new(raw_command_shell(comspec));
    builder.arg("/d");
    builder.arg("/c");
    builder.raw_arg(command);
    builder
}

pub(crate) fn scrollback_editor_argv(path: &std::path::Path) -> std::io::Result<Vec<String>> {
    let editor = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    scrollback_editor_argv_with_env(path, editor.as_deref())
}

fn scrollback_editor_argv_with_env(
    path: &std::path::Path,
    editor: Option<&str>,
) -> std::io::Result<Vec<String>> {
    let mut argv = match editor.filter(|value| !value.trim().is_empty()) {
        Some(editor) => command_line_to_argv(editor).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("failed to parse editor command {editor:?}"),
            )
        })?,
        None => vec!["notepad.exe".to_string()],
    };
    if argv.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "editor command must not be empty",
        ));
    }
    argv.push(path.display().to_string());
    Ok(argv)
}

pub(crate) fn configure_background_command_platform(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(CREATE_NO_WINDOW);
}

pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    if current_job_kills_processes_on_close()? {
        launch_server_daemon_with_wmi(command)
    } else {
        command.spawn().map(|child| child.id())
    }
}

fn launch_server_daemon_with_wmi(command: &std::process::Command) -> std::io::Result<u32> {
    // WMI resolves the class from this Rust type name, including CIM casing.
    #[allow(non_camel_case_types)]
    #[derive(serde::Deserialize)]
    struct Win32_Process;

    // WMI serializes this embedded object using the matching CIM class name.
    #[allow(non_camel_case_types)]
    #[derive(serde::Serialize)]
    struct Win32_ProcessStartup {
        #[serde(rename = "CreateFlags")]
        create_flags: u32,
        #[serde(rename = "EnvironmentVariables")]
        environment_variables: Vec<String>,
    }

    #[derive(serde::Serialize)]
    struct CreateInput {
        #[serde(rename = "CommandLine")]
        command_line: String,
        #[serde(rename = "CurrentDirectory")]
        current_directory: String,
        #[serde(rename = "ProcessStartupInformation")]
        process_startup_information: Win32_ProcessStartup,
    }

    #[derive(serde::Deserialize)]
    struct CreateOutput {
        #[serde(rename = "ProcessId")]
        process_id: Option<u32>,
        #[serde(rename = "ReturnValue")]
        return_value: u32,
    }

    let current_directory = command
        .get_current_dir()
        .map(std::path::Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)?;
    let input = CreateInput {
        command_line: windows_command_line(command)?,
        current_directory: unicode_windows_value(
            &current_directory.into_os_string(),
            "working directory",
        )?,
        process_startup_information: Win32_ProcessStartup {
            create_flags: DETACHED_PROCESS,
            environment_variables: effective_command_environment(command)?,
        },
    };

    let connection = wmi::WMIConnection::new()
        .map_err(|err| std::io::Error::other(format!("failed to connect to WMI: {err}")))?;
    let output: CreateOutput = connection
        .exec_class_method::<Win32_Process, _>("Create", &input)
        .map_err(|err| std::io::Error::other(format!("WMI Win32_Process.Create failed: {err}")))?;
    if output.return_value != 0 {
        return Err(std::io::Error::other(format!(
            "WMI Win32_Process.Create returned error {}",
            output.return_value
        )));
    }
    output.process_id.ok_or_else(|| {
        std::io::Error::other("WMI Win32_Process.Create succeeded without a process id")
    })
}

fn windows_command_line(command: &std::process::Command) -> std::io::Result<String> {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|value| {
            unicode_windows_value(value, "server command argument")
                .map(|value| quote_windows_command_line_arg(&value))
        })
        .collect::<std::io::Result<Vec<_>>>()
        .map(|parts| parts.join(" "))
}

fn effective_command_environment(command: &std::process::Command) -> std::io::Result<Vec<String>> {
    let mut environment = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                unicode_windows_value(&key, "inherited environment variable name")?,
                unicode_windows_value(&value, "inherited environment variable value")?,
            ))
        })
        .collect::<std::io::Result<Vec<(String, String)>>>()?;
    for (key, value) in command.get_envs() {
        let key = unicode_windows_value(key, "environment variable name")?;
        environment.retain(|(inherited, _)| windows_environment_key_cmp(inherited, &key).is_ne());
        if let Some(value) = value {
            environment.push((
                key,
                unicode_windows_value(value, "environment variable value")?,
            ));
        }
    }
    environment.sort_unstable_by(|(left, _), (right, _)| windows_environment_key_cmp(left, right));
    Ok(environment
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect())
}

fn windows_environment_key_cmp(left: &str, right: &str) -> Ordering {
    let left_wide: Vec<u16> = left.encode_utf16().collect();
    let right_wide: Vec<u16> = right.encode_utf16().collect();
    // SAFETY: both pointers remain valid for the call and lengths count UTF-16 units.
    match unsafe {
        CompareStringOrdinal(
            left_wide.as_ptr(),
            left_wide.len() as i32,
            right_wide.as_ptr(),
            right_wide.len() as i32,
            1,
        )
    } {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        _ => left.cmp(right),
    }
}

fn unicode_windows_value(value: &OsStr, label: &str) -> std::io::Result<String> {
    value.to_str().map(str::to_owned).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} is not valid Unicode"),
        )
    })
}

fn current_process_is_in_job() -> std::io::Result<bool> {
    let mut in_job = 0;
    // SAFETY: `in_job` is a valid writable BOOL for the duration of the call.
    if unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(in_job != 0)
}

fn current_job_kills_processes_on_close() -> std::io::Result<bool> {
    if !current_process_is_in_job()? {
        return Ok(false);
    }

    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: `limits` is writable and its exact buffer size is supplied.
    if unsafe {
        QueryInformationJobObject(
            null_mut(),
            JobObjectExtendedLimitInformation,
            &mut limits as *mut _ as *mut c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(limits.BasicLimitInformation.LimitFlags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0)
}

pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(DETACHED_PROCESS);
}

pub fn current_process_is_detached_server_daemon() -> bool {
    if !unsafe { GetConsoleWindow() }.is_null() {
        return false;
    }

    matches!(current_process_is_in_job(), Ok(false))
}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    select_pane_foreground_job_cached(child_pid)
}

pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    let snapshot = ProcessSnapshot::new(snapshot_processes());
    available_pane_shell_from_snapshot(child_pid, &snapshot)
}

fn available_pane_shell_from_snapshot(
    child_pid: u32,
    snapshot: &ProcessSnapshot,
) -> Option<String> {
    let shell = snapshot.entry(child_pid)?;
    if !super::is_pane_shell_process_name(&shell.name) {
        return None;
    }
    descendant_entries(child_pid, snapshot)
        .is_empty()
        .then(|| shell.name.clone())
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let snapshot = cached_foreground_processes();
    let entry = snapshot.entry(process_group_id)?;
    Some(ForegroundJob {
        process_group_id,
        processes: vec![foreground_process_from_entry(entry)],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    select_pane_foreground_job_cached(child_pid).map(|job| job.process_group_id)
}

pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let process_parameters = read_process_parameters(process.0)?;
    read_unicode_string(process.0, process_parameters.current_directory.dos_path)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn select_pane_foreground_job_cached(shell_pid: u32) -> Option<ForegroundJob> {
    let snapshot = cached_foreground_processes();
    let (job, retry_with_fresh_snapshot) =
        select_pane_foreground_job_from_snapshot(shell_pid, &snapshot)?;
    if !retry_with_fresh_snapshot {
        return Some(job);
    }

    let snapshot = fresh_foreground_processes();
    select_pane_foreground_job_from_snapshot(shell_pid, &snapshot).map(|(job, _)| job)
}

fn select_pane_foreground_job_from_snapshot(
    shell_pid: u32,
    snapshot: &ProcessSnapshot,
) -> Option<(ForegroundJob, bool)> {
    if let Some(job) = FOREGROUND_SELECTION_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .get(shell_pid, snapshot)
    {
        return Some((job, false));
    }

    let job = select_pane_foreground_job_from_snapshot_uncached(shell_pid, snapshot)?;
    let cached = prepare_cached_foreground_selection(shell_pid, snapshot, &job);
    let retry_with_fresh_snapshot = job.process_group_id != shell_pid && cached.is_none();
    FOREGROUND_SELECTION_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .remember(shell_pid, cached);
    Some((job, retry_with_fresh_snapshot))
}

fn select_pane_foreground_job_from_snapshot_uncached(
    shell_pid: u32,
    snapshot: &ProcessSnapshot,
) -> Option<ForegroundJob> {
    select_pane_foreground_job_from_snapshot_with_runtime_inspection(
        shell_pid,
        snapshot,
        |shell| process_is_git_bash(shell.pid),
        |entry| process_runtime_marker(entry.pid),
    )
}

fn select_pane_foreground_job_from_snapshot_with_runtime_inspection(
    shell_pid: u32,
    snapshot: &ProcessSnapshot,
    shell_is_git_bash: impl FnOnce(&WindowsProcessEntry) -> bool,
    mut runtime_marker: impl FnMut(&WindowsProcessEntry) -> Option<String>,
) -> Option<ForegroundJob> {
    let entries = &snapshot.entries;
    let shell = snapshot.entry(shell_pid)?;
    let descendants = descendant_entries(shell_pid, snapshot);
    let mut candidates = Vec::new();
    for entry in std::iter::once(shell).chain(descendants) {
        if process_entry_identifies_agent(entry) {
            candidates.push(entry);
        }
    }

    if let Some(selected) = select_topmost_agent_chain_candidate(&candidates, snapshot) {
        return Some(foreground_job_from_entry(selected));
    }
    if !candidates.is_empty() || !shell_is_git_bash(shell) {
        return Some(foreground_job_from_entry(shell));
    }

    let escaped_agent_indices = snapshot.agent_indices();
    if escaped_agent_indices.is_empty() {
        return Some(foreground_job_from_entry(shell));
    }

    let Some(shell_runtime_marker) = runtime_marker(shell).filter(|marker| !marker.is_empty())
    else {
        return Some(foreground_job_from_entry(shell));
    };
    let matching_candidates: Vec<_> = escaped_agent_indices
        .iter()
        .map(|&index| &entries[index])
        .filter(|entry| runtime_marker(entry).as_deref() == Some(shell_runtime_marker.as_str()))
        .collect();
    let selected =
        select_topmost_agent_chain_candidate(&matching_candidates, snapshot).unwrap_or(shell);
    Some(foreground_job_from_entry(selected))
}

#[cfg(test)]
fn select_pane_foreground_job(
    shell_pid: u32,
    entries: &[WindowsProcessEntry],
) -> Option<ForegroundJob> {
    select_pane_foreground_job_from_snapshot_uncached(
        shell_pid,
        &ProcessSnapshot::new(entries.to_vec()),
    )
}

fn process_entry_identifies_agent(entry: &WindowsProcessEntry) -> bool {
    crate::detect::identify_agent(&entry.name).is_some()
        || crate::detect::identify_agent_in_job(&foreground_job_from_entry(entry)).is_some()
}

fn foreground_job_from_entry(entry: &WindowsProcessEntry) -> ForegroundJob {
    ForegroundJob {
        process_group_id: entry.pid,
        processes: vec![foreground_process_from_entry(entry)],
    }
}

fn select_topmost_agent_chain_candidate<'a>(
    candidates: &[&'a WindowsProcessEntry],
    snapshot: &ProcessSnapshot,
) -> Option<&'a WindowsProcessEntry> {
    if candidates.is_empty() {
        return None;
    }

    candidates.iter().copied().find(|entry| {
        candidates.iter().all(|other| {
            entry.pid == other.pid || process_is_ancestor(entry.pid, other.pid, snapshot)
        })
    })
}

fn process_is_ancestor(ancestor_pid: u32, descendant_pid: u32, snapshot: &ProcessSnapshot) -> bool {
    let mut current = descendant_pid;
    let mut visited = HashSet::new();
    while visited.insert(current) {
        let Some(parent) = snapshot.entry(current).map(|entry| entry.parent_pid) else {
            return false;
        };
        if parent == ancestor_pid {
            return true;
        }
        if parent == 0 {
            return false;
        }
        current = parent;
    }

    false
}

fn descendant_entries(root_pid: u32, snapshot: &ProcessSnapshot) -> Vec<&WindowsProcessEntry> {
    let mut output = Vec::new();
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    visited.insert(root_pid);
    if let Some(root_children) = snapshot.children_by_parent.get(&root_pid) {
        for &index in root_children {
            let entry = &snapshot.entries[index];
            if visited.insert(entry.pid) {
                queue.push_back(entry);
            }
        }
    }
    while let Some(entry) = queue.pop_front() {
        output.push(entry);
        if let Some(next) = snapshot.children_by_parent.get(&entry.pid) {
            for &index in next {
                let child = &snapshot.entries[index];
                if visited.insert(child.pid) {
                    queue.push_back(child);
                }
            }
        }
    }
    output
}

fn foreground_process_from_entry(entry: &WindowsProcessEntry) -> super::ForegroundProcess {
    let command = entry.command();
    super::ForegroundProcess {
        pid: entry.pid,
        name: entry.name.clone(),
        argv0: command.argv0.clone(),
        argv: command.argv.clone(),
        cmdline: command.cmdline.clone(),
    }
}

fn snapshot_processes() -> Vec<WindowsProcessEntry> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let _snapshot = ProcessHandle(snapshot);

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut output = Vec::new();
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while ok {
        let pid = entry.th32ProcessID;
        let name = nul_terminated_utf16_to_string(&entry.szExeFile);
        output.push(WindowsProcessEntry {
            pid,
            parent_pid: entry.th32ParentProcessID,
            name,
            command: OnceLock::new(),
        });
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    output
}

fn cached_foreground_processes() -> Arc<ProcessSnapshot> {
    let mut cache = FOREGROUND_PROCESS_SNAPSHOT_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    cache.snapshot(FOREGROUND_PROCESS_SNAPSHOT_CACHE_TTL, snapshot_processes)
}

fn fresh_foreground_processes() -> Arc<ProcessSnapshot> {
    let mut cache = FOREGROUND_PROCESS_SNAPSHOT_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    cache.snapshot(Duration::ZERO, snapshot_processes)
}

fn prepare_cached_foreground_selection(
    shell_pid: u32,
    snapshot: &ProcessSnapshot,
    job: &ForegroundJob,
) -> Option<CachedForegroundSelection> {
    if job.process_group_id == shell_pid {
        return None;
    }
    let shell_identity = ProcessIdentity::open(shell_pid)?;
    let selected_identity = ProcessIdentity::open(job.process_group_id)?;
    let descendants = snapshot.descendant_signatures(shell_pid);
    let descendant_identities = descendants
        .iter()
        .map(|entry| ProcessIdentity::open(entry.pid))
        .collect::<Option<Vec<_>>>()?;
    CachedForegroundSelection::from_snapshot_with_identities(
        shell_pid,
        snapshot,
        job,
        descendants,
        descendant_identities,
        shell_identity,
        selected_identity,
    )
}

impl CachedForegroundSelection {
    fn from_snapshot_with_identities(
        shell_pid: u32,
        snapshot: &ProcessSnapshot,
        job: &ForegroundJob,
        descendants: Vec<ProcessSignature>,
        descendant_identities: Vec<ProcessIdentity>,
        shell_identity: ProcessIdentity,
        selected_identity: ProcessIdentity,
    ) -> Option<Self> {
        if job.process_group_id == shell_pid {
            return None;
        }
        let shell_entry = snapshot.entry(shell_pid)?;
        if shell_identity.creation_time() != shell_entry.command().creation_time {
            return None;
        }
        let shell = ProcessSignature::from_entry(shell_entry);
        let selected_entry = snapshot.entry(job.process_group_id)?;
        if selected_identity.creation_time() != selected_entry.command().creation_time {
            return None;
        }
        let selected = ProcessSignature::from_entry(selected_entry);
        let descendants_match_identities = descendants.len() == descendant_identities.len()
            && descendants
                .iter()
                .zip(&descendant_identities)
                .all(|(signature, identity)| {
                    snapshot.entry(signature.pid).is_some_and(|entry| {
                        identity.creation_time() == entry.command().creation_time
                    })
                });
        if !descendants_match_identities
            || !shell_identity.running()
            || !selected_identity.running()
            || !descendant_identities.iter().all(ProcessIdentity::running)
        {
            return None;
        }
        let now = Instant::now();
        Some(Self {
            shell,
            selected,
            descendants,
            descendant_identities,
            shell_identity,
            selected_identity,
            job: job.clone(),
            verified_at: now,
            last_used: now,
        })
    }
}

impl ForegroundSelectionCache {
    fn get(&mut self, shell_pid: u32, snapshot: &ProcessSnapshot) -> Option<ForegroundJob> {
        if let Some(cached) = self.entries.get_mut(&shell_pid) {
            let current_descendants = descendant_entries(shell_pid, snapshot);
            let topology_matches = current_descendants.len() == cached.descendants.len()
                && cached
                    .descendants
                    .iter()
                    .all(|entry| entry.matches(snapshot.entry(entry.pid)));
            let valid = cached.verified_at.elapsed() < FOREGROUND_SELECTION_RECHECK
                && cached.shell_identity.running()
                && cached.selected_identity.running()
                && cached
                    .descendant_identities
                    .iter()
                    .all(ProcessIdentity::running)
                && cached.shell.matches(snapshot.entry(shell_pid))
                && cached
                    .selected
                    .matches(snapshot.entry(cached.job.process_group_id))
                && topology_matches;
            if valid {
                cached.last_used = Instant::now();
                return Some(cached.job.clone());
            }
        }
        self.entries.remove(&shell_pid);
        None
    }

    fn remember(&mut self, shell_pid: u32, cached: Option<CachedForegroundSelection>) {
        let Some(cached) = cached else {
            self.entries.remove(&shell_pid);
            return;
        };
        self.entries
            .retain(|_, cached| cached.last_used.elapsed() < FOREGROUND_SELECTION_CACHE_RETENTION);
        if self.entries.len() >= FOREGROUND_SELECTION_CACHE_CAPACITY {
            self.entries.clear();
        }
        self.entries.insert(shell_pid, cached);
    }

    #[cfg(test)]
    fn remember_for_test(
        &mut self,
        shell_pid: u32,
        snapshot: &ProcessSnapshot,
        job: &ForegroundJob,
    ) {
        let descendants = snapshot.descendant_signatures(shell_pid);
        let descendant_identities = descendants
            .iter()
            .map(|_| ProcessIdentity::Stub {
                running: true,
                creation_time: None,
            })
            .collect();
        let cached = CachedForegroundSelection::from_snapshot_with_identities(
            shell_pid,
            snapshot,
            job,
            descendants,
            descendant_identities,
            ProcessIdentity::Stub {
                running: true,
                creation_time: None,
            },
            ProcessIdentity::Stub {
                running: true,
                creation_time: None,
            },
        );
        self.remember(shell_pid, cached);
    }
}

impl ProcessSnapshotCache {
    fn snapshot(
        &mut self,
        max_age: Duration,
        build: impl FnOnce() -> Vec<WindowsProcessEntry>,
    ) -> Arc<ProcessSnapshot> {
        if let Some(cached) = &self.cached {
            if cached.built_at.elapsed() < max_age {
                return Arc::clone(&cached.snapshot);
            }
        }

        let snapshot = Arc::new(ProcessSnapshot::new(build()));
        self.cached = Some(CachedProcessSnapshot {
            built_at: Instant::now(),
            snapshot: Arc::clone(&snapshot),
        });
        snapshot
    }
}

fn read_process_command(pid: u32, name: &str) -> WindowsProcessCommand {
    let Some(process) =
        ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)
    else {
        let creation_time = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)
            .and_then(|process| process_creation_time(process.0));
        return WindowsProcessCommand::from_cmdline(name, creation_time, None);
    };
    let creation_time = process_creation_time(process.0);
    let cmdline = read_process_parameters(process.0)
        .and_then(|parameters| read_unicode_string(process.0, parameters.command_line));
    WindowsProcessCommand::from_cmdline(name, creation_time, cmdline)
}

impl WindowsProcessCommand {
    fn from_cmdline(name: &str, creation_time: Option<u64>, cmdline: Option<String>) -> Self {
        let argv = cmdline.as_deref().and_then(command_line_to_argv);
        let argv0 = argv
            .as_ref()
            .and_then(|argv| argv.first().cloned())
            .or_else(|| (!name.is_empty()).then(|| name.to_string()));
        Self {
            creation_time,
            argv0,
            argv,
            cmdline,
        }
    }
}

fn process_is_git_bash(pid: u32) -> bool {
    let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };
    let Some(creation_time) = process_creation_time(process.0) else {
        return false;
    };
    {
        let mut cache = GIT_BASH_PROCESS_CACHE
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.get_mut(&pid) {
            if cached.creation_time == creation_time {
                cached.last_used = Instant::now();
                return cached.is_git_bash;
            }
        }
    }

    let is_git_bash = process_executable_path(process.0)
        .as_deref()
        .is_some_and(|path| is_git_bash_executable_path(std::path::Path::new(path)));
    let mut cache = GIT_BASH_PROCESS_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if cache.len() >= PROCESS_RUNTIME_MARKER_CACHE_CAPACITY {
        cache.retain(|_, cached| {
            cached.last_used.elapsed() < PROCESS_RUNTIME_MARKER_CACHE_RETENTION
        });
        if cache.len() >= PROCESS_RUNTIME_MARKER_CACHE_CAPACITY {
            cache.clear();
        }
    }
    cache.insert(
        pid,
        CachedGitBashProcess {
            creation_time,
            is_git_bash,
            last_used: Instant::now(),
        },
    );
    is_git_bash
}

fn process_executable_path(process: HANDLE) -> Option<String> {
    let mut path = vec![0_u16; 32_768];
    let mut len = path.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    String::from_utf16(&path[..len as usize]).ok()
}

fn command_uses_git_bash(command: &portable_pty::CommandBuilder) -> bool {
    let Some(program) = command.get_argv().first() else {
        return false;
    };
    let path = std::path::Path::new(program);
    if path.is_absolute() {
        return is_git_bash_executable_path(path);
    }
    if program.to_string_lossy().contains(['/', '\\']) {
        return false;
    }

    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    let candidate_name = if file_name.eq_ignore_ascii_case("bash") {
        "bash.exe"
    } else if file_name.eq_ignore_ascii_case("bash.exe") {
        file_name
    } else {
        return false;
    };
    let search_path = command
        .get_env("PATH")
        .map(OsStr::to_os_string)
        .or_else(|| std::env::var_os("PATH"));
    search_path.is_some_and(|search_path| {
        std::env::split_paths(&search_path)
            .map(|directory| directory.join(candidate_name))
            .find(|candidate| candidate.is_file())
            .is_some_and(|candidate| is_git_bash_executable_path(&candidate))
    })
}

fn is_git_bash_executable_path(path: &std::path::Path) -> bool {
    let Some(file_name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    if !file_name.eq_ignore_ascii_case("bash.exe") || !path.is_absolute() || !path.is_file() {
        return false;
    }

    let Some(bin_dir) = path.parent() else {
        return false;
    };
    if !bin_dir
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        return false;
    }

    let Some(mut root) = bin_dir.parent() else {
        return false;
    };
    if root
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("usr"))
    {
        let Some(parent) = root.parent() else {
            return false;
        };
        root = parent;
    }

    root.join("usr").join("bin").join("msys-2.0.dll").is_file()
        && root.join("cmd").join("git.exe").is_file()
}

fn process_runtime_marker(pid: u32) -> Option<String> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)?;
    let creation_time = process_creation_time(process.0)?;
    {
        let mut cache = PROCESS_RUNTIME_MARKER_CACHE
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.get_mut(&pid) {
            if cached.creation_time == creation_time
                && (cached.marker.is_some()
                    || cached.cached_at.elapsed() < PROCESS_RUNTIME_MARKER_NEGATIVE_TTL)
            {
                cached.last_used = Instant::now();
                return cached.marker.clone();
            }
        }
    }

    let marker = process_runtime_marker_from_handle(process.0)?;
    let mut cache = PROCESS_RUNTIME_MARKER_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if cache.len() >= PROCESS_RUNTIME_MARKER_CACHE_CAPACITY {
        cache.retain(|_, cached| {
            cached.last_used.elapsed() < PROCESS_RUNTIME_MARKER_CACHE_RETENTION
        });
        if cache.len() >= PROCESS_RUNTIME_MARKER_CACHE_CAPACITY {
            cache.clear();
        }
    }
    cache.insert(
        pid,
        CachedProcessRuntimeMarker {
            creation_time,
            marker: marker.clone(),
            cached_at: Instant::now(),
            last_used: Instant::now(),
        },
    );
    marker
}

fn process_creation_time(process: HANDLE) -> Option<u64> {
    let mut creation_time = FILETIME::default();
    let mut exit_time = FILETIME::default();
    let mut kernel_time = FILETIME::default();
    let mut user_time = FILETIME::default();
    if unsafe {
        GetProcessTimes(
            process,
            &mut creation_time,
            &mut exit_time,
            &mut kernel_time,
            &mut user_time,
        )
    } == 0
    {
        return None;
    }
    Some((u64::from(creation_time.dwHighDateTime) << 32) | u64::from(creation_time.dwLowDateTime))
}

fn process_runtime_marker_from_handle(process: HANDLE) -> Option<Option<String>> {
    let parameters = read_process_parameters(process)?;
    let environment = read_process_environment(process, parameters.environment)?;
    Some(environment_variable_from_utf16(
        &environment,
        PANE_RUNTIME_MARKER_ENV_VAR,
    ))
}

fn read_process_environment(process: HANDLE, address: *const c_void) -> Option<Vec<u16>> {
    if address.is_null() {
        return None;
    }

    let mut memory = MaybeUninit::<MEMORY_BASIC_INFORMATION>::uninit();
    let queried = unsafe {
        VirtualQueryEx(
            process,
            address,
            memory.as_mut_ptr(),
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if queried == 0 {
        return None;
    }
    let memory = unsafe { memory.assume_init() };
    let address = address as usize;
    let base = memory.BaseAddress as usize;
    let offset = address.checked_sub(base)?;
    let available = memory.RegionSize.checked_sub(offset)?;
    let read_len = available.min(MAX_PROCESS_ENVIRONMENT_BYTES);
    if read_len < size_of::<u16>() {
        return None;
    }

    let max_units = read_len / size_of::<u16>();
    let chunk_units = PROCESS_ENVIRONMENT_READ_CHUNK_BYTES / size_of::<u16>();
    let mut environment = Vec::new();
    while environment.len() < max_units {
        let unit_count = (max_units - environment.len()).min(chunk_units);
        let chunk_bytes = unit_count * size_of::<u16>();
        let mut chunk = vec![0_u16; unit_count];
        let mut bytes_read = 0;
        let offset = environment.len().checked_mul(size_of::<u16>())?;
        let chunk_address = address.checked_add(offset)?;
        if unsafe {
            ReadProcessMemory(
                process,
                chunk_address as *const c_void,
                chunk.as_mut_ptr().cast::<c_void>(),
                chunk_bytes,
                &mut bytes_read,
            )
        } == 0
        {
            break;
        }
        chunk.truncate(bytes_read / size_of::<u16>());
        if chunk.is_empty() {
            break;
        }
        environment.extend_from_slice(&chunk);
        if let Some(end) = environment
            .windows(2)
            .position(|pair| pair == [0, 0])
            .map(|index| index + 2)
        {
            environment.truncate(end);
            return Some(environment);
        }
        if bytes_read < chunk_bytes {
            break;
        }
    }
    None
}

fn environment_variable_from_utf16(environment: &[u16], name: &str) -> Option<String> {
    for variable in environment.split(|unit| *unit == 0) {
        if variable.is_empty() {
            break;
        }
        let Some(separator) = variable.iter().position(|unit| *unit == u16::from(b'=')) else {
            continue;
        };
        let Ok(variable_name) = String::from_utf16(&variable[..separator]) else {
            continue;
        };
        if variable_name.eq_ignore_ascii_case(name) {
            return String::from_utf16(&variable[separator + 1..]).ok();
        }
    }
    None
}

fn read_process_parameters(process: HANDLE) -> Option<RtlUserProcessParameters> {
    let mut basic_info = MaybeUninit::<PROCESS_BASIC_INFORMATION>::uninit();
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            ProcessBasicInformation,
            basic_info.as_mut_ptr().cast::<c_void>(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            null_mut(),
        )
    };
    if status != STATUS_SUCCESS as NTSTATUS {
        return None;
    }

    let basic_info = unsafe { basic_info.assume_init() };
    if basic_info.PebBaseAddress.is_null() {
        return None;
    }

    let peb = read_process_value::<Peb>(process, basic_info.PebBaseAddress.cast::<c_void>())?;
    if peb.process_parameters.is_null() {
        return None;
    }

    read_process_value::<RtlUserProcessParameters>(process, peb.process_parameters.cast())
}

fn command_line_to_argv(command_line: &str) -> Option<Vec<String>> {
    let wide: Vec<u16> = command_line
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut argc = 0;
    let argv_ptr = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
    if argv_ptr.is_null() || argc <= 0 {
        return None;
    }

    let argv_slice = unsafe { std::slice::from_raw_parts(argv_ptr, argc as usize) };
    let mut argv = Vec::with_capacity(argc as usize);
    for &arg in argv_slice {
        if arg.is_null() {
            continue;
        }
        let mut len = 0;
        unsafe {
            while *arg.add(len) != 0 {
                len += 1;
            }
            argv.push(String::from_utf16_lossy(std::slice::from_raw_parts(
                arg, len,
            )));
        }
    }
    unsafe {
        LocalFree(argv_ptr.cast());
    }
    Some(argv)
}

fn nul_terminated_utf16_to_string(buffer: &[u16]) -> String {
    let len = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    if child_pid == 0 {
        return Vec::new();
    }

    let snapshot = ProcessSnapshot::new(snapshot_processes());
    session_processes_from_snapshot(child_pid, &snapshot)
}

fn session_processes_from_snapshot(child_pid: u32, snapshot: &ProcessSnapshot) -> Vec<u32> {
    if snapshot.entry(child_pid).is_none() {
        return Vec::new();
    }

    let mut pids = vec![child_pid];
    pids.extend(
        descendant_entries(child_pid, snapshot)
            .into_iter()
            .map(|entry| entry.pid),
    );
    pids
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    if signal == Signal::Hangup {
        return;
    }

    for &pid in pids {
        let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
            continue;
        };
        unsafe {
            TerminateProcess(process.0, 1);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    let Some(process) = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return false;
    };

    let mut exit_code = 0;
    let ok = unsafe { GetExitCodeProcess(process.0, &mut exit_code) } != 0;
    ok && exit_code == STILL_ACTIVE
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    if text.contains('\0') {
        return false;
    }
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);
    let Some(byte_len) = utf16.len().checked_mul(size_of::<u16>()) else {
        return false;
    };

    unsafe {
        let owner = GetConsoleWindow();
        if owner.is_null() || OpenClipboard(owner) == 0 {
            return false;
        }
        let _clipboard = ClipboardGuard;

        if EmptyClipboard() == 0 {
            return false;
        }

        let memory = GlobalAlloc(GMEM_MOVEABLE, byte_len);
        if memory.is_null() {
            return false;
        }

        let locked = GlobalLock(memory);
        if locked.is_null() {
            GlobalFree(memory);
            return false;
        }
        copy_nonoverlapping(utf16.as_ptr(), locked.cast::<u16>(), utf16.len());
        GlobalUnlock(memory);

        if SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() {
            GlobalFree(memory);
            return false;
        }

        true
    }
}

pub fn read_clipboard_text() -> Option<String> {
    None
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    let operation = wide_null("open");
    let url = wide_null(url);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result as isize > 32 {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "failed to open URL with ShellExecuteW: code {}",
            result as isize
        )))
    }
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    for attempt in 0..10 {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            let _clipboard = ClipboardGuard;
            if let Some(bytes) = read_registered_png_clipboard() {
                return Some(ClipboardImage {
                    bytes,
                    extension: "png",
                });
            }
            for format in [CF_DIBV5 as u32, CF_DIB as u32] {
                if let Some(bytes) =
                    clipboard_global_bytes(format, clipboard_image::MAX_CLIPBOARD_ALLOCATION)
                {
                    if let Some(bytes) = clipboard_image::dib_to_png(&bytes) {
                        return Some(ClipboardImage {
                            bytes,
                            extension: "png",
                        });
                    }
                }
            }
            return None;
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    None
}

fn read_registered_png_clipboard() -> Option<Vec<u8>> {
    static PNG_FORMAT: LazyLock<u32> = LazyLock::new(|| {
        let name = wide_null("PNG");
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    });
    if *PNG_FORMAT == 0 {
        return None;
    }
    let bytes = clipboard_global_bytes(
        *PNG_FORMAT,
        crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD + 64 * 1024,
    )?;
    clipboard_image::validated_png(&bytes)
}

fn clipboard_global_bytes(format: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return None;
    }
    let data = unsafe { GlobalLock(handle) };
    if data.is_null() {
        return None;
    }
    let size = unsafe { GlobalSize(handle) };
    if size == 0 || size > max_bytes {
        unsafe {
            GlobalUnlock(handle);
        }
        return None;
    }
    let mut bytes = vec![0_u8; size];
    unsafe {
        copy_nonoverlapping(data.cast::<u8>(), bytes.as_mut_ptr(), size);
        GlobalUnlock(handle);
    }
    Some(bytes)
}

pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    let title = title.to_owned();
    let body = body.unwrap_or(&title).to_owned();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("herdr-windows-notification".into())
        .spawn(move || show_desktop_notification_on_thread(&title, &body, ready_tx))?;
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|err| match err {
            std::sync::mpsc::RecvTimeoutError::Timeout => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Windows notification setup timed out",
            ),
            std::sync::mpsc::RecvTimeoutError::Disconnected => std::io::Error::other(
                "Windows notification thread exited before reporting readiness",
            ),
        })?
}

fn show_desktop_notification_on_thread(
    title: &str,
    body: &str,
    ready_tx: std::sync::mpsc::SyncSender<std::io::Result<bool>>,
) {
    let class_name = wide_null("STATIC");
    let window_name = wide_null("Herdr notifications");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            window_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let _ = ready_tx.send(Err(std::io::Error::last_os_error()));
        return;
    }

    let mut notification = unsafe { std::mem::zeroed::<NOTIFYICONDATAW>() };
    notification.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    notification.hWnd = hwnd;
    notification.uID = 1;
    notification.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    notification.uFlags = NIF_TIP;
    if !notification.hIcon.is_null() {
        notification.uFlags |= NIF_ICON;
    }
    copy_wide_truncated(&mut notification.szTip, "Herdr");

    if unsafe { Shell_NotifyIconW(NIM_ADD, &notification) } == 0 {
        let _ = ready_tx.send(Err(std::io::Error::other(
            "failed to add Herdr notification-area icon",
        )));
        unsafe {
            DestroyWindow(hwnd);
        }
        return;
    }

    notification.uFlags = NIF_INFO;
    notification.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
    copy_wide_truncated(&mut notification.szInfoTitle, title);
    copy_wide_truncated(&mut notification.szInfo, body);
    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &notification) } == 0 {
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &notification);
            DestroyWindow(hwnd);
        }
        let _ = ready_tx.send(Err(std::io::Error::other(
            "failed to show Herdr desktop notification",
        )));
        return;
    }

    let _ = ready_tx.send(Ok(true));
    std::thread::sleep(Duration::from_secs(10));
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &notification);
        DestroyWindow(hwnd);
    }
}

fn copy_wide_truncated<const N: usize>(destination: &mut [u16; N], value: &str) {
    destination.fill(0);
    let mut offset = 0;
    for ch in value.chars() {
        let mut units = [0; 2];
        let encoded = ch.encode_utf16(&mut units);
        if offset + encoded.len() >= N {
            break;
        }
        destination[offset..offset + encoded.len()].copy_from_slice(encoded);
        offset += encoded.len();
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

struct ProcessHandle(HANDLE);

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}

impl ProcessHandle {
    fn open(pid: u32, access: u32) -> Option<Self> {
        if pid == 0 {
            return None;
        }
        let handle = unsafe { OpenProcess(access, 0, pid) };
        (!handle.is_null()).then_some(Self(handle))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Peb {
    reserved1: [u8; 2],
    being_debugged: u8,
    reserved2: [u8; 1],
    reserved3: [*mut c_void; 2],
    ldr: *mut c_void,
    process_parameters: *mut RtlUserProcessParameters,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CurDir {
    dos_path: UNICODE_STRING,
    handle: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RtlUserProcessParameters {
    maximum_length: u32,
    length: u32,
    flags: u32,
    debug_flags: u32,
    console_handle: HANDLE,
    console_flags: u32,
    standard_input: HANDLE,
    standard_output: HANDLE,
    standard_error: HANDLE,
    current_directory: CurDir,
    dll_path: UNICODE_STRING,
    image_path_name: UNICODE_STRING,
    command_line: UNICODE_STRING,
    environment: *mut c_void,
}

fn read_process_value<T: Copy>(process: HANDLE, address: *const c_void) -> Option<T> {
    if address.is_null() {
        return None;
    }

    let mut value = MaybeUninit::<T>::uninit();
    let mut bytes_read = 0;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            address,
            value.as_mut_ptr().cast::<c_void>(),
            size_of::<T>(),
            &mut bytes_read,
        )
    } != 0;

    (ok && bytes_read == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

fn read_unicode_string(process: HANDLE, unicode: UNICODE_STRING) -> Option<String> {
    if unicode.Buffer.is_null() || unicode.Length == 0 || !unicode.Length.is_multiple_of(2) {
        return None;
    }

    let char_len = usize::from(unicode.Length / 2);
    let mut buffer = vec![0_u16; char_len];
    let mut bytes_read = 0;
    let ok = unsafe {
        ReadProcessMemory(
            process,
            unicode.Buffer.cast::<c_void>(),
            buffer.as_mut_ptr().cast::<c_void>(),
            usize::from(unicode.Length),
            &mut bytes_read,
        )
    } != 0;

    if !ok || bytes_read != usize::from(unicode.Length) {
        return None;
    }

    String::from_utf16(&buffer).ok()
}

// Prefix-mode ASCII input source support (see `switch_ascii_input_source_in_prefix`).
//
// Windows IMEs live in the terminal-emulator process, not in herdr. Empirically:
//   - `WM_IME_CONTROL` / `IMC_GETOPENSTATUS` reads whether the IME is open
//     (composing native characters) reliably across the process boundary (this
//     is what kren-select uses), so we detect state with it. The read goes
//     through `SendMessageTimeoutW` (`SMTO_ABORTIFHUNG`) so a hung host process
//     cannot block us indefinitely.
//   - Writing the state back (`IMC_SETOPENSTATUS` / `IMC_SETCONVERSIONMODE`)
//     changes the flag value but does NOT affect real input in terminal/TSF
//     hosts, so we cannot switch by writing the mode.
//   - `ImmGetContext` on the foreground window returns null across the process
//     boundary, so the ImmGetOpenStatus/ImmSetOpenStatus path is unavailable.
// Therefore we switch the way kren-select does: inject the IME toggle key with
// `SendInput`, which reaches the foreground input queue like a real keypress.
//
// The toggle key is language-specific, so we pick it from the foreground
// keyboard layout's language id. Only Korean is mapped today; other IMEs are
// detected and left untouched (a no-op) rather than toggled with the wrong key.

/// `WM_IME_CONTROL` sub-command that reads whether the IME is open, i.e.
/// composing native characters. This is `IMC_GETOPENSTATUS` (0x0005); for the
/// Korean IME "open" is exactly the Hangul state and "closed" is English/ASCII
/// direct input, which is the state we detect and toggle.
const IMC_GETOPENSTATUS: usize = 0x0005;

/// Virtual key that toggles Hangul/English on Korean IMEs.
const VK_HANGUL: u16 = 0x15;

/// Primary language id (low 10 bits of a LANGID) for Korean.
const LANG_KOREAN: u32 = 0x12;

/// Whether the IME reports itself open, i.e. composing native characters
/// (Hangul for the Korean IME). `IMC_GETOPENSTATUS` returns nonzero when the
/// IME is open and zero when it is in direct English/ASCII input.
fn ime_open(open_status: isize) -> bool {
    open_status != 0
}

/// Timeout (ms) for the cross-process IME open-status read. Short enough that a
/// hung terminal never freezes prefix-mode entry/exit.
const IME_STATUS_READ_TIMEOUT_MS: u32 = 200;

/// Reads the IME open status (`IMC_GETOPENSTATUS`) with a bounded timeout.
///
/// `WM_IME_CONTROL` crosses into the terminal-emulator process, and a plain
/// `SendMessageW` would block herdr's client thread until that process responds
/// (indefinitely if it is hung). `SendMessageTimeoutW` with `SMTO_ABORTIFHUNG`
/// caps the wait; on timeout or failure this returns `None` and callers leave
/// the IME untouched rather than blocking or guessing.
fn read_ime_open_status(ime_hwnd: HWND) -> Option<isize> {
    let mut result: usize = 0;
    // SAFETY: `ime_hwnd` is a non-null IME window from `ImmGetDefaultIMEWnd`, and
    // `result` is a valid out-pointer for the message's `DWORD_PTR` result.
    let ret = unsafe {
        SendMessageTimeoutW(
            ime_hwnd,
            WM_IME_CONTROL,
            IMC_GETOPENSTATUS,
            0,
            SMTO_ABORTIFHUNG,
            IME_STATUS_READ_TIMEOUT_MS,
            &mut result,
        )
    };
    if ret == 0 {
        // Timed out or failed; do not block or assume a state.
        return None;
    }
    Some(result as isize)
}

/// The IME toggle key for a keyboard layout language id, or `None` when the
/// language's toggle key is not known. `langid` is the full LANGID (LOWORD of
/// an `HKL`); the primary language is its low 10 bits.
///
/// Only Korean is mapped: `VK_HANGUL` is the Hangul/English toggle. Japanese
/// (half/full-width) and Chinese use different keys per IME, so they return
/// `None` and are left untouched instead of toggled incorrectly.
fn toggle_key_for_language(langid: u32) -> Option<u16> {
    match langid & 0x3FF {
        LANG_KOREAN => Some(VK_HANGUL),
        _ => None,
    }
}

/// Builds the key-down then key-up `INPUT` pair for `vk`.
fn key_tap_inputs(vk: u16) -> [INPUT; 2] {
    let key_event = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    [key_event(0), key_event(KEYEVENTF_KEYUP)]
}

/// Injects a key-down then key-up for `vk` via `SendInput`.
///
/// Returns `true` when the key-down was queued and the IME may have toggled.
/// Thin wrapper over [`send_vk_tap_with`] that plugs in the real `SendInput`;
/// the injection policy lives there so it can be unit-tested without the OS.
fn send_vk_tap(vk: u16) -> bool {
    send_vk_tap_with(vk, |events| {
        // SAFETY: `events` outlives the call; its `INPUT_KEYBOARD` entries have
        // the `ki` union variant fully initialized, which is the variant
        // SendInput reads for keyboard input. `size_of::<INPUT>()` is the
        // required `cbSize`.
        unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        }
    })
}

/// Core key-tap logic with the raw event injector abstracted behind `inject`,
/// which returns how many of the passed events it actually queued. This keeps
/// the success / partial-injection / total-failure branches unit-testable
/// without touching the real `SendInput`.
///
/// `SendInput` returns how many events it queued; a short count means injection
/// was blocked (e.g. by UIPI). Returns `true` whenever the key-down was queued,
/// because the IME may have toggled and callers must retain restoration state.
/// When only the key-down landed, the key-up is retried so the key is not left
/// logically held down.
fn send_vk_tap_with(vk: u16, mut inject: impl FnMut(&[INPUT]) -> u32) -> bool {
    let inputs = key_tap_inputs(vk);
    let sent = inject(&inputs);
    if sent as usize == inputs.len() {
        return true;
    }

    if sent == 1 {
        // The key-down landed and may already have toggled the IME. Retry the
        // dropped key-up, but report that restoration state is still required.
        let key_up = [inputs[1]];
        let up_sent = inject(&key_up);
        tracing::warn!(
            vk,
            sent,
            expected = inputs.len(),
            key_up_retry_sent = up_sent,
            "SendInput dropped the IME toggle key-up; retried key-up"
        );
        return true;
    }

    tracing::warn!(
        vk,
        sent,
        expected = inputs.len(),
        "SendInput did not inject the IME toggle key tap"
    );
    false
}

pub(crate) fn pump_input_source_runloop() {}

/// Switch the foreground window's IME to ASCII-capable input for prefix mode.
///
/// Returns `None` (nothing to restore) when there is no foreground IME, the
/// keyboard language has no known toggle key, or the IME is already
/// ASCII-capable, matching the macOS contract.
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    // SAFETY: all calls are Win32 UI functions invoked on the client's main
    // thread. Every HWND is null-checked before use; `fg_thread` is a thread id
    // (not a handle) used only as `GetKeyboardLayout` input, where 0 harmlessly
    // falls back to the calling thread's layout.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return None;
        }

        // Pick the toggle key for the foreground keyboard language. Unknown
        // languages (Japanese, Chinese, ...) are left untouched.
        let fg_thread = GetWindowThreadProcessId(fg, null_mut());
        let langid = (GetKeyboardLayout(fg_thread) as usize as u32) & 0xFFFF;
        let Some(toggle_vk) = toggle_key_for_language(langid) else {
            tracing::debug!(
                langid = format!("{langid:#06x}"),
                "prefix IME switch: no toggle key for keyboard language, leaving IME as-is"
            );
            return None;
        };

        // Detect the open (Hangul) state via the bounded read path.
        let ime_hwnd = ImmGetDefaultIMEWnd(fg);
        if ime_hwnd.is_null() {
            return None;
        }
        let Some(open) = read_ime_open_status(ime_hwnd) else {
            tracing::debug!("prefix IME switch skipped: IME open-status read timed out");
            return None;
        };
        if !ime_open(open) {
            // Already in English/ASCII input; nothing to switch or restore.
            return None;
        }

        // The bounded cross-process status read can take long enough for focus
        // to change. Recheck immediately before using the global input queue.
        if GetForegroundWindow() != fg {
            tracing::debug!("prefix IME switch skipped: foreground window changed");
            return None;
        }

        // Toggle to ASCII by injecting the language's IME toggle key. Only arm
        // restoration when the toggle actually landed, so we never try to
        // restore a switch that never happened.
        if !send_vk_tap(toggle_vk) {
            tracing::warn!(
                langid = format!("{langid:#06x}"),
                "prefix IME switch: toggle injection failed, leaving IME as-is"
            );
            return None;
        }
        tracing::debug!(
            langid = format!("{langid:#06x}"),
            "switched host IME to ASCII for prefix mode"
        );
        Some(InputSourceRestore {
            toggle_vk,
            origin_hwnd: fg as isize,
        })
    }
}

/// Restores the native (Hangul) IME state that was active before prefix mode.
///
/// Only constructed by [`switch_to_ascii_input_source`] after it successfully
/// toggled the IME to English/ASCII. Dropping it re-injects the same toggle key
/// to go back, but only after two guards, so restoration never fights the user
/// or another application:
///   - the same window that was switched must still be focused, otherwise the
///     toggle would land on whatever app the user moved to;
///   - the IME must still be in English (our switch still in effect), otherwise
///     the user manually returned to Hangul during prefix mode and we must leave
///     their choice alone.
///
/// `origin_hwnd` stores the foreground window at switch time as raw pointer bits
/// (`isize`, not `HWND`) so the guard stays `Send` when parked in the client's
/// prefix-input state across `.await` points.
#[derive(Debug)]
pub(crate) struct InputSourceRestore {
    toggle_vk: u16,
    origin_hwnd: isize,
}

impl Drop for InputSourceRestore {
    fn drop(&mut self) {
        // SAFETY: all calls are Win32 UI functions invoked on the client's main
        // thread. Every HWND is null-checked before use.
        unsafe {
            // Guard 1: only restore if the window we switched is still focused,
            // so the toggle never lands on a different application.
            let fg = GetForegroundWindow();
            if fg.is_null() || fg as isize != self.origin_hwnd {
                tracing::debug!(
                    "prefix IME restore skipped: foreground window changed since switch"
                );
                return;
            }

            // Guard 2: only restore if the IME is still in English (our switch is
            // still in effect). If the user manually switched back to Hangul
            // during prefix mode, leave their choice untouched.
            let ime_hwnd = ImmGetDefaultIMEWnd(fg);
            if ime_hwnd.is_null() {
                return;
            }
            let Some(open) = read_ime_open_status(ime_hwnd) else {
                tracing::debug!("prefix IME restore skipped: IME open-status read timed out");
                return;
            };
            if ime_open(open) {
                tracing::debug!("prefix IME restore skipped: IME already back to native input");
                return;
            }

            // The bounded cross-process status read can take long enough for
            // focus to change. Recheck immediately before using SendInput.
            if GetForegroundWindow() != fg {
                tracing::debug!("prefix IME restore skipped: foreground window changed");
                return;
            }

            if send_vk_tap(self.toggle_vk) {
                tracing::debug!("restored host IME after prefix mode");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Command, Stdio},
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use windows_sys::Win32::System::Console::{
        AllocConsole, FreeConsole, GetConsoleProcessList, GetConsoleWindow,
    };

    #[test]
    fn private_remote_directory_supports_long_paths() {
        let base = std::env::temp_dir().join(format!(
            "herdr-private-remote-dir-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&base).expect("create test base");
        let private = base.join("x".repeat(240));

        super::create_remote_private_dir(&private).expect("create private long-path directory");
        fs::write(private.join("probe"), b"ok").expect("write inherited private file");

        fs::remove_dir_all(base).expect("remove test directory");
    }

    #[test]
    fn windows_conpty_native_encoder_uses_canonical_phase_and_repeat_count() {
        let key = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        )
        .with_windows_record(crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 3,
            virtual_key_code: 27,
            virtual_scan_code: 1,
            unicode: 27,
            control_key_state: 0,
        });

        assert_eq!(
            super::encode_windows_conpty_fallback(&key),
            Some(b"\x1b[27;1;27;1;0;3_".to_vec())
        );
        let mut release = key.with_kind(crossterm::event::KeyEventKind::Release);
        release.repeat_count = 3;
        assert_eq!(
            super::encode_windows_conpty_fallback(&release),
            Some(b"\x1b[27;1;27;0;0;1_".to_vec())
        );
    }

    #[test]
    fn windows_conpty_native_encoder_preserves_semantic_escape_fallback() {
        let escape = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::empty(),
        );

        assert_eq!(
            super::encode_windows_conpty_fallback(&escape),
            Some(b"\x1b[27;1;27;1;0;1_\x1b[27;1;27;0;0;1_".to_vec())
        );
        assert_eq!(
            super::encode_windows_conpty_fallback(
                &escape
                    .clone()
                    .with_kind(crossterm::event::KeyEventKind::Repeat),
            ),
            None
        );
        assert_eq!(
            super::encode_windows_conpty_fallback(
                &escape
                    .clone()
                    .with_kind(crossterm::event::KeyEventKind::Release),
            ),
            None
        );
        assert_eq!(
            super::encode_windows_conpty_fallback(&escape.clone().with_vt_bytes(vec![27])),
            None
        );
        assert_eq!(
            super::encode_windows_conpty_fallback(&crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::ALT,
            ),),
            None
        );
    }

    #[test]
    fn windows_conpty_native_encoder_preserves_semantic_shift_enter_fallback() {
        let shift_enter = crate::input::TerminalKey::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::SHIFT,
        );

        assert_eq!(
            super::encode_windows_conpty_fallback(&shift_enter),
            Some(b"\x1b[13;28;13;1;16;1_".to_vec())
        );
        assert_eq!(
            super::encode_windows_conpty_fallback(&crate::input::TerminalKey::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::empty(),
            )),
            None
        );
    }

    #[test]
    fn windows_notification_text_is_null_terminated_and_unicode_safe() {
        let mut destination = [u16::MAX; 6];
        super::copy_wide_truncated(&mut destination, "abc😀def");

        assert_eq!(String::from_utf16(&destination[..5]).unwrap(), "abc😀");
        assert_eq!(destination[5], 0);
    }

    #[test]
    fn powershell_agent_command_omits_argument_list_when_no_arguments_are_passed() {
        let argv = vec!["opencode".into()];

        assert_eq!(
            super::interactive_shell_command(&argv, "powershell.exe").as_deref(),
            Some("& opencode")
        );
    }

    #[test]
    fn cmd_agent_command_encodes_edge_arguments_without_cmd_expansion() {
        use base64::Engine as _;

        assert_eq!(super::super::quote_powershell_arg("@options"), "'@options'");
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "100%".into(),
            "wow!".into(),
            "a'b".into(),
        ];
        let command = super::interactive_shell_command(&argv, "cmd.exe").unwrap();
        let encoded = command.split_whitespace().last().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let utf16 = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        assert_eq!(
            String::from_utf16(&utf16).unwrap(),
            "$p=Start-Process -FilePath pi -ArgumentList '\"\" \"two words\" 100% wow! a''b' -NoNewWindow -Wait -PassThru"
        );
    }

    #[test]
    fn windows_shells_round_trip_agent_arguments_through_a_real_command() {
        let _lock = crate::integration::integration_env_lock();
        let base = std::env::temp_dir().join(format!(
            "herdr-agent-argv-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        fs::create_dir_all(&base).unwrap();
        let helper = base.join("pi.cmd");
        fs::write(
            &helper,
            "@echo off\r\n>\"%HERDR_ARGV_CAPTURE%\" (\r\necho(%~1\r\necho(%~2\r\necho(%~3\r\necho(%~4\r\necho(%~5\r\necho(%~6\r\n)\r\n",
        )
        .unwrap();
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "100%".into(),
            "wow!".into(),
            "a'b".into(),
            "@options".into(),
        ];
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = format!("{};{}", base.display(), inherited_path.to_string_lossy());
        let run_command = |shell: &str, command: &str, capture: &std::path::Path| {
            let mut process = if shell == "cmd.exe" {
                let mut process = Command::new("cmd.exe");
                process.args(["/d", "/c", command]);
                process
            } else {
                let mut process = Command::new("powershell.exe");
                process.args(["-NoLogo", "-NoProfile", "-Command", command]);
                process
            };
            process
                .env("PATH", &path)
                .env("HERDR_ARGV_CAPTURE", capture)
                .status()
                .unwrap()
        };

        for shell in ["powershell.exe", "cmd.exe"] {
            let no_args_capture = base.join(format!("{shell}-no-args.txt"));
            let no_args_command = super::interactive_shell_command(&["pi".into()], shell).unwrap();
            let status = run_command(shell, &no_args_command, &no_args_capture);
            assert!(status.success(), "{shell} argument-free command failed");
            assert_eq!(
                fs::read_to_string(no_args_capture)
                    .unwrap()
                    .replace("\r\n", "\n"),
                "\n\n\n\n\n\n"
            );

            let capture = base.join(format!("{shell}.txt"));
            let command = super::interactive_shell_command(&argv, shell).unwrap();
            let status = run_command(shell, &command, &capture);
            assert!(status.success(), "{shell} command failed");
            assert_eq!(
                fs::read_to_string(capture).unwrap().replace("\r\n", "\n"),
                "\ntwo words\n100%\nwow!\na'b\n@options\n"
            );
        }

        let _ = fs::remove_dir_all(base);
    }

    const CONSOLE_TEST_CHILD_ENV: &str = "HERDR_TEST_CONSOLE_CHILD_MODE";
    const CONSOLE_TEST_PARENT_PID_ENV: &str = "HERDR_TEST_CONSOLE_PARENT_PID";
    const WMI_DAEMON_TEST_CHILD_ENV: &str = "HERDR_TEST_WMI_DAEMON_CHILD";

    #[test]
    fn windows_environment_keys_use_unicode_case_insensitive_ordering() {
        assert_eq!(
            super::windows_environment_key_cmp("hérdr", "HÉRDR"),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn windows_wmi_daemon_preserves_environment_and_working_directory() {
        if let Some(capture) = std::env::var_os(WMI_DAEMON_TEST_CHILD_ENV) {
            let cwd = std::env::current_dir().expect("WMI daemon test working directory");
            fs::write(
                capture,
                format!(
                    "{}\n{}",
                    cwd.display(),
                    super::current_process_is_detached_server_daemon()
                ),
            )
            .expect("write WMI daemon test capture");
            return;
        }

        let base = std::env::temp_dir().join(format!(
            "herdr-wmi-daemon-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        fs::create_dir_all(&base).unwrap();
        let capture = base.join("capture.txt");
        let test_exe = std::env::current_exe().expect("resolve test executable");
        let mut child = Command::new(test_exe);
        child
            .arg("windows_wmi_daemon_preserves_environment_and_working_directory")
            .current_dir(&base)
            .env(WMI_DAEMON_TEST_CHILD_ENV, &capture)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let pid = super::launch_server_daemon_with_wmi(&child)
            .expect("launch detached process through WMI");
        assert_ne!(pid, 0, "WMI returned an invalid process id");

        let expected = format!("{}\ntrue", base.display());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if fs::read_to_string(&capture).is_ok_and(|captured| captured == expected) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "WMI daemon child did not write the expected capture"
            );
            thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_dir_all(base);
    }

    fn console_process_ids() -> Vec<u32> {
        let mut process_ids = vec![0; 8];
        loop {
            let count = unsafe {
                GetConsoleProcessList(process_ids.as_mut_ptr(), process_ids.len() as u32)
            } as usize;
            if count == 0 {
                return Vec::new();
            }
            if count <= process_ids.len() {
                process_ids.truncate(count);
                return process_ids;
            }
            process_ids.resize(count, 0);
        }
    }

    #[test]
    fn windows_background_and_server_daemon_commands_do_not_have_consoles() {
        if let Some(mode) = std::env::var_os(CONSOLE_TEST_CHILD_ENV) {
            assert!(
                unsafe { GetConsoleWindow() }.is_null(),
                "{} child opened or inherited a console window",
                mode.to_string_lossy()
            );
            let parent_pid = std::env::var(CONSOLE_TEST_PARENT_PID_ENV)
                .expect("console test parent pid")
                .parse::<u32>()
                .expect("numeric console test parent pid");
            assert!(
                !console_process_ids().contains(&parent_pid),
                "{} child inherited the parent console",
                mode.to_string_lossy()
            );
            return;
        }

        let allocated_console = if console_process_ids().is_empty() {
            assert_ne!(unsafe { AllocConsole() }, 0, "allocate test console");
            true
        } else {
            false
        };

        let parent_pid = std::process::id().to_string();
        let test_exe = std::env::current_exe().expect("resolve test executable");
        let configurations: [(&str, fn(&mut Command)); 2] = [
            ("background", super::configure_background_command_platform),
            ("server daemon", super::detach_server_daemon_command),
        ];
        for (mode, configure) in configurations {
            let mut child = Command::new(&test_exe);
            child
                .arg("windows_background_and_server_daemon_commands_do_not_have_consoles")
                .env(CONSOLE_TEST_CHILD_ENV, mode)
                .env(CONSOLE_TEST_PARENT_PID_ENV, &parent_pid)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            configure(&mut child);

            let status = child.status().expect("spawn console isolation test child");
            assert!(
                status.success(),
                "{mode} child opened or inherited a console"
            );
        }

        let command = format!(
            r#""{}" windows_background_and_server_daemon_commands_do_not_have_consoles"#,
            test_exe.display()
        );
        let status = crate::platform::detached_custom_command_process(&command)
            .env(CONSOLE_TEST_CHILD_ENV, "detached custom command descendant")
            .env(CONSOLE_TEST_PARENT_PID_ENV, &parent_pid)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn detached custom command test child");
        assert!(
            status.success(),
            "detached custom command descendant opened or inherited a console"
        );

        if allocated_console {
            unsafe {
                FreeConsole();
            }
        }
    }

    fn argv_strings(argv: &[std::ffi::OsString]) -> Vec<String> {
        argv.into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn pane_custom_command_uses_cmd() {
        let builder = super::pane_custom_command_pty_builder_with_comspec(
            "echo hello",
            Some(r"C:\Windows\System32\cmd.exe".into()),
        );

        assert_eq!(
            argv_strings(builder.get_argv()),
            [r"C:\Windows\System32\cmd.exe", "/d", "/c"]
        );
    }

    #[test]
    fn detached_custom_command_uses_cmd() {
        let expected_shell = std::env::var_os("ComSpec")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into())
            .to_string_lossy()
            .into_owned();

        let process = super::detached_custom_command_process_platform("echo hello");

        assert_eq!(process.get_program().to_string_lossy(), expected_shell);
        assert_eq!(
            process
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["/d", "/c", "echo hello"]
        );
    }

    #[test]
    fn custom_command_falls_back_when_comspec_is_empty() {
        let builder =
            super::pane_custom_command_pty_builder_with_comspec("echo hello", Some("".into()));

        assert_eq!(
            argv_strings(builder.get_argv()),
            [r"C:\Windows\System32\cmd.exe", "/d", "/c"]
        );
    }

    #[test]
    fn detached_custom_command_preserves_quoted_command_tail() {
        let path = std::env::temp_dir().join(format!(
            "herdr-raw-command-quotes-{}.txt",
            std::process::id()
        ));
        let command = format!(r#"echo "hi" > "{}""#, path.display());

        let status = super::detached_custom_command_process_platform(&command)
            .status()
            .expect("spawn raw command");

        assert!(status.success(), "{status:?}");
        let content = std::fs::read_to_string(&path).expect("read command output");
        let _ = std::fs::remove_file(&path);
        assert!(content.contains(r#""hi""#), "{content:?}");
        assert!(!content.contains(r#"\"hi\""#), "{content:?}");
    }

    #[test]
    fn windows_process_cwd_reads_child_launch_directory() {
        let cwd = std::env::temp_dir().join(format!("herdr-cwd-test-{}", std::process::id()));
        fs::create_dir_all(&cwd).expect("create cwd fixture");

        let shell =
            std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
        let mut child = Command::new(shell)
            .args(["/D", "/Q", "/C", "ping -n 11 127.0.0.1 > NUL"])
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = None;
        while Instant::now() < deadline {
            observed = super::process_cwd(child.id());
            if observed.as_deref() == Some(cwd.as_path()) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_dir_all(&cwd);

        assert_eq!(observed.as_deref(), Some(cwd.as_path()));
    }

    #[test]
    fn windows_process_environment_reads_runtime_marker() {
        let shell =
            std::env::var_os("ComSpec").unwrap_or_else(|| r"C:\Windows\System32\cmd.exe".into());
        let mut child = Command::new(shell)
            .args(["/D", "/Q", "/C", "ping -n 11 127.0.0.1 > NUL"])
            .env(super::PANE_RUNTIME_MARKER_ENV_VAR, "pane-test")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut observed = None;
        while Instant::now() < deadline {
            observed = super::process_runtime_marker(child.id());
            if observed.as_deref() == Some("pane-test") {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(observed.as_deref(), Some("pane-test"));
    }

    #[test]
    fn windows_process_tree_selects_direct_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| panic!("Git Bash fallback must not run after normal detection succeeds"),
            |_| panic!("runtime marker must not be read after normal detection succeeds"),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes.len(), 1);
        assert_eq!(job.processes[0].name, "codex.exe");
    }

    #[test]
    fn windows_process_tree_still_inspects_unusual_escaped_argv0() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(20, 99, "launcher.exe", &["codex.exe"]),
        ]);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| true,
            |_| Some("pane-a".to_string()),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "launcher.exe");
    }

    #[test]
    fn windows_process_tree_still_inspects_unusual_descendant_argv0() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "launcher.exe", &["codex.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "launcher.exe");
    }

    #[test]
    fn windows_process_tree_shares_snapshot_candidates_across_git_bash_panes() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(
                11,
                10,
                "bash.exe",
                &[r"C:\Program Files\Git\usr\bin\bash.exe"],
            ),
            test_entry(12, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(
                20,
                99,
                "sh.exe",
                &[r"C:\Program Files\Git\usr\bin\sh.exe", "/c/npm/codex"],
            ),
            test_entry(
                30,
                20,
                "node.exe",
                &[
                    r"C:\Program Files\nodejs\node.exe",
                    r"C:\Users\user\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js",
                ],
            ),
            test_entry(
                40,
                30,
                "codex.exe",
                &[r"C:\npm\node_modules\@openai\codex\bin\codex.exe"],
            ),
            test_entry(50, 98, "claude.exe", &["claude.exe"]),
        ]);
        let mut inspected = Vec::new();
        assert!(snapshot.agent_indices.get().is_none());
        let marker = |entry: &super::WindowsProcessEntry| match entry.pid {
            12 | 50 => Some("pane-b".to_string()),
            _ => Some("pane-a".to_string()),
        };

        let first = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| true,
            |entry| {
                inspected.push(entry.pid);
                marker(entry)
            },
        )
        .unwrap();
        let indices = snapshot.agent_indices.get().unwrap();
        let second = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            12,
            &snapshot,
            |_| true,
            marker,
        )
        .unwrap();

        assert_eq!(first.process_group_id, 20);
        assert_eq!(first.processes[0].name, "sh.exe");
        assert_eq!(second.process_group_id, 50);
        assert_eq!(indices, &[3, 4, 5, 6]);
        assert!(std::ptr::eq(indices, snapshot.agent_indices.get().unwrap()));
        assert_eq!(inspected, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn windows_process_tree_skips_runtime_inspection_for_non_git_bash_shell() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 99, "codex.exe", &["codex.exe"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| false,
            |_| panic!("runtime marker must not be read for non-Git-Bash panes"),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 10);
    }

    #[test]
    fn windows_process_tree_skips_runtime_inspection_without_agent_candidate() {
        let entries = vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(20, 99, "git.exe", &["git.exe", "status"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| true,
            |_| panic!("runtime marker must not be read without an agent candidate"),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 10);
    }

    #[test]
    fn windows_process_tree_rejects_missing_or_empty_shell_runtime_marker() {
        let entries = vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(20, 99, "codex.exe", &["codex.exe"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        for shell_marker in [None, Some(String::new())] {
            let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
                10,
                &snapshot,
                |_| true,
                |entry| {
                    if entry.pid == 10 {
                        shell_marker.clone()
                    } else {
                        Some("pane-a".to_string())
                    }
                },
            )
            .unwrap();

            assert_eq!(job.process_group_id, 10);
        }
    }

    #[test]
    fn windows_process_tree_rejects_runtime_marker_from_another_pane() {
        let entries = vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(20, 99, "codex.exe", &["codex.exe"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| true,
            |entry| Some(if entry.pid == 10 { "pane-a" } else { "pane-b" }.to_string()),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "bash.exe");
    }

    #[test]
    fn windows_process_tree_rejects_ambiguous_runtime_marker_candidates() {
        let entries = vec![
            test_entry(10, 1, "bash.exe", &[r"C:\Program Files\Git\bin\bash.exe"]),
            test_entry(20, 99, "codex.exe", &["codex.exe"]),
            test_entry(30, 98, "claude.exe", &["claude.exe"]),
        ];
        let snapshot = super::ProcessSnapshot::new(entries);

        let job = super::select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            10,
            &snapshot,
            |_| true,
            |_| Some("pane-a".to_string()),
        )
        .unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "bash.exe");
    }

    #[test]
    fn windows_foreground_process_snapshot_is_shared_within_ttl() {
        let mut cache = super::ProcessSnapshotCache { cached: None };
        let mut builds = 0;
        let mut first_build_completed_at = None;

        let first = cache.snapshot(Duration::from_secs(60), || {
            builds += 1;
            let entries = vec![test_entry(10, 1, "powershell.exe", &["powershell.exe"])];
            first_build_completed_at = Some(Instant::now());
            entries
        });
        assert!(cache.cached.as_ref().unwrap().built_at >= first_build_completed_at.unwrap());
        let second = cache.snapshot(Duration::from_secs(60), || {
            builds += 1;
            Vec::new()
        });
        let refreshed = cache.snapshot(Duration::ZERO, || {
            builds += 1;
            vec![test_entry(20, 1, "pwsh.exe", &["pwsh.exe"])]
        });

        assert!(Arc::ptr_eq(&first, &second));
        assert!(!Arc::ptr_eq(&second, &refreshed));
        assert_eq!(builds, 2);
        assert_eq!(refreshed.entries[0].pid, 20);
    }

    #[test]
    fn windows_foreground_selection_cache_reuses_live_agent_and_invalidates_changes() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
        ]);
        let job = super::foreground_job_from_entry(snapshot.entry(20).unwrap());
        let mut cache = super::ForegroundSelectionCache::default();
        cache.remember_for_test(10, &snapshot, &job);

        assert_eq!(cache.get(10, &snapshot), Some(job.clone()));

        cache.entries.get_mut(&10).unwrap().selected_identity = super::ProcessIdentity::Stub {
            running: false,
            creation_time: None,
        };
        assert_eq!(cache.get(10, &snapshot), None);

        cache.remember_for_test(10, &snapshot, &job);
        let overlap = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "claude.exe", &["claude.exe"]),
        ]);
        assert_eq!(cache.get(10, &overlap), None);

        cache.remember_for_test(10, &snapshot, &job);
        let changed = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "git.exe", &["git.exe"]),
        ]);
        assert_eq!(cache.get(10, &changed), None);

        cache.remember_for_test(10, &snapshot, &job);
        cache.entries.get_mut(&10).unwrap().verified_at = Instant::now()
            .checked_sub(super::FOREGROUND_SELECTION_RECHECK + Duration::from_secs(1))
            .unwrap();
        assert_eq!(cache.get(10, &snapshot), None);
    }

    #[test]
    fn windows_foreground_selection_cache_rejects_reused_descendant_pid() {
        let original = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "node.exe", &["node.exe", "worker.js"]),
        ]);
        let job = super::foreground_job_from_entry(original.entry(20).unwrap());
        let mut cache = super::ForegroundSelectionCache::default();
        cache.remember_for_test(10, &original, &job);

        let cached = cache.entries.get_mut(&10).unwrap();
        let reused_index = cached
            .descendants
            .iter()
            .position(|entry| entry.pid == 30)
            .unwrap();
        cached.descendant_identities[reused_index] = super::ProcessIdentity::Stub {
            running: false,
            creation_time: None,
        };
        let replacement = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "node.exe", &["node.exe", "codex.js"]),
        ]);

        assert_eq!(cache.get(10, &replacement), None);
    }

    #[test]
    fn windows_foreground_selection_cache_rejects_identity_change_before_insertion() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry_with_creation_time(10, 1, "powershell.exe", &["powershell.exe"], Some(1)),
            test_entry_with_creation_time(20, 10, "codex.exe", &["codex.exe"], Some(2)),
            test_entry_with_creation_time(30, 10, "node.exe", &["node.exe", "worker.js"], Some(3)),
        ]);
        let job = super::foreground_job_from_entry(snapshot.entry(20).unwrap());
        let descendants = snapshot.descendant_signatures(10);
        let descendant_identities = vec![
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(2),
            },
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(4),
            },
        ];
        let mut cache = super::ForegroundSelectionCache::default();

        let cached = super::CachedForegroundSelection::from_snapshot_with_identities(
            10,
            &snapshot,
            &job,
            descendants,
            descendant_identities,
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(1),
            },
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(2),
            },
        );
        cache.remember(10, cached);

        assert!(cache.entries.is_empty());
    }

    #[test]
    fn windows_foreground_selection_cache_accepts_limited_information_identity() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry_without_cmdline(10, 1, "powershell.exe", 1),
            test_entry_without_cmdline(20, 10, "codex.exe", 2),
        ]);
        let job = super::foreground_job_from_entry(snapshot.entry(20).unwrap());
        let cached = super::CachedForegroundSelection::from_snapshot_with_identities(
            10,
            &snapshot,
            &job,
            snapshot.descendant_signatures(10),
            vec![super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(2),
            }],
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(1),
            },
            super::ProcessIdentity::Stub {
                running: true,
                creation_time: Some(2),
            },
        );

        assert!(cached.is_some());
        assert_eq!(job.processes[0].argv0.as_deref(), Some("codex.exe"));
        assert!(job.processes[0].cmdline.is_none());
    }

    #[test]
    fn windows_foreground_selection_cache_prunes_unused_entries_on_insertion() {
        let first_snapshot = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
        ]);
        let first_job = super::foreground_job_from_entry(first_snapshot.entry(20).unwrap());
        let mut cache = super::ForegroundSelectionCache::default();
        cache.remember_for_test(10, &first_snapshot, &first_job);
        cache.entries.get_mut(&10).unwrap().last_used = Instant::now()
            .checked_sub(super::FOREGROUND_SELECTION_CACHE_RETENTION + Duration::from_secs(1))
            .unwrap();

        let second_snapshot = super::ProcessSnapshot::new(vec![
            test_entry(11, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(21, 11, "claude.exe", &["claude.exe"]),
        ]);
        let second_job = super::foreground_job_from_entry(second_snapshot.entry(21).unwrap());
        cache.remember_for_test(11, &second_snapshot, &second_job);

        assert!(!cache.entries.contains_key(&10));
        assert!(cache.entries.contains_key(&11));
    }

    #[test]
    fn windows_foreground_selection_cache_does_not_retain_shell_result() {
        let snapshot = super::ProcessSnapshot::new(vec![test_entry(
            10,
            1,
            "powershell.exe",
            &["powershell.exe"],
        )]);
        let shell = super::foreground_job_from_entry(snapshot.entry(10).unwrap());
        let mut cache = super::ForegroundSelectionCache::default();

        cache.remember_for_test(10, &snapshot, &shell);

        assert!(cache.entries.is_empty());
    }

    #[test]
    fn windows_foreground_selection_cache_retains_live_escaped_agent() {
        let snapshot = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "bash.exe", &["bash.exe"]),
            test_entry(20, 99, "launcher.exe", &["codex.exe"]),
        ]);
        let escaped = super::foreground_job_from_entry(snapshot.entry(20).unwrap());
        let mut cache = super::ForegroundSelectionCache::default();

        cache.remember_for_test(10, &snapshot, &escaped);

        assert_eq!(cache.get(10, &snapshot), Some(escaped.clone()));
        cache.entries.get_mut(&10).unwrap().selected_identity = super::ProcessIdentity::Stub {
            running: false,
            creation_time: None,
        };
        assert_eq!(cache.get(10, &snapshot), None);
    }

    #[test]
    fn windows_process_tree_selects_wrapped_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "cmd.exe", &["cmd.exe"]),
            test_entry(
                20,
                10,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\codex\\bin\\codex.js",
                ],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "node.exe");
    }

    #[test]
    fn windows_process_tree_selects_cmd_wrapped_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(
                20,
                10,
                "cmd.exe",
                &[
                    "cmd.exe",
                    "/D",
                    "/S",
                    "/C",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\codex.cmd --model gpt-5",
                ],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "cmd.exe");
    }

    #[test]
    fn windows_process_tree_selects_topmost_codex_process_in_single_agent_chain() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(
                20,
                10,
                "node.exe",
                &[
                    "node.exe",
                    "C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\bin\\codex.js",
                ],
            ),
            test_entry(
                30,
                20,
                "codex.exe",
                &["C:\\Users\\herdr\\AppData\\Roaming\\npm\\node_modules\\@openai\\codex\\node_modules\\@openai\\codex-win32-x64\\vendor\\x86_64-pc-windows-msvc\\bin\\codex.exe"],
            ),
            test_entry(40, 30, "node_repl.exe", &["node_repl.exe"]),
            test_entry(
                50,
                40,
                "codex.exe",
                &["codex.exe", "app-server", "--listen", "stdio://"],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "node.exe");
    }

    #[test]
    fn windows_process_tree_keeps_topmost_agent_over_different_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "claude.exe", &["claude.exe"]),
            test_entry(
                30,
                20,
                "cmd.exe",
                &["cmd.exe", "/D", "/S", "/C", "codex mcp-server"],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes[0].name, "claude.exe");
    }

    #[test]
    fn windows_process_tree_keeps_root_agent_over_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "claude.exe", &["claude.exe"]),
            test_entry(
                20,
                10,
                "cmd.exe",
                &["cmd.exe", "/D", "/S", "/C", "codex mcp-server"],
            ),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "claude.exe");
    }

    #[test]
    fn windows_process_tree_returns_shell_for_same_agent_siblings() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "codex.exe", &["codex.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_process_tree_returns_shell_for_plain_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "git.exe", &["git.exe", "status"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_shell_is_available_only_without_descendants() {
        let shell_only = super::ProcessSnapshot::new(vec![test_entry(
            10,
            1,
            "powershell.exe",
            &["powershell.exe"],
        )]);
        assert_eq!(
            super::available_pane_shell_from_snapshot(10, &shell_only).as_deref(),
            Some("powershell.exe")
        );

        let busy = super::ProcessSnapshot::new(vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "git.exe", &["git.exe", "status"]),
        ]);
        assert_eq!(super::available_pane_shell_from_snapshot(10, &busy), None);

        let replaced =
            super::ProcessSnapshot::new(vec![test_entry(10, 1, "vim.exe", &["vim.exe"])]);
        assert_eq!(
            super::available_pane_shell_from_snapshot(10, &replaced),
            None
        );
    }

    #[test]
    fn windows_process_tree_returns_shell_for_multiple_agent_descendants() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "claude.exe", &["claude.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn windows_session_processes_collects_shell_and_descendants() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "cmd.exe", &["cmd.exe"]),
            test_entry(30, 20, "node.exe", &["node.exe"]),
            test_entry(40, 1, "unrelated.exe", &["unrelated.exe"]),
        ];

        let snapshot = super::ProcessSnapshot::new(entries);
        let mut pids = super::session_processes_from_snapshot(10, &snapshot);
        pids.sort_unstable();

        assert_eq!(pids, vec![10, 20, 30]);
    }

    #[test]
    fn windows_process_tree_ignores_pid_reuse_cycles() {
        let entries = vec![
            test_entry(10, 30, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 20, "node.exe", &["node.exe"]),
        ];

        let snapshot = super::ProcessSnapshot::new(entries);
        let descendants = super::descendant_entries(10, &snapshot);

        assert_eq!(
            descendants
                .iter()
                .map(|entry| entry.pid)
                .collect::<Vec<_>>(),
            vec![20, 30]
        );
    }

    #[test]
    fn windows_process_tree_returns_shell_when_candidate_parent_chain_cycles() {
        let entries = vec![
            test_entry(10, 40, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
            test_entry(30, 10, "codex.exe", &["codex.exe"]),
            test_entry(40, 10, "node.exe", &["node.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 10);
        assert_eq!(job.processes[0].name, "powershell.exe");
    }

    #[test]
    fn scrollback_editor_argv_uses_editor_env_and_appends_path() {
        let path = std::path::Path::new(r"C:\Users\User\AppData\Local\Temp\herdr scrollback.txt");
        let argv = super::scrollback_editor_argv_with_env(
            path,
            Some(r#""C:\Program Files\Microsoft VS Code\Code.exe" --wait"#),
        )
        .unwrap();

        assert_eq!(argv[0], r"C:\Program Files\Microsoft VS Code\Code.exe");
        assert_eq!(argv[1], "--wait");
        assert_eq!(argv[2], path.display().to_string());
    }

    #[test]
    fn scrollback_editor_argv_falls_back_to_notepad() {
        let path = std::path::Path::new(r"C:\Temp\herdr-scrollback.txt");
        let argv = super::scrollback_editor_argv_with_env(path, None).unwrap();

        assert_eq!(
            argv,
            vec!["notepad.exe".to_string(), path.display().to_string()]
        );
    }

    fn test_entry(
        pid: u32,
        parent_pid: u32,
        name: &str,
        argv: &[&str],
    ) -> super::WindowsProcessEntry {
        test_entry_with_creation_time(pid, parent_pid, name, argv, None)
    }

    fn test_entry_without_cmdline(
        pid: u32,
        parent_pid: u32,
        name: &str,
        creation_time: u64,
    ) -> super::WindowsProcessEntry {
        let command = super::OnceLock::new();
        command
            .set(super::WindowsProcessCommand::from_cmdline(
                name,
                Some(creation_time),
                None,
            ))
            .unwrap();
        super::WindowsProcessEntry {
            pid,
            parent_pid,
            name: name.to_string(),
            command,
        }
    }

    fn test_entry_with_creation_time(
        pid: u32,
        parent_pid: u32,
        name: &str,
        argv: &[&str],
        creation_time: Option<u64>,
    ) -> super::WindowsProcessEntry {
        let command = super::OnceLock::new();
        command
            .set(super::WindowsProcessCommand {
                creation_time,
                argv0: argv.first().map(|value| (*value).to_string()),
                argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
                cmdline: Some(argv.join(" ")),
            })
            .unwrap();
        super::WindowsProcessEntry {
            pid,
            parent_pid,
            name: name.to_string(),
            command,
        }
    }

    #[test]
    fn process_environment_variable_parser_reads_case_insensitive_marker() {
        let environment: Vec<u16> = "PATH=C:\\Windows\0herdr_pane_runtime_id=pane-a\0\0"
            .encode_utf16()
            .collect();

        assert_eq!(
            super::environment_variable_from_utf16(
                &environment,
                super::PANE_RUNTIME_MARKER_ENV_VAR,
            )
            .as_deref(),
            Some("pane-a")
        );
    }

    #[test]
    fn pane_runtime_markers_are_distinct() {
        let first = super::next_pane_runtime_marker();
        let second = super::next_pane_runtime_marker();

        assert_ne!(first, second);
    }

    #[test]
    fn pane_runtime_marker_is_added_only_to_git_bash_environment() {
        let root = std::env::temp_dir().join(format!(
            "herdr-git-bash-test-{}",
            super::next_pane_runtime_marker()
        ));
        fs::create_dir_all(root.join("bin")).expect("create Git Bash bin fixture");
        fs::create_dir_all(root.join("usr").join("bin")).expect("create Git Bash usr/bin fixture");
        fs::create_dir_all(root.join("cmd")).expect("create Git Bash cmd fixture");
        fs::write(root.join("bin").join("bash.exe"), []).expect("create Bash fixture");
        fs::write(root.join("usr").join("bin").join("msys-2.0.dll"), [])
            .expect("create MSYS runtime fixture");
        fs::write(root.join("cmd").join("git.exe"), []).expect("create Git fixture");

        let mut git_bash = portable_pty::CommandBuilder::new(root.join("bin").join("bash.exe"));
        super::apply_pane_runtime_marker_platform(&mut git_bash);
        let mut path_resolved_git_bash = portable_pty::CommandBuilder::new("bash.exe");
        path_resolved_git_bash.env("PATH", root.join("bin"));
        super::apply_pane_runtime_marker_platform(&mut path_resolved_git_bash);
        let mut cmd = portable_pty::CommandBuilder::new("cmd.exe");
        super::apply_pane_runtime_marker_platform(&mut cmd);

        assert!(git_bash
            .get_env(super::PANE_RUNTIME_MARKER_ENV_VAR)
            .is_some_and(|value| !value.is_empty()));
        assert!(path_resolved_git_bash
            .get_env(super::PANE_RUNTIME_MARKER_ENV_VAR)
            .is_some_and(|value| !value.is_empty()));
        assert!(cmd.get_env(super::PANE_RUNTIME_MARKER_ENV_VAR).is_none());
        fs::remove_dir_all(root).expect("remove Git Bash fixture");
    }

    #[test]
    fn ime_open_reflects_open_status() {
        // IMC_GETOPENSTATUS returns nonzero when the IME is open (Hangul
        // composing) and zero for direct English/ASCII input.
        assert!(super::ime_open(1));
        assert!(!super::ime_open(0));
        // Any nonzero value is treated as open, not just 1.
        assert!(super::ime_open(2));
    }

    #[test]
    fn toggle_key_maps_korean_and_ignores_other_languages() {
        // Korean (0x0412) -> Hangul/English toggle.
        assert_eq!(
            super::toggle_key_for_language(0x0412),
            Some(super::VK_HANGUL)
        );
        // Korean with a different sublanguage still resolves by primary id.
        assert_eq!(
            super::toggle_key_for_language(0x0812),
            Some(super::VK_HANGUL)
        );
        // Japanese (0x0411) and Chinese (0x0804) have no mapped key yet.
        assert_eq!(super::toggle_key_for_language(0x0411), None);
        assert_eq!(super::toggle_key_for_language(0x0804), None);
        // English (0x0409): nothing to toggle.
        assert_eq!(super::toggle_key_for_language(0x0409), None);
    }

    #[test]
    fn send_vk_tap_reports_success_when_full_tap_is_queued() {
        let mut calls = 0;
        let ok = super::send_vk_tap_with(super::VK_HANGUL, |events| {
            calls += 1;
            events.len() as u32
        });
        assert!(ok, "a fully queued tap is reported as success");
        assert_eq!(calls, 1, "a clean tap needs no retry");
    }

    #[test]
    fn send_vk_tap_retries_keyup_and_reports_toggle_on_partial_injection() {
        let mut calls = 0;
        let mut retry_len = 0;
        let mut retry_is_keyup = false;
        let ok = super::send_vk_tap_with(super::VK_HANGUL, |events| {
            calls += 1;
            if calls == 1 {
                // Only the key-down is queued; the key-up is dropped.
                1
            } else {
                retry_len = events.len();
                // SAFETY: keyboard inputs, so reading the `ki` union is valid.
                retry_is_keyup =
                    unsafe { events[0].Anonymous.ki.dwFlags } == super::KEYEVENTF_KEYUP;
                events.len() as u32
            }
        });
        assert!(ok, "the queued key-down may have toggled the IME");
        assert_eq!(calls, 2, "the dropped key-up is retried exactly once");
        assert_eq!(retry_len, 1, "only the key-up is retried");
        assert!(retry_is_keyup, "the retry injects the key-up event");
    }

    #[test]
    fn send_vk_tap_reports_toggle_when_keyup_retry_fails() {
        let mut calls = 0;
        let ok = super::send_vk_tap_with(super::VK_HANGUL, |_events| {
            calls += 1;
            if calls == 1 {
                1
            } else {
                0
            }
        });
        assert!(ok, "the queued key-down may have toggled the IME");
        assert_eq!(calls, 2, "the dropped key-up is retried exactly once");
    }

    #[test]
    fn send_vk_tap_reports_failure_without_retry_when_nothing_is_queued() {
        let mut calls = 0;
        let ok = super::send_vk_tap_with(super::VK_HANGUL, |_events| {
            calls += 1;
            0
        });
        assert!(!ok, "a fully blocked tap is reported as failure");
        assert_eq!(
            calls, 1,
            "nothing was queued, so there is no key-up to retry"
        );
    }

    #[test]
    fn key_tap_inputs_emit_keydown_then_keyup() {
        let inputs = super::key_tap_inputs(super::VK_HANGUL);
        // SAFETY: both entries are keyboard inputs, so reading the `ki` union is valid.
        unsafe {
            assert_eq!(inputs[0].r#type, super::INPUT_KEYBOARD);
            assert_eq!(inputs[0].Anonymous.ki.wVk, super::VK_HANGUL);
            assert_eq!(inputs[0].Anonymous.ki.dwFlags, 0, "first event is key-down");
            assert_eq!(inputs[1].r#type, super::INPUT_KEYBOARD);
            assert_eq!(inputs[1].Anonymous.ki.wVk, super::VK_HANGUL);
            assert_eq!(
                inputs[1].Anonymous.ki.dwFlags,
                super::KEYEVENTF_KEYUP,
                "second event is key-up"
            );
        }
    }
}
