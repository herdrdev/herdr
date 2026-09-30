#![cfg(unix)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, client_handshake, register_runtime_dir, register_spawned_herdr_pid,
    send_input, unregister_spawned_herdr_pid, wait_for_disconnect, wait_for_socket,
};

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

struct RequestError {
    retryable: bool,
    message: String,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        unregister_spawned_herdr_pid(pid);
    }
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unique_test_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/hlh-{}-{n}", std::process::id()))
}

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket: &Path) -> SpawnedHerdr {
    spawn_server_with_env(config_home, runtime_dir, api_socket, &[])
}

fn spawn_server_with_env(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    extra_env: &[(&str, &str)],
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    let config_path = config_home.join("herdr/config.toml");
    fs::write(
        &config_path,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    )
    .unwrap();

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
    // A debug build resolves XDG_CONFIG_HOME under `herdr-dev`, while this test
    // writes `herdr/config.toml`. Do not drop the explicit path.
    cmd.env("HERDR_CONFIG_PATH", &config_path);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    // Without a sandboxed state root the spawned server reads the developer's
    // real ~/.local/state/herdr, including any agent manifest herdr downloaded
    // there, and writes its own runtime state back into it.
    cmd.env("XDG_STATE_HOME", runtime_dir.join("state"));
    cmd.env("HERDR_SOCKET_PATH", api_socket);
    cmd.env(
        "HERDR_CLIENT_SOCKET_PATH",
        runtime_dir.join("herdr-client.sock"),
    );
    cmd.env("SHELL", "/bin/sh");
    for (key, value) in extra_env {
        cmd.env(key, value);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_named_session_server(
    config_home: &Path,
    runtime_dir: &Path,
    session_name: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    let config_path = config_home.join("herdr-dev/config.toml");
    fs::write(
        &config_path,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    )
    .unwrap();

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
    // Keep the config path bound to the fixture. Do not let a build-mode app
    // directory rename make this handoff setup run on defaults.
    cmd.env("HERDR_CONFIG_PATH", &config_path);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    // Without a sandboxed state root the spawned server reads the developer's
    // real ~/.local/state/herdr, including any agent manifest herdr downloaded
    // there, and writes its own runtime state back into it.
    cmd.env("XDG_STATE_HOME", runtime_dir.join("state"));
    cmd.env("HERDR_SESSION", session_name);
    cmd.env_remove("HERDR_SOCKET_PATH");
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_default_session_server(config_home: &Path, runtime_dir: &Path) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    let config_path = config_home.join("herdr-dev/config.toml");
    fs::write(
        &config_path,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    )
    .unwrap();

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
    // Keep the config path bound to the fixture. Do not let a build-mode app
    // directory rename make this handoff setup run on defaults.
    cmd.env("HERDR_CONFIG_PATH", &config_path);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    // Without a sandboxed state root the spawned server reads the developer's
    // real ~/.local/state/herdr, including any agent manifest herdr downloaded
    // there, and writes its own runtime state back into it.
    cmd.env("XDG_STATE_HOME", runtime_dir.join("state"));
    cmd.env_remove("HERDR_SESSION");
    cmd.env_remove("HERDR_SOCKET_PATH");
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_server_with_args_and_socket_env(
    config_home: &Path,
    runtime_dir: &Path,
    session_name: Option<&str>,
    api_socket_env: Option<&Path>,
    client_socket_env: Option<&Path>,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    let config_path = config_home.join("herdr-dev/config.toml");
    fs::write(
        &config_path,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    if let Some(session_name) = session_name {
        cmd.arg("--session");
        cmd.arg(session_name);
    }
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    // Keep the config path bound to the fixture. Do not let a build-mode app
    // directory rename make this handoff setup run on defaults.
    cmd.env("HERDR_CONFIG_PATH", &config_path);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    // Without a sandboxed state root the spawned server reads the developer's
    // real ~/.local/state/herdr, including any agent manifest herdr downloaded
    // there, and writes its own runtime state back into it.
    cmd.env("XDG_STATE_HOME", runtime_dir.join("state"));
    cmd.env_remove("HERDR_SESSION");
    if let Some(api_socket_env) = api_socket_env {
        cmd.env("HERDR_SOCKET_PATH", api_socket_env);
    } else {
        cmd.env_remove("HERDR_SOCKET_PATH");
    }
    if let Some(client_socket_env) = client_socket_env {
        cmd.env("HERDR_CLIENT_SOCKET_PATH", client_socket_env);
    } else {
        cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    }
    cmd.env("SHELL", "/bin/sh");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn try_request(
    socket_path: &Path,
    request: serde_json::Value,
) -> Result<serde_json::Value, RequestError> {
    let mut stream = UnixStream::connect(socket_path).map_err(|err| RequestError {
        retryable: true,
        message: format!("connect {}: {err}", socket_path.display()),
    })?;
    let request_text = request.to_string();
    stream
        .write_all(request_text.as_bytes())
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("write request to {}: {err}", socket_path.display()),
        })?;
    stream.write_all(b"\n").map_err(|err| RequestError {
        retryable: true,
        message: format!("write newline to {}: {err}", socket_path.display()),
    })?;
    stream.flush().map_err(|err| RequestError {
        retryable: true,
        message: format!("flush request to {}: {err}", socket_path.display()),
    })?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|err| RequestError {
            retryable: true,
            message: format!("read response from {}: {err}", socket_path.display()),
        })?;
    if line.is_empty() {
        return Err(RequestError {
            retryable: true,
            message: format!(
                "empty response from {} for request {request_text}",
                socket_path.display()
            ),
        });
    }
    serde_json::from_str(&line).map_err(|err| RequestError {
        retryable: false,
        message: format!(
            "parse response from {} for request {request_text}: {err}; response was {line:?}",
            socket_path.display()
        ),
    })
}

fn request(socket_path: &Path, request: serde_json::Value) -> serde_json::Value {
    try_request(socket_path, request).unwrap_or_else(|err| panic!("{}", err.message))
}

fn assert_ok(response: serde_json::Value) {
    assert!(
        response.get("result").is_some(),
        "api request failed: {response}"
    );
}

fn wait_for_api(socket_path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match try_request(
            socket_path,
            serde_json::json!({"id":"test:ping","method":"ping","params":{}}),
        ) {
            Ok(response) if response.get("result").is_some() => return,
            Ok(response) => panic!("api ping returned non-success response: {response}"),
            Err(err) if !err.retryable => panic!("{}", err.message),
            Err(err) => {
                last_error = err.message;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "api did not become ready at {}; last error: {last_error}",
        socket_path.display()
    );
}

fn wait_for_api_shutdown(socket_path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match try_request(
            socket_path,
            serde_json::json!({"id":"test:shutdown-ping","method":"ping","params":{}}),
        ) {
            Err(err) if err.retryable => return,
            Err(err) => panic!("api shutdown check failed: {}", err.message),
            Ok(_) => thread::sleep(Duration::from_millis(25)),
        }
    }
    panic!("api did not shut down at {}", socket_path.display());
}

fn write_plugin_manifest(root: &Path, plugin_id: &str) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("herdr-plugin.toml"),
        format!(
            r#"id = "{plugin_id}"
name = "Live handoff test"
version = "0.1.0"
min_herdr_version = "0.6.10"
platforms = ["linux", "macos", "windows"]
"#
        ),
    )
    .unwrap();
}

