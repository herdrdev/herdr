use super::*;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

fn test_app() -> (super::super::launchd_spawn::PrivateDir, PathBuf) {
    let dir = super::super::launchd_spawn::PrivateDir::create_in(&std::env::temp_dir()).unwrap();
    let app = dir.path().join("Herdr Server.app");
    ensure_app(&app).unwrap();
    (dir, app)
}

fn wait_child(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("server app did not exit");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn repeat_start_preserves_the_signed_helper_identity() {
    let (_dir, app) = test_app();
    let path = app.join(EXECUTABLE);
    let before = std::fs::metadata(&path).unwrap();
    let signed_binary = std::fs::read(&path).unwrap();
    ensure_app(&app).unwrap();
    let after = std::fs::metadata(&path).unwrap();
    assert_eq!(before.ino(), after.ino());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(signed_binary, std::fs::read(path).unwrap());
}

#[test]
fn damaged_bundle_is_reported_without_replacing_a_privacy_identity() {
    let (_dir, app) = test_app();
    let plist = app.join("Contents/Info.plist");
    std::fs::write(&plist, b"damaged").unwrap();
    assert!(ensure_app(&app).is_err());
    assert_eq!(std::fs::read(plist).unwrap(), b"damaged");
}

#[test]
fn wrapper_preserves_arguments_working_directory_and_environment_removals() {
    use std::ffi::OsStr;
    let mut command = Command::new("/path with spaces/herdr");
    command.args(["server", "a&b"]).current_dir("/tmp");
    command
        .env("HERDR_SESSION", "work")
        .env_remove("HERDR_SOCKET_PATH");
    let wrapped = wrap_command(&command, Path::new("/test/helper"));
    assert_eq!(wrapped.get_program(), "/test/helper");
    assert_eq!(
        wrapped.get_args().collect::<Vec<_>>(),
        ["/path with spaces/herdr", "server", "a&b"]
    );
    assert_eq!(wrapped.get_current_dir(), Some(Path::new("/tmp")));
    assert!(wrapped
        .get_envs()
        .any(|(key, value)| key == "HERDR_SESSION" && value == Some(OsStr::new("work"))));
    assert!(wrapped
        .get_envs()
        .any(|(key, value)| key == "HERDR_SOCKET_PATH" && value.is_none()));
}

#[test]
fn launcher_keeps_identity_alive_after_the_original_server_exits() {
    let (dir, app) = test_app();
    let out = dir.path().join("replacement-finished");
    let mut child = Command::new(app.join(EXECUTABLE))
        .args([
            "/bin/sh",
            "-c",
            "(sleep 0.5; echo done > \"$1\") & exit 0",
            "sh",
        ])
        .arg(&out)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the replacement still needs the responsible app"
    );
    assert!(wait_child(&mut child).success());
    assert_eq!(std::fs::read_to_string(out).unwrap().trim(), "done");
}

#[test]
fn launcher_returns_server_failure_and_forwards_termination() {
    let (_dir, app) = test_app();
    let helper = app.join(EXECUTABLE);
    let mut failed = Command::new(&helper)
        .args(["/bin/sh", "-c", "exit 42"])
        .spawn()
        .unwrap();
    assert_eq!(wait_child(&mut failed).code(), Some(42));
    let mut child = Command::new(helper)
        .args(["/bin/sleep", "30"])
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // SAFETY: signals only the disposable helper owned by this test.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGTERM) }, 0);
    assert_eq!(wait_child(&mut child).code(), Some(128 + libc::SIGTERM));
}

#[test]
#[ignore = "subprocess of launchd_identity_survives_server_replacement"]
// The original exits before its replacement by design; launchd reaps the orphan.
#[allow(clippy::zombie_processes)]
fn responsibility_probe() {
    let Some(out) = std::env::var_os("HERDR_APP_PROBE_OUT") else {
        return;
    };
    if let Ok(previous) = std::env::var("HERDR_APP_PREVIOUS_SERVER") {
        let pid: i32 = previous.parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        // SAFETY: kill with signal zero only checks whether the test's
        // original process has exited and been reaped.
        while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    type Responsible = unsafe extern "C" fn(i32) -> i32;
    // SAFETY: resolve the macOS responsibility query used by the privacy
    // subsystem, and call it with the current process ID.
    let (parent, responsible) = unsafe {
        let symbol = libc::dlsym(
            libc::RTLD_DEFAULT,
            c"responsibility_get_pid_responsible_for_pid".as_ptr(),
        );
        assert!(!symbol.is_null());
        let query = std::mem::transmute::<*mut std::ffi::c_void, Responsible>(symbol);
        (libc::getppid(), query(libc::getpid()))
    };
    std::fs::write(
        out,
        format!("{} {parent} {responsible}", std::process::id()),
    )
    .unwrap();
    if let Some(replacement_out) = std::env::var_os("HERDR_APP_REPLACEMENT_OUT") {
        use std::os::unix::process::CommandExt;
        let mut replacement = probe_command();
        replacement
            .env("HERDR_APP_PROBE_OUT", replacement_out)
            .env_remove("HERDR_APP_REPLACEMENT_OUT")
            .env("HERDR_APP_PREVIOUS_SERVER", std::process::id().to_string());
        // SAFETY: this runs only setsid in the forked test child, as live
        // handoff does before executing its replacement server.
        unsafe {
            replacement.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        replacement.spawn().unwrap();
    } else {
        let release = PathBuf::from(std::env::var_os("HERDR_APP_PROBE_RELEASE").unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn probe_command() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "platform::macos::server_app::tests::responsibility_probe",
            "--ignored",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[test]
fn launchd_identity_survives_server_replacement() {
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    if !Command::new("/bin/launchctl")
        .args(["print", &domain])
        .output()
        .unwrap()
        .status
        .success()
        || std::env::var_os("SSH_CONNECTION").is_some()
    {
        eprintln!("skipping: no GUI launchd domain");
        return;
    }
    let (dir, app) = test_app();
    let initial = dir.path().join("initial");
    let replacement = dir.path().join("replacement");
    let release = dir.path().join("release");
    let mut probe = probe_command();
    probe
        .env("HERDR_APP_PROBE_OUT", &initial)
        .env("HERDR_APP_REPLACEMENT_OUT", &replacement)
        .env("HERDR_APP_PROBE_RELEASE", &release);
    let command = wrap_command(&probe, &app.join(EXECUTABLE));
    let label = super::super::launchd_spawn::spawn_launchd_job(&command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !replacement.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // Release and unload only this test's launcher before assertions, so
    // a failed attribution check cannot leave an app running indefinitely.
    std::fs::write(release, b"done").unwrap();
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .output();
    let parse = |path: &Path| -> Vec<i32> {
        std::fs::read_to_string(path)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse().unwrap())
            .collect()
    };
    let initial = parse(&initial);
    let replacement = parse(&replacement);
    assert_eq!(
        initial[1], initial[2],
        "the helper is the initial server's responsible app"
    );
    assert_ne!(
        initial[0], initial[2],
        "the server must not be responsible for itself"
    );
    assert_eq!(
        replacement[2], initial[2],
        "responsibility survives the original server's exit"
    );
    assert_ne!(replacement[0], replacement[2]);
}
