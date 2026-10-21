//! Integration tests for headless server mode.

#![cfg(unix)]

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
        support::stop_spawned_herdr(&mut *self.child);
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
    client_socket_path: &Path,
) -> SpawnedHerdr {
    spawn_server_with_relay(
        config_home,
        runtime_dir,
        api_socket_path,
        client_socket_path,
        None,
    )
}

fn spawn_server_with_relay(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    _client_socket_path: &Path,
    relay_socket: Option<&Path>,
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
    support::isolate_herdr_test_process(&mut cmd);
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    if let Some(socket) = relay_socket {
        cmd.env("HERDR_GRAM_REVERSE_SOCKET", socket);
    } else {
        cmd.env_remove("HERDR_GRAM_REVERSE_SOCKET");
    }

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

/// Send one JSON request line over the API socket and parse the single-line reply.
fn api_request(socket_path: &Path, request: &str) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(stream, "{request}").unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(response.trim()).expect("API reply should be valid JSON")
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
fn gram_file_round_trip_upload_download_delete() {
    use base64::Engine as _;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // Upload "hello world" in two chunks under one upload id, exercising the
    // offset-continuation path (chunk 1 must start where chunk 0 ended).
    let upload_id = "int-upload-1";
    let chunk0 = base64::engine::general_purpose::STANDARD.encode(b"hello ");
    let chunk1 = base64::engine::general_purpose::STANDARD.encode(b"world");
    let r0 = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"u0","method":"gram.upload_chunk","params":{{"upload_id":"{upload_id}","offset":0,"data_base64":"{chunk0}"}}}}"#
        ),
    );
    assert_eq!(
        r0["result"]["type"], "ok",
        "chunk 0 should be accepted: {r0}"
    );
    let r1 = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"u1","method":"gram.upload_chunk","params":{{"upload_id":"{upload_id}","offset":6,"data_base64":"{chunk1}"}}}}"#
        ),
    );
    assert_eq!(
        r1["result"]["type"], "ok",
        "chunk 1 should be accepted: {r1}"
    );

    // Attach the assembled upload to an owner post.
    let post = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"p","method":"gram.post","params":{{"text":"here","file":{{"upload_id":"{upload_id}","name":"greeting.txt","mime":"text/plain"}}}}}}"#
        ),
    );
    assert_eq!(
        post["result"]["type"], "gram_sent",
        "post should succeed: {post}"
    );
    let file = &post["result"]["message"]["file"];
    assert_eq!(file["name"], "greeting.txt");
    assert_eq!(file["size"].as_u64(), Some(11));
    assert_eq!(file["mime"], "text/plain");
    // sha256("hello world")
    assert_eq!(
        file["sha256"],
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
    let message_id = post["result"]["message"]["id"]
        .as_str()
        .expect("message id")
        .to_string();

    // The bounded API returns exact bytes and the digest of the complete file.
    let got = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"g","method":"gram.get_file_chunk","params":{{"id":"{message_id}","offset":0}}}}"#
        ),
    );
    assert_eq!(
        got["result"]["type"], "gram_file_chunk",
        "get_file_chunk failed: {got}"
    );
    assert_eq!(got["result"]["size"], 11);
    assert_eq!(got["result"]["offset"], 0);
    assert_eq!(got["result"]["sha256"], file["sha256"]);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(got["result"]["data_base64"].as_str().expect("data_base64"))
        .unwrap();
    assert_eq!(bytes, b"hello world");

    // The bytes live on disk under the message id. A debug build namespaces the
    // config dir as `herdr-dev`, matching the spawned binary.
    let app_dir = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    let file_dir = config_home.join(format!("{app_dir}/gram-files/{message_id}"));
    assert!(
        file_dir.join("greeting.txt").exists(),
        "file bytes should exist at {file_dir:?}"
    );

    // Delete the message (owner authority) and confirm the bytes are purged.
    let del = api_request(
        &api_socket,
        &format!(r#"{{"id":"d","method":"gram.delete","params":{{"id":"{message_id}"}}}}"#),
    );
    assert_eq!(del["result"]["type"], "ok", "delete should succeed: {del}");
    let gone = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"g2","method":"gram.get_file_chunk","params":{{"id":"{message_id}","offset":0}}}}"#
        ),
    );
    assert!(
        gone.get("error").is_some(),
        "get_file after delete should error: {gone}"
    );
    assert!(
        !file_dir.exists(),
        "file bytes should be purged after delete: {file_dir:?}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn opted_in_remote_gram_refuses_methods_outside_restricted_relay() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let relay_socket = base.join("absent-relay.sock");
    let spawned = spawn_server_with_relay(
        &config_home,
        &runtime_dir,
        &api_socket,
        &client_socket,
        Some(&relay_socket),
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let ping = api_request(&api_socket, r#"{"id":"ping","method":"ping","params":{}}"#);
    assert_eq!(
        ping["result"]["type"], "pong",
        "server should be healthy: {ping}"
    );
    let post = api_request(
        &api_socket,
        r#"{"id":"post","method":"gram.post","params":{"text":"must not enter a second store"}}"#,
    );
    assert_eq!(
        post["error"]["code"], "gram_relay_unsupported",
        "owner post must not silently write the remote store: {post}"
    );
    let list = api_request(
        &api_socket,
        r#"{"id":"list","method":"gram.list","params":{}}"#,
    );
    assert_eq!(
        list["error"]["code"], "gram_relay_unavailable",
        "supported methods must use the absent coordinator, not the remote store: {list}"
    );

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn gram_download_refuses_late_credentials_without_reveal() {
    use base64::Engine as _;
    use std::process::Command;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");
    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let mut bytes = vec![b'a'; 512 * 1024];
    bytes.extend(std::iter::repeat_n(b'b', 300_000));
    let synthetic_token = format!("{}{}", "ghp_", "0a1b2c3d4e".repeat(4));
    bytes.extend_from_slice(format!(" {synthetic_token}\n").as_bytes());
    let upload_id = "late-secret-upload";
    for (index, chunk) in bytes.chunks(256 * 1024).enumerate() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        let offset = index * 256 * 1024;
        let response = api_request(
            &api_socket,
            &format!(
                r#"{{"id":"upload-{index}","method":"gram.upload_chunk","params":{{"upload_id":"{upload_id}","offset":{offset},"data_base64":"{encoded}"}}}}"#
            ),
        );
        assert_eq!(
            response["result"]["type"], "ok",
            "upload failed: {response}"
        );
    }
    let post = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"post","method":"gram.post","params":{{"text":"attachment","file":{{"upload_id":"{upload_id}","name":"report.txt","mime":"text/plain"}}}}}}"#
        ),
    );
    assert_eq!(post["result"]["type"], "gram_sent", "post failed: {post}");
    let id = post["result"]["message"]["id"]
        .as_str()
        .expect("message id");
    let out = base.join("download.txt");
    let run_download = |reveal: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr"));
        command
            .args(["gram", "get-file", id, "--out"])
            .arg(&out)
            .env("HERDR_SOCKET_PATH", &api_socket)
            .env("XDG_CONFIG_HOME", &config_home)
            .env_remove("HERDR_PANE_ID")
            .env("XDG_RUNTIME_DIR", &runtime_dir);
        if reveal {
            command.arg("--reveal");
        }
        command.output().expect("gram get-file CLI")
    };
    let refused = run_download(false);
    assert_eq!(
        refused.status.code(),
        Some(3),
        "unexpected result: {refused:?}"
    );
    assert!(!out.exists(), "refusal must leave no partial download");
    let allowed = run_download(true);
    assert!(
        allowed.status.success(),
        "explicit reveal failed: {allowed:?}"
    );
    assert_eq!(fs::read(&out).unwrap(), bytes);

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn gram_file_rejects_oversized_mime_and_unknown_caller() {
    use base64::Engine as _;

    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    // An over-long mime is rejected. Crucially, use a VALID staged upload so the
    // ONLY thing that can reject the post is the mime cap — otherwise the finalize
    // step would reject an unknown upload_id with the same invalid_params code and
    // the test would pass even if the mime cap were removed.
    let chunk = base64::engine::general_purpose::STANDARD.encode(b"secret-key");
    let up_mime = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"um","method":"gram.upload_chunk","params":{{"upload_id":"mimecap","offset":0,"data_base64":"{chunk}"}}}}"#
        ),
    );
    assert_eq!(
        up_mime["result"]["type"], "ok",
        "mime-cap upload: {up_mime}"
    );
    let big_mime = "x".repeat(300);
    let bad_mime = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"m","method":"gram.post","params":{{"text":"","file":{{"upload_id":"mimecap","name":"x.txt","mime":"{big_mime}"}}}}}}"#
        ),
    );
    assert_eq!(
        bad_mime["error"]["code"], "invalid_params",
        "an oversized mime must be rejected: {bad_mime}"
    );
    let mime_msg = bad_mime["error"]["message"].as_str().unwrap_or_default();
    assert!(
        mime_msg.contains("mime"),
        "the rejection must be the mime cap, not a different invalid_params: {bad_mime}"
    );

    // Upload + post a real file to get a message id.
    let up = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"u","method":"gram.upload_chunk","params":{{"upload_id":"u1","offset":0,"data_base64":"{chunk}"}}}}"#
        ),
    );
    assert_eq!(up["result"]["type"], "ok", "upload: {up}");
    let post = api_request(
        &api_socket,
        r#"{"id":"p","method":"gram.post","params":{"text":"","file":{"upload_id":"u1","name":"k.txt","mime":"text/plain"}}}"#,
    );
    let message_id = post["result"]["message"]["id"]
        .as_str()
        .expect("message id")
        .to_string();

    // A caller_pane_id that names no live pane is rejected as unknown_caller — NOT
    // silently treated as the owner — so an agent cannot bypass the audience check
    // by supplying a bogus pane. (An omitted caller_pane_id is still the owner;
    // that cooperative-filtering boundary is documented, not authenticated.)
    let denied = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"d","method":"gram.get_file","params":{{"id":"{message_id}","caller_pane_id":"w9:p9"}}}}"#
        ),
    );
    assert_eq!(
        denied["error"]["code"], "unknown_caller",
        "a bogus caller pane must not fall through to owner authority: {denied}"
    );
    let denied_chunk = api_request(
        &api_socket,
        &format!(
            r#"{{"id":"dc","method":"gram.get_file_chunk","params":{{"id":"{message_id}","offset":0,"caller_pane_id":"w9:p9"}}}}"#
        ),
    );
    assert_eq!(denied_chunk["error"]["code"], "unknown_caller");

    // The owner (no caller pane) can still read it.
    let owner = api_request(
        &api_socket,
        &format!(r#"{{"id":"o","method":"gram.get_file","params":{{"id":"{message_id}"}}}}"#),
    );
    assert_eq!(
        owner["result"]["type"], "gram_file_content",
        "the owner can read the file: {owner}"
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

    // Stop the server.
    support::stop_spawned_herdr(&mut *spawned.child);
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
    support::isolate_herdr_test_process(&mut cmd);
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

fn server_log_path(config_home: &Path) -> PathBuf {
    let app_dir_name = if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    };
    config_home.join(app_dir_name).join("herdr-server.log")
}

fn wait_for_exit(child: &mut Box<dyn Child + Send + Sync>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn wait_for_log_line(path: &Path, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let content = fs::read_to_string(path).unwrap_or_default();
        if let Some(line) = content.lines().find(|line| line.contains(needle)) {
            return line.to_string();
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "log {} did not contain {needle:?}:\n{}",
        path.display(),
        fs::read_to_string(path).unwrap_or_default()
    );
}

#[test]
fn server_survives_hangup_and_logs_why_it_stops() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let pid = spawned.child.process_id().expect("server pid") as libc::pid_t;

    assert_eq!(unsafe { libc::kill(pid, libc::SIGHUP) }, 0);
    // Closing the terminal hangs up the server's whole session too.
    spawned.close_master();
    assert!(
        !wait_for_exit(&mut spawned.child, Duration::from_millis(500)),
        "server must not stop on SIGHUP"
    );
    assert!(ping_socket(&api_socket).contains("pong"));

    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    assert!(
        wait_for_exit(&mut spawned.child, Duration::from_secs(10)),
        "server must stop on SIGTERM"
    );
    let line = wait_for_log_line(
        &server_log_path(&config_home),
        "server shutdown initiated",
        Duration::from_secs(5),
    );
    assert!(line.contains("reason=SIGTERM"), "{line}");

    cleanup_spawned_herdr(spawned, base);
}

#[test]
fn server_stop_request_logs_its_caller() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut spawned = spawn_server(&config_home, &runtime_dir, &api_socket, &client_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));

    let mut stream = UnixStream::connect(&api_socket).unwrap();
    writeln!(
        stream,
        r#"{{"id":"stop","method":"server.stop","params":{{}}}}"#
    )
    .unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    assert!(response.contains("\"ok\""), "{response}");

    assert!(wait_for_exit(&mut spawned.child, Duration::from_secs(10)));
    let line = wait_for_log_line(
        &server_log_path(&config_home),
        "server shutdown initiated",
        Duration::from_secs(5),
    );
    let expected = if cfg!(any(target_os = "linux", target_os = "macos")) {
        format!("server.stop request from pid {}", std::process::id())
    } else {
        "server.stop request".to_string()
    };
    assert!(line.contains(&expected), "{line}");

    cleanup_spawned_herdr(spawned, base);
}
