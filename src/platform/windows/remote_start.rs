//! Same-account desktop placement for an explicit remote cold start.

use std::{
    io::{self, Write},
    mem::size_of,
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use interprocess::local_socket::{
    prelude::*, GenericNamespaced, ListenerNonblockingMode, ListenerOptions,
};
use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use serde::{Deserialize, Serialize};
use windows::{
    core::{Interface as _, BSTR, HRESULT},
    Win32::{
        Foundation::{VARIANT_FALSE, VARIANT_TRUE},
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            TaskScheduler::{
                IExecAction, ITaskFolder, ITaskService, TaskScheduler, TASK_ACTION_EXEC,
                TASK_CREATE, TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA,
                TASK_RUN_USE_SESSION_ID,
            },
            Variant::VARIANT,
        },
    },
};
use windows_sys::Win32::{
    Foundation::{LocalFree, ERROR_FILE_NOT_FOUND, HANDLE},
    Security::{
        Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
        GetTokenInformation, LookupAccountNameW, TokenElevation, TokenSessionId, TokenUser,
        OWNER_SECURITY_INFORMATION, SID_NAME_USE, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS,
        TOKEN_QUERY, TOKEN_USER,
    },
    System::{
        Console::{
            FreeConsole, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        },
        RemoteDesktop::{
            WTSActive, WTSDisconnected, WTSDomainName, WTSEnumerateSessionsW, WTSFreeMemory,
            WTSQuerySessionInformationW, WTSUserName, WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW,
        },
        Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
    },
};

const BOOTSTRAP_ARG: &str = "--windows-remote-start";
const START_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(25);
// These values identify the requested server. The task supplies its own user environment.
const START_ENV: &[&str] = &[
    "HERDR_CONFIG_PATH",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "HERDR_SESSION",
    "HERDR_SOCKET_PATH",
    "HERDR_CLIENT_SOCKET_PATH",
    "HERDR_STARTUP_CWD",
    "HERDR_START_EPOCH",
];

#[derive(Serialize, Deserialize)]
struct Bootstrap {
    environment: Vec<(String, Option<String>)>,
    cwd: PathBuf,
    explicit_session: Option<String>,
}

impl Bootstrap {
    fn capture(command: &Command) -> io::Result<Self> {
        let mut environment = Vec::new();
        for &key in START_ENV {
            let value = command
                .get_envs()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
                .unwrap_or_else(|| std::env::var_os(key));
            let value = value
                .map(|value| {
                    value
                        .into_string()
                        .map_err(|_| io::Error::other(format!("{key} is not valid Unicode")))
                })
                .transpose()?;
            environment.push((key.to_owned(), value));
        }
        Ok(Self {
            environment,
            cwd: command
                .get_current_dir()
                .map(Path::to_path_buf)
                .map(Ok)
                .unwrap_or_else(std::env::current_dir)?,
            explicit_session: crate::session::explicit_session_requested()
                .then(|| crate::session::active_name().unwrap_or_else(|| "default".to_owned())),
        })
    }

    fn apply(self, program: String) -> io::Result<Vec<String>> {
        std::env::set_current_dir(self.cwd)?;
        for (key, value) in self.environment {
            if !START_ENV.contains(&key.as_str()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected remote startup setting",
                ));
            }
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let mut args = vec![program];
        if let Some(session) = self.explicit_session {
            args.extend(["--session".to_owned(), session]);
        }
        args.push("server".to_owned());
        Ok(args)
    }
}

