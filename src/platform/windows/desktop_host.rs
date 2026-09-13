use std::{
    ffi::c_void,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    },
};

use interprocess::local_socket::traits::StreamCommon as _;
use windows::{
    core::{Interface as _, BSTR},
    Win32::{
        Foundation::{VARIANT_FALSE, VARIANT_TRUE},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            TaskScheduler::{
                IExecAction, IRunningTask, ITaskFolder, ITaskService, TaskScheduler,
                TASK_ACTION_EXEC, TASK_CREATE, TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA,
                TASK_RUN_USE_SESSION_ID,
            },
            Variant::VARIANT,
        },
    },
};
use windows_sys::Win32::{
    Foundation::HANDLE,
    Security::{
        GetLengthSid, GetTokenInformation, LookupAccountNameW, TokenSessionId, TokenUser,
        SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    },
    System::{
        Console::{
            FreeConsole, GetConsoleProcessList, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
            STD_OUTPUT_HANDLE,
        },
        RemoteDesktop::{
            WTSActive, WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory,
            WTSQuerySessionInformationW, WTSUserName, WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW,
        },
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

const DESKTOP_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const DESKTOP_START_POLL: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) fn detach_remote_desktop_console() -> io::Result<()> {
    let mut process_ids = [0_u32; 2];
    let count =
        unsafe { GetConsoleProcessList(process_ids.as_mut_ptr(), process_ids.len() as u32) };
    if count == 0 {
        return Ok(());
    }
    if count != 1 || process_ids[0] != std::process::id() {
        return Err(io::Error::other(
            "the Windows desktop launch inherited a shared console",
        ));
    }
    if unsafe { FreeConsole() } == 0 {
        return Err(io::Error::last_os_error());
    }
    // FreeConsole leaves stale handles that can be recycled before the startup banner writes.
    // Rust treats null standard handles as detached streams and silently accepts writes.
    for handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        if unsafe { SetStdHandle(handle, std::ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct ProcessIdentity {
    sid: Vec<u8>,
    session_id: u32,
}

#[derive(Debug, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum RemoteDesktopInspection {
    Ready { pid: u32, windows_session: u32 },
    Start { windows_session: u32 },
    Conflict { pid: u32, windows_session: u32 },
    NoLogin,
    MultipleLogins,
}

pub(super) fn desktop_account_sid() -> io::Result<Vec<u8>> {
    Ok(current_process_identity()?.sid)
}

pub(crate) fn inspect_remote_desktop_host() -> io::Result<RemoteDesktopInspection> {
    let current = current_process_identity()?;
    let eligible = eligible_desktop_sessions(&current.sid)?;
    if crate::server::autodetect::is_server_listening() {
        let socket_path = crate::server::socket_paths::client_socket_path();
        let stream = crate::ipc::connect_local_stream(&socket_path)?;
        let (pid, peer) = peer_identity(&stream)?;
        return Ok(
            if peer.sid == current.sid && eligible.contains(&peer.session_id) {
                RemoteDesktopInspection::Ready {
                    pid,
                    windows_session: peer.session_id,
                }
            } else {
                RemoteDesktopInspection::Conflict {
                    pid,
                    windows_session: peer.session_id,
                }
            },
        );
    }
    Ok(match eligible.as_slice() {
        [] => RemoteDesktopInspection::NoLogin,
        [windows_session] => RemoteDesktopInspection::Start {
            windows_session: *windows_session,
        },
        _ => RemoteDesktopInspection::MultipleLogins,
    })
}

pub(crate) fn verify_remote_desktop_stream(stream: &crate::ipc::LocalStream) -> io::Result<()> {
    let (pid, peer) = peer_identity(stream)?;
    let current = current_process_identity()?;
    if peer.sid != current.sid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("remote Herdr server process {pid} belongs to a different Windows account"),
        ));
    }
    if !eligible_desktop_sessions(&current.sid)?.contains(&peer.session_id) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "remote Herdr server process {pid} is not in an active desktop login for this Windows account; use another --session or stop it explicitly"
            ),
        ));
    }
    Ok(())
}

