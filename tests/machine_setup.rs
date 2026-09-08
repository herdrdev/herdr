#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

// No real SSH connection or server is started. Stop at startup after recording setup actions.
const SSH: &str = r#"#!/bin/sh
for arg do
    last=$arg
    if [ "$arg" = 'StrictHostKeyChecking=yes' ]; then strict_host_key_check=yes; fi
done
if [ "$FAKE_STRICT_HOST_KEY_FAILURE" = yes ] && [ "$strict_host_key_check" = yes ]; then
    echo 'No RSA host key is known for fake-host and you have requested strict checking.' >&2
    echo 'Host key verification failed.' >&2
    exit 255
fi
if [ "$last" = 'command -v herdr' ]; then
    echo /home/remote/.local/bin/herdr
    exit 0
fi
case "$last" in
    'tee '*) cat >/dev/null; exit 0 ;;
esac
case "$last" in
    *'herdr-remote-output-ready:1'*) script=$last ;;
    *) script=$(cat) ;;
esac
printf '\n%s\n' 'herdr-remote-output-ready:1'
case "$script" in
    *'uname -s'*) uname -s; uname -m ;;
    *'version='*) echo /home/remote/.local/bin/herdr ;;
    *'status client --json'*)
        if [ "$FAKE_INSTALLED" = new ] || [ -f "$FAKE_ROOT/installed" ]; then
            printf '%s\n' "$FAKE_CLIENT_STATUS"
        else
            echo '{"version":"0.8.2","protocol":20}'
        fi ;;
    *'status server --json'*)
        if [ -f "$FAKE_ROOT/stopped" ]; then
            echo '{"running":false}'
        elif [ "$FAKE_STRICT_HOST_KEY_FAILURE" = yes ]; then
            echo '{"running":true,"version":"0.8.2","capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1,"surface_interest":true,"health_check":true}}'
        else
            echo '{"running":true,"version":"0.8.2","capabilities":{"live_handoff":true,"detached_server_daemon":true}}'
        fi ;;
    *'server live-handoff'*) echo handoff >>"$FAKE_ROOT/actions"; exit 1 ;;
    *'server stop'*) echo stop >>"$FAKE_ROOT/actions"; touch "$FAKE_ROOT/stopped" ;;
    *'remote-client-bridge'*) echo start >>"$FAKE_ROOT/actions"; echo 'test startup failure' >&2; exit 1 ;;
    *'mkdir -p'*) printf '/fake/tmp\000/fake/herdr\000' ;;
    *'chmod 755'*) echo install >>"$FAKE_ROOT/actions"; touch "$FAKE_ROOT/installed" ;;
    *'command -v herdr'*) echo /home/remote/.local/bin/herdr ;;
    *) echo "unexpected fake SSH script: $script" >&2; exit 1 ;;
esac
"#;

struct SetupResult {
    output: String,
    actions: String,
    prompts: usize,
    success: bool,
}

fn setup(installed: &str, answer: &str, handoff: bool) -> SetupResult {
    setup_with_strict_host_key_failure(installed, answer, handoff, false)
}

