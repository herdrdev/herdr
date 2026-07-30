use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::c_void,
    mem::{size_of, MaybeUninit},
    path::PathBuf,
    ptr::{copy_nonoverlapping, null_mut},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
    Win32::{
        Foundation::{
            CloseHandle, GlobalFree, LocalFree, HANDLE, HWND, INVALID_HANDLE_VALUE, NTSTATUS,
            STATUS_SUCCESS, UNICODE_STRING,
        },
        System::{
            Console::GetConsoleWindow,
            DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
            },
            JobObjects::IsProcessInJob,
            Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
            Ole::CF_UNICODETEXT,
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, OpenProcess, TerminateProcess,
                CREATE_NO_WINDOW, DETACHED_PROCESS, PROCESS_BASIC_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
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

pub(crate) fn encode_windows_conpty_shift_enter(key: crate::input::TerminalKey) -> Option<Vec<u8>> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

    if key.code != KeyCode::Enter || key.modifiers != KeyModifiers::SHIFT {
        return None;
    }

    let key_down = !matches!(key.kind, KeyEventKind::Release);
    Some(format!("\x1b[13;28;13;{};16;1_", u8::from(key_down)).into_bytes())
}

#[derive(Debug)]
struct CachedProcessSnapshot {
    built_at: Instant,
    entries: Arc<Vec<WindowsProcessEntry>>,
}

#[derive(Debug)]
struct ProcessSnapshotCache {
    cached: Option<CachedProcessSnapshot>,
}

static FOREGROUND_PROCESS_SNAPSHOT_CACHE: Mutex<ProcessSnapshotCache> =
    Mutex::new(ProcessSnapshotCache { cached: None });

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsProcessEntry {
    pid: u32,
    parent_pid: u32,
    name: String,
    argv0: Option<String>,
    argv: Option<Vec<String>>,
    cmdline: Option<String>,
}

pub fn raise_server_nofile_limit() {}

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

pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(DETACHED_PROCESS);
}

pub fn current_process_is_detached_server_daemon() -> bool {
    if !unsafe { GetConsoleWindow() }.is_null() {
        return false;
    }

    let mut in_job = 0;
    unsafe { IsProcessInJob(GetCurrentProcess(), null_mut(), &mut in_job) != 0 && in_job == 0 }
}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let entries = snapshot_processes();
    select_pane_foreground_job(child_pid, &entries)
}

pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    available_pane_shell_from_snapshot(child_pid, &snapshot_processes())
}