fn peer_identity(stream: &crate::ipc::LocalStream) -> io::Result<(u32, ProcessIdentity)> {
    let pid = stream
        .peer_creds()?
        .pid()
        .ok_or_else(|| io::Error::other("Windows named pipe did not report its server PID"))?;
    Ok((pid, process_identity(pid)?))
}

fn eligible_desktop_sessions(current_sid: &[u8]) -> io::Result<Vec<u32>> {
    let mut sessions = std::ptr::null_mut();
    let mut count = 0_u32;
    if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut sessions, &mut count) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sessions = WtsMemory(sessions.cast::<c_void>());
    let entries = unsafe {
        std::slice::from_raw_parts(sessions.0.cast::<WTS_SESSION_INFOW>(), count as usize)
    };
    let mut eligible = Vec::new();
    for entry in entries.iter().filter(|entry| entry.State == WTSActive) {
        // An unreadable session may belong to this account; do not infer a unique login.
        if session_account_sid(entry.SessionId)?.as_deref() == Some(current_sid) {
            eligible.push(entry.SessionId);
        }
    }
    eligible.sort_unstable();
    eligible.dedup();
    Ok(eligible)
}

fn session_account_sid(session_id: u32) -> io::Result<Option<Vec<u8>>> {
    let user = wts_session_string(session_id, WTSUserName)?;
    if user.is_empty() {
        return Ok(None);
    }
    let domain = wts_session_string(session_id, WTSDomainName)?;
    let account = if domain.is_empty() {
        user
    } else {
        format!(r"{domain}\{user}")
    };
    lookup_account_sid(&account).map(Some)
}

fn wts_session_string(session_id: u32, class: i32) -> io::Result<String> {
    let mut value = std::ptr::null_mut();
    let mut bytes = 0_u32;
    if unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session_id,
            class,
            &mut value,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let value = WtsMemory(value.cast::<c_void>());
    let units = unsafe { std::slice::from_raw_parts(value.0.cast::<u16>(), bytes as usize / 2) };
    let units = units
        .iter()
        .position(|unit| *unit == 0)
        .map(|end| &units[..end])
        .unwrap_or(units);
    String::from_utf16(units).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn lookup_account_sid(account: &str) -> io::Result<Vec<u8>> {
    let account = std::ffi::OsStr::new(account)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut sid_bytes = 0_u32;
    let mut domain_units = 0_u32;
    let mut sid_type: SID_NAME_USE = 0;
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_bytes,
            std::ptr::null_mut(),
            &mut domain_units,
            &mut sid_type,
        );
    }
    if sid_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut sid = vec![0_u8; sid_bytes as usize];
    let mut domain = vec![0_u16; domain_units as usize];
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_bytes,
            domain.as_mut_ptr(),
            &mut domain_units,
            &mut sid_type,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid_len = unsafe { GetLengthSid(sid.as_mut_ptr().cast()) } as usize;
    sid.truncate(sid_len);
    Ok(sid)
}

