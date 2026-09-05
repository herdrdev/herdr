#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHECK_NOTICE: &str = "# Tailscale SSH requires an additional check.";
const CHECK_URL: &str = "# To authenticate, visit: https://login.tailscale.com/a/test";
const LATER_FAILURE: &str = "ssh: later setup probe failed";

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for fake ssh");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ssh_check_message_is_visible_while_authentication_waits() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    let temp_dir = std::env::temp_dir().join(format!(
        "herdr-remote-auth-test-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_dir).expect("create test directory");

    let started_path = temp_dir.join("ssh-started");
    let approval_path = temp_dir.join("ssh-approved");
    let advanced_path = temp_dir.join("ssh-advanced");
    let first_done_path = temp_dir.join("ssh-first-done");
    let ssh_path = temp_dir.join("ssh");
    fs::write(
        &ssh_path,
        format!(
            r#"#!/bin/sh
if [ ! -e "$FAKE_SSH_FIRST_DONE" ]; then
    : > "$FAKE_SSH_FIRST_DONE"
    : > "$FAKE_SSH_STARTED"
    printf '%s\n%s\n' '{CHECK_NOTICE}' '{CHECK_URL}' >&2
    while [ ! -e "$FAKE_SSH_APPROVED" ]; do
        /bin/sleep 0.01
    done
    printf 'Linux\nx86_64\n'
    exit 0
fi
: > "$FAKE_SSH_ADVANCED"
printf '%s\n' '{LATER_FAILURE}' >&2
exit 255
"#
        ),
    )
    .expect("write fake ssh");
    fs::set_permissions(&ssh_path, fs::Permissions::from_mode(0o755))
        .expect("make fake ssh executable");

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{inherited_path}", temp_dir.display());
    let mut child = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["--remote", "check-host"])
        .env("PATH", path)
        .env("FAKE_SSH_STARTED", &started_path)
        .env("FAKE_SSH_APPROVED", &approval_path)
        .env("FAKE_SSH_ADVANCED", &advanced_path)
        .env("FAKE_SSH_FIRST_DONE", &first_done_path)
        .env("HERDR_CONFIG_PATH", temp_dir.join("config.toml"))
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_REMOTE_BINARY")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start remote attach");

    let stderr = child.stderr.take().expect("remote attach stderr");
    let (line_tx, line_rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line_tx
                .send(line.expect("read remote attach stderr"))
                .is_err()
            {
                break;
            }
        }
    });

    wait_for_file(&started_path, Duration::from_secs(2));
    let notice = line_rx.recv_timeout(Duration::from_millis(500));
    let url = line_rx.recv_timeout(Duration::from_millis(500));
    fs::write(&approval_path, b"approved").expect("release fake ssh approval");
    wait_for_file(&advanced_path, Duration::from_secs(2));

    let status = child.wait().expect("wait for remote attach");
    reader.join().expect("join stderr reader");
    let later_lines = line_rx.try_iter().collect::<Vec<_>>();
    let _ = fs::remove_dir_all(&temp_dir);

    assert_eq!(notice.as_deref(), Ok(CHECK_NOTICE));
    assert_eq!(url.as_deref(), Ok(CHECK_URL));
    assert!(
        later_lines.iter().any(|line| line == LATER_FAILURE),
        "later SSH stderr should also be visible: {later_lines:?}"
    );
    assert!(
        later_lines.iter().any(|line| {
            line.contains("error: remote binary discovery failed") && line.contains(LATER_FAILURE)
        }),
        "failed SSH stderr should remain in the contextual error: {later_lines:?}"
    );
    assert!(!status.success());
}
