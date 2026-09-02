//! Integration tests for thin client mode.

#![cfg(unix)]

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, client_shell_handshake, read_server_message, register_runtime_dir,
    register_spawned_herdr_pid, unregister_spawned_herdr_pid, wait_for_client_shell_bootstrap,
    wait_for_message_variant, wait_for_message_variants, wait_for_socket, wait_until,
    CURRENT_ENDPOINT_PROTOCOL_GENERATION as CURRENT_PROTOCOL, SERVER_MESSAGE_PANE_SURFACE,
    SERVER_MESSAGE_PANE_SURFACE_PATCH, SERVER_MESSAGE_SEMANTIC_NOTIFICATION,
    SERVER_MESSAGE_SERVER_SHUTDOWN,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-client-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
}

impl SpawnedHerdr {
    fn close_master(&mut self) {
        drop(self._master.take());
    }
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        self.close_master();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn spawn_client_process(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
) -> SpawnedHerdr {
    spawn_client_process_with_args(config_home, runtime_dir, api_socket_path, &["client"])
}

fn spawn_client_shell_process(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
) -> SpawnedHerdr {
    spawn_client_process_with_args(config_home, runtime_dir, api_socket_path, &["client"])
}

fn spawn_client_process_with_args(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
    args: &[&str],
) -> SpawnedHerdr {
    spawn_client_process_with_args_and_env(config_home, runtime_dir, api_socket_path, args, &[])
}

fn spawn_client_process_with_args_and_env(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> SpawnedHerdr {
    register_runtime_dir(runtime_dir);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.args(args);
    cmd.env("HERDR_DISABLE_SOUND", "1");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: Some(pair.master),
        child,
    }
}

fn spawn_server(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
    client_socket_path: &PathBuf,
) -> SpawnedHerdr {
    spawn_server_with_config(
        config_home,
        runtime_dir,
        api_socket_path,
        client_socket_path,
        "onboarding = false\n",
    )
}

fn spawn_server_with_config(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket_path: &PathBuf,
    _client_socket_path: &PathBuf,
    config: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(config_home.join(app_dir_name()).join("config.toml"), config).unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: Some(pair.master),
        child,
    }
}

fn ping_socket(socket_path: &PathBuf) -> String {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");

    let request = r#"{"id":"1","method":"ping","params":{}}"#;
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response.trim().to_string()
}

fn send_json_request(socket_path: &PathBuf, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn first_pane_id_in_workspace(socket_path: &PathBuf, workspace_id: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let request = format!(
            r#"{{"id":"pane_list","method":"pane.list","params":{{"workspace_id":"{workspace_id}"}}}}"#
        );
        let panes = send_json_request(socket_path, &request);
        if let Some(pane_id) = panes["result"]["panes"]
            .as_array()
            .and_then(|panes| panes.first())
            .and_then(|pane| pane["pane_id"].as_str())
        {
            return pane_id.to_string();
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("pane.list did not return a pane for workspace {workspace_id} before timeout");
}

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn client_connects_and_receives_pane_surface() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) = client_shell_handshake(&mut stream, CURRENT_PROTOCOL, 54, 23)
        .expect("handshake should succeed");
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none(), "{error:?}");
    wait_for_client_shell_bootstrap(&mut stream, Duration::from_secs(10))
        .expect("should receive the shell snapshot and pane surface");

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn direct_attach_initial_mouse_capture_follows_config() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let config_path = config_home.join(app_dir_name()).join("config.toml");

    let spawned_server = spawn_server_with_config(
        &config_home,
        &runtime_dir,
        &api_socket,
        &client_socket,
        "onboarding = false\n[ui]\nmouse_capture = false\n",
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));
    let created = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "create-workspace-for-direct-attach",
            "method": "workspace.create",
            "params": {"cwd": base},
        })
        .to_string(),
    );
    let terminal_id = created["result"]["root_pane"]["terminal_id"]
        .as_str()
        .expect("created terminal id")
        .to_string();

    let mut attach = spawn_client_process_with_args(
        &config_home,
        &runtime_dir,
        &api_socket,
        &["terminal", "attach", &terminal_id],
    );
    let output = spawn_pty_drain(
        attach
            ._master
            .as_ref()
            .expect("direct attach master")
            .try_clone_reader()
            .expect("clone direct attach PTY reader"),
    );
    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(20), || {
            read_output(&output).contains("\x1b[?7l")
        }),
        "direct attach terminal setup should complete; output: {:?}",
        read_output(&output)
    );
    assert!(
        !read_output(&output).contains("\x1b[?1000h"),
        "mouse capture disabled must not enable host mouse reporting; output: {:?}",
        read_output(&output)
    );
    assert!(
        read_output(&output).contains("\x1b[?2004h"),
        "direct attach must enable host bracketed paste; output: {:?}",
        read_output(&output)
    );

    let restore_watermark = output_len(&output);
    attach
        ._master
        .as_ref()
        .expect("direct attach master")
        .take_writer()
        .expect("direct attach PTY writer")
        .write_all(b"\x02q")
        .expect("detach direct attach client");
    let restore_output = drain_until_client_exits(&mut attach, &output, restore_watermark);
    assert!(
        restore_output.contains("\x1b[?2004l"),
        "direct attach must disable host bracketed paste on restore; output: {restore_output:?}"
    );
    drop(attach);

    fs::write(
        &config_path,
        "onboarding = false\n[ui]\nmouse_capture = true\n",
    )
    .unwrap();
    let attach = spawn_client_process_with_args(
        &config_home,
        &runtime_dir,
        &api_socket,
        &["terminal", "attach", &terminal_id],
    );
    let output = spawn_pty_drain(
        attach
            ._master
            .as_ref()
            .expect("direct attach master")
            .try_clone_reader()
            .expect("clone direct attach PTY reader"),
    );
    assert!(
        wait_until(Duration::from_secs(5), Duration::from_millis(20), || {
            read_output(&output).contains("\x1b[?7l")
        }),
        "direct attach terminal setup should complete; output: {:?}",
        read_output(&output)
    );
    assert!(
        read_output(&output).contains("\x1b[?1000h"),
        "mouse capture enabled must retain host mouse reporting; output: {:?}",
        read_output(&output)
    );

    drop(spawned_server);
    cleanup_spawned_herdr(attach, base);
}