pub(crate) fn start_remote_desktop_server(windows_session: u32, bootstrap: &str) -> io::Result<()> {
    if !selected_login_is_unique(windows_session)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the selected Windows desktop login is no longer the only active login for this account",
        ));
    }
    let windows_session_i32 = i32::try_from(windows_session).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the selected Windows session identifier is too large",
        )
    })?;

    let _com = ComApartment::initialize()?;
    let service: ITaskService = unsafe {
        CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| task_error("create Task Scheduler service", error))?
    };
    let empty = VARIANT::default();
    unsafe {
        service
            .Connect(&empty, &empty, &empty, &empty)
            .map_err(|error| task_error("connect to Task Scheduler", error))?;
    }
    let root = unsafe {
        service
            .GetFolder(&BSTR::from(r"\"))
            .map_err(|error| task_error("open the Task Scheduler root", error))?
    };
    let definition = unsafe {
        service
            .NewTask(0)
            .map_err(|error| task_error("create a Task Scheduler definition", error))?
    };
    let user = unsafe {
        service
            .ConnectedUser()
            .map_err(|error| task_error("read the Task Scheduler account", error))?
    };
    let principal = unsafe {
        definition
            .Principal()
            .map_err(|error| task_error("configure the Task Scheduler principal", error))?
    };
    unsafe {
        principal
            .SetUserId(&user)
            .and_then(|()| principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN))
            .and_then(|()| principal.SetRunLevel(TASK_RUNLEVEL_LUA))
            .map_err(|error| task_error("configure the desktop task account", error))?;
    }

    let settings = unsafe {
        definition
            .Settings()
            .map_err(|error| task_error("configure desktop task settings", error))?
    };
    unsafe {
        settings
            .SetAllowDemandStart(VARIANT_TRUE)
            .and_then(|()| settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE))
            .and_then(|()| settings.SetStopIfGoingOnBatteries(VARIANT_FALSE))
            .and_then(|()| settings.SetExecutionTimeLimit(&BSTR::from("PT0S")))
            .and_then(|()| settings.SetHidden(VARIANT_TRUE))
            .map_err(|error| task_error("configure desktop task settings", error))?;
    }

    let executable = std::env::current_exe()?;
    let executable = executable
        .to_str()
        .ok_or_else(|| io::Error::other("Herdr executable path is not valid Unicode"))?;
    let working_directory = std::env::current_dir()?;
    let working_directory = working_directory
        .to_str()
        .ok_or_else(|| io::Error::other("SSH working directory is not valid Unicode"))?;
    let arguments = super::desktop_server_args(bootstrap)
        .iter()
        .map(|arg| crate::platform::quote_windows_command_line_arg(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let actions = unsafe {
        definition
            .Actions()
            .map_err(|error| task_error("configure the desktop task action", error))?
    };
    let action: IExecAction = unsafe {
        actions
            .Create(TASK_ACTION_EXEC)
            .and_then(|action| action.cast())
            .map_err(|error| task_error("create the desktop task action", error))?
    };
    unsafe {
        action
            .SetPath(&BSTR::from(executable))
            .and_then(|()| action.SetArguments(&BSTR::from(arguments)))
            .and_then(|()| action.SetWorkingDirectory(&BSTR::from(working_directory)))
            .map_err(|error| task_error("configure the desktop task executable", error))?;
    }

    let task_name = BSTR::from(format!(
        "Herdr Desktop Launch {}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let registered = unsafe {
        root.RegisterTaskDefinition(
            &task_name,
            &definition,
            TASK_CREATE.0,
            &empty,
            &empty,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &empty,
        )
        .map_err(|error| task_error("register the one-time desktop task", error))?
    };

    // Recheck after registration, immediately before selecting the RunEx session.
    let recheck = selected_login_is_unique(windows_session);
    if !matches!(recheck, Ok(true)) {
        unsafe { root.DeleteTask(&task_name, 0) }
            .map_err(|error| task_error("remove the one-time desktop task", error))?;
        return match recheck {
            Ok(false) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the selected Windows desktop login changed before Herdr could start",
            )),
            Err(error) => Err(error),
            Ok(true) => unreachable!("handled above"),
        };
    }
    let running = match unsafe {
        registered.RunEx(
            &empty,
            TASK_RUN_USE_SESSION_ID.0,
            windows_session_i32,
            &BSTR::default(),
        )
    } {
        Ok(running) => running,
        Err(error) => {
            unsafe { root.DeleteTask(&task_name, 0) }
                .map_err(|cleanup| task_error("remove the one-time desktop task", cleanup))?;
            return Err(task_error("run the one-time desktop task", error));
        }
    };
    let mut cleanup = TaskLaunchCleanup {
        root,
        task_name,
        running,
        stop_owned_process: true,
        task_deleted: false,
    };
    let deadline = std::time::Instant::now() + DESKTOP_START_TIMEOUT;
    loop {
        let _ = unsafe { cleanup.running.Refresh() };
        let owned_pid = unsafe { cleanup.running.EnginePID() }.unwrap_or(0);
        match inspect_remote_desktop_host() {
            Ok(RemoteDesktopInspection::Ready {
                pid,
                windows_session: actual,
            }) if actual == windows_session && owned_pid != 0 => {
                cleanup.stop_owned_process = pid != owned_pid;
                unsafe { cleanup.root.DeleteTask(&cleanup.task_name, 0) }
                    .map_err(|error| task_error("remove the one-time desktop task", error))?;
                cleanup.task_deleted = true;
                return Ok(());
            }
            Ok(RemoteDesktopInspection::Conflict { pid, .. }) if pid == owned_pid => {
                return Err(io::Error::other(
                    "the launched Herdr server did not enter the selected Windows desktop",
                ));
            }
            Ok(_) | Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(DESKTOP_START_POLL);
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the Windows desktop Herdr server did not become ready",
                ));
            }
            Err(error) => return Err(error),
        }
    }
}

