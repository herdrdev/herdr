#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHECK_NOTICE: &str = "# Tailscale SSH requires an additional check.";
const CHECK_URL: &str = "# To authenticate, visit: https://login.tailscale.com/a/test";
const LATER_FAILURE: &str = "ssh: later setup probe failed";

struct TestCleanup {
    temp_dir: PathBuf,
    child: Option<Child>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Drop for TestCleanup {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // The child leads a private process group and has not been reaped.
            // Kill its fake SSH descendants too, not just the Herdr launcher.
            // SAFETY: the negative PID targets only this test's process group.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

fn wait_for_file(
    path: &Path,
    timeout: Duration,
    framed_shell: bool,
    stderr: &mpsc::Receiver<Vec<u8>>,
    fases: &Path,
) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            let captured = stderr.try_iter().flatten().collect::<Vec<_>>();
            let captured = String::from_utf8_lossy(&captured);
            panic!(
                "timed out waiting for fake ssh marker {:?} (framed_shell={framed_shell}); probe_windows: {}; primeira_chamada: {}; segunda_chamada: {}; stderr_bytes: {}; aviso: {}; url: {}; erro_final: {}",
                path.file_name(), fases.join("probe-windows").exists(),
                fases.join("primeira-chamada").exists(), fases.join("segunda-chamada").exists(),
                captured.len(), captured.contains(CHECK_NOTICE), captured.contains(CHECK_URL), captured.contains(LATER_FAILURE)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ssh_check_message_is_visible_while_authentication_waits() {
    check_authentication_output(false);
    check_authentication_output(true);
}

fn check_authentication_output(framed_shell: bool) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "herdr-remote-auth-test-{}-{nonce}",
        std::process::id()
    ));
    let mut cleanup = TestCleanup {
        temp_dir: temp_dir.clone(),
        child: None,
        reader: None,
    };
    fs::create_dir_all(&temp_dir).expect("create test directory");
    fs::set_permissions(&temp_dir, fs::Permissions::from_mode(0o700))
        .expect("secure test directory");
    fs::create_dir(temp_dir.join("runtime")).expect("create runtime directory");
    fs::set_permissions(temp_dir.join("runtime"), fs::Permissions::from_mode(0o700))
        .expect("secure runtime directory");

    let started_path = temp_dir.join("ssh-started");
    let approval_path = temp_dir.join("ssh-approved");
    let advanced_path = temp_dir.join("ssh-advanced");
    let first_done_path = temp_dir.join("ssh-first-done");
    let ssh_path = temp_dir.join("ssh");
    fs::write(
        &ssh_path,
        format!(
            r#"#!/bin/sh
case "$*" in
    *powershell.exe*) : > "$FAKE_SSH_FASES/probe-windows"; exit 1 ;;
esac
authenticate() {{
    : > "$FAKE_SSH_STARTED"
    printf '/bin/sh: expected probe noise\n%s\n%s' '{CHECK_NOTICE}' '{CHECK_URL}' >&2
    while [ ! -e "$FAKE_SSH_APPROVED" ]; do
        /bin/sleep 0.01
    done
}}
if [ ! -e "$FAKE_SSH_FIRST_DONE" ]; then
    : > "$FAKE_SSH_FASES/primeira-chamada"
    : > "$FAKE_SSH_FIRST_DONE"
    if [ "$FAKE_SSH_FRAMED" = 0 ]; then authenticate; fi
    /bin/cat >/dev/null
    printf 'login banner\nherdr-remote-output-ready:1\nLinux\nx86_64\n'
    exit 0
fi
: > "$FAKE_SSH_FASES/segunda-chamada"
if [ "$FAKE_SSH_FRAMED" = 1 ] && [ ! -e "$FAKE_SSH_STARTED" ]; then
    authenticate
fi
/bin/cat >/dev/null
: > "$FAKE_SSH_ADVANCED"
printf '\n%s\n' '{LATER_FAILURE}' >&2
exit 255
"#
        ),
    )
    .expect("write fake ssh");
    fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o755))
        .expect("make fake ssh executable");

    let path = format!("{}:/usr/bin:/bin", temp_dir.display());
    let child = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["--remote", "check-host"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &temp_dir)
        .env("TMPDIR", &temp_dir)
        .env("XDG_CONFIG_HOME", temp_dir.join("config"))
        .env("XDG_DATA_HOME", temp_dir.join("data"))
        .env("XDG_CACHE_HOME", temp_dir.join("cache"))
        .env("XDG_STATE_HOME", temp_dir.join("state"))
        .env("XDG_RUNTIME_DIR", temp_dir.join("runtime"))
        .env("PRETORIAN_HERDR_CONFIG_DIR", temp_dir.join("config-herdr"))
        .env("PRETORIAN_HERDR_STATE_DIR", temp_dir.join("state-herdr"))
        .env("FAKE_SSH_FRAMED", if framed_shell { "1" } else { "0" })
        .env("FAKE_SSH_FASES", &temp_dir)
        .env("FAKE_SSH_STARTED", &started_path)
        .env("FAKE_SSH_APPROVED", &approval_path)
        .env("FAKE_SSH_ADVANCED", &advanced_path)
        .env("FAKE_SSH_FIRST_DONE", &first_done_path)
        .env("HERDR_CONFIG_PATH", temp_dir.join("config.toml"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .expect("start remote attach");

    cleanup.child = Some(child);
    let child = cleanup.child.as_mut().expect("registered child");
    let mut stderr = child.stderr.take().expect("remote attach stderr");
    let (line_tx, line_rx) = mpsc::channel();
    cleanup.reader = Some(thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(len) = stderr.read(&mut buffer) {
            if len == 0 || line_tx.send(buffer[..len].to_vec()).is_err() {
                break;
            }
        }
    }));

    wait_for_file(
        &started_path,
        Duration::from_secs(2),
        framed_shell,
        &line_rx,
        &temp_dir,
    );
    let mut before_approval = Vec::new();
    while !String::from_utf8_lossy(&before_approval).contains(CHECK_URL) {
        match line_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(bytes) => before_approval.extend(bytes),
            Err(_) => break,
        }
    }
    fs::write(&approval_path, b"approved").expect("release fake ssh approval");
    wait_for_file(
        &advanced_path,
        Duration::from_secs(2),
        framed_shell,
        &line_rx,
        &temp_dir,
    );

    let status = child.wait().expect("wait for remote attach");
    cleanup.child = None;
    cleanup
        .reader
        .take()
        .expect("registered stderr reader")
        .join()
        .expect("join stderr reader");
    let later_bytes = line_rx.try_iter().flatten().collect::<Vec<_>>();
    let later_lines = String::from_utf8_lossy(&later_bytes);

    let before_approval = String::from_utf8_lossy(&before_approval);
    assert!(before_approval.contains(CHECK_NOTICE), "{before_approval}");
    assert!(
        before_approval.contains(CHECK_URL),
        "partial prompt was hidden: {before_approval}"
    );
    assert!(
        later_lines.contains(LATER_FAILURE),
        "later SSH stderr should also be visible: {later_lines:?}"
    );
    assert!(
        later_lines.lines().any(|line| {
            line.contains("erro: falha na busca do executável remoto")
                && line.contains(LATER_FAILURE)
        }),
        "failed SSH stderr should remain in the contextual error: {later_lines:?}"
    );
    assert!(!status.success());
}