pub(crate) fn bootstrap_remote_server(args: Vec<String>) -> io::Result<Vec<String>> {
    if args.get(1).map(String::as_str) != Some(BOOTSTRAP_ARG) {
        return Ok(args);
    }
    let [program, _, pipe] = args.as_slice() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Windows remote server startup",
        ));
    };
    let _cleanup = TaskActionCleanup(pipe);
    let mut stream = crate::ipc::connect_local_stream(Path::new(pipe))?;
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + START_TIMEOUT;
    let mut length = [0; 4];
    read_until(&mut stream, &mut length, deadline)?;
    let length = u32::from_le_bytes(length) as usize;
    // Windows environment values are bounded; this also bounds a malformed local request.
    if length > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote startup settings are too large",
        ));
    }
    let mut payload = vec![0; length];
    read_until(&mut stream, &mut payload, deadline)?;
    let result = (|| {
        let identity = process_identity(std::process::id())?;
        if identity.session == 0 || identity.elevated {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote server must start in an ordinary signed-in Windows session",
            ));
        }
        verify_starter_owner(&stream, &identity.sid)?;
        detach_console()?;
        let bootstrap: Bootstrap = serde_json::from_slice(&payload).map_err(io::Error::other)?;
        bootstrap.apply(program.clone())
    })();
    write_until(&mut stream, &[u8::from(result.is_ok())], deadline)?;
    if let Err(error) = &result {
        let message = error.to_string();
        write_until(&mut stream, &(message.len() as u32).to_le_bytes(), deadline)?;
        write_until(&mut stream, message.as_bytes(), deadline)?;
    }
    result
}

pub(crate) fn launch_remote_server_daemon_command(
    command: &mut Command,
    allow_headless: bool,
) -> io::Result<u32> {
    let identity = process_identity(std::process::id())?;
    let sessions = eligible_sessions(&identity.sid)?;
    let session = match sessions.as_slice() {
        [] => {
            require_headless_remote_start(allow_headless)?;
            eprintln!("Starting Herdr in the SSH session; Codex sandbox, computer use and desktop tools may fail.");
            tracing::warn!("No signed-in Windows session; starting remote server in the SSH session");
            return super::launch_server_daemon_command(command);
        }
        [session] => *session,
        _ => return Err(io::Error::other("multiple signed-in Windows sessions belong to this account; start Herdr in the intended session first")),
    };
    let bootstrap = serde_json::to_vec(&Bootstrap::capture(command)?).map_err(io::Error::other)?;
    let name = format!(
        "herdr-remote-start-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos()
    );
    let listener = ListenerOptions::new()
        .name(name.as_str().to_ns_name::<GenericNamespaced>()?)
        .security_descriptor(startup_security_descriptor(&identity.sid)?)
        .create_sync()?;
    listener.set_nonblocking(ListenerNonblockingMode::Both)?;
    let task = start_task(command, &identity.sid, session, &name)?;
    let outcome = (|| {
        let deadline = Instant::now() + START_TIMEOUT;
        let mut stream = accept_task_action(&listener, deadline)?;
        let pid = peer_pid(&stream)?;
        let peer = process_identity(pid)?;
        if peer.sid != identity.sid || peer.session != session || peer.elevated {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows remote server did not enter the intended account and login session",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows remote start expired before admission",
            ));
        }
        // No fallback after admission: a missing readiness response is an uncertain launch.
        write_until(
            &mut stream,
            &(bootstrap.len() as u32).to_le_bytes(),
            deadline,
        )?;
        write_until(&mut stream, &bootstrap, deadline)?;
        let mut ready = [0];
        read_until(&mut stream, &mut ready, deadline)?;
        if ready != [1] {
            let mut length = [0; 4];
            read_until(&mut stream, &mut length, deadline)?;
            let length = (u32::from_le_bytes(length) as usize).min(4096);
            let mut message = vec![0; length];
            read_until(&mut stream, &mut message, deadline)?;
            return Err(io::Error::other(format!(
                "the Windows server could not prepare its signed-in session: {}",
                String::from_utf8_lossy(&message)
            )));
        }
        Ok(pid)
    })();
    finish_start_attempt(outcome, task.remove())
}