#[test]
fn client_sees_headless_startup_config_diagnostic() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let app_dir = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    fs::create_dir_all(config_home.join(app_dir)).unwrap();
    fs::write(
        config_home.join(app_dir).join("config.toml"),
        "[keys\nprefix = \"ctrl+a\"\n",
    )
    .unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", &api_socket);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    let spawned = SpawnedHerdr {
        _master: Some(pair.master),
        child,
    };
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let client = spawn_client_shell_process(&config_home, &runtime_dir, &api_socket);
    let output = spawn_pty_drain(
        client
            ._master
            .as_ref()
            .expect("client shell master")
            .try_clone_reader()
            .expect("clone client shell reader"),
    );
    assert!(
        wait_until(Duration::from_secs(8), Duration::from_millis(20), || {
            let output = read_output(&output);
            output.contains("config.toml") && output.contains("herdr config check")
        }),
        "client shell should render startup config diagnostic; output: {:?}",
        read_output(&output)
    );

    drop(spawned);
    cleanup_spawned_herdr(client, base);
}

#[test]
fn server_unreachable_shows_clear_error() {
    // when server is unreachable, the client exits quickly
    // with an actionable connection-failed message.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_herdr"))
        .arg("client")
        .env("HERDR_DISABLE_SOUND", "1")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("HERDR_SOCKET_PATH", &api_socket)
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_ENV")
        .output()
        .expect("client command should run");

    assert!(
        !output.status.success(),
        "client should fail when no server is running"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to connect to server"),
        "stderr should mention connection failure: {stderr}"
    );
    assert!(
        stderr.contains("Is herdr server running?"),
        "stderr should include actionable guidance: {stderr}"
    );
    assert!(
        stderr.contains("Socket path:"),
        "stderr should include attempted socket path: {stderr}"
    );

    cleanup_test_base(&base);
}

#[test]
fn server_crash_after_attach_causes_lost_connection_error() {
    // attach a real thin client connection, kill server unexpectedly,
    // assert clean non-zero client exit plus lost-connection signal.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Attach a real thin client (client subcommand) through PTY so handshake and
    // terminal setup paths are exercised.
    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);

    // Prove attached before kill by waiting for recognizable rendered app content.
    let mut thin_reader = thin_client
        ._master
        .as_ref()
        .expect("thin client master")
        .try_clone_reader()
        .expect("clone client PTY reader");
    let (attached_before_kill, attach_output) = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut buf = [0u8; 4096];
        let mut seen = false;
        let mut output = String::new();
        while Instant::now() < deadline {
            match thin_reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let out = String::from_utf8_lossy(&buf[..n]);
                    output.push_str(&out);
                    if out.contains("\u{2500}")
                        || out.contains("workspace")
                        || out.contains("pane")
                        || out.contains("terminal")
                    {
                        seen = true;
                        break;
                    }
                    if output.to_lowercase().contains("herdr:") {
                        break;
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(30)),
                Err(_) => thread::sleep(Duration::from_millis(30)),
            }
        }
        (seen, output)
    };
    assert!(
        attached_before_kill,
        "thin client must complete attach and receive frame before server crash; output: {attach_output:?}"
    );

    // Kill server unexpectedly.
    if let Some(pid) = spawned.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    spawned.close_master();

    // Client should exit non-zero after connection loss.
    let mut crash_output = String::new();
    let exited = {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut exited = false;
        while Instant::now() < deadline {
            if thin_client.child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            // Keep draining client output so the process can progress to exit.
            let mut buf = [0u8; 1024];
            if let Ok(n) = thin_reader.read(&mut buf) {
                if n > 0 {
                    crash_output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        exited
    };
    assert!(exited, "thin client should exit after server SIGKILL");

    let status = thin_client.child.wait().expect("wait thin client status");
    assert!(
        !status.success(),
        "thin client should exit non-zero after lost server connection"
    );

    // Drain trailing output and require the explicit user-visible lost-connection message.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        match thin_reader.read(&mut buf) {
            Ok(n) if n > 0 => crash_output.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(_) => break,
            Err(_) => break,
        }
        thread::sleep(Duration::from_millis(30));
    }

    let crash_output_lc = crash_output.to_lowercase();
    assert!(
        crash_output_lc.contains("lost connection to server"),
        "thin client must emit explicit lost-connection message after server crash; output: {crash_output:?}"
    );

    // Ensure server is gone.
    let _ = spawned.child.wait();

    cleanup_test_base(&base);
}

