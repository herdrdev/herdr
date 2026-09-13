#![cfg(all(unix, not(target_os = "macos")))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const PROFILE_ID: &str = "0123456789abcdef0123456789abcdef";
const SSH: &str = r#"#!/bin/sh
for arg do
    last=$arg
    printf '%s\n' "$arg" >> "$TEST_ROOT/ssh-args"
done
case "$last" in
    *'command -v herdr') printf 'login banner\nherdr-remote-output-ready:1\n%s\n' "$TEST_REMOTE_HERDR" ;;
    *'remote-api-bridge --check')
        if [ "$TEST_MODE" = old ]; then exit 2; fi
        exec /bin/sh -c "$last" ;;
    *'remote-api-bridge')
        if [ "$TEST_MODE" = offline ]; then echo 'test remote connection failed' >&2; exit 255; fi
        exec /bin/sh -c "$last" ;;
    '/bin/sh -s')
        script=$(cat)
        printf 'login banner\nherdr-remote-output-ready:1\n'
        case "$script" in
            *'uname -s'*) uname -s; uname -m ;;
            *'version='*) printf '%s\n' "$TEST_REMOTE_HERDR" ;;
            *) echo "unexpected discovery: $script" >&2; exit 2 ;;
        esac ;;
    *) echo "unexpected command: $last" >&2; exit 2 ;;
esac
"#;

struct Harness {
    root: PathBuf,
    remote: UnixListener,
    local: UnixListener,
    protocol: u64,
}

impl Harness {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(format!(
            "/var/tmp/hma-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let app = if cfg!(debug_assertions) {
            "herdr-dev"
        } else {
            "herdr"
        };
        let state = root.join("state").join(app).join("client");
        let session = root.join("config").join(app).join("sessions/fleet");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&session).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/ssh"), SSH).unwrap();
        fs::set_permissions(root.join("bin/ssh"), fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_herdr"), root.join("remote herdr")).unwrap();
        fs::write(state.join("endpoints.json"), serde_json::to_vec(&json!({
            "version": 1,
            "ssh": [{"id": PROFILE_ID, "label": "mac", "target": "fake-mac", "session": "fleet", "enabled": true}]
        })).unwrap()).unwrap();
        let remote = UnixListener::bind(session.join("herdr.sock")).unwrap();
        remote.set_nonblocking(true).unwrap();
        let local = UnixListener::bind(root.join("local.sock")).unwrap();
        local.set_nonblocking(true).unwrap();
        let status = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["status", "client", "--json"])
            .output()
            .unwrap();
        let status: Value = serde_json::from_slice(&status.stdout).unwrap();
        Self {
            root,
            remote,
            local,
            protocol: status["protocol"].as_u64().unwrap(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
        command
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_RUNTIME_DIR", &self.root)
            .env("TEST_ROOT", &self.root)
            .env("TEST_REMOTE_HERDR", self.root.join("remote herdr"))
            .env("HERDR_SOCKET_PATH", self.root.join("local.sock"))
            .env(
                "HERDR_CLIENT_SOCKET_PATH",
                self.root.join("never-client.sock"),
            )
            .env("HERDR_SESSION", "wrong-inherited-session")
            .env("HERDR_PANE_ID", "wrong-local-pane")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_REMOTE_BINARY");
        command
    }

    fn serve(&self, result: Value, protocol: u64) -> std::thread::JoinHandle<Value> {
        let listener = self.remote.try_clone().unwrap();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "remote API request did not arrive"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let ping = request["method"] == "ping";
                let response = if ping {
                    json!({"id": request["id"], "result": {"type": "pong", "version": "test", "protocol": protocol}})
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                    let mut response = result.clone();
                    response["id"] = request["id"].clone();
                    response
                };
                writeln!(stream, "{response}").unwrap();
                if !ping || protocol == 0 {
                    return request;
                }
            }
        })
    }

    fn assert_local_untouched(&self) {
        assert_eq!(
            self.local.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert!(!self.root.join("never-client.sock").exists());
        assert!(!fs::read_dir(&self.root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("herdr-api-")));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn machine_api_routes_structured_payload_and_remote_errors_without_local_fallback() {
    let harness = Harness::new();
    let server = harness.serve(
        json!({"error":{"code":"test_remote_error","message":"remote rejected prompt"}}),
        harness.protocol,
    );
    let prompt = "quotes ' \" ; $(touch should-not-exist)\n--machine other";
    let output = harness
        .command(&["--machine", "mac", "agent", "prompt", "w4:p1", prompt])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(error["error"]["code"], "test_remote_error");
    let request = server.join().unwrap();
    assert_eq!(request["method"], "agent.prompt");
    assert_eq!(request["params"]["text"], prompt);
    let ssh_args = fs::read_to_string(harness.root.join("ssh-args")).unwrap();
    assert!(ssh_args.contains("StrictHostKeyChecking=yes"));
    assert!(ssh_args.contains("BatchMode=yes"));
    assert!(ssh_args.contains("--session fleet remote-api-bridge"));
    assert!(!ssh_args.contains("should-not-exist"));
    harness.assert_local_untouched();
}

#[test]
fn machine_api_profile_id_routes_large_list_responses() {
    let harness = Harness::new();
    let data = "remote data ".repeat(20_000);
    let server = harness.serve(
        json!({"result":{"type":"agent_list","agents":[],"test_data":data}}),
        harness.protocol,
    );
    let response = success(
        harness
            .command(&["--machine", PROFILE_ID, "agent", "list"])
            .output()
            .unwrap(),
    );
    assert_eq!(response["result"]["test_data"], data);
    assert_eq!(server.join().unwrap()["method"], "agent.list");
    harness.assert_local_untouched();
}

#[test]
fn machine_api_never_inherits_the_callers_pane() {
    let harness = Harness::new();
    let server = harness.serve(json!({"result":{"type":"ok"}}), harness.protocol);
    success(
        harness
            .command(&["--machine=mac", "pane", "current"])
            .output()
            .unwrap(),
    );
    let request = server.join().unwrap();
    assert_eq!(request["method"], "pane.current");
    assert!(request["params"]["caller_pane_id"].is_null());
    harness.assert_local_untouched();
}

#[test]
fn machine_api_remote_paths_and_wait_parameters_reach_the_server() {
    for (args, method, expected) in [
        (
            vec![
                "worktree",
                "create",
                "--cwd",
                "~/Projects/herdr",
                "--branch",
                "review",
                "--path",
                "/srv/review",
            ],
            "worktree.create",
            json!({"cwd":"~/Projects/herdr", "branch":"review", "path":"/srv/review"}),
        ),
        (
            vec![
                "agent",
                "wait",
                "w4:p1",
                "--until",
                "idle",
                "--timeout",
                "2000",
            ],
            "agent.wait",
            json!({"target":"w4:p1", "until":["idle"], "timeout_ms":2000}),
        ),
    ] {
        let harness = Harness::new();
        let server = harness.serve(json!({"result":{"type":"ok"}}), harness.protocol);
        let mut command = harness.command(&["--machine", "mac"]);
        success(command.args(args).output().unwrap());
        let request = server.join().unwrap();
        assert_eq!(request["method"], method);
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&request["params"][key], value);
        }
        harness.assert_local_untouched();
    }
}

