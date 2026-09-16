use std::ffi::{CStr, OsStr};
use std::io;
use std::process::Command;

use libc::{c_char, c_int, c_void, mach_port_t, uid_t};

const SERVER_CONTEXT_ENV: &str = "HERDR_MACOS_SERVER_CONTEXT";
const USER_CONTEXT: &str = "user";
const TASK_BOOTSTRAP_PORT: c_int = 4;

type GetRoot = unsafe extern "C" fn(mach_port_t, *mut mach_port_t) -> c_int;
type LookupUser =
    unsafe extern "C" fn(mach_port_t, *const c_char, uid_t, *mut mach_port_t) -> c_int;
type SetSpecialPort = unsafe extern "C" fn(mach_port_t, c_int, mach_port_t) -> c_int;
type Deallocate = unsafe extern "C" fn(mach_port_t, mach_port_t) -> c_int;

unsafe extern "C" {
    static mut mach_task_self_: mach_port_t;
    fn task_set_special_port(task: mach_port_t, which: c_int, port: mach_port_t) -> c_int;
    fn mach_port_deallocate(task: mach_port_t, port: mach_port_t) -> c_int;
}

pub(crate) fn configure_server_daemon_context(command: &mut Command) {
    // Handoff inherits the source context, including intentionally direct launches.
    // Older replacements also cannot consume this marker and would leak it.
    if command.get_args().any(|arg| arg == "--handoff-import") {
        command.env_remove(SERVER_CONTEXT_ENV);
    } else {
        command.env(SERVER_CONTEXT_ENV, USER_CONTEXT);
    }
}

/// Called after exec, before logging, worker threads, or pane creation.
pub(crate) fn prepare_server_process(handoff_import: bool) -> io::Result<bool> {
    let requested = std::env::var_os(SERVER_CONTEXT_ENV);
    // This is a one-shot launch request, not environment for panes or plugins.
    std::env::remove_var(SERVER_CONTEXT_ENV);
    if !needs_user_context(requested.as_deref(), handoff_import) {
        return Ok(false);
    }
    let api = BootstrapApi::load()?;
    api.adopt_user_context()?;
    Ok(true)
}

fn needs_user_context(requested: Option<&OsStr>, handoff_import: bool) -> bool {
    // A replacement must preserve its source's policy, not silently rehome a
    // direct server. Fixed sources already pass on the durable context by fork.
    !handoff_import && requested == Some(OsStr::new(USER_CONTEXT))
}

struct BootstrapApi {
    bootstrap: *mut mach_port_t,
    task: mach_port_t,
    get_root: GetRoot,
    lookup_user: LookupUser,
    set_special_port: SetSpecialPort,
    deallocate: Deallocate,
}

impl BootstrapApi {
    fn load() -> io::Result<Self> {
        // These bootstrap interfaces are private macOS APIs, also used by tmux.
        // Resolve all symbols before changing anything so their removal cannot
        // prevent Herdr from launching with its inherited context.
        let bootstrap = symbol(c"bootstrap_port")?.cast::<mach_port_t>();
        let get_root = symbol(c"bootstrap_get_root")?;
        let lookup_user = symbol(c"bootstrap_look_up_per_user")?;
        Ok(Self {
            bootstrap,
            task: unsafe { mach_task_self_ },
            get_root: unsafe { std::mem::transmute::<*mut c_void, GetRoot>(get_root) },
            lookup_user: unsafe { std::mem::transmute::<*mut c_void, LookupUser>(lookup_user) },
            set_special_port: task_set_special_port,
            deallocate: mach_port_deallocate,
        })
    }

    fn adopt_user_context(&self) -> io::Result<()> {
        let inherited = unsafe { *self.bootstrap };
        let mut root = SendRight::new(self);
        check("bootstrap_get_root", unsafe {
            (self.get_root)(inherited, &mut root.port)
        })?;
        root.validate()?;

        let mut user = SendRight::new(self);
        check("bootstrap_look_up_per_user", unsafe {
            (self.lookup_user)(root.port, std::ptr::null(), libc::getuid(), &mut user.port)
        })?;
        user.validate()?;

        // setsid() does not detach from Aqua's Mach bootstrap namespace. Select
        // the per-user endpoint before any children inherit the logout-scoped one.
        check("task_set_bootstrap_port", unsafe {
            (self.set_special_port)(self.task, TASK_BOOTSTRAP_PORT, user.port)
        })?;
        unsafe {
            *self.bootstrap = user.port;
        }
        // The global now owns the lookup's send right, in addition to the task's
        // retained reference. Only release the old global after both are updated.
        user.port = 0;
        drop(SendRight {
            api: self,
            port: inherited,
        });
        Ok(())
    }
}

fn symbol(name: &CStr) -> io::Result<*mut c_void> {
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    if address.is_null() {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "macOS service-context API unavailable: {}",
                name.to_string_lossy()
            ),
        ))
    } else {
        Ok(address)
    }
}

fn check(operation: &str, status: c_int) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{operation} failed (Mach error {status})"
        )))
    }
}

struct SendRight<'a> {
    api: &'a BootstrapApi,
    port: mach_port_t,
}