/// Any of the mouse-disable modes emitted by `clear_host_mouse_reporting` on
/// terminal restore. Their presence in the client's PTY output proves the
/// restore path (`TerminalGuard::Drop` → `restore_terminal_state`) ran.
const MOUSE_TEARDOWN_MARKERS: [&str; 2] = ["\u{1b}[?1003l", "\u{1b}[?1000l"];

fn output_has_mouse_teardown(output: &str) -> bool {
    MOUSE_TEARDOWN_MARKERS
        .iter()
        .all(|marker| output.contains(marker))
}

/// Shared buffer fed by a background PTY reader thread. Reading on a thread
/// keeps the blocking `Box<dyn Read>` (which has no timeout) off the test's
/// main thread, so a client that never exits fails the deadline instead of
/// hanging the whole test forever.
type SharedOutput = std::sync::Arc<Mutex<String>>;

fn spawn_pty_drain(mut reader: Box<dyn Read + Send>) -> SharedOutput {
    let output: SharedOutput = std::sync::Arc::new(Mutex::new(String::new()));
    let thread_output = output.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => thread_output
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
    });
    output
}

fn read_output(output: &SharedOutput) -> String {
    output.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Current captured byte length, used as a watermark so a test can search only
/// the output emitted *after* a trigger. The teardown markers also appear in
/// normal attach-phase output, so matching the whole buffer is meaningless.
fn output_len(output: &SharedOutput) -> usize {
    output.lock().unwrap_or_else(|p| p.into_inner()).len()
}

/// Spawns a server + real thin client under a PTY and waits until the client
/// has attached and rendered a frame. Returns the pieces plus a shared buffer
/// that keeps accumulating PTY output (including teardown) on a background
/// thread.
fn attach_thin_client(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket: &PathBuf,
    client_socket: &PathBuf,
) -> (SpawnedHerdr, SpawnedHerdr, SharedOutput) {
    attach_thin_client_with_config(
        config_home,
        runtime_dir,
        api_socket,
        client_socket,
        "onboarding = false\n",
    )
}

fn attach_thin_client_with_config(
    config_home: &PathBuf,
    runtime_dir: &PathBuf,
    api_socket: &PathBuf,
    client_socket: &PathBuf,
    config: &str,
) -> (SpawnedHerdr, SpawnedHerdr, SharedOutput) {
    let spawned_server =
        spawn_server_with_config(config_home, runtime_dir, api_socket, client_socket, config);
    wait_for_socket(api_socket, Duration::from_secs(10));
    wait_for_socket(client_socket, Duration::from_secs(10));

    let thin_client = spawn_client_process(config_home, runtime_dir, api_socket);
    let reader = thin_client
        ._master
        .as_ref()
        .expect("thin client master")
        .try_clone_reader()
        .expect("clone client PTY reader");
    let output = spawn_pty_drain(reader);

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut attached = false;
    while Instant::now() < deadline {
        let out = read_output(&output);
        if out.contains('\u{2500}')
            || out.contains("workspace")
            || out.contains("pane")
            || out.contains("terminal")
        {
            attached = true;
            break;
        }
        if out.to_lowercase().contains("herdr:") {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    assert!(
        attached,
        "thin client must attach and render a frame; output: {:?}",
        read_output(&output)
    );

    (spawned_server, thin_client, output)
}

#[test]
fn client_shell_detaches_restores_and_freshly_reattaches_to_current_state() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let created = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "client-shell-lifecycle-workspace",
            "method": "workspace.create",
            "params": {"cwd": base, "focus": true, "label": "shell-lifecycle"},
        })
        .to_string(),
    );
    assert_eq!(created["result"]["type"], "workspace_created", "{created}");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();
    send_pane_shell_command(&api_socket, &pane_id, "printf 'SHELL_LIFECYCLE_INITIAL\\n'");

    let mut client_a = spawn_client_shell_process(&config_home, &runtime_dir, &api_socket);
    let output_a = spawn_pty_drain(
        client_a
            ._master
            .as_ref()
            .expect("first client shell PTY")
            .try_clone_reader()
            .expect("clone first client shell reader"),
    );
    assert!(
        wait_until(Duration::from_secs(8), Duration::from_millis(20), || {
            let output = read_output(&output_a);
            output.contains("shell-lifecycle") && output.contains("SHELL_LIFECYCLE_INITIAL")
        }),
        "client shell should compose one coherent snapshot and pane surface; output: {:?}",
        read_output(&output_a)
    );

    let detach_watermark = output_len(&output_a);
    client_a
        ._master
        .as_ref()
        .expect("first client shell PTY")
        .take_writer()
        .expect("first client shell writer")
        .write_all(b"\x02q")
        .expect("detach first client shell");
    let detach_output = drain_until_client_exits(&mut client_a, &output_a, detach_watermark);
    assert!(
        output_has_mouse_teardown(&detach_output),
        "client shell should restore the host terminal after detach; output: {detach_output:?}"
    );
    assert!(
        ping_socket(&api_socket).contains("pong"),
        "server should remain alive after client shell detach"
    );
    drop(client_a);

    send_pane_shell_command(
        &api_socket,
        &pane_id,
        "printf 'SHELL_LIFECYCLE_DETACHED\\n'",
    );
    let mut client_b = spawn_client_shell_process(&config_home, &runtime_dir, &api_socket);
    let output_b = spawn_pty_drain(
        client_b
            ._master
            .as_ref()
            .expect("reattached client shell PTY")
            .try_clone_reader()
            .expect("clone reattached client shell reader"),
    );
    assert!(
        wait_until(Duration::from_secs(8), Duration::from_millis(20), || {
            let output = read_output(&output_b);
            output.contains("shell-lifecycle") && output.contains("SHELL_LIFECYCLE_DETACHED")
        }),
        "fresh client shell should receive current state and detached-period output; output: {:?}",
        read_output(&output_b)
    );

    let disconnect_watermark = output_len(&output_b);
    if let Some(pid) = server.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    server.close_master();
    let disconnect_output =
        drain_until_client_exits(&mut client_b, &output_b, disconnect_watermark);
    assert!(
        output_has_mouse_teardown(&disconnect_output),
        "client shell should restore the host terminal after endpoint loss; output: {disconnect_output:?}"
    );
    assert!(
        disconnect_output
            .to_lowercase()
            .contains("lost connection to server"),
        "client shell should explain endpoint loss; output: {disconnect_output:?}"
    );

    drop(server);
    cleanup_spawned_herdr(client_b, base);
}