#[test]
fn machine_api_status_reports_remote_identity_not_local_installation_state() {
    let harness = Harness::new();
    let server = harness.serve(json!({}), 0);
    let status = success(
        harness
            .command(&["--machine", "mac", "status", "server", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(status["session"], "fleet");
    assert_eq!(status["socket"], format!("machine:{PROFILE_ID}/fleet"));
    assert!(status["server_binary_stale"].is_null());
    assert_eq!(server.join().unwrap()["method"], "ping");
    harness.assert_local_untouched();
}

#[test]
fn machine_api_usage_errors_do_not_connect() {
    let harness = Harness::new();
    for args in [
        vec!["--machine", "missing", "agent", "list"],
        vec!["--machine", "mac", "config", "reset-keys"],
        vec![
            "--machine",
            "mac",
            "agent",
            "explain",
            "--file",
            "/any/file",
            "--agent",
            "pi",
        ],
        vec![
            "--machine",
            "mac",
            "pane",
            "split",
            "--current",
            "--direction",
            "right",
        ],
        vec!["--machine", "mac", "--session", "local", "agent", "list"],
    ] {
        assert_eq!(
            harness.command(&args).output().unwrap().status.code(),
            Some(2)
        );
    }
    assert!(!harness.root.join("ssh-args").exists());
    harness.assert_local_untouched();
}

#[test]
fn machine_api_rejects_old_bridges_and_disconnected_machines() {
    for (mode, message) in [
        ("old", "update Herdr"),
        ("offline", "test remote connection failed"),
    ] {
        let harness = Harness::new();
        let output = harness
            .command(&["--machine", "mac", "agent", "list"])
            .env("TEST_MODE", mode)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        harness.assert_local_untouched();
    }
}

#[test]
fn machine_api_server_stop_is_sent_only_to_the_selected_machine() {
    let harness = Harness::new();
    let server = harness.serve(json!({"result":{"type":"ok"}}), harness.protocol);
    let output = harness
        .command(&["--machine", "mac", "server", "stop"])
        .env("HERDR_SOCKET_PATH", harness.root.join("missing-local.sock"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        harness.root.join("ssh-args").exists(),
        "server stop bypassed machine routing"
    );
    assert_eq!(server.join().unwrap()["method"], "server.stop");
    harness.assert_local_untouched();
}

#[test]
fn machine_api_protocol_mismatch_never_sends_the_mutation() {
    let harness = Harness::new();
    let server = harness.serve(json!({}), 0);
    let output = harness
        .command(&["--machine", "mac", "pane", "close", "w4:p1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(server.join().unwrap()["method"], "ping");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("protocol_mismatch"), "{error}");
    assert!(error.contains("machine 'mac'"), "{error}");
    assert!(!error.contains("HERDR_SOCKET_PATH="), "{error}");
    harness.assert_local_untouched();
}