fn finish_start_attempt<T>(startup: io::Result<T>, cleanup: io::Result<()>) -> io::Result<T> {
    match (startup, cleanup) {
        (Err(startup), Err(cleanup)) => Err(io::Error::new(
            startup.kind(),
            format!("{startup}; startup task cleanup also failed: {cleanup}"),
        )),
        (Ok(_), Err(cleanup)) => Err(io::Error::new(
            cleanup.kind(),
            format!("could not remove temporary startup task: {cleanup}"),
        )),
        (startup, Ok(())) => startup,
    }
}

pub(crate) struct WindowsSignInRequired;

impl std::fmt::Debug for WindowsSignInRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::fmt::Display for WindowsSignInRequired {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Waiting for Windows sign-in. Sign in on the Windows host with the same account.",
        )
    }
}

impl std::error::Error for WindowsSignInRequired {}

pub(crate) fn is_windows_sign_in_required(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|cause| cause.is::<WindowsSignInRequired>())
}

fn require_headless_remote_start(allow_headless: bool) -> io::Result<()> {
    let loaded = crate::config::Config::load();
    if let Some(summary) = crate::config::config_diagnostic_summary(&loaded.diagnostics) {
        eprintln!("{summary}");
    }
    if allow_headless || loaded.config.server.allow_headless_remote_start {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            WindowsSignInRequired,
        ))
    }
}

fn peer_pid(stream: &crate::ipc::LocalStream) -> io::Result<u32> {
    stream
        .peer_creds()?
        .pid()
        .ok_or_else(|| io::Error::other("Windows startup pipe did not report its peer process"))
}

fn accept_task_action(
    listener: &crate::ipc::LocalListener,
    deadline: Instant,
) -> io::Result<crate::ipc::LocalStream> {
    loop {
        match listener.accept() {
            Ok(stream) => return Ok(stream),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, format!("Herdr did not start in the signed-in Windows session within {}s; try connecting again", START_TIMEOUT.as_secs())));
                }
                std::thread::sleep(POLL);
            }
            Err(error) => return Err(error),
        }
    }
}

fn verify_starter_owner(stream: &crate::ipc::LocalStream, account: &str) -> io::Result<()> {
    let crate::ipc::LocalStream::NamedPipe(pipe) = stream;
    let mut owner = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let error = unsafe {
        GetSecurityInfo(
            pipe.as_handle().as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let owner = sid_string(owner);
    unsafe { LocalFree(descriptor.cast()) };
    let owner = owner?;
    if owner != account {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "remote starter belongs to another Windows account",
        ));
    }
    Ok(())
}

fn startup_security_descriptor(account: &str) -> io::Result<SecurityDescriptor> {
    // Bind ownership to the account even when SSH's elevated token owner is Administrators.
    let sddl = widestring::U16CString::from_str(format!(
        "O:{account}D:P(A;;GA;;;SY)(A;;GRGW;;;{account})S:(ML;;NRNW;;;ME)"
    ))
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    SecurityDescriptor::deserialize(&sddl)
}

fn read_until(
    stream: &mut crate::ipc::LocalStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        match crate::ipc::poll_local_stream_read_count(stream, bytes) {
            Ok(crate::ipc::LocalStreamReadCount::Closed) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Windows remote starter disconnected",
                ))
            }
            Ok(crate::ipc::LocalStreamReadCount::Data(count)) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Ok(crate::ipc::LocalStreamReadCount::Pending) => std::thread::sleep(POLL),
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows remote start did not complete in time",
            ));
        }
    }
    Ok(())
}

fn write_until(
    stream: &mut crate::ipc::LocalStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        // Keep each nonblocking write within the local socket's 512-byte buffer.
        match stream.write(&bytes[..bytes.len().min(512)]) {
            // PIPE_NOWAIT reports zero bytes when the pipe buffer is full.
            Ok(0) => std::thread::sleep(POLL),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(POLL)
            }
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Windows remote start did not complete in time",
            ));
        }
    }
    Ok(())
}