fn captured_window_titles(output: &SharedOutput) -> Vec<String> {
    read_output(output)
        .split("\x1b]0;")
        .skip(1)
        .filter_map(|suffix| {
            suffix
                .split_once('\x07')
                .map(|(title, _)| title.to_string())
        })
        .collect()
}

fn wait_for_window_title(output: &SharedOutput, expected_suffix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(title) = captured_window_titles(output)
            .into_iter()
            .find(|title| title.ends_with(expected_suffix))
        {
            return title;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "outer window title ending in {expected_suffix:?} was not emitted; titles: {:?}; output: {:?}",
        captured_window_titles(output),
        read_output(output)
    );
}

fn wait_for_pane_terminal_title(socket_path: &PathBuf, pane_id: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let request = serde_json::json!({
            "id": "window-title-pane-get",
            "method": "pane.get",
            "params": {"pane_id": pane_id},
        });
        let response = send_json_request(socket_path, &request.to_string());
        if response["result"]["pane"]["terminal_title"].as_str() == Some(expected) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("pane {pane_id} did not report terminal title {expected:?}");
}

fn send_pane_shell_command(socket_path: &PathBuf, pane_id: &str, command: &str) {
    let request = serde_json::json!({
        "id": "window-title-command",
        "method": "pane.send_input",
        "params": {
            "pane_id": pane_id,
            "text": command,
            "keys": ["Enter"],
        }
    });
    let response = send_json_request(socket_path, &request.to_string());
    assert_eq!(response["result"]["type"], "ok", "{response}");
}