fn setup_with_strict_host_key_failure(
    installed: &str,
    answer: &str,
    handoff: bool,
    strict_host_key_failure: bool,
) -> SetupResult {
    let root = std::path::PathBuf::from(format!(
        "/var/tmp/herdr-machine-setup-{}-{}-{}-{}-{}",
        std::process::id(),
        installed,
        answer.trim().is_empty(),
        handoff,
        strict_host_key_failure
    ));
    let app = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("config").join(app)).unwrap();
    fs::write(root.join("bin/ssh"), SSH).unwrap();
    fs::set_permissions(root.join("bin/ssh"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        root.join("config").join(app).join("config.toml"),
        "onboarding = false\n[remote]\nmanage_ssh_config = false\n",
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["status", "client", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());

    let pair = native_pty_system().openpty(PtySize::default()).unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    if handoff {
        command.args(["--remote", "fake-host", "--handoff"]);
    } else {
        command.args(["machine", "add", "fake-host", "--label", "VPS"]);
    }
    command.env(
        "PATH",
        format!("{}:/usr/bin:/bin", root.join("bin").display()),
    );
    command.env("HOME", &root);
    command.env("XDG_CONFIG_HOME", root.join("config"));
    command.env("XDG_STATE_HOME", root.join("state"));
    command.env("XDG_RUNTIME_DIR", &root);
    command.env("FAKE_ROOT", &root);
    command.env("FAKE_INSTALLED", installed);
    command.env(
        "FAKE_STRICT_HOST_KEY_FAILURE",
        if strict_host_key_failure { "yes" } else { "no" },
    );
    command.env(
        "FAKE_CLIENT_STATUS",
        String::from_utf8(status.stdout).unwrap(),
    );
    for name in [
        "HERDR_ENV",
        "HERDR_SESSION",
        "HERDR_SOCKET_PATH",
        "HERDR_CLIENT_SOCKET_PATH",
        "HERDR_REMOTE_BINARY",
        "HERDR_CONFIG_PATH",
    ] {
        command.env_remove(name);
    }
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    let reading = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(len) = reader.read(&mut buffer) {
            if len == 0 || tx.send(buffer[..len].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut output = String::new();
    let mut prompts = 0;
    let mut timed_out = false;
    loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(bytes) => {
                output.push_str(&String::from_utf8_lossy(&bytes));
                let count = output.matches("[y/N]").count() + output.matches("[Y/n]").count();
                if count > prompts {
                    writer
                        .write_all(if prompts == 0 {
                            answer.as_bytes()
                        } else {
                            b"n\n"
                        })
                        .unwrap();
                    writer.flush().unwrap();
                    prompts = count;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                child.kill().unwrap();
                break;
            }
        }
    }
    let status = child.wait().unwrap();
    drop(writer);
    drop(pair.master);
    reading.join().unwrap();
    let actions = fs::read_to_string(root.join("actions")).unwrap_or_default();
    // Every scenario either cancels or reaches the intentionally failed startup.
    let saved = root
        .join("state")
        .join(app)
        .join("client/endpoints.json")
        .exists();
    fs::remove_dir_all(root).unwrap();
    assert!(
        !saved,
        "failed or cancelled setup saved the machine: {output}"
    );
    assert!(!timed_out, "setup timed out: {output}");
    SetupResult {
        output,
        actions,
        prompts,
        success: status.success(),
    }
}

#[test]
fn machine_add_reports_strict_host_key_failure() {
    let result = setup_with_strict_host_key_failure("new", "", false, true);
    assert!(!result.success, "{}", result.output);
    assert_eq!(result.prompts, 0, "{}", result.output);
    assert!(result.actions.is_empty(), "{}", result.output);
    assert!(
        result.output.contains("Host key verification failed"),
        "{}",
        result.output
    );
    assert!(
        result
            .output
            .contains("saved machines use strict host-key checking"),
        "{}",
        result.output
    );
    assert!(
        !result.output.contains("lost connection to server"),
        "{}",
        result.output
    );
}

#[test]
fn machine_setup_old_install_requires_one_explicit_stop_approval() {
    let result = setup("old", "y\n", false);
    assert_eq!(result.prompts, 1, "{}", result.output);
    assert!(
        result
            .output
            .contains("This stops active remote pane processes"),
        "{}",
        result.output
    );
    assert_eq!(
        result.actions, "install\nstop\nstart\n",
        "{}",
        result.output
    );
}

#[test]
fn machine_setup_old_install_enter_cancels_without_remote_changes() {
    let result = setup("old", "\n", false);
    assert!(result.actions.is_empty(), "{}", result.output);
    assert!(
        result.output.contains("machine was not saved"),
        "{}",
        result.output
    );
}

#[test]
fn machine_setup_new_install_old_server_enter_does_not_stop_or_handoff() {
    let result = setup("new", "\n", false);
    assert!(result.actions.is_empty(), "{}", result.output);
    assert!(
        result
            .output
            .contains("This stops active remote pane processes"),
        "{}",
        result.output
    );
}

#[test]
fn machine_setup_new_install_old_server_approval_stops_then_starts_without_installing() {
    let result = setup("new", "y\n", false);
    assert_eq!(result.prompts, 1, "{}", result.output);
    assert_eq!(result.actions, "stop\nstart\n", "{}", result.output);
}

#[test]
fn machine_setup_explicit_remote_handoff_remains_available_but_restart_fallback_defaults_to_no() {
    let result = setup("new", "\n", true);
    assert_eq!(result.actions, "handoff\n", "{}", result.output);
    assert!(
        result
            .output
            .contains("This stops active remote pane processes"),
        "{}",
        result.output
    );
}