fn detach_console() -> io::Result<()> {
    // FreeConsole also succeeds when the task has no console.
    if unsafe { FreeConsole() } == 0 {
        return Err(io::Error::last_os_error());
    }
    if !super::current_process_is_detached_server_daemon() {
        return Err(io::Error::other(
            "Windows remote server is still tied to its launcher",
        ));
    }
    let null = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("NUL")?
        .into_raw_handle();
    // The process owns this handle for the lifetime of its standard streams.
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        if unsafe { SetStdHandle(which, null) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn start_task(
    command: &Command,
    sid: &str,
    session: u32,
    pipe: &str,
) -> io::Result<TaskRegistration> {
    let (_com, service) = task_service()?;
    let empty = VARIANT::default();
    let root = unsafe { service.GetFolder(&BSTR::from(r"\")) }.map_err(task_error)?;
    let definition = unsafe { service.NewTask(0) }.map_err(task_error)?;
    let principal = unsafe { definition.Principal() }.map_err(task_error)?;
    unsafe {
        principal
            .SetUserId(&BSTR::from(sid))
            .and_then(|()| principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN))
            .and_then(|()| principal.SetRunLevel(TASK_RUNLEVEL_LUA))
    }
    .map_err(task_error)?;
    let settings = unsafe { definition.Settings() }.map_err(task_error)?;
    unsafe {
        settings
            .SetAllowDemandStart(VARIANT_TRUE)
            .and_then(|()| settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE))
            .and_then(|()| settings.SetStopIfGoingOnBatteries(VARIANT_FALSE))
            .and_then(|()| settings.SetRunOnlyIfIdle(VARIANT_FALSE))
            .and_then(|()| settings.SetRestartCount(0))
            .and_then(|()| settings.SetExecutionTimeLimit(&BSTR::from("PT0S")))
            .and_then(|()| settings.SetHidden(VARIANT_TRUE))
    }
    .map_err(task_error)?;
    let actions = unsafe { definition.Actions() }.map_err(task_error)?;
    let action: IExecAction = unsafe {
        actions
            .Create(TASK_ACTION_EXEC)
            .and_then(|action| action.cast())
    }
    .map_err(task_error)?;
    let executable = command
        .get_program()
        .to_str()
        .ok_or_else(|| io::Error::other("Herdr executable path is not valid Unicode"))?;
    let arguments = format!(
        "{} {}",
        BOOTSTRAP_ARG,
        super::super::quote_windows_command_line_arg(pipe)
    );
    unsafe {
        action
            .SetPath(&BSTR::from(executable))
            .and_then(|()| action.SetArguments(&BSTR::from(arguments)))
    }
    .map_err(task_error)?;
    let name = task_name(pipe);
    let security = VARIANT::from(BSTR::from(format!("O:{sid}D:P(A;;GA;;;SY)(A;;GA;;;{sid})")));
    let registered = unsafe {
        root.RegisterTaskDefinition(
            &name,
            &definition,
            TASK_CREATE.0,
            &empty,
            &empty,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &security,
        )
    }
    .map_err(task_error)?;
    let registration = TaskRegistration {
        root,
        name,
        removed: false,
        _com,
    };
    let outcome = (|| {
        // Session choice is discovered for this account; never guess Session 1 or the console user.
        if eligible_sessions(sid)?.as_slice() != [session] {
            return Err(io::Error::other(
                "the signed-in Windows session changed before Herdr could start; retry",
            ));
        }
        let session = i32::try_from(session).map_err(io::Error::other)?;
        unsafe { registered.RunEx(&empty, TASK_RUN_USE_SESSION_ID.0, session, &BSTR::default()) }
            .map(|_| ())
            .map_err(task_error)
    })();
    if let Err(startup) = outcome {
        return finish_start_attempt(Err(startup), registration.remove());
    }
    Ok(registration)
}

struct TaskRegistration {
    root: ITaskFolder,
    name: BSTR,
    removed: bool,
    _com: ComApartment,
}

impl TaskRegistration {
    fn remove(mut self) -> io::Result<()> {
        remove_task(&self.root, &self.name)?;
        self.removed = true;
        Ok(())
    }
}

impl Drop for TaskRegistration {
    fn drop(&mut self) {
        if !self.removed {
            // Removing a registration never stops its admitted server process.
            if let Err(error) = remove_task(&self.root, &self.name) {
                tracing::warn!(%error, "could not remove temporary remote start registration");
            }
        }
    }
}

struct TaskActionCleanup<'a>(&'a str);

impl Drop for TaskActionCleanup<'_> {
    fn drop(&mut self) {
        let result = (|| {
            let (_com, service) = task_service()?;
            let root = unsafe { service.GetFolder(&BSTR::from(r"\")) }.map_err(task_error)?;
            remove_task(&root, &task_name(self.0))
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "could not remove temporary remote start registration");
        }
    }
}

fn task_name(pipe: &str) -> BSTR {
    BSTR::from(format!("Herdr Remote Start {pipe}"))
}

fn remove_task(root: &ITaskFolder, name: &BSTR) -> io::Result<()> {
    match unsafe { root.DeleteTask(name, 0) } {
        Err(error) if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND) => Ok(()),
        result => result.map_err(task_error),
    }
}

fn task_service() -> io::Result<(ComApartment, ITaskService)> {
    let com = ComApartment::initialize()?;
    let service: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
            .map_err(task_error)?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }.map_err(task_error)?;
    Ok((com, service))
}

fn task_error(error: windows::core::Error) -> io::Error {
    io::Error::other(format!(
        "Windows could not start Herdr in the signed-in session: {error}"
    ))
}

struct ComApartment;
impl ComApartment {
    fn initialize() -> io::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(task_error)?;
        Ok(Self)
    }
}
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct ProcessIdentity {
    sid: String,
    session: u32,
    elevated: bool,
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
    let mut needed = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut user = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            user.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let user = unsafe { &*user.as_ptr().cast::<TOKEN_USER>() };
    let sid = sid_string(user.User.Sid)?;
    let mut session = 0_u32;
    token_information(token.as_raw_handle(), TokenSessionId, &mut session)?;
    let mut elevation = TOKEN_ELEVATION::default();
    token_information(token.as_raw_handle(), TokenElevation, &mut elevation)?;
    Ok(ProcessIdentity {
        sid,
        session,
        elevated: elevation.TokenIsElevated != 0,
    })
}

fn token_information<T>(
    token: HANDLE,
    class: TOKEN_INFORMATION_CLASS,
    value: &mut T,
) -> io::Result<()> {
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token,
            class,
            (value as *mut T).cast(),
            size_of::<T>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn sid_string(sid: *mut std::ffi::c_void) -> io::Result<String> {
    let mut value = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut value) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let text = unsafe { widestring::U16CStr::from_ptr_str(value) }.to_string_lossy();
    unsafe { LocalFree(value.cast()) };
    Ok(text)
}

fn eligible_sessions(sid: &str) -> io::Result<Vec<u32>> {
    let mut sessions = std::ptr::null_mut();
    let mut count = 0;
    if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut sessions, &mut count) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let memory = WtsMemory(sessions.cast());
    let entries = if count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(memory.0.cast::<WTS_SESSION_INFOW>(), count as usize) }
    };
    let mut eligible = Vec::new();
    for entry in entries {
        if entry.SessionId != 0
            && (entry.State == WTSActive || entry.State == WTSDisconnected)
            && session_sid(entry.SessionId)?.as_deref() == Some(sid)
        {
            eligible.push(entry.SessionId);
        }
    }
    Ok(eligible)
}