#[test]
fn configured_window_title_tracks_all_tokens_and_focused_osc_only() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let (server, client, output) = attach_thin_client_with_config(
        &config_home,
        &runtime_dir,
        &api_socket,
        &client_socket,
        "onboarding = false\n[ui]\nwindow_title = \"H={hostname}|W={workspace}|T={tab}|P={pane}|O={terminal_title}\"\n",
    );

    let created = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "create-workspace",
            "method": "workspace.create",
            "params": {"cwd": base, "focus": true},
        })
        .to_string(),
    );
    assert_eq!(created["result"]["type"], "workspace_created", "{created}");
    let workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("pane id")
        .to_string();
    let tab_id = created["result"]["tab"]["tab_id"]
        .as_str()
        .expect("tab id")
        .to_string();

    for request in [
        serde_json::json!({
            "id": "rename-workspace",
            "method": "workspace.rename",
            "params": {"workspace_id": workspace_id, "label": "space-a"},
        }),
        serde_json::json!({
            "id": "rename-tab",
            "method": "tab.rename",
            "params": {"tab_id": tab_id, "label": "tab-a"},
        }),
        serde_json::json!({
            "id": "rename-pane",
            "method": "pane.rename",
            "params": {"pane_id": pane_id, "label": "pane-a"},
        }),
    ] {
        let response = send_json_request(&api_socket, &request.to_string());
        assert!(response.get("result").is_some(), "{response}");
    }

    let renamed = wait_for_window_title(&output, "|W=space-a|T=tab-a|P=pane-a|O=");
    assert!(renamed.starts_with("H="));
    assert!(
        !renamed.starts_with("H=|"),
        "hostname token was empty: {renamed}"
    );

    send_pane_shell_command(&api_socket, &pane_id, r"printf '\033]0;building\007'");
    wait_for_window_title(&output, "|W=space-a|T=tab-a|P=pane-a|O=building");

    let second_tab = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "second-tab",
            "method": "tab.create",
            "params": {"workspace_id": workspace_id, "focus": true},
        })
        .to_string(),
    );
    assert_eq!(second_tab["result"]["type"], "tab_created", "{second_tab}");
    let second_pane_id = second_tab["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("second pane id")
        .to_string();
    wait_for_window_title(&output, "|W=space-a|T=2|P=|O=");
    let titles_before_hidden_update = captured_window_titles(&output).len();
    send_pane_shell_command(&api_socket, &pane_id, r"printf '\033]0;hidden update\007'");
    // Intentionally consume the AppState title through a read-only request
    // before the queued source is handled.
    wait_for_pane_terminal_title(&api_socket, &pane_id, "hidden update");
    send_pane_shell_command(
        &api_socket,
        &second_pane_id,
        r"printf '\033]0;foreground marker\007'",
    );
    wait_for_window_title(&output, "|W=space-a|T=2|P=|O=foreground marker");
    assert!(
        captured_window_titles(&output)[titles_before_hidden_update..]
            .iter()
            .all(|title| !title.ends_with("|O=hidden update")),
        "a hidden pane title reached the outer terminal"
    );

    let focused = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "focus-first-tab",
            "method": "tab.focus",
            "params": {"tab_id": tab_id},
        })
        .to_string(),
    );
    assert_eq!(focused["result"]["tab"]["focused"], true, "{focused}");
    wait_for_window_title(&output, "|W=space-a|T=tab-a|P=pane-a|O=hidden update");

    drop(server);
    cleanup_spawned_herdr(client, base);
}

/// Polls until the client exits, then returns only the output captured after
/// the `since` byte watermark. Panics if the client does not exit within the
/// deadline.
fn drain_until_client_exits(
    thin_client: &mut SpawnedHerdr,
    output: &SharedOutput,
    since: usize,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut exited = false;
    while Instant::now() < deadline {
        if thin_client.child.try_wait().ok().flatten().is_some() {
            exited = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // Give the reader thread a beat to flush trailing teardown bytes.
    thread::sleep(Duration::from_millis(100));
    let full = read_output(output);
    assert!(exited, "thin client should exit; output: {full:?}");
    full.get(since..).unwrap_or_default().to_string()
}

/// Attaches a thin client, runs `trigger` to force an exit, and asserts the
/// client emits the mouse teardown after that point. The teardown markers also
/// appear in normal attach output, so only bytes emitted after the trigger
/// (past the watermark) count.
fn assert_client_restores_terminal(trigger: impl FnOnce(&mut SpawnedHerdr, &mut SpawnedHerdr)) {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let (mut spawned_server, mut thin_client, pty_output) =
        attach_thin_client(&config_home, &runtime_dir, &api_socket, &client_socket);

    let since = output_len(&pty_output);
    trigger(&mut spawned_server, &mut thin_client);

    let output = drain_until_client_exits(&mut thin_client, &pty_output, since);
    assert!(
        output_has_mouse_teardown(&output),
        "client must emit mouse teardown after trigger; output after trigger: {output:?}"
    );

    // SpawnedHerdr::Drop kills and reaps both processes with a bounded wait.
    drop(spawned_server);
    cleanup_spawned_herdr(thin_client, base);
}

/// The `--remote` ssh-death path: killing the bridge closes the socket, the
/// client sees EOF and unwinds normally, so the terminal is restored. This is
/// the path that does NOT deliver a signal to the client. Guards against a
/// regression that would leave mouse reporting on after an ssh disconnect.
#[test]
fn client_restores_terminal_on_server_eof() {
    assert_client_restores_terminal(|server, _client| {
        // Kill the server unexpectedly; the client socket closes and the
        // client reader hits EOF, mirroring the ssh bridge dying under
        // `herdr --remote`.
        if let Some(pid) = server.child.process_id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        server.close_master();
    });
}

/// A direct SIGHUP/SIGTERM with a writable terminal follows the graceful quit
/// path and emits the terminal teardown. Actual terminal-window closure also
/// makes the PTY unwritable and is covered separately below.
#[test]
fn client_restores_terminal_on_sighup() {
    assert_client_restores_terminal(|_server, client| {
        let pid = client.child.process_id().expect("thin client pid") as libc::pid_t;
        unsafe {
            libc::kill(pid, libc::SIGHUP);
        }
    });
}

fn read_until_client_attaches(client: &SpawnedHerdr) -> String {
    let master = client._master.as_ref().expect("thin client master");
    let fd = master.as_raw_fd().expect("thin client PTY file descriptor");
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert_ne!(flags, -1, "read thin client PTY flags");
    assert_ne!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        -1,
        "make thin client PTY nonblocking"
    );

    let mut reader = master.try_clone_reader().expect("clone client PTY reader");
    let mut output = String::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let mut buf = [0u8; 4096];
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => output.push_str(&String::from_utf8_lossy(&buf[..n])),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("read thin client PTY: {err}"),
        }
        if output.contains('\u{2500}')
            || output.contains("workspace")
            || output.contains("pane")
            || output.contains("terminal")
        {
            return output;
        }
    }
    panic!("thin client must attach and render a frame; output: {output:?}");
}