impl<'a> SendRight<'a> {
    fn new(api: &'a BootstrapApi) -> Self {
        Self { api, port: 0 }
    }

    fn validate(&self) -> io::Result<()> {
        if self.port == 0 || self.port == mach_port_t::MAX {
            Err(io::Error::other("macOS returned an invalid bootstrap port"))
        } else {
            Ok(())
        }
    }
}

impl Drop for SendRight<'_> {
    fn drop(&mut self) {
        if self.port != 0 && self.port != mach_port_t::MAX {
            unsafe {
                (self.api.deallocate)(self.api.task, self.port);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Calls {
        fail: Option<&'static str>,
        installed: Vec<mach_port_t>,
        released: Vec<mach_port_t>,
    }

    thread_local! {
        static CALLS: RefCell<Calls> = RefCell::default();
    }

    unsafe extern "C" fn get_root(_: mach_port_t, out: *mut mach_port_t) -> c_int {
        if CALLS.with(|calls| calls.borrow().fail == Some("root")) {
            return 5;
        }
        unsafe { *out = 20 };
        0
    }

    unsafe extern "C" fn lookup_user(
        _: mach_port_t,
        _: *const c_char,
        _: uid_t,
        out: *mut mach_port_t,
    ) -> c_int {
        let failure = CALLS.with(|calls| calls.borrow().fail);
        if failure == Some("lookup") {
            return 5;
        }
        unsafe { *out = if failure == Some("invalid") { 0 } else { 30 } };
        0
    }

    unsafe extern "C" fn set_special_port(_: mach_port_t, _: c_int, port: mach_port_t) -> c_int {
        CALLS.with(|calls| {
            let mut calls = calls.borrow_mut();
            if calls.fail == Some("install") {
                5
            } else {
                calls.installed.push(port);
                0
            }
        })
    }

    unsafe extern "C" fn deallocate(_: mach_port_t, port: mach_port_t) -> c_int {
        CALLS.with(|calls| calls.borrow_mut().released.push(port));
        0
    }

    fn fake_api(bootstrap: &mut mach_port_t) -> BootstrapApi {
        BootstrapApi {
            bootstrap,
            task: 1,
            get_root,
            lookup_user,
            set_special_port,
            deallocate,
        }
    }

    #[test]
    fn adoption_transfers_user_right_and_releases_old_and_root_rights() {
        CALLS.with(|calls| *calls.borrow_mut() = Calls::default());
        let mut bootstrap = 10;
        fake_api(&mut bootstrap).adopt_user_context().unwrap();
        assert_eq!(bootstrap, 30);
        CALLS.with(|calls| {
            let calls = calls.borrow();
            assert_eq!(calls.installed, [30]);
            assert_eq!(calls.released, [10, 20]);
        });
    }

    #[test]
    fn failed_adoption_keeps_inherited_context_and_releases_temporary_rights() {
        for (failure, released) in [
            ("root", vec![]),
            ("lookup", vec![20]),
            ("invalid", vec![20]),
            ("install", vec![30, 20]),
        ] {
            CALLS.with(|calls| {
                *calls.borrow_mut() = Calls {
                    fail: Some(failure),
                    ..Calls::default()
                }
            });
            let mut bootstrap = 10;
            assert!(fake_api(&mut bootstrap).adopt_user_context().is_err());
            assert_eq!(bootstrap, 10, "{failure}");
            CALLS.with(|calls| {
                let calls = calls.borrow();
                assert!(calls.installed.is_empty(), "{failure}");
                assert_eq!(calls.released, released, "{failure}");
            });
        }
    }

    #[test]
    fn missing_private_symbol_is_a_recoverable_error() {
        let error = symbol(c"herdr_nonexistent_bootstrap_symbol").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn direct_and_handoff_servers_keep_inherited_context() {
        assert!(!needs_user_context(None, false));
        assert!(!needs_user_context(Some(OsStr::new("unknown")), false));
        assert!(needs_user_context(Some(OsStr::new(USER_CONTEXT)), false));
        assert!(!needs_user_context(None, true));
        assert!(!needs_user_context(Some(OsStr::new(USER_CONTEXT)), true));
    }

    #[test]
    fn handoff_commands_remove_even_an_inherited_launch_marker() {
        let mut command = Command::new("herdr");
        command.args(["server", "--handoff-import", "socket", "token"]);
        command.env(SERVER_CONTEXT_ENV, USER_CONTEXT);
        crate::platform::detach_server_daemon_command(&mut command);
        assert!(command
            .get_envs()
            .any(|(key, value)| key == SERVER_CONTEXT_ENV && value.is_none()));
    }

    #[test]
    fn only_detached_server_commands_request_user_context() {
        let direct = Command::new("herdr");
        assert!(!direct.get_envs().any(|(key, _)| key == SERVER_CONTEXT_ENV));
        let mut detached = Command::new("herdr");
        crate::platform::detach_server_daemon_command(&mut detached);
        assert!(detached.get_envs().any(|(key, value)| {
            key == SERVER_CONTEXT_ENV && value == Some(OsStr::new(USER_CONTEXT))
        }));
    }
}
