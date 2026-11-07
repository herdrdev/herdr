use super::*;
use std::time::{Duration, Instant};

fn hung_helper(case: &str) {
    let marker = std::env::temp_dir().join(format!(
        "herdr-clipboard-hang-{}-{case}",
        std::process::id()
    ));
    let script = format!("echo $$ > '{}'; {case}", marker.display());
    let (cancel, watchdog) = std::sync::mpsc::channel();
    let watchdog_marker = marker.clone();
    let watchdog = std::thread::spawn(move || {
        if watchdog.recv_timeout(Duration::from_secs(8)).is_err() {
            if let Ok(pid) = std::fs::read_to_string(&watchdog_marker) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    });
    let started = Instant::now();
    let mut command = Command::new("sh");
    command.args(["-c", &script]);
    let failed = if case == "exec sleep 30" {
        clipboard::run(
            command,
            Some(&vec![b'x'; 1024 * 1024]),
            0,
            Instant::now() + clipboard::TIMEOUT,
        )
        .is_err()
    } else {
        read_clipboard_image_with_spawned_command_max(command, 16).is_none()
    };
    let elapsed = started.elapsed();
    let pid: u32 = std::fs::read_to_string(&marker)
        .expect("helper pid")
        .trim()
        .parse()
        .expect("numeric pid");
    let survived = process_exists(pid);
    let _ = cancel.send(());
    watchdog.join().expect("watchdog joined");
    let _ = std::fs::remove_file(marker);
    assert!(failed, "hung helper must fail");
    assert!(
        elapsed < Duration::from_secs(7),
        "clipboard deadline exceeded: {elapsed:?}"
    );
    assert!(!survived, "helper must be killed and reaped before return");
}

#[test]
fn clipboard_hung_stdin_is_bounded() {
    hung_helper("exec sleep 30");
}

#[test]
fn clipboard_hung_stdout_is_bounded() {
    hung_helper("exec sleep 30 # stdout");
}

#[test]
fn clipboard_hung_exit_after_stdout_closes_is_bounded() {
    hung_helper("exec 1>&-; exec sleep 30");
}

#[test]
fn clipboard_slow_success_is_not_a_startup_failure() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 0.3; printf image"]);
    assert_eq!(
        read_clipboard_image_with_spawned_command_max(command, 16),
        Some(b"image".to_vec())
    );
}

struct ForkingHelper {
    directory: PathBuf,
}

impl ForkingHelper {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!(
            "herdr-forking-clipboard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("timestamp")
                .as_nanos(),
        ));
        std::fs::create_dir(&directory).expect("helper directory");
        let helper = Self { directory };
        std::fs::write(
            helper.directory.join("wl-copy"),
            format!("#!/bin/sh\n{script}\n"),
        )
        .expect("helper script");
        std::fs::set_permissions(
            helper.directory.join("wl-copy"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("executable helper");
        helper
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.directory.join("wl-copy"));
        command.env("OWNER_PID", self.directory.join("owner"));
        command
    }

    fn owner_pid(&self) -> u32 {
        std::fs::read_to_string(self.directory.join("owner"))
            .expect("owner pid")
            .trim()
            .parse()
            .expect("numeric owner pid")
    }
}

impl Drop for ForkingHelper {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.directory.join("owner")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                if helper_is_running(pid as u32) {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                while helper_is_running(pid as u32) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn clipboard_forking_wl_copy_preserves_successful_owner() {
    let helper = ForkingHelper::new(
        "cat >/dev/null\nsleep 30 </dev/null >/dev/null 2>&1 &\nprintf '%s' \"$!\" > \"$OWNER_PID\"\nexit 0",
    );
    let started = Instant::now();
    let result = clipboard::run(
        helper.command(),
        Some(b"clipboard text"),
        0,
        started + clipboard::TIMEOUT,
    );
    let elapsed = started.elapsed();
    let owner = helper.owner_pid();
    assert!(result.is_ok(), "forking helper must succeed: {result:?}");
    assert!(
        elapsed < Duration::from_secs(2),
        "must not wait on the owner: {elapsed:?}"
    );
    let stat = std::fs::read_to_string(format!("/proc/{owner}/stat")).expect("owner must survive");
    assert!(
        !stat[stat.rfind(')').expect("process name") + 2..].starts_with('Z'),
        "successful owner must not be killed"
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(owner as i32, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn clipboard_large_image_round_trip() {
    let mut payload: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    payload[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let bytes = clipboard::run(
        Command::new("cat"),
        Some(&payload),
        payload.len(),
        Instant::now() + clipboard::TIMEOUT,
    )
    .expect("large image round trip");
    assert_eq!(bytes, payload);
    assert!(bytes_match_image_signature("png", &bytes));
}

#[test]
fn clipboard_failed_wrapper_kills_descendants() {
    let helper = ForkingHelper::new("sleep 30 &\nprintf '%s' \"$!\" > \"$OWNER_PID\"\nexit 7");
    let started = Instant::now();
    let result = clipboard::run(helper.command(), None, 16, started + clipboard::TIMEOUT);
    let owner = helper.owner_pid();
    assert!(result.is_err(), "failed wrapper must fail");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "failure must not wait for inherited stdout"
    );
    // A container's init may leave the killed descendant as a zombie.
    let deadline = Instant::now() + Duration::from_secs(2);
    while helper_is_running(owner) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !helper_is_running(owner),
        "failed wrapper's descendant must not survive"
    );
}

fn helper_is_running(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, state)| !state.starts_with('Z'))
    })
}

#[test]
fn clipboard_zombie_helper_is_not_running() {
    let mut child = Command::new("sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("child");
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut zombie = false;
    while Instant::now() < deadline {
        zombie = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, state)| state.starts_with('Z'))
        });
        if zombie {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let exists = process_exists(pid);
    let running = helper_is_running(pid);
    let _ = child.kill();
    let _ = child.wait();
    assert!(zombie && exists, "test must observe an unreaped zombie");
    assert!(
        !running,
        "a zombie cannot keep a clipboard pipe or selection alive"
    );
}