fn available_pane_shell_from_snapshot(
    child_pid: u32,
    entries: &[WindowsProcessEntry],
) -> Option<String> {
    let shell = entries.iter().find(|entry| entry.pid == child_pid)?;
    if !super::is_pane_shell_process_name(&shell.name) {
        return None;
    }
    descendant_entries(child_pid, entries)
        .is_empty()
        .then(|| shell.name.clone())
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let entries = cached_foreground_processes();
    let entry = entries.iter().find(|entry| entry.pid == process_group_id)?;
    Some(ForegroundJob {
        process_group_id,
        processes: vec![foreground_process_from_entry(entry)],
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    let entries = cached_foreground_processes();
    select_pane_foreground_job(child_pid, &entries).map(|job| job.process_group_id)
}

pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let process_parameters = read_process_parameters(process.0)?;
    read_unicode_string(process.0, process_parameters.current_directory.dos_path)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn select_pane_foreground_job(
    shell_pid: u32,
    entries: &[WindowsProcessEntry],
) -> Option<ForegroundJob> {
    let shell = entries.iter().find(|entry| entry.pid == shell_pid)?;
    let descendants = descendant_entries(shell_pid, entries);
    let mut candidates = Vec::new();
    for entry in std::iter::once(shell).chain(descendants) {
        if crate::detect::identify_agent_in_job(&foreground_job_from_entry(entry)).is_some() {
            candidates.push(entry);
        }
    }

    let selected = select_topmost_agent_chain_candidate(&candidates, entries).unwrap_or(shell);
    Some(foreground_job_from_entry(selected))
}

fn foreground_job_from_entry(entry: &WindowsProcessEntry) -> ForegroundJob {
    ForegroundJob {
        process_group_id: entry.pid,
        processes: vec![foreground_process_from_entry(entry)],
    }
}

fn select_topmost_agent_chain_candidate<'a>(
    candidates: &[&'a WindowsProcessEntry],
    entries: &[WindowsProcessEntry],
) -> Option<&'a WindowsProcessEntry> {
    let parent_by_pid: HashMap<u32, u32> = entries
        .iter()
        .map(|entry| (entry.pid, entry.parent_pid))
        .collect();

    candidates.iter().copied().find(|entry| {
        candidates.iter().all(|other| {
            entry.pid == other.pid || process_is_ancestor(entry.pid, other.pid, &parent_by_pid)
        })
    })
}

fn process_is_ancestor(
    ancestor_pid: u32,
    descendant_pid: u32,
    parent_by_pid: &HashMap<u32, u32>,
) -> bool {
    let mut current = descendant_pid;
    let mut visited = HashSet::new();
    while visited.insert(current) {
        let Some(parent) = parent_by_pid.get(&current).copied() else {
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

fn descendant_entries(root_pid: u32, entries: &[WindowsProcessEntry]) -> Vec<&WindowsProcessEntry> {
    let mut children: HashMap<u32, Vec<&WindowsProcessEntry>> = HashMap::new();
    for entry in entries {
        children.entry(entry.parent_pid).or_default().push(entry);
    }

    let mut output = Vec::new();
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    visited.insert(root_pid);
    if let Some(root_children) = children.get(&root_pid) {
        for entry in root_children.iter().copied() {
            if visited.insert(entry.pid) {
                queue.push_back(entry);
            }
        }
    }
    while let Some(entry) = queue.pop_front() {
        output.push(entry);
        if let Some(next) = children.get(&entry.pid) {
            for child in next.iter().copied() {
                if visited.insert(child.pid) {
                    queue.push_back(child);
                }
            }
        }
    }
    output
}

fn foreground_process_from_entry(entry: &WindowsProcessEntry) -> super::ForegroundProcess {
    super::ForegroundProcess {
        pid: entry.pid,
        name: entry.name.clone(),
        argv0: entry.argv0.clone(),
        argv: entry.argv.clone(),
        cmdline: entry.cmdline.clone(),
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
        let cmdline = process_command_line(pid);
        let argv = cmdline.as_deref().and_then(command_line_to_argv);
        let argv0 = argv
            .as_ref()
            .and_then(|argv| argv.first().cloned())
            .or_else(|| (!name.is_empty()).then(|| name.clone()));
        output.push(WindowsProcessEntry {
            pid,
            parent_pid: entry.th32ParentProcessID,
            name,
            argv0,
            argv,
            cmdline,
        });
        ok = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    output
}

fn cached_foreground_processes() -> Arc<Vec<WindowsProcessEntry>> {
    let mut cache = FOREGROUND_PROCESS_SNAPSHOT_CACHE
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    cache.snapshot(FOREGROUND_PROCESS_SNAPSHOT_CACHE_TTL, snapshot_processes)
}

impl ProcessSnapshotCache {
    fn snapshot(
        &mut self,
        max_age: Duration,
        build: impl FnOnce() -> Vec<WindowsProcessEntry>,
    ) -> Arc<Vec<WindowsProcessEntry>> {
        if let Some(cached) = &self.cached {
            if cached.built_at.elapsed() < max_age {
                return Arc::clone(&cached.entries);
            }
        }

        let entries = Arc::new(build());
        self.cached = Some(CachedProcessSnapshot {
            built_at: Instant::now(),
            entries: Arc::clone(&entries),
        });
        entries
    }
}

fn process_command_line(pid: u32) -> Option<String> {
    let process = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ)?;
    let parameters = read_process_parameters(process.0)?;
    read_unicode_string(process.0, parameters.command_line)
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

    let entries = snapshot_processes();
    session_processes_from_entries(child_pid, &entries)
}

fn session_processes_from_entries(child_pid: u32, entries: &[WindowsProcessEntry]) -> Vec<u32> {
    if !entries.iter().any(|entry| entry.pid == child_pid) {
        return Vec::new();
    }

    let mut pids = vec![child_pid];
    pids.extend(
        descendant_entries(child_pid, entries)
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

// Windows does not wire clipboard-image bridging into semantic input yet.
#[cfg_attr(windows, allow(dead_code))]
pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
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
    fn windows_notification_text_is_null_terminated_and_unicode_safe() {
        let mut destination = [u16::MAX; 6];
        super::copy_wide_truncated(&mut destination, "abc😀def");

        assert_eq!(String::from_utf16(&destination[..5]).unwrap(), "abc😀");
        assert_eq!(destination[5], 0);
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

        for shell in ["powershell.exe", "cmd.exe"] {
            let capture = base.join(format!("{shell}.txt"));
            let command = super::interactive_shell_command(&argv, shell).unwrap();
            let status = if shell == "cmd.exe" {
                Command::new("cmd.exe")
                    .args(["/d", "/c", &command])
                    .env("PATH", &path)
                    .env("HERDR_ARGV_CAPTURE", &capture)
                    .status()
                    .unwrap()
            } else {
                Command::new("powershell.exe")
                    .args(["-NoLogo", "-NoProfile", "-Command", &command])
                    .env("PATH", &path)
                    .env("HERDR_ARGV_CAPTURE", &capture)
                    .status()
                    .unwrap()
            };
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
    fn windows_process_tree_selects_direct_agent_descendant() {
        let entries = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "codex.exe", &["codex.exe"]),
        ];

        let job = super::select_pane_foreground_job(10, &entries).unwrap();

        assert_eq!(job.process_group_id, 20);
        assert_eq!(job.processes.len(), 1);
        assert_eq!(job.processes[0].name, "codex.exe");
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
        assert_eq!(refreshed[0].pid, 20);
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
        let shell_only = vec![test_entry(10, 1, "powershell.exe", &["powershell.exe"])];
        assert_eq!(
            super::available_pane_shell_from_snapshot(10, &shell_only).as_deref(),
            Some("powershell.exe")
        );

        let busy = vec![
            test_entry(10, 1, "powershell.exe", &["powershell.exe"]),
            test_entry(20, 10, "git.exe", &["git.exe", "status"]),
        ];
        assert_eq!(super::available_pane_shell_from_snapshot(10, &busy), None);

        let replaced = vec![test_entry(10, 1, "vim.exe", &["vim.exe"])];
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

        let mut pids = super::session_processes_from_entries(10, &entries);
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

        let descendants = super::descendant_entries(10, &entries);

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
        super::WindowsProcessEntry {
            pid,
            parent_pid,
            name: name.to_string(),
            argv0: argv.first().map(|value| (*value).to_string()),
            argv: Some(argv.iter().map(|value| (*value).to_string()).collect()),
            cmdline: Some(argv.join(" ")),
        }
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