fn link_plugin(socket_path: &Path, root: &Path) {
    assert_ok(request(
        socket_path,
        serde_json::json!({
            "id": "test:plugin:link",
            "method": "plugin.link",
            "params": {"path": root, "enabled": true}
        }),
    ));
}

fn listed_plugin_ids(socket_path: &Path) -> Vec<String> {
    let response = request(
        socket_path,
        serde_json::json!({"id":"test:plugin:list","method":"plugin.list","params":{}}),
    );
    assert_ok(response.clone());
    response["result"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plugin| plugin["plugin_id"].as_str().unwrap().to_string())
        .collect()
}

fn saved_plugin_ids(registry_path: &Path) -> Vec<String> {
    let mut ids =
        serde_json::from_str::<Vec<serde_json::Value>>(&fs::read_to_string(registry_path).unwrap())
            .unwrap()
            .into_iter()
            .map(|plugin| plugin["plugin_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
    ids.sort();
    ids
}

fn wait_for_output(socket_path: &Path, pane_id: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_text = String::new();
    let mut last_response = serde_json::Value::Null;
    while Instant::now() < deadline {
        let response = request(
            socket_path,
            serde_json::json!({
                "id": "test:pane:read",
                "method": "pane.read",
                "params": {
                    "pane_id": pane_id,
                    "source": "visible",
                    "lines": 20,
                    "format": "text",
                    "strip_ansi": true
                }
            }),
        );
        last_response = response.clone();
        let text = response["result"]["read"]["text"]
            .as_str()
            .unwrap_or_default();
        last_text = text.to_string();
        if text.contains(needle) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "pane output did not contain {needle:?}; last text was {last_text:?}; last response was {last_response}"
    );
}

/// A pane created through `workspace.create` only becomes a valid `agent.start`
/// target once its freshly spawned shell shows up as the foreground process, so
/// retry while the server still reports the pane as busy.
fn start_agent_when_shell_is_ready(
    socket_path: &Path,
    request_body: serde_json::Value,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let response = request(socket_path, request_body.clone());
        if response["error"]["code"].as_str() != Some("agent_pane_busy")
            || Instant::now() >= deadline
        {
            return response;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Block until `pane_id` no longer reports `agent_status: "working"`.
///
/// Detection reports Working for as long as a pane produced output inside
/// `AGENT_RECENT_OUTPUT_WINDOW`, and a hook report of idle is rescanned back
/// into Working while that window is open. A fixed sleep cannot express that
/// precondition: on a loaded machine the shell can echo the launch command
/// several seconds after `pane.send_input`, so the window is still open when
/// the sleep ends and the later done label silently becomes working.
fn wait_for_agent_status_not_working(socket_path: &Path, pane_id: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last = String::new();
    while Instant::now() < deadline {
        let response = request(
            socket_path,
            serde_json::json!({
                "id": "test:pane:await-quiet",
                "method": "pane.get",
                "params": {"pane_id": pane_id}
            }),
        );
        last = response["result"]["pane"]["agent_status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if last != "working" {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("pane {pane_id} still reports agent_status {last:?} after {timeout:?}");
}

/// `pane.get` and agent reports are eventually consistent, so the first query
/// after a report can still return the previous status.
fn wait_for_pane_agent_status(
    socket_path: &Path,
    request_id: &str,
    pane_id: &str,
    expected: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        let pane = request(
            socket_path,
            serde_json::json!({
                "id": request_id,
                "method": "pane.get",
                "params": {"pane_id": pane_id}
            }),
        );
        let observed = pane["result"]["pane"]["agent_status"].as_str();
        if observed == Some(expected) {
            return pane;
        }
        if Instant::now() >= deadline {
            panic!(
                "pane {pane_id} never reported agent_status {expected:?} after {timeout:?}; \
                 last observed agent_status: {observed:?}; last response: {pane}"
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_file_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_text = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path) {
            last_text = text;
            if last_text.contains(needle) {
                return last_text;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "{} did not contain {needle:?}; last text was {last_text:?}",
        path.display()
    );
}

#[cfg(target_os = "linux")]
fn server_ptmx_fd_count(pid: u32) -> usize {
    let Ok(entries) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        // ptmx master node: /dev/ptmx or /dev/pts/ptmx (devpts); slaves /dev/pts/<N> excluded.
        .filter(|target| target == Path::new("/dev/ptmx") || target == Path::new("/dev/pts/ptmx"))
        .count()
}

#[cfg(target_os = "macos")]
fn server_ptmx_fd_count(pid: u32) -> usize {
    let Ok(output) = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-Fn"])
        .output()
    else {
        return 0;
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| *line == "n/dev/ptmx")
        .count()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_server_ptmx_fd_count(pid: u32, expected: usize, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut last_count = 0;
    while Instant::now() < deadline {
        last_count = server_ptmx_fd_count(pid);
        if last_count == expected {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("server pid {pid} had {last_count} ptmx master fds; expected {expected}");
}

#[cfg(target_os = "linux")]
fn wait_for_replacement_server_pid(runtime_dir: &Path, old_pid: u32, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    let mut last_pids = Vec::new();
    while Instant::now() < deadline {
        last_pids = support::herdr_server_pids_for_runtime_dir(runtime_dir).unwrap_or_default();
        if let Some(pid) = last_pids.iter().copied().find(|pid| *pid != old_pid) {
            return pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "replacement server for {} did not appear; last pids: {:?}",
        runtime_dir.display(),
        last_pids
    );
}

#[cfg(target_os = "macos")]
fn wait_for_replacement_server_pid(_runtime_dir: &Path, old_pid: u32, timeout: Duration) -> u32 {
    let handoff_socket_pattern = format!("herdr-handoff-{old_pid}.sock");
    let deadline = Instant::now() + timeout;
    let mut last_stdout = String::new();
    while Instant::now() < deadline {
        if let Ok(output) = std::process::Command::new("pgrep")
            .args(["-af", &handoff_socket_pattern])
            .output()
        {
            last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            for line in last_stdout.lines() {
                let Some(pid_text) = line.split_whitespace().next() else {
                    continue;
                };
                let Ok(pid) = pid_text.parse::<u32>() else {
                    continue;
                };
                if pid != old_pid {
                    return pid;
                }
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "replacement server for {} did not appear; last pgrep output: {}",
        _runtime_dir.display(),
        last_stdout
    );
}

fn unused_local_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_http_contains(port: u16, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut last_response = String::new();
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ =
                stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            last_response = response;
            if last_response.contains(needle) {
                return last_response;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "http server on port {port} did not return {needle:?}; last response was {last_response:?}"
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn live_server_holds_one_pty_master_fd_per_pane() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let server_pid = spawned
        .child
        .process_id()
        .expect("test server should expose pid");
    wait_for_server_ptmx_fd_count(server_pid, 0, Duration::from_secs(5));

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 1, Duration::from_secs(5));

    let second = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-second",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": true
            }
        }),
    );
    assert_ok(second.clone());
    let second_pane_id = second["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_for_server_ptmx_fd_count(server_pid, 2, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split-third",
            "method": "pane.split",
            "params": {
                "target_pane_id": second_pane_id,
                "direction": "down",
                "focus": true
            }
        }),
    ));
    wait_for_server_ptmx_fd_count(server_pid, 3, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    let replacement_pid =
        wait_for_replacement_server_pid(&runtime_dir, server_pid, Duration::from_secs(10));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_server_ptmx_fd_count(replacement_pid, 3, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_named_session_socket_paths() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let session_dir = config_home.join("herdr-dev/sessions/work");
    let api_socket = session_dir.join("herdr.sock");
    let client_socket = session_dir.join("herdr-client.sock");

    let spawned = spawn_named_session_server(&config_home, &runtime_dir, "work");
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert!(
        !config_home.join("herdr-dev/herdr.sock").exists(),
        "named handoff unexpectedly bound the default session API socket"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_ignores_leaked_default_socket_env_for_named_session() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let default_session_dir = config_home.join("herdr-dev");
    let default_api_socket = default_session_dir.join("herdr.sock");
    let default_client_socket = default_session_dir.join("herdr-client.sock");
    let work_session_dir = config_home.join("herdr-dev/sessions/work");
    let work_api_socket = work_session_dir.join("herdr.sock");
    let work_client_socket = work_session_dir.join("herdr-client.sock");

    let default_spawned = spawn_default_session_server(&config_home, &runtime_dir);
    wait_for_socket(&default_api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let work_spawned = spawn_server_with_args_and_socket_env(
        &config_home,
        &runtime_dir,
        Some("work"),
        Some(&default_api_socket),
        Some(&default_client_socket),
    );
    wait_for_socket(&work_api_socket, Duration::from_secs(10));

    assert_ok(request(
        &work_api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(work_spawned);
    wait_for_api(&default_api_socket, Duration::from_secs(10));
    wait_for_api(&work_api_socket, Duration::from_secs(10));
    wait_for_socket(&work_client_socket, Duration::from_secs(5));

    let _ = request(
        &work_api_socket,
        serde_json::json!({"id":"test:stop-work","method":"server.stop","params":{}}),
    );
    let _ = request(
        &default_api_socket,
        serde_json::json!({"id":"test:stop-default","method":"server.stop","params":{}}),
    );
    drop(default_spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_client_socket_env_without_api_socket_env() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = config_home.join("herdr-dev/herdr.sock");
    let client_socket = runtime_dir.join("custom-client.sock");

    let spawned = spawn_server_with_args_and_socket_env(
        &config_home,
        &runtime_dir,
        None,
        None,
        Some(&client_socket),
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_installed_plugins() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = config_home.join("herdr-dev/herdr.sock");
    let registry_path = config_home.join("herdr-dev/plugins.json");
    let existing_plugin = base.join("plugins/existing");
    let added_plugin = base.join("plugins/added");
    write_plugin_manifest(&existing_plugin, "test.live-handoff-existing");
    write_plugin_manifest(&added_plugin, "test.live-handoff-added");

    let spawned = spawn_default_session_server(&config_home, &runtime_dir);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    link_plugin(&api_socket, &existing_plugin);
    assert_eq!(
        listed_plugin_ids(&api_socket),
        ["test.live-handoff-existing"]
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_eq!(
        listed_plugin_ids(&api_socket),
        ["test.live-handoff-existing"]
    );
    link_plugin(&api_socket, &added_plugin);
    assert_eq!(
        saved_plugin_ids(&registry_path),
        ["test.live-handoff-added", "test.live-handoff-existing"]
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_accepted_group_history() {
    fn wait_for_catalog(
        socket_path: &Path,
        authority: Option<&str>,
        state: &str,
        error_fragment: Option<&str>,
    ) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last = serde_json::Value::Null;
        while Instant::now() < deadline {
            last = request(
                socket_path,
                serde_json::json!({"id":"test:fleet","method":"fleet.list","params":{}}),
            );
            let found = last["result"]["snapshot"]["authority_catalogs"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|catalog| {
                    catalog["connection"] == "history-source"
                        && catalog["state"] == state
                        && authority.is_none_or(|authority| catalog["authority_id"] == authority)
                        && error_fragment.is_none_or(|fragment| {
                            catalog["error"]
                                .as_str()
                                .is_some_and(|error| error.contains(fragment))
                        })
                });
            if found {
                return last;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("fleet catalog did not reach {state}: {last}");
    }

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = config_home.join("herdr-dev/herdr.sock");
    let authority_socket = base.join("authority.sock");
    let config_path = config_home.join("herdr-dev/config.toml");
    let data_dir = config_home.join("herdr-dev");
    let authority = "AQEBAQEBAQEBAQEBAQEBAQ";

    let mut spawned = spawn_default_session_server(&config_home, &runtime_dir);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    fs::write(
        &config_path,
        format!(
            "onboarding = false\n[ui]\nshow_home_on_start = false\n\
             [remote.fleet]\nself_name = \"replacement\"\ntimeout_ms = 100\n\
             refresh_interval_ms = 100\n[[remote.fleet.hosts]]\n\
             name = \"history-source\"\nlocal = true\nsocket = \"{}\"\n",
            authority_socket.display()
        ),
    )
    .unwrap();
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:reload","method":"server.reload_config","params":{}}),
    ));
    wait_for_catalog(&api_socket, None, "unavailable", None);

    fs::write(
        data_dir.join("authority-acceptance-ledger-v2.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "version": 2,
            "authorities": [{
                "authority_id": authority,
                "revision": 2,
                "groups": [{
                    "id": {"owner": authority, "local": 1},
                    "revision": 2,
                    "state": "deleted"
                }],
                "memberships": []
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let old_pid = spawned
        .child
        .process_id()
        .expect("test server should expose pid");
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    wait_for_api(&api_socket, Duration::from_secs(10));
    let old_exit_deadline = Instant::now() + Duration::from_secs(5);
    while spawned.child.try_wait().unwrap().is_none() && Instant::now() < old_exit_deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        spawned.child.try_wait().unwrap().is_some(),
        "retiring server {old_pid} did not exit"
    );
    drop(spawned);

    let listener = UnixListener::bind(&authority_socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop_fixture = Arc::new(AtomicBool::new(false));
    let fixture_stop = Arc::clone(&stop_fixture);
    let fixture = thread::spawn(move || {
        while !fixture_stop.load(Ordering::Acquire) {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("authority fixture accept failed: {error}"),
            };
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let incoming: serde_json::Value = serde_json::from_str(&line).unwrap();
            let result = match incoming["method"].as_str() {
                Some("agent.list") => {
                    serde_json::json!({"type": "agent_list", "agents": []})
                }
                Some("ping") => serde_json::json!({
                    "type": "pong",
                    "version": support::expected_version(),
                    "protocol": support::CURRENT_PROTOCOL
                }),
                Some("group.host_snapshot") => serde_json::json!({
                    "type": "group_host_snapshot",
                    "snapshot": {
                        "authority_id": authority,
                        "revision": 3,
                        "groups": [{
                            "id": {"owner": authority, "local": 1},
                            "revision": 3,
                            "state": "active",
                            "name": "Revived"
                        }],
                        "memberships": []
                    }
                }),
                Some("session.snapshot") => serde_json::json!({
                    "type": "session_snapshot",
                    "snapshot": {
                        "version": support::expected_version(),
                        "protocol": support::CURRENT_PROTOCOL,
                        "workspaces": [],
                        "tabs": [],
                        "panes": [],
                        "layouts": [],
                        "agents": []
                    }
                }),
                Some("loop.list") => serde_json::json!({"type": "loop_list", "loops": []}),
                method => panic!("unexpected authority fixture method: {method:?}"),
            };
            writeln!(
                stream,
                "{}",
                serde_json::json!({"id": incoming["id"], "result": result})
            )
            .unwrap();
        }
    });

    let response = wait_for_catalog(
        &api_socket,
        Some(authority),
        "stale",
        Some("group 1 attempts to replace an observed tombstone"),
    );
    // Revision 3 is valid by itself and would be fresh without retained history.
    // Only a replacement that adopted revision 2's tombstone can reject this revival.
    let catalog = response["result"]["snapshot"]["authority_catalogs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|catalog| catalog["authority_id"] == authority)
        .unwrap();
    assert_eq!(catalog["snapshot"]["revision"], 3);
    assert_eq!(catalog["snapshot"]["groups"][0]["state"], "active");
    assert_eq!(catalog["snapshot"]["groups"][0]["name"], "Revived");

    stop_fixture.store(true, Ordering::Release);
    fixture.join().unwrap();
    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_pane_process_io() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let marker = base.join("child.pid");
    let second_marker = base.join("second-child.pid");
    let hup_marker = base.join("hup");
    let second_hup_marker = base.join("second-hup");
    let received_marker = base.join("received");
    let second_received_marker = base.join("second-received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let split = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:split",
            "method": "pane.split",
            "params": {
                "target_pane_id": pane_id,
                "direction": "right",
                "focus": false
            }
        }),
    );
    assert_ok(split.clone());
    let second_pane_id = split["result"]["pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let command = format!(
        "sh -c 'echo READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        hup_marker.display(),
        received_marker.display()
    );
    let second_command = format!(
        "sh -c 'echo SECOND_READY $$ > {}; trap \"echo HUP >> {}\" HUP; while read line; do echo second:$line; echo second:$line >> {}; done'",
        second_marker.display(),
        second_hup_marker.display(),
        second_received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": second_command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    support::wait_for_file(&second_marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();
    let second_pid_text = fs::read_to_string(&second_marker).unwrap();
    let second_child_pid: u32 = second_pid_text
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:before-log",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "before_replay", "keys": ["Enter"]}
        }),
    ));
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    assert!(
        wait_for_disconnect(&mut client_stream, Duration::from_secs(5)).unwrap(),
        "connected clients should disconnect during live handoff"
    );
    thread::sleep(Duration::from_millis(300));
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);
    assert_eq!(unsafe { libc::kill(second_child_pid as libc::pid_t, 0) }, 0);
    assert!(
        !hup_marker.exists(),
        "pane process received HUP during handoff"
    );
    assert!(
        !second_hup_marker.exists(),
        "second pane process received HUP during handoff"
    );
    wait_for_output(&api_socket, &pane_id, "got:before_replay");

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-handoff");
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:second-pane:send",
            "method": "pane.send_input",
            "params": {"pane_id": second_pane_id, "text": "after-handoff-second", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &second_received_marker,
        "second:after-handoff-second",
        Duration::from_secs(5),
    );
    // The visible pane may wrap the final suffix; the file assertion above
    // already verifies the complete process output.
    wait_for_output(&api_socket, &second_pane_id, "second:after-handoff");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_latched_gate_and_done_label() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let fake_codex = base.join("codex");

    fs::create_dir_all(&base).unwrap();
    // Long enough that the fake agent outlives the whole test even when the
    // suite runs under load: if it exits, the pane loses its agent and the
    // latched label reads back as unknown rather than done. The server is
    // stopped at the end of the test, which reaps it.
    fs::write(&fake_codex, "#!/bin/sh\nsleep 600\n").unwrap();
    fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let done_created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:done-workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let done_pane_id = done_created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:run-agent",
            "method": "pane.send_input",
            "params": {
                "pane_id": done_pane_id,
                "text": fake_codex.display().to_string(),
                "keys": ["Enter"]
            }
        }),
    ));
    // The shell echoes the launch command into the PTY. That echo has to age
    // out of the recent-output window before the idle report below, otherwise
    // the report-triggered screen rescan turns it back into Working. Wait for
    // the observed status instead of a fixed delay.
    wait_for_output(&api_socket, &done_pane_id, "codex");
    wait_for_agent_status_not_working(&api_socket, &done_pane_id, Duration::from_secs(60));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:old-session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex",
                "agent": "codex",
                "seq": 1,
                "agent_session_id": "handoff-old-session",
                "session_start_source": "startup"
            }
        }),
    ));
    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:gate-workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:working",
            "method": "pane.report_agent",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "working",
                "seq": 1,
                "v": 2,
                "agent_session_id": "handoff-old-session",
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:idle",
            "method": "pane.report_agent",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "idle",
                "seq": 2,
                "v": 2,
                "agent_session_id": "handoff-old-session",
                "completion": "complete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    let source = "herdr:codex-closing-block";
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": source,
                "agent": "codex",
                "state": "blocked",
                "seq": 1,
                "v": 2,
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [{"n": 1, "label": "Gate", "text": "Choose the release path"}],
                "items": [],
                "decisions": []
            }
        }),
    ));
    let before = wait_for_pane_agent_status(
        &api_socket,
        "test:pane:before",
        &pane_id,
        "blocked",
        Duration::from_secs(5),
    );
    assert_eq!(before["result"]["pane"]["agent_status"], "blocked");
    assert_eq!(
        before["result"]["pane"]["gates"].as_array().unwrap().len(),
        1
    );
    wait_for_pane_agent_status(
        &api_socket,
        "test:done:before",
        &done_pane_id,
        "done",
        Duration::from_secs(15),
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:next-turn",
            "method": "pane.report_agent",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "working",
                "seq": 3,
                "v": 2,
                "agent_session_id": "handoff-old-session",
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:new-session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex",
                "agent": "codex",
                "seq": 2,
                "agent_session_id": "handoff-new-session",
                "session_start_source": "clear"
            }
        }),
    ));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    // Let the imported runtime complete several detector ticks. A restored
    // lifecycle label is not preserved if the new detector immediately
    // replaces it with its own default Unknown state.
    thread::sleep(Duration::from_secs(1));

    let after = request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:after",
            "method": "pane.get",
            "params": {"pane_id": pane_id}
        }),
    );
    assert_eq!(
        after["result"]["pane"]["gates"].as_array().unwrap().len(),
        1
    );
    assert_eq!(after["result"]["pane"]["agent_status"], "blocked");
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:empty-after-handoff",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": source,
                "agent": "codex",
                "state": "idle",
                "seq": 2,
                "v": 2,
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    let after_empty_report = wait_for_pane_agent_status(
        &api_socket,
        "test:pane:after-empty-report",
        &pane_id,
        "blocked",
        Duration::from_secs(5),
    );
    assert_eq!(
        after_empty_report["result"]["pane"]["gates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:done:after-handoff-completion",
            "method": "pane.report_agent",
            "params": {
                "pane_id": done_pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "idle",
                "seq": 4,
                "v": 2,
                "agent_session_id": "handoff-new-session",
                "completion": "complete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    wait_for_pane_agent_status(
        &api_socket,
        "test:done:after",
        &done_pane_id,
        "done",
        Duration::from_secs(15),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn cold_restart_preserves_blocked_closing_gate_without_agent_session_id() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    fs::create_dir_all(&base).unwrap();
    let first = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:cold:workspace",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:cold:blocked",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "blocked",
                "seq": 1,
                "v": 2,
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [{"n": 1, "label": "Gate", "text": "Choose the release path"}],
                "items": [],
                "decisions": []
            }
        }),
    ));
    wait_for_pane_agent_status(
        &api_socket,
        "test:cold:before",
        &pane_id,
        "blocked",
        Duration::from_secs(5),
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:cold:stop","method":"server.stop","params":{}}),
    ));
    wait_for_api_shutdown(&api_socket, Duration::from_secs(5));
    drop(first);

    let restarted = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_api(&api_socket, Duration::from_secs(10));
    let after = wait_for_pane_agent_status(
        &api_socket,
        "test:cold:after",
        &pane_id,
        "blocked",
        Duration::from_secs(10),
    );
    assert_eq!(
        after["result"]["pane"]["gates"].as_array().unwrap().len(),
        1
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:cold:empty-report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": "herdr:codex-closing-block",
                "agent": "codex",
                "state": "idle",
                "seq": 2,
                "v": 2,
                "completion": "incomplete",
                "parse_status": "ok",
                "workers_unknown": false,
                "gates": [],
                "items": [],
                "decisions": []
            }
        }),
    ));
    let after_empty_report = wait_for_pane_agent_status(
        &api_socket,
        "test:cold:after-empty-report",
        &pane_id,
        "blocked",
        Duration::from_secs(5),
    );
    assert_eq!(
        after_empty_report["result"]["pane"]["gates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:cold:stop-again","method":"server.stop","params":{}}),
    );
    drop(restarted);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_keyboard_protocol_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("keyboard-ready");
    let received_marker = base.join("keyboard-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>5u")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");
    send_input(&mut client_stream, b"\x1b[13;2u").unwrap();

    wait_for_file_contains(&received_marker, "1b5b31333b3275", Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_modify_other_keys_for_client_input() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let script = base.join("read-raw.py");
    let ready_marker = base.join("modify-ready");
    let received_marker = base.join("modify-received");

    fs::create_dir_all(&base).unwrap();
    fs::write(
        &script,
        format!(
            r#"import os
import pathlib
import select
import sys
import tty

sys.stdout.buffer.write(b"\x1b[>4;2m")
sys.stdout.flush()
pathlib.Path({ready:?}).write_text("ready")
tty.setraw(sys.stdin.fileno())
ready_fds, _, _ = select.select([sys.stdin.fileno()], [], [], 5)
data = os.read(sys.stdin.fileno(), 32) if ready_fds else b""
pathlib.Path({received:?}).write_text(data.hex())
"#,
            ready = ready_marker.display().to_string(),
            received = received_marker.display().to_string()
        ),
    )
    .unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("python3 {}", script.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&ready_marker, Duration::from_secs(5));

    let protocol = request(
        &api_socket,
        serde_json::json!({"id":"test:protocol","method":"ping","params":{}}),
    )["result"]["protocol"]
        .as_u64()
        .unwrap() as u32;
    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));

    let mut client_stream = UnixStream::connect(&client_socket).unwrap();
    let (server_protocol, error) = client_handshake(&mut client_stream, protocol, 80, 24).unwrap();
    assert_eq!(server_protocol, protocol);
    assert!(error.is_none(), "client handshake failed: {error:?}");
    send_input(&mut client_stream, b"\x1b[13;2u").unwrap();

    wait_for_file_contains(
        &received_marker,
        "1b5b32373b323b31337e",
        Duration::from_secs(5),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_accepts_canonical_pane_id_from_child_env() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let pane_id_marker = base.join("pane-id");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:print-id",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("printf '%s' \"$HERDR_PANE_ID\" > {}", pane_id_marker.display()), "keys": ["Enter"]}
        }),
    ));
    let old_pane_id = wait_for_file_contains(&pane_id_marker, &pane_id, Duration::from_secs(5));
    assert!(
        old_pane_id == pane_id,
        "unexpected pane id from env: {old_pane_id:?}"
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:old-pane-report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": old_pane_id,
                "source": "handoff-test",
                "agent": "pi",
                "state": "working"
            }
        }),
    ));
    wait_for_pane_agent_status(
        &api_socket,
        "test:old-pane-report:wait",
        &pane_id,
        "working",
        Duration::from_secs(5),
    );
    let agents = request(
        &api_socket,
        serde_json::json!({"id":"test:agent-list","method":"agent.list","params":{}}),
    );
    let found = agents["result"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|agent| {
            agent["agent"].as_str() == Some("pi")
                && agent["agent_status"].as_str() == Some("working")
        });
    assert!(
        found,
        "old pane id report did not update restored pane: {agents}"
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_unmanaged_agent_name_bound_to_saved_session() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let old_session = base.join("old-session.jsonl");
    let new_session = base.join("new-session.jsonl");
    let started_marker = base.join("agent-started");
    let fake_pi = base.join("pi");
    fs::create_dir_all(&base).unwrap();
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nexport HERDR_AGENT=pi\necho started > {}\nexec python3 -c 'import time; time.sleep(30)'\n",
            started_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:start-agent",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": fake_pi, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&started_marker, Duration::from_secs(5));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": pane_id,
                "source": "herdr:pi",
                "agent": "pi",
                "seq": 1,
                "agent_session_path": old_session,
                "session_start_source": "startup"
            }
        }),
    ));
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:report",
            "method": "pane.report_agent",
            "params": {
                "pane_id": pane_id,
                "source": "herdr:pi",
                "agent": "pi",
                "state": "idle",
                "seq": 2,
                "agent_session_path": old_session
            }
        }),
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let response = request(
            &api_socket,
            serde_json::json!({
                "id": "test:agent:wait-for-process",
                "method": "agent.get",
                "params": {"target": pane_id}
            }),
        );
        if response.get("result").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "agent process was not detected: {response}"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:rename",
            "method": "agent.rename",
            "params": {"target": pane_id, "name": "reviewer"}
        }),
    ));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:agent:new-session",
            "method": "pane.report_agent_session",
            "params": {
                "pane_id": pane_id,
                "source": "herdr:pi",
                "agent": "pi",
                "seq": 3,
                "agent_session_path": new_session,
                "session_start_source": "new"
            }
        }),
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let old_name = request(
            &api_socket,
            serde_json::json!({
                "id": "test:agent:get-old-name",
                "method": "agent.get",
                "params": {"target": "reviewer"}
            }),
        );
        if old_name["error"]["code"] == "agent_not_found" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "old session alias was not cleared: {old_name}"
        );
        thread::sleep(Duration::from_millis(25));
    }

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_agent_started_pane_after_agent_exits() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let started_marker = base.join("agent-started");
    let exited_marker = base.join("agent-exited");
    let shell_marker = base.join("shell-after-agent");
    let bin = base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fake_pi = bin.join("pi");
    fs::write(
        &fake_pi,
        format!(
            "#!/bin/sh\nexport HERDR_AGENT=pi\necho started > {}\n/bin/sleep 1\necho exited > {}\n",
            started_marker.display(),
            exited_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_pi, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/bin:/usr/bin", bin.display());

    let spawned = spawn_server_with_env(
        &config_home,
        &runtime_dir,
        &api_socket,
        &[("PATH", path.as_str())],
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);
    let workspace = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace-create",
            "method": "workspace.create",
            "params": { "cwd": "/tmp", "focus": false }
        }),
    );
    assert_ok(workspace.clone());
    let pane_id = workspace["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    let started = start_agent_when_shell_is_ready(
        &api_socket,
        serde_json::json!({
            "id": "test:agent-start",
            "method": "agent.start",
            "params": {
                "name": "handoff-agent",
                "kind": "pi",
                "pane_id": pane_id,
                "timeout_ms": 5000
            }
        }),
        Duration::from_secs(10),
    );
    assert_ok(started);
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));
    thread::sleep(Duration::from_millis(300));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-agent",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_keeps_shell_pane_after_foreground_process_exits() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let started_marker = base.join("foreground-started");
    let exited_marker = base.join("foreground-exited");
    let shell_marker = base.join("shell-after-foreground");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo started > {}; sleep 1; echo exited > {}'",
        started_marker.display(),
        exited_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&started_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    support::wait_for_file(&exited_marker, Duration::from_secs(5));

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:shell-after-foreground",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": format!("echo alive > {}", shell_marker.display()), "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&shell_marker, Duration::from_secs(5));

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_python_http_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let web_root = base.join("web");
    fs::create_dir_all(&web_root).unwrap();
    fs::write(
        web_root.join("index.html"),
        "hello-from-python-before-and-after",
    )
    .unwrap();
    let port = unused_local_port();

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": web_root, "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run-python",
            "method": "pane.send_input",
            "params": {
                "pane_id": pane_id,
                "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                "keys": ["Enter"]
            }
        }),
    ));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    assert_ok(request(
        &api_socket,
        serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
    ));
    drop(spawned);
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_http_contains(
        port,
        "hello-from-python-before-and-after",
        Duration::from_secs(10),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    let _ = client_socket;
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_preserves_http_servers_across_multiple_sessions() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let sessions = [
        (None, config_home.join("herdr-dev/herdr.sock")),
        (
            Some("work"),
            config_home.join("herdr-dev/sessions/work/herdr.sock"),
        ),
    ];
    let mut spawned = Vec::new();
    let mut ports = Vec::new();

    for (session_name, api_socket) in &sessions {
        let web_root = base.join(format!("web-{}", session_name.unwrap_or("default")));
        fs::create_dir_all(&web_root).unwrap();
        fs::write(
            web_root.join("index.html"),
            format!("hello-from-{}", session_name.unwrap_or("default")),
        )
        .unwrap();
        let port = unused_local_port();
        let server = if let Some(session_name) = session_name {
            spawn_named_session_server(&config_home, &runtime_dir, session_name)
        } else {
            spawn_default_session_server(&config_home, &runtime_dir)
        };
        wait_for_socket(api_socket, Duration::from_secs(10));
        let created = request(
            api_socket,
            serde_json::json!({
                "id": "test:workspace:create",
                "method": "workspace.create",
                "params": {"cwd": web_root, "focus": true}
            }),
        );
        let pane_id = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ok(request(
            api_socket,
            serde_json::json!({
                "id": "test:pane:run-python",
                "method": "pane.send_input",
                "params": {
                    "pane_id": pane_id,
                    "text": format!("python3 -m http.server {port} --bind 127.0.0.1"),
                    "keys": ["Enter"]
                }
            }),
        ));
        wait_for_http_contains(
            port,
            &format!("hello-from-{}", session_name.unwrap_or("default")),
            Duration::from_secs(10),
        );
        spawned.push(server);
        ports.push((port, session_name.unwrap_or("default").to_string()));
    }
    register_runtime_dir(&runtime_dir);

    for (_session_name, api_socket) in &sessions {
        assert_ok(request(
            api_socket,
            serde_json::json!({"id":"test:handoff","method":"server.live_handoff","params":{}}),
        ));
    }
    drop(spawned);

    for (_session_name, api_socket) in &sessions {
        wait_for_api(api_socket, Duration::from_secs(10));
    }
    for (port, label) in ports {
        wait_for_http_contains(
            port,
            &format!("hello-from-{label}"),
            Duration::from_secs(10),
        );
    }

    for (_session_name, api_socket) in &sessions {
        let _ = request(
            api_socket,
            serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
        );
    }
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_bad_expected_protocol_rolls_back_old_server() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({
            "id": "test:bad-handoff",
            "method": "server.live_handoff",
            "params": {"expected_protocol": 999999}
        }),
    );
    assert!(
        failed.get("error").is_some(),
        "bad protocol handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-failed-handoff",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": "after-failed-handoff", "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        "got:after-failed-handoff",
        Duration::from_secs(5),
    );
    wait_for_output(&api_socket, &pane_id, "got:after-failed-handoff");

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