#[test]
fn client_exits_cleanly_when_terminal_and_transport_hang_up() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned_server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);
    read_until_client_attaches(&thin_client);

    // Freeze the client so the dead terminal and transport EOF are both
    // observable when it resumes, making the `--remote` shutdown race deterministic.
    let client_pid = thin_client.child.process_id().expect("thin client pid") as libc::pid_t;
    assert_eq!(
        unsafe { libc::kill(client_pid, libc::SIGSTOP) },
        0,
        "stop thin client"
    );
    let server_pid = spawned_server.child.process_id().expect("server pid") as libc::pid_t;
    assert_eq!(
        unsafe { libc::kill(server_pid, libc::SIGKILL) },
        0,
        "kill server transport"
    );
    spawned_server.close_master();
    thin_client.close_master();
    assert_eq!(
        unsafe { libc::kill(client_pid, libc::SIGCONT) },
        0,
        "resume thin client"
    );

    let deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = thin_client.child.try_wait().expect("poll thin client") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };

    drop(spawned_server);
    cleanup_spawned_herdr(thin_client, base);

    let status = status.expect("thin client should exit after terminal and transport hang up");
    assert!(
        status.success(),
        "thin client should exit cleanly after terminal and transport hang up, got {status}"
    );
}

#[test]
fn client_exits_cleanly_when_terminal_hangs_up() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned_server = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);
    let attached_output = read_until_client_attaches(&thin_client);

    // Closing the final PTY master models the outer terminal disappearing: the
    // foreground client receives SIGHUP and writes to stdout/stderr fail.
    thin_client.close_master();
    let deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = thin_client.child.try_wait().expect("poll thin client") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let server_response = ping_socket(&api_socket);

    drop(spawned_server);
    cleanup_spawned_herdr(thin_client, base);

    let status = status.unwrap_or_else(|| {
        panic!("thin client did not exit after PTY hangup; attach output: {attached_output:?}")
    });
    assert!(
        status.success(),
        "thin client should exit cleanly after PTY hangup, got {status}; attach output: {attached_output:?}"
    );
    assert!(
        server_response.contains("pong"),
        "server should survive client PTY hangup: {server_response}"
    );
}