fn session_sid(session: u32) -> io::Result<Option<String>> {
    let user = wts_string(session, WTSUserName)?;
    if user.is_empty() {
        return Ok(None);
    }
    let domain = wts_string(session, WTSDomainName)?;
    let account = if domain.is_empty() {
        user
    } else {
        format!(r"{domain}\{user}")
    };
    use std::os::windows::ffi::OsStrExt as _;
    let account: Vec<u16> = std::ffi::OsStr::new(&account)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut sid_bytes = 0;
    let mut domain_units = 0;
    let mut kind: SID_NAME_USE = 0;
    unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_bytes,
            std::ptr::null_mut(),
            &mut domain_units,
            &mut kind,
        )
    };
    if sid_bytes == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut sid = vec![0usize; (sid_bytes as usize).div_ceil(size_of::<usize>())];
    let mut domain = vec![0u16; domain_units as usize];
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_bytes,
            domain.as_mut_ptr(),
            &mut domain_units,
            &mut kind,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid_string(sid.as_mut_ptr().cast()).map(Some)
}

fn wts_string(session: u32, class: i32) -> io::Result<String> {
    let mut value = std::ptr::null_mut();
    let mut bytes = 0;
    if unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session,
            class,
            &mut value,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let memory = WtsMemory(value.cast());
    if bytes == 0 {
        return Ok(String::new());
    }
    let units = unsafe { std::slice::from_raw_parts(memory.0.cast::<u16>(), bytes as usize / 2) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    String::from_utf16(&units[..end])
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

struct WtsMemory(*mut std::ffi::c_void);
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

    #[test]
    fn start_attempt_keeps_startup_primary_and_cleanup_failure_visible() {
        let error = finish_start_attempt::<u32>(
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "wrong startup identity",
            )),
            Err(io::Error::other("scheduler unavailable")),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("wrong startup identity"));
        assert!(error.to_string().contains("scheduler unavailable"));
        let error = finish_start_attempt::<u32>(
            Ok(42),
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cleanup denied",
            )),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("cleanup denied"));
        let error = finish_start_attempt::<u32>(
            Err(io::Error::new(io::ErrorKind::TimedOut, "startup timed out")),
            Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "startup timed out");
        assert_eq!(finish_start_attempt(Ok(42), Ok(())).unwrap(), 42);
    }

    #[test]
    fn no_login_fallback_requires_valid_host_config_opt_in() {
        const CASE: &str = "HERDR_TEST_HEADLESS_REMOTE_START_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let result = require_headless_remote_start(false);
            if case == "enabled" {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                assert!(is_windows_sign_in_required(&error));
                assert!(error.to_string().contains("Waiting for Windows sign-in"));
                // main's Result termination reports io::Error through Debug.
                assert!(format!("{error:?}").contains("Waiting for Windows sign-in"));
                require_headless_remote_start(true).unwrap();
            }
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "herdr-no-login-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        for (case, content) in [
            ("missing", None),
            ("disabled", Some("[server]\nallow_headless_remote_start = false\n")),
            ("enabled", Some("[server]\nallow_headless_remote_start = true\nfuture_option = true\n[future]\noption = true\n")),
            ("wrong_type", Some("[server]\nallow_headless_remote_start = 'true'\n")),
            ("invalid_other_field", Some("[server]\nallow_headless_remote_start = true\nheadless_cols = 'wide'\n")),
            ("malformed", Some("[server]\nallow_headless_remote_start = true\n[")),
            ("unreadable", None),
        ] {
            let path = root.join(case);
            if let Some(content) = content {
                std::fs::write(&path, content).unwrap();
            } else if case == "unreadable" {
                std::fs::create_dir(&path).unwrap();
            }
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "platform::windows::remote_start::tests::no_login_fallback_requires_valid_host_config_opt_in", "--nocapture"])
                .env(CASE, case)
                .env(crate::config::CONFIG_PATH_ENV_VAR, path)
                .status().unwrap();
            assert!(status.success(), "no-login policy case {case} failed");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_task_registration_is_already_clean() {
        let (_com, service) = task_service().unwrap();
        let root = unsafe { service.GetFolder(&BSTR::from(r"\")) }.unwrap();
        let name = task_name(&format!(
            "cleanup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        remove_task(&root, &name).unwrap();
    }

    #[test]
    fn remote_bootstrap_preserves_identity_precedence_and_cwd() {
        const CASE: &str = "HERDR_TEST_REMOTE_BOOTSTRAP_CASE";
        if let Ok(case) = std::env::var(CASE) {
            let cwd = std::env::temp_dir();
            let mut command = Command::new("herdr.exe");
            command.current_dir(&cwd);
            let explicit = case != "inherited";
            let session = if case == "default" {
                "default"
            } else {
                "named"
            };
            let mut args = vec!["herdr.exe".to_owned(), "server".to_owned()];
            if explicit {
                args.extend(["--session".to_owned(), session.to_owned()]);
                command
                    .env_remove("HERDR_SOCKET_PATH")
                    .env_remove("HERDR_CLIENT_SOCKET_PATH");
            }
            crate::session::configure_from_args(&args).unwrap();
            let bootstrap = Bootstrap::capture(&command).unwrap();
            std::env::remove_var("HERDR_SESSION");
            std::env::remove_var("HERDR_SOCKET_PATH");
            std::env::remove_var("HERDR_CLIENT_SOCKET_PATH");
            let args = bootstrap.apply("herdr.exe".to_owned()).unwrap();
            crate::session::configure_from_args(&args).unwrap();
            assert_eq!(std::env::current_dir().unwrap(), cwd);
            assert_eq!(crate::session::explicit_session_requested(), explicit);
            if explicit {
                assert_eq!(
                    crate::api::socket_path(),
                    crate::session::api_socket_path_for((session != "default").then_some(session))
                );
                assert!(std::env::var_os("HERDR_CLIENT_SOCKET_PATH").is_none());
            } else {
                assert_eq!(crate::api::socket_path(), PathBuf::from("custom-api.sock"));
                assert_eq!(
                    crate::server::socket_paths::client_socket_path(),
                    PathBuf::from("custom-api-client.sock")
                );
                assert_eq!(crate::session::active_name().as_deref(), Some("named"));
            }
            assert_eq!(std::env::var("HERDR_START_EPOCH").unwrap(), "42");
            return;
        }
        for case in ["inherited", "default", "named"] {
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "platform::windows::remote_start::tests::remote_bootstrap_preserves_identity_precedence_and_cwd", "--nocapture"])
                .env(CASE, case)
                .env("HERDR_SESSION", "named")
                .env("HERDR_SOCKET_PATH", "custom-api.sock")
                .env("HERDR_CLIENT_SOCKET_PATH", "custom-client.sock")
                .env("HERDR_START_EPOCH", "42")
                .status().unwrap();
            assert!(status.success(), "bootstrap case {case} failed");
        }
    }

    #[test]
    fn admission_waits_for_data_and_rejects_incomplete_payload() {
        let name = format!("herdr-start-admission-test-{}", std::process::id());
        let account = process_identity(std::process::id()).unwrap().sid;
        let listener = ListenerOptions::new()
            .name(name.as_str().to_ns_name::<GenericNamespaced>().unwrap())
            .security_descriptor(startup_security_descriptor(&account).unwrap())
            .create_sync()
            .unwrap();
        listener
            .set_nonblocking(ListenerNonblockingMode::Both)
            .unwrap();
        assert_eq!(
            accept_task_action(&listener, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let mut client = crate::ipc::connect_local_stream(Path::new(&name)).unwrap();
        let mut server = accept_task_action(&listener, Instant::now() + START_TIMEOUT).unwrap();
        verify_starter_owner(&client, &account).unwrap();
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            write_until(&mut client, &[7; 64 * 1024], Instant::now() + START_TIMEOUT).unwrap();
            write_until(&mut client, &[5, 6], Instant::now() + START_TIMEOUT).unwrap();
        });
        let mut received = [0; 64 * 1024];
        read_until(&mut server, &mut received, Instant::now() + START_TIMEOUT).unwrap();
        assert!(received.iter().all(|&byte| byte == 7));
        let mut payload = [0; 4];
        let error =
            read_until(&mut server, &mut payload, Instant::now() + START_TIMEOUT).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
        ));
        writer.join().unwrap();
    }
}
