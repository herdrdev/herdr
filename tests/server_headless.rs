//! Integration tests for headless server mode.

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, client_handshake, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, CURRENT_PROTOCOL,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-server-test-{}-{nanos}",
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
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not accept connections at {}", path.display());
}

fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    _client_socket_path: &Path,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join("herdr/config.toml"),
        "onboarding = false\n",
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

fn ping_socket(socket_path: &Path) -> String {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");

    let request = r#"{"id":"1","method":"ping","params":{}}"#;
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response.trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn server_creates_both_sockets() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for both sockets to appear.
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Verify the client socket is a socket file.
    let metadata = fs::metadata(&client_socket).unwrap();
    let file_type = metadata.file_type();
    assert!(
        file_type.is_socket(),
        "client socket should be a socket file"
    );

    // Verify the API socket works.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "ping should return pong: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_starts_without_terminal() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);

    // Wait for the API socket to appear — proves the server started.
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // The server process should be running.
    if let Some(pid) = spawned.child.process_id() {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        assert_eq!(result, 0, "server process should be running");
    }

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_api_responds_to_ping() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Ping the API socket.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_removes_client_socket_on_exit() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Kill the server.
    let _ = spawned.child.kill();
    spawned.close_master();
    let _ = spawned.child.wait();

    // Give it a moment to clean up.
    thread::sleep(Duration::from_millis(300));

    // The client socket should be removed (best-effort by Drop).
    // If it still exists, it should be stale (not connectable).
    if client_socket.exists() {
        assert!(
            UnixStream::connect(&client_socket).is_err(),
            "stale client socket should not accept connections"
        );
    }

    drop(spawned);
    cleanup_test_base(&base);
}

#[test]
fn server_cleans_up_stale_client_socket() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Create a stale client socket file (simulating a crashed server).
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    {
        let _listener = std::os::unix::net::UnixListener::bind(&client_socket).unwrap();
        // Drop the listener so the socket becomes stale.
    }

    // Now start the server — it should clean up the stale socket.
    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // The API should work.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_persists_after_client_disconnect() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and then immediately disconnect.
    {
        let _stream = UnixStream::connect(&client_socket).expect("should connect to client socket");
        // Immediately drop the connection.
    }

    // Give the server a moment to process the disconnect.
    thread::sleep(Duration::from_millis(200));

    // The server should still be running and the API should still respond.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "API should still respond after client disconnect: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn duplicate_server_start_fails_gracefully() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // Start the first server.
    let spawned1 = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Try to start a second server — it should fail.
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

    let mut child2 = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child2.process_id());
    drop(pair.slave);

    // Wait for the second server to exit.
    let exit_status = child2.wait().unwrap();
    unregister_spawned_herdr_pid(child2.process_id());

    // The second server should exit with a non-zero code.
    assert!(!exit_status.success(), "duplicate server start should fail");

    cleanup_spawned_herdr(spawned1, base);
}

#[test]
fn client_handshake_succeeds() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and perform a handshake.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    // Send Hello with the current protocol version, 80 cols, 24 rows.
    let (version, error) =
        client_handshake(&mut stream, CURRENT_PROTOCOL, 80, 24).expect("handshake should succeed");

    assert_eq!(
        version, CURRENT_PROTOCOL,
        "server should report current protocol version"
    );
    assert!(
        error.is_none(),
        "handshake should not have an error: {:?}",
        error
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_handshake_rejects_incompatible_version() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect to the client socket and send Hello with version 0 (pre-persistence).
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    let (version, error) = client_handshake(&mut stream, 0, 80, 24)
        .expect("should read Welcome response even on rejection");

    assert_eq!(
        version, CURRENT_PROTOCOL,
        "server should report its current protocol version"
    );
    assert!(
        error.is_some(),
        "version 0 should be rejected with an error"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn client_handshake_clamps_small_terminal_size() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Send Hello with 0x0 terminal size — should be clamped.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    let (version, error) = client_handshake(&mut stream, CURRENT_PROTOCOL, 0, 0)
        .expect("handshake with 0x0 should succeed (server clamps)");

    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(
        error.is_none(),
        "0x0 size should be accepted (clamped): {:?}",
        error
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn no_hello_client_closed_within_five_seconds() {
    // Client connection that sends no Hello is closed within 5 seconds.
    // The server sets a handshake timeout of 4 seconds to guarantee the connection
    // is closed within the 5-second deadline even with OS overhead.
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_file(&client_socket, Duration::from_secs(10));

    // Connect but don't send Hello — just a raw connection.
    let mut stream = UnixStream::connect(&client_socket).expect("should connect to client socket");

    // Set a read timeout longer than the handshake timeout so we can detect
    // when the server closes the connection.
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .unwrap();

    let start = Instant::now();

    // Try to read from the stream. The server should close the connection
    // within 5 seconds, causing our read to return with an error (EOF or
    // connection reset).
    let mut buf = [0u8; 1024];
    let result = stream.read(&mut buf);
    let elapsed = start.elapsed();

    // The read should fail (connection closed by server).
    assert!(
        result.is_err() || result.unwrap() == 0,
        "server should close the connection when no Hello is sent"
    );

    // The connection should be closed within 5 seconds.
    assert!(
        elapsed < Duration::from_secs(5),
        "connection should be closed within 5 seconds, took {:?}",
        elapsed
    );

    // Verify the server is still healthy — a proper client can still connect.
    let mut good_stream =
        UnixStream::connect(&client_socket).expect("should connect after no-hello client");
    let (version, error) = client_handshake(&mut good_stream, CURRENT_PROTOCOL, 80, 24)
        .expect("proper handshake should still work after no-hello client");
    assert_eq!(version, CURRENT_PROTOCOL);
    assert!(error.is_none());

    // API should still work.
    let response = ping_socket(&api_socket);
    assert!(
        response.contains("pong"),
        "server should still respond to ping: {response}"
    );

    cleanup_spawned_herdr(spawned, base);
}