#[test]
fn client_receives_pane_surface_after_pane_output() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) = client_shell_handshake(&mut stream, CURRENT_PROTOCOL, 54, 23)
        .expect("handshake should succeed");
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none(), "{error:?}");
    wait_for_client_shell_bootstrap(&mut stream, Duration::from_secs(10))
        .expect("initial client shell bootstrap");

    let created = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "create-output-workspace",
            "method": "workspace.create",
            "params": {"label": "output", "focus": true}
        })
        .to_string(),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id");
    assert!(wait_for_message_variant(
        &mut stream,
        Duration::from_secs(5),
        SERVER_MESSAGE_PANE_SURFACE,
    )
    .expect("wait for created workspace surface"));

    let sent = send_json_request(
        &api_socket,
        &serde_json::json!({
            "id": "send-output",
            "method": "pane.send_text",
            "params": {"pane_id": pane_id, "text": "printf 'test-output\\n'\\n"}
        })
        .to_string(),
    );
    assert!(sent.get("error").is_none(), "{sent}");
    assert!(
        wait_for_message_variants(
            &mut stream,
            Duration::from_secs(5),
            &[
                SERVER_MESSAGE_PANE_SURFACE,
                SERVER_MESSAGE_PANE_SURFACE_PATCH,
            ],
        )
        .expect("wait for post-output pane surface"),
        "should receive a pane surface update after pane output"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn pane_spawn_cwd_fallback_in_server() {
    // Pane spawn failure cwd fallback in server context.
    // This test verifies that the server can start even with invalid
    // session data pointing to non-existent directories.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let data_dir = config_home.join(app_dir_name());
    let missing_cwd = base.join("missing-cwd-for-test");
    let missing_cwd = missing_cwd.to_str().expect("test cwd should be UTF-8");
    fs::create_dir_all(&data_dir).unwrap();
    let session = serde_json::json!({
        "version": 2,
        "workspaces": [{
            "custom_name": "missing-cwd",
            "layout": { "Pane": 0 },
            "panes": { "0": { "cwd": missing_cwd } },
            "zoomed": false,
            "focused": 0,
            "root_pane": 0
        }],
        "active": 0,
        "selected": 0
    });
    fs::write(
        data_dir.join("session.json"),
        serde_json::to_vec_pretty(&session).unwrap(),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let workspaces = send_json_request(
        &api_socket,
        r#"{"id":"workspace_list","method":"workspace.list","params":{}}"#,
    );
    let restored_workspace = workspaces["result"]["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["label"] == "missing-cwd")
        .expect("server should restore workspace with missing pane cwd");
    let workspace_id = restored_workspace["workspace_id"]
        .as_str()
        .expect("restored workspace should have public id");
    let pane_id = first_pane_id_in_workspace(&api_socket, workspace_id);
    let pane = send_json_request(
        &api_socket,
        &format!(r#"{{"id":"pane_get","method":"pane.get","params":{{"pane_id":"{pane_id}"}}}}"#),
    );
    assert_eq!(pane["result"]["pane"]["workspace_id"], workspace_id);
    let cwd = pane["result"]["pane"]["cwd"]
        .as_str()
        .expect("restored pane should report fallback cwd");
    assert_ne!(cwd, missing_cwd);
    assert!(
        std::path::Path::new(cwd).exists(),
        "fallback cwd should exist: {cwd}"
    );

    let client_shell = spawn_client_shell_process(&config_home, &runtime_dir, &api_socket);
    let output = spawn_pty_drain(
        client_shell
            ._master
            .as_ref()
            .expect("restored client shell PTY")
            .try_clone_reader()
            .expect("clone restored client shell reader"),
    );
    assert!(
        wait_until(Duration::from_secs(8), Duration::from_millis(20), || {
            read_output(&output).contains("missing-cwd")
        }),
        "client shell should render the restored session; output: {:?}",
        read_output(&output)
    );

    drop(spawned);
    cleanup_spawned_herdr(client_shell, base);
}

#[test]
fn graceful_shutdown_sends_server_shutdown_to_client() {
    // Issue 2 fix: SIGINT triggers initiate_shutdown → ServerShutdown
    // broadcast to all clients before the server exits.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
    let (version, error) = client_shell_handshake(&mut stream, CURRENT_PROTOCOL, 54, 23)
        .expect("handshake should succeed");
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none(), "{error:?}");
    wait_for_client_shell_bootstrap(&mut stream, Duration::from_secs(5))
        .expect("client shell bootstrap");

    // Send SIGINT to the server process to trigger graceful shutdown.
    if let Some(pid) = spawned.child.process_id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGINT);
        }
    }

    // The client should receive a ServerShutdown message
    // before the connection is closed, not just an abrupt EOF.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let result = read_server_message(&mut stream);
    match result {
        Ok((variant, _payload)) => {
            assert_eq!(
                variant, SERVER_MESSAGE_SERVER_SHUTDOWN,
                "expected ServerShutdown, got variant {variant}"
            );
        }
        Err(e) => {
            panic!("expected ServerShutdown message before connection close, got error: {e}");
        }
    }

    // Wait for the server to exit.
    spawned.close_master();
    let _ = spawned.child.wait();

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn client_receives_notify_on_agent_state_change() {
    // Notification events (sound/toast) are forwarded as
    // ServerMessage::Notify to connected clients when an agent state change
    // is triggered via the API (pane.report_agent).
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Enable toast and sound in config so the server produces notifications.
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        "onboarding = false\n[ui.toast]\nenabled = true\n[ui.sound]\nenabled = true\n",
    )
    .unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);

    // Spawn the server directly (not using spawn_server helper because it
    // overwrites the config file with a minimal one).
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", &api_socket);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    let spawned = SpawnedHerdr {
        _master: Some(pair.master),
        child,
    };
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&client_socket).expect("should connect");
    let (version, error) = client_shell_handshake(&mut stream, CURRENT_PROTOCOL, 54, 23)
        .expect("handshake should succeed");
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none(), "{error:?}");
    wait_for_client_shell_bootstrap(&mut stream, Duration::from_secs(5))
        .expect("client shell bootstrap");

    // Create a workspace via the API.
    let mut ws_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let request = r#"{"id":"1","method":"workspace.create","params":{}}"#;
    writeln!(ws_stream, "{}", request).unwrap();
    let mut reader = BufReader::new(ws_stream);
    let mut ws_response = String::new();
    reader.read_line(&mut ws_response).unwrap();

    // Extract the workspace ID and pane ID from the response.
    let ws_id = ws_response
        .split('"')
        .find(|s| s.starts_with("w_"))
        .unwrap_or("w_1")
        .to_string();

    // Get pane list to find a pane ID.
    let mut pane_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let pane_request =
        format!(r#"{{"id":"2","method":"pane.list","params":{{"workspace_id":"{ws_id}"}}}}"#);
    writeln!(pane_stream, "{}", pane_request).unwrap();
    let mut pane_reader = BufReader::new(pane_stream);
    let mut pane_response = String::new();
    pane_reader.read_line(&mut pane_response).unwrap();

    // Extract first pane ID (format: p_<ws>_<pane>).
    let pane_id = pane_response
        .split('"')
        .find(|s| s.starts_with("p_"))
        .unwrap_or("p_1_1")
        .to_string();

    // Report agent as Blocked via the API — this should trigger a
    // ServerMessage::Notify with kind=Sound (Request sound).
    let mut report_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let report_request = format!(
        r#"{{"id":"3","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"blocked","source":"test"}}}}"#
    );
    writeln!(report_stream, "{}", report_request).unwrap();
    let mut report_reader = BufReader::new(report_stream);
    let mut report_response = String::new();
    report_reader.read_line(&mut report_response).unwrap();

    // Read messages from the client stream and look for the semantic notification.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut found_notify = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _payload)) => {
                if variant == SERVER_MESSAGE_SEMANTIC_NOTIFICATION {
                    found_notify = true;
                    break;
                }
                // Snapshot and pane-surface messages may arrive first.
            }
            Err(_) => {
                break;
            }
        }
    }

    assert!(
        found_notify,
        "client should receive a semantic notification after pane.report_agent"
    );

    // Now report Idle from Working — this should trigger a Done sound
    // if the pane is in a background workspace.
    // First, create a second workspace to make the first one "background".
    let mut ws2_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let ws2_request = r#"{"id":"4","method":"workspace.create","params":{}}"#;
    writeln!(ws2_stream, "{}", ws2_request).unwrap();
    let mut ws2_reader = BufReader::new(ws2_stream);
    let mut ws2_response = String::new();
    ws2_reader.read_line(&mut ws2_response).unwrap();

    // Focus the new workspace (making the first one background).
    let ws2_id = ws2_response
        .split('"')
        .find(|s| s.starts_with("w_"))
        .unwrap_or("w_2")
        .to_string();
    let mut focus_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let focus_request = format!(
        r#"{{"id":"5","method":"workspace.focus","params":{{"workspace_id":"{ws2_id}"}}}}"#
    );
    writeln!(focus_stream, "{}", focus_request).unwrap();
    let mut focus_reader = BufReader::new(focus_stream);
    let mut focus_response = String::new();
    focus_reader.read_line(&mut focus_response).unwrap();

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should stay responsive after workspace focus"
    );

    // Report agent as Working first, then Idle — this transition in a
    // background workspace should trigger a Done sound notification.
    let mut work_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let work_request = format!(
        r#"{{"id":"6","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"working","source":"test"}}}}"#
    );
    writeln!(work_stream, "{}", work_request).unwrap();
    let mut work_reader = BufReader::new(work_stream);
    let mut work_response = String::new();
    work_reader.read_line(&mut work_response).unwrap();

    assert!(
        wait_until(Duration::from_secs(2), Duration::from_millis(25), || {
            ping_socket(&api_socket).contains("pong")
        }),
        "server should stay responsive after working state report"
    );

    let mut idle_stream = UnixStream::connect(&api_socket).expect("connect to API");
    let idle_request = format!(
        r#"{{"id":"7","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"idle","source":"test"}}}}"#
    );
    writeln!(idle_stream, "{}", idle_request).unwrap();
    let mut idle_reader = BufReader::new(idle_stream);
    let mut idle_response = String::new();
    idle_reader.read_line(&mut idle_response).unwrap();

    // Read messages and look for the done semantic notification.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut found_done_notify = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match read_server_message(&mut stream) {
            Ok((variant, _payload)) => {
                if variant == SERVER_MESSAGE_SEMANTIC_NOTIFICATION {
                    found_done_notify = true;
                    break;
                }
                // Snapshot and pane-surface messages may arrive first.
            }
            Err(e) => {
                eprintln!("read error while looking for done notification: {e}");
                break;
            }
        }
    }

    assert!(
        found_done_notify,
        "client should receive a semantic notification when a background pane transitions Working→Idle"
    );

    cleanup_spawned_herdr(spawned, base);
}