fn live_handoff_import_failure_rolls_back_old_server_at(failure_point: &str) {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let marker = base.join("child.pid");
    let received_marker = base.join("received");

    let spawned = spawn_server_with_env(
        &config_home,
        &runtime_dir,
        &api_socket,
        &[("HERDR_TEST_HANDOFF_IMPORT_FAIL", failure_point)],
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    register_runtime_dir(&runtime_dir);

    let created = request(
        &api_socket,
        serde_json::json!({
            "id": "test:workspace:create",
            "method": "workspace.create",
            "params": {"cwd": "/tmp", "focus": true}
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let command = format!(
        "sh -c 'echo READY $$ > {}; while read line; do echo got:$line; echo got:$line >> {}; done'",
        marker.display(),
        received_marker.display()
    );
    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:run",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]}
        }),
    ));
    support::wait_for_file(&marker, Duration::from_secs(5));
    let pid_text = fs::read_to_string(&marker).unwrap();
    let child_pid: u32 = pid_text.split_whitespace().last().unwrap().parse().unwrap();

    let failed = request(
        &api_socket,
        serde_json::json!({"id":"test:handoff-fail","method":"server.live_handoff","params":{}}),
    );
    assert!(
        failed.get("error").is_some(),
        "{failure_point} handoff should fail: {failed}"
    );
    wait_for_api(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(5));
    assert_eq!(unsafe { libc::kill(child_pid as libc::pid_t, 0) }, 0);

    assert_ok(request(
        &api_socket,
        serde_json::json!({
            "id": "test:pane:send-after-import-failure",
            "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": failure_point, "keys": ["Enter"]}
        }),
    ));
    wait_for_file_contains(
        &received_marker,
        &format!("got:{failure_point}"),
        Duration::from_secs(5),
    );

    let _ = request(
        &api_socket,
        serde_json::json!({"id":"test:stop","method":"server.stop","params":{}}),
    );
    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn live_handoff_after_restored_failure_rolls_back_old_server() {
    live_handoff_import_failure_rolls_back_old_server_at("after_restored");
}
