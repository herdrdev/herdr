//! Gate B integration tests for the ClientShell protocol.

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, client_shell_handshake, drain_messages, register_runtime_dir,
    register_spawned_herdr_pid, send_client_shell_focus, send_detach, unregister_spawned_herdr_pid,
    wait_for_client_shell_bootstrap, wait_for_message_variant, wait_for_message_variants,
    CURRENT_ENDPOINT_PROTOCOL_GENERATION as CURRENT_PROTOCOL, SERVER_MESSAGE_PANE_SURFACE,
    SERVER_MESSAGE_PANE_SURFACE_PATCH,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    PathBuf::from(format!(
        "/tmp/herdr-multi-client-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let done = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if done == pid as libc::pid_t || done == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_server(config: &Path, runtime: &Path, api: &Path) -> SpawnedHerdr {
    fs::create_dir_all(config.join("herdr")).unwrap();
    fs::create_dir_all(runtime).unwrap();
    register_runtime_dir(runtime);
    fs::write(config.join("herdr/config.toml"), "onboarding = false\n").unwrap();
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
    cmd.env("XDG_CONFIG_HOME", config);
    cmd.env("XDG_RUNTIME_DIR", runtime);
    cmd.env("HERDR_SOCKET_PATH", api);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn spawn_client(config: &Path, runtime: &Path, api: &Path) -> SpawnedHerdr {
    register_runtime_dir(runtime);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("client");
    cmd.env("HERDR_DISABLE_SOUND", "1");
    cmd.env("XDG_CONFIG_HOME", config);
    cmd.env("XDG_RUNTIME_DIR", runtime);
    cmd.env("HERDR_SOCKET_PATH", api);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);
    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn cleanup(server: SpawnedHerdr, base: PathBuf) {
    drop(server);
    cleanup_test_base(&base);
}

fn api_request(socket: &Path, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    writeln!(stream, "{request}").unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

fn create_pane(socket: &Path, label: &str) -> String {
    let result = api_request(
        socket,
        &format!(r#"{{"id":"create","method":"workspace.create","params":{{"label":"{label}"}}}}"#),
    );
    assert!(
        result.get("error").is_none(),
        "workspace.create failed: {result}"
    );
    result
        .pointer("/result/root_pane/pane_id")
        .unwrap()
        .as_str()
        .unwrap()
        .into()
}

fn pane_input(socket: &Path, pane: &str, text: &str) {
    let escaped = text.replace('"', "\\\"");
    let result = api_request(
        socket,
        &format!(
            r#"{{"id":"input","method":"pane.send_input","params":{{"pane_id":"{pane}","text":"{escaped}","keys":["Enter"]}}}}"#
        ),
    );
    assert!(
        result.get("error").is_none(),
        "pane.send_input failed: {result}"
    );
}

fn pane_text(socket: &Path, pane: &str) -> String {
    let result = api_request(
        socket,
        &format!(
            r#"{{"id":"read","method":"pane.read","params":{{"pane_id":"{pane}","source":"recent","lines":200}}}}"#
        ),
    );
    result
        .pointer("/result/read/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into()
}

fn pane_contains(socket: &Path, pane: &str, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_text(socket, pane).contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn shell(socket: &Path, cols: u16, rows: u16) -> UnixStream {
    let mut stream = UnixStream::connect(socket).unwrap();
    let (version, error) =
        client_shell_handshake(&mut stream, CURRENT_PROTOCOL, cols, rows).unwrap();
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none(), "ClientShell handshake failed: {error:?}");
    wait_for_client_shell_bootstrap(&mut stream, Duration::from_secs(5)).unwrap();
    stream
}

fn tty_size(socket: &Path, pane: &str, timeout: Duration) -> (u16, u16) {
    let marker = format!(
        "SIZE_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    pane_input(socket, pane, &format!("echo {marker}; stty size"));
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut found = false;
        for line in pane_text(socket, pane).lines() {
            if line.contains(&marker) {
                found = true;
                continue;
            }
            if found {
                let mut words = line.split_whitespace();
                if let (Some(r), Some(c)) = (words.next(), words.next()) {
                    if let (Ok(r), Ok(c)) = (r.parse(), c.parse()) {
                        return (r, c);
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("pane did not report tty size: {}", pane_text(socket, pane));
}

fn wait_for_tty_size(
    socket: &Path,
    pane: &str,
    timeout: Duration,
    expected: impl Fn((u16, u16)) -> bool,
) -> (u16, u16) {
    let deadline = Instant::now() + timeout;
    loop {
        let size = tty_size(
            socket,
            pane,
            deadline.saturating_duration_since(Instant::now()),
        );
        if expected(size) {
            return size;
        }
        assert!(Instant::now() < deadline, "unexpected tty size: {size:?}");
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn same_tab_geometry_follows_meaningful_client_activity() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config = base.join("config");
    let runtime = base.join("runtime");
    let api = runtime.join("herdr.sock");
    let clients = runtime.join("herdr-client.sock");
    let server = spawn_server(&config, &runtime, &api);
    wait_for_socket(&api, Duration::from_secs(10));
    wait_for_file(&clients, Duration::from_secs(10));
    let pane = create_pane(&api, "effective-size");
    let _large = shell(&clients, 120, 40);
    let mut small = shell(&clients, 80, 24);
    let initial = tty_size(&api, &pane, Duration::from_secs(5));
    assert!(
        initial.0 > 24 && initial.1 > 80,
        "a passive second connection must not steal geometry: {initial:?}"
    );

    send_client_shell_focus(&mut small, true).unwrap();
    let reduced = wait_for_tty_size(&api, &pane, Duration::from_secs(5), |(rows, cols)| {
        rows <= 24 && cols <= 80
    });

    send_detach(&mut small).unwrap();
    drop(small);
    wait_for_tty_size(&api, &pane, Duration::from_secs(5), |(rows, cols)| {
        rows > reduced.0 && cols > reduced.1
    });
    cleanup(server, base);
}

#[test]
fn api_pane_output_is_fanned_out_as_pane_surface_updates() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config = base.join("config");
    let runtime = base.join("runtime");
    let api = runtime.join("herdr.sock");
    let clients = runtime.join("herdr-client.sock");
    let server = spawn_server(&config, &runtime, &api);
    wait_for_socket(&api, Duration::from_secs(10));
    wait_for_file(&clients, Duration::from_secs(10));
    let pane = create_pane(&api, "fanout");
    let mut a = shell(&clients, 100, 30);
    let mut b = shell(&clients, 100, 30);
    drain_messages(&mut a);
    drain_messages(&mut b);
    let marker = format!(
        "FANOUT_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    pane_input(&api, &pane, &format!("printf '{marker}\\n'"));
    assert!(pane_contains(&api, &pane, &marker, Duration::from_secs(5)));
    let surface_updates = [
        SERVER_MESSAGE_PANE_SURFACE,
        SERVER_MESSAGE_PANE_SURFACE_PATCH,
    ];
    assert!(wait_for_message_variants(&mut a, Duration::from_secs(5), &surface_updates).unwrap());
    assert!(wait_for_message_variants(&mut b, Duration::from_secs(5), &surface_updates).unwrap());
    cleanup(server, base);
}

#[test]
fn crashed_client_shell_does_not_affect_survivor() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config = base.join("config");
    let runtime = base.join("runtime");
    let api = runtime.join("herdr.sock");
    let clients = runtime.join("herdr-client.sock");
    let server = spawn_server(&config, &runtime, &api);
    wait_for_socket(&api, Duration::from_secs(10));
    wait_for_file(&clients, Duration::from_secs(10));
    let mut survivor = shell(&clients, 100, 30);
    let crashed = spawn_client(&config, &runtime, &api);
    // Give the supported client process time to complete its ClientShell hello;
    // the point of this test is a connected peer dying, not a failed launch.
    thread::sleep(Duration::from_secs(1));
    let pid = crashed.child.process_id().unwrap();
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    drop(crashed);
    let response = api_request(&api, r#"{"id":"ping","method":"ping","params":{}}"#);
    assert!(response.to_string().contains("pong"));
    pane_input(&api, &create_pane(&api, "survivor"), "printf 'survivor\\n'");
    assert!(wait_for_message_variant(
        &mut survivor,
        Duration::from_secs(5),
        SERVER_MESSAGE_PANE_SURFACE
    )
    .unwrap());
    cleanup(server, base);
}

#[test]
fn rapid_client_shell_connect_disconnect_remains_healthy() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config = base.join("config");
    let runtime = base.join("runtime");
    let api = runtime.join("herdr.sock");
    let clients = runtime.join("herdr-client.sock");
    let server = spawn_server(&config, &runtime, &api);
    wait_for_socket(&api, Duration::from_secs(10));
    wait_for_file(&clients, Duration::from_secs(10));
    for i in 0..10 {
        let mut client = shell(&clients, 80 + i, 24);
        send_detach(&mut client).unwrap();
        drop(client);
    }
    let final_client = shell(&clients, 100, 30);
    drop(final_client);
    let response = api_request(&api, r#"{"id":"ping","method":"ping","params":{}}"#);
    assert!(response.to_string().contains("pong"));
    cleanup(server, base);
}