fn selected_login_is_unique(windows_session: u32) -> io::Result<bool> {
    Ok(
        eligible_desktop_sessions(&current_process_identity()?.sid)?.as_slice()
            == [windows_session],
    )
}

fn task_error(context: &str, error: windows::core::Error) -> io::Error {
    io::Error::other(format!("failed to {context}: {error}"))
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> io::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|error| task_error("initialize COM", error))?;
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct TaskLaunchCleanup {
    root: ITaskFolder,
    task_name: BSTR,
    running: IRunningTask,
    stop_owned_process: bool,
    task_deleted: bool,
}

impl Drop for TaskLaunchCleanup {
    fn drop(&mut self) {
        unsafe {
            if self.stop_owned_process {
                let _ = self.running.Stop();
            }
            if !self.task_deleted {
                let _ = self.root.DeleteTask(&self.task_name, 0);
            }
        }
    }
}

fn current_process_identity() -> io::Result<ProcessIdentity> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_identity(token.as_raw_handle())
}

fn process_identity(pid: u32) -> io::Result<ProcessIdentity> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_identity(token.as_raw_handle())
}

fn token_identity(token: HANDLE) -> io::Result<ProcessIdentity> {
    let mut required = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut user = vec![0_u8; required as usize];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let sid_ptr = unsafe { std::ptr::read_unaligned(user.as_ptr().cast::<TOKEN_USER>()) }
        .User
        .Sid;
    let sid_len = unsafe { GetLengthSid(sid_ptr) } as usize;
    if sid_len == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe { std::slice::from_raw_parts(sid_ptr.cast::<u8>(), sid_len) }.to_vec();

    let mut session_id = 0_u32;
    let mut returned = 0_u32;
    if unsafe {
        GetTokenInformation(
            token,
            TokenSessionId,
            (&mut session_id as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(ProcessIdentity { sid, session_id })
}

struct WtsMemory(*mut c_void);

impl Drop for WtsMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WTSFreeMemory(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write as _, process::Command};
    use windows_sys::Win32::System::Console::GetStdHandle;

    #[test]
    fn desktop_console_detach_clears_stale_standard_handles() {
        const CHILD: &str = "HERDR_TEST_DESKTOP_CONSOLE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let mut process_ids = [0_u32; 2];
            assert_eq!(
                unsafe { GetConsoleProcessList(process_ids.as_mut_ptr(), 2) },
                1
            );
            assert_eq!(process_ids[0], std::process::id());
            detach_remote_desktop_console().expect("detach owned console");
            assert_eq!(
                unsafe { GetConsoleProcessList(process_ids.as_mut_ptr(), 2) },
                0
            );
            for handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                assert!(unsafe { GetStdHandle(handle) }.is_null());
            }
            std::io::stdout()
                .write_all(b"ready\n")
                .expect("detached stdout");
            std::io::stderr()
                .write_all(b"ready\n")
                .expect("detached stderr");
            return;
        }
        let mut child = Command::new(std::env::current_exe().expect("test executable"));
        child
            .arg("desktop_console_detach_clears_stale_standard_handles")
            .env(CHILD, "1");
        crate::platform::configure_background_command(&mut child);
        assert!(child.status().expect("console test child").success());
    }
}
