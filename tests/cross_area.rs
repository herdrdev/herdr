//! Cross-area integration tests for end-to-end persistence flows.

#![cfg(unix)]

mod support;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use serde_json::{json, Value};
use support::{
    cleanup_test_base, expected_version, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, CURRENT_PROTOCOL,
};

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-cross-area-test-{}-{nanos}",
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

fn spawn_server(config_home: &Path, runtime_dir: &Path, api_socket_path: &Path) -> SpawnedHerdr {
    spawn_server_with_path(config_home, runtime_dir, api_socket_path, None)
}

fn spawn_server_with_path(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    path_override: Option<&Path>,
) -> SpawnedHerdr {
    spawn_server_with_config_text(
        config_home,
        runtime_dir,
        api_socket_path,
        path_override,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    )
}

fn spawn_server_with_config_text(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    path_override: Option<&Path>,
    config_text: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    let config_path = config_home.join("herdr/config.toml");
    fs::write(&config_path, config_text).unwrap();

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
    cmd.env("XDG_STATE_HOME", config_home.join("state"));
    // A debug build resolves XDG_CONFIG_HOME under `herdr-dev`, while this test
    // writes `herdr/config.toml`. Do not drop the explicit path.
    cmd.env("HERDR_CONFIG_PATH", &config_path);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    if let Some(path) = path_override {
        cmd.env("PATH", path);
    }

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: Some(pair.master),
        child,
    }
}

fn spawn_client_process(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
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
    cmd.arg("client");
    cmd.env("HERDR_DISABLE_SOUND", "1");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_STATE_HOME", config_home.join("state"));
    // The paired server writes this path. Keep the client on the same real
    // config instead of debug-build defaults; do not drop it.
    cmd.env("HERDR_CONFIG_PATH", config_home.join("herdr/config.toml"));
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

fn send_json_request(socket_path: &Path, id: &str, method: &str, params: Value) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    let request = json!({
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(stream, "{}", request).unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn ping_socket(socket_path: &Path) -> String {
    let response = send_json_request(socket_path, "ping", "ping", json!({}));
    response.to_string()
}

fn workspace_create(socket_path: &Path, label: &str) -> Value {
    send_json_request(
        socket_path,
        "workspace_create",
        "workspace.create",
        json!({ "focus": true, "label": label }),
    )
}

fn workspace_list(socket_path: &Path) -> Value {
    send_json_request(socket_path, "workspace_list", "workspace.list", json!({}))
}

fn workspace_count(socket_path: &Path) -> usize {
    workspace_list(socket_path)["result"]["workspaces"]
        .as_array()
        .map(|workspaces| workspaces.len())
        .unwrap_or(0)
}

fn workspace_id_by_label(response: &Value, label: &str) -> String {
    response["result"]["workspaces"]
        .as_array()
        .expect("workspace.list should return workspaces array")
        .iter()
        .find(|workspace| workspace["label"] == label)
        .and_then(|workspace| workspace["workspace_id"].as_str())
        .expect("workspace with matching label should exist")
        .to_string()
}

fn wait_for_child_exit(child: &mut Box<dyn Child + Send + Sync>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

fn pane_send_input(socket_path: &Path, pane_id: &str, text: &str) {
    let response = send_json_request(
        socket_path,
        "pane_send_input",
        "pane.send_input",
        json!({
            "pane_id": pane_id,
            "text": text,
            "keys": ["Enter"]
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.send_input should succeed: {response}"
    );
}

fn pane_send_text(socket_path: &Path, pane_id: &str, text: &str) {
    let response = send_json_request(
        socket_path,
        "pane_send_text",
        "pane.send_text",
        json!({
            "pane_id": pane_id,
            "text": text
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.send_text should succeed: {response}"
    );
}

fn pane_read_recent(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json_request(
        socket_path,
        "pane_read",
        "pane.read",
        json!({
            "pane_id": pane_id,
            "source": "recent",
            "lines": 200
        }),
    );

    response["result"]["read"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn pane_read_recent_contains(
    socket_path: &Path,
    pane_id: &str,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let text = pane_read_recent(socket_path, pane_id);
        if text.contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn pane_report_agent(socket_path: &Path, pane_id: &str, agent: &str, state: &str, source: &str) {
    let response = send_json_request(
        socket_path,
        "pane_report_agent",
        "pane.report_agent",
        json!({
            "pane_id": pane_id,
            "agent": agent,
            "state": state,
            "source": source,
        }),
    );
    assert!(
        response.get("error").is_none(),
        "pane.report_agent should succeed: {response}"
    );
}

fn pane_agent_status(socket_path: &Path, pane_id: &str) -> Option<String> {
    let response = send_json_request(
        socket_path,
        "pane_get",
        "pane.get",
        json!({ "pane_id": pane_id }),
    );
    response["result"]["pane"]["agent_status"]
        .as_str()
        .map(|status| status.to_string())
}

fn wait_for_agent_status(
    socket_path: &Path,
    pane_id: &str,
    expected: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_agent_status(socket_path, pane_id).as_deref() == Some(expected) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

// ---------------------------------------------------------------------------
// Minimal protocol helpers (bincode v2 varint + framing)
// ---------------------------------------------------------------------------

fn encode_varint_u32(v: u32) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else if v < 65_536 {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&(v as u16).to_le_bytes());
        buf
    } else {
        let mut buf = vec![252u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

fn encode_varint_u16(v: u16) -> Vec<u8> {
    if v < 251 {
        vec![v as u8]
    } else {
        let mut buf = vec![251u8];
        buf.extend_from_slice(&v.to_le_bytes());
        buf
    }
}

fn encode_string(value: &str) -> Vec<u8> {
    let mut encoded = encode_varint_u32(value.len() as u32);
    encoded.extend_from_slice(value.as_bytes());
    encoded
}

fn frame_message(payload: &[u8]) -> Vec<u8> {
    let mut framed = (payload.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(payload);
    framed
}

fn decode_varint_u32(payload: &[u8], offset: usize) -> Result<(u32, usize), String> {
    if offset >= payload.len() {
        return Err("payload too short for varint".into());
    }
    let first = payload[offset];
    match first {
        0..=250 => Ok((first as u32, 1)),
        251 => {
            if offset + 3 > payload.len() {
                return Err("payload too short for u16 varint".into());
            }
            let v = u16::from_le_bytes(
                payload[offset + 1..offset + 3]
                    .try_into()
                    .map_err(|e: std::array::TryFromSliceError| e.to_string())?,
            );
            Ok((v as u32, 3))
        }
        252 => {
            if offset + 5 > payload.len() {
                return Err("payload too short for u32 varint".into());
            }
            let v = u32::from_le_bytes(
                payload[offset + 1..offset + 5]
                    .try_into()
                    .map_err(|e: std::array::TryFromSliceError| e.to_string())?,
            );
            Ok((v, 5))
        }
        _ => Err(format!("unsupported varint tag: {first}")),
    }
}

fn client_handshake(stream: &mut UnixStream, version: u32, cols: u16, rows: u16) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");

    // ClientMessage::Hello = variant 0
    let build_version = expected_version();
    let mut payload = encode_varint_u32(0);
    payload.extend_from_slice(&encode_varint_u32(version));
    payload.extend_from_slice(&encode_string(&build_version));
    payload.extend_from_slice(&encode_varint_u16(cols));
    payload.extend_from_slice(&encode_varint_u16(rows));
    payload.extend_from_slice(&encode_varint_u32(8)); // cell_width_px
    payload.extend_from_slice(&encode_varint_u32(16)); // cell_height_px
    payload.extend_from_slice(&encode_varint_u32(0)); // RenderEncoding::SemanticFrame
    payload.extend_from_slice(&encode_varint_u32(0)); // ClientKeybindings::Server
    payload.extend_from_slice(&encode_varint_u32(0)); // ClientLaunchMode::App

    stream
        .write_all(&frame_message(&payload))
        .expect("write hello");
    stream.flush().expect("flush hello");

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .expect("read welcome length");
    let len = u32::from_le_bytes(len_buf) as usize;
    assert!(len > 0 && len <= 2 * 1024 * 1024, "unexpected welcome size");

    let mut welcome_payload = vec![0u8; len];
    stream
        .read_exact(&mut welcome_payload)
        .expect("read welcome payload");

    let mut offset = 0;
    let (variant, consumed) = decode_varint_u32(&welcome_payload, offset).expect("decode variant");
    offset += consumed;
    assert_eq!(variant, 0, "expected ServerMessage::Welcome variant");

    let (_server_version, consumed) =
        decode_varint_u32(&welcome_payload, offset).expect("decode version");
    offset += consumed;

    let (build_len, consumed) =
        decode_varint_u32(&welcome_payload, offset).expect("decode build version length");
    offset += consumed + build_len as usize;

    let (_encoding, consumed) =
        decode_varint_u32(&welcome_payload, offset).expect("decode render encoding");
    offset += consumed;

    let option_tag = *welcome_payload
        .get(offset)
        .expect("welcome payload should contain Option tag");
    if option_tag == 1 {
        let (str_len, consumed) =
            decode_varint_u32(&welcome_payload, offset + 1).expect("decode error length");
        let start = offset + 1 + consumed;
        let end = start + str_len as usize;
        let err = String::from_utf8(welcome_payload[start..end].to_vec()).expect("utf8 error");
        panic!("handshake rejected: {err}");
    }
}

fn send_client_input(stream: &mut UnixStream, data: &[u8]) {
    // ClientMessage::Input = variant 1
    let mut payload = encode_varint_u32(1);
    payload.extend_from_slice(&encode_varint_u32(data.len() as u32));
    payload.extend_from_slice(data);
    stream
        .write_all(&frame_message(&payload))
        .expect("write input");
    stream.flush().expect("flush input");
}

fn send_client_detach(stream: &mut UnixStream) {
    // ClientMessage::Detach = variant 4
    let payload = encode_varint_u32(4);
    stream
        .write_all(&frame_message(&payload))
        .expect("write detach");
    stream.flush().expect("flush detach");
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct FrameWire {
    cells: Vec<CellWire>,
    width: u16,
    height: u16,
    cursor: Option<CursorWire>,
    hyperlinks: Vec<String>,
    graphics: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CellWire {
    symbol: String,
    fg: u32,
    bg: u32,
    modifier: u16,
    skip: bool,
    hyperlink: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CursorWire {
    x: u16,
    y: u16,
    visible: bool,
    shape: u8,
}

fn decode_frame_payload(payload: &[u8]) -> io::Result<FrameWire> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
        .and_then(|(frame, consumed): (FrameWire, usize)| {
            if consumed != payload.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "frame payload had trailing bytes: consumed={}, len={}",
                        consumed,
                        payload.len()
                    ),
                ));
            }
            Ok(frame)
        })
}

fn frame_contains_colored_symbol(frame: &FrameWire, symbol: &str, rgb: (u8, u8, u8)) -> bool {
    let (r, g, b) = rgb;
    let fg = 0x02_00_00_00 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
    frame
        .cells
        .iter()
        .any(|cell| cell.symbol == symbol && cell.fg == fg)
}

fn frame_contains_text(frame: &FrameWire, needle: &str) -> bool {
    if frame.cells.is_empty() {
        return false;
    }

    let width = frame.width.max(1) as usize;
    let mut text = String::new();
    for row in frame.cells.chunks(width) {
        for cell in row {
            let _ = (cell.fg, cell.bg, cell.modifier, cell.skip);
            text.push_str(&cell.symbol);
        }
        text.push('\n');
    }
    let _ = (frame.height, frame.graphics.len());
    if let Some(cursor) = frame.cursor.as_ref() {
        let _ = (cursor.x, cursor.y, cursor.visible, cursor.shape);
    }

    text.contains(needle)
}

fn read_server_variant(stream: &mut UnixStream, timeout: Duration) -> io::Result<u32> {
    stream.set_read_timeout(Some(timeout))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length payload",
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;

    let (variant, _consumed) = decode_varint_u32(&payload, 0)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(variant)
}

fn read_server_message_payload(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<(u32, Vec<u8>)> {
    stream.set_read_timeout(Some(timeout))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length payload",
        ));
    }

    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;

    let (variant, consumed) = decode_varint_u32(&payload, 0)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((variant, payload[consumed..].to_vec()))
}

fn wait_for_frame_matching(
    stream: &mut UnixStream,
    timeout: Duration,
    predicate: impl Fn(&FrameWire) -> bool,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_message_payload(stream, slice) {
            Ok((1, payload)) => {
                let frame = decode_frame_payload(&payload)?;
                if predicate(&frame) {
                    return Ok(true);
                }
            }
            Ok((_variant, _payload)) => {}
            Err(err) if is_timeout(&err) => {}
            Err(err) => return Err(err),
        }
    }

    Ok(false)
}

fn wait_for_frame(stream: &mut UnixStream, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_variant(stream, slice) {
            Ok(1) => return true, // ServerMessage::Frame
            Ok(_) => {}
            Err(err) if is_timeout(&err) => {}
            Err(_) => return false,
        }
    }
    false
}

fn drain_server_messages(stream: &mut UnixStream, max_drain: Duration) {
    let deadline = Instant::now() + max_drain;
    while Instant::now() < deadline {
        match read_server_variant(stream, Duration::from_millis(40)) {
            Ok(_) => {}
            Err(err) if is_timeout(&err) => break,
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-area tests
// ---------------------------------------------------------------------------

#[test]
fn cross_area_detach_and_reattach_preserves_state() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Local attach (client A).
    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, CURRENT_PROTOCOL, 100, 30);
    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));

    // Use herdr: create a workspace and write output into its pane.
    let create = workspace_create(&api_socket, "cross-ssh-state");
    let workspace_id = create["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();
    let pane_id = create["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    pane_send_input(&api_socket, &pane_id, "echo LOCAL_BEFORE_DETACH");
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "LOCAL_BEFORE_DETACH",
        Duration::from_secs(5)
    ));

    // Detach local client.
    send_client_detach(&mut client_a);
    drop(client_a);

    // Simulate activity while detached.
    pane_send_text(&api_socket, &pane_id, "echo DETACHED_UPDATE\n");
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "DETACHED_UPDATE",
        Duration::from_secs(5)
    ));

    // Reattach from another terminal/session (client B).
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, CURRENT_PROTOCOL, 80, 24);
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(5)),
        "reattached client should receive frame"
    );

    let listed = workspace_list(&api_socket);
    assert_eq!(
        workspace_id,
        workspace_id_by_label(&listed, "cross-ssh-state"),
        "reattached session should see same workspace"
    );

    let readback = pane_read_recent(&api_socket, &pane_id);
    assert!(
        readback.contains("DETACHED_UPDATE"),
        "pane output should include detached-period output: {readback}"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_agent_process_survives_detach_and_reattach() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let bin_dir = base.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_pi = bin_dir.join("pi");
    fs::write(&fake_pi, "#!/bin/sh\nprintf 'Working...\\n'\nsleep 8\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&fake_pi).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&fake_pi, perms).unwrap();
    }

    let inherited_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{}", bin_dir.display(), inherited_path);

    let server = spawn_server_with_config_text(
        &config_home,
        &runtime_dir,
        &api_socket,
        Some(Path::new(&path_override)),
        "onboarding = false\n[ui]\nshow_home_on_start = false\n[notepad]\nvisible_tabs = []\n",
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, CURRENT_PROTOCOL, 100, 30);
    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));

    let created = workspace_create(&api_socket, "agent-persist");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    // Ensure detected agent surface is populated by running fake `pi`.
    pane_send_text(&api_socket, &pane_id, "pi");
    pane_send_input(&api_socket, &pane_id, "");
    let detected_before_hook = {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut detected = false;
        while Instant::now() < deadline {
            let response = send_json_request(
                &api_socket,
                "pane_get",
                "pane.get",
                json!({ "pane_id": &pane_id }),
            );
            if response["result"]["pane"]["agent"].as_str() == Some("pi") {
                detected = true;
                break;
            }
            thread::sleep(Duration::from_millis(60));
        }
        detected
    };
    assert!(
        detected_before_hook,
        "expected fake pi process to be detected before hook status assertions"
    );

    // Use agent status surfaces directly instead of a generic sleep command.
    pane_report_agent(&api_socket, &pane_id, "pi", "working", "cross-area-test");
    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "working", Duration::from_secs(3)),
        "pane agent status should become working before detach"
    );

    // Detach and ensure status persists through API while detached.
    send_client_detach(&mut client_a);
    drop(client_a);

    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "working", Duration::from_secs(3)),
        "agent status should remain working while detached"
    );

    // Reattach and ensure client-side state reflects the persisted working status.
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, CURRENT_PROTOCOL, 80, 24);
    let saw_working_on_client =
        wait_for_frame_matching(&mut client_b, Duration::from_secs(5), |frame| {
            // ac7: the reattached working-state dot uses activity blue.
            frame_contains_colored_symbol(frame, "●", (137, 180, 250))
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_working_on_client,
        "reattached client frame should expose persisted agent working status"
    );

    // Transition to blocked and verify API + client surfaces both observe it.
    // The fake process remains visibly working, so blocked is the deterministic
    // higher-priority semantic transition for this cross-area projection test.
    pane_report_agent(&api_socket, &pane_id, "pi", "blocked", "cross-area-test");
    assert!(
        wait_for_agent_status(&api_socket, &pane_id, "blocked", Duration::from_secs(3)),
        "pane agent status should transition to blocked"
    );

    let saw_blocked_on_client =
        wait_for_frame_matching(&mut client_b, Duration::from_secs(5), |frame| {
            frame_contains_colored_symbol(frame, "○", (243, 139, 168))
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_blocked_on_client,
        "reattached client frame should show blocked status after transition"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_client_and_api_workspace_views_are_consistent() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    // Tall enough that the always-visible Snoozed/Settled shelves leave the
    // new workspace row on screen.
    client_handshake(&mut client, CURRENT_PROTOCOL, 100, 40);
    assert!(wait_for_frame(&mut client, Duration::from_secs(2)));
    drain_server_messages(&mut client, Duration::from_millis(300));

    let before = workspace_count(&api_socket);

    // Create a workspace via API while the client is attached.
    let created = workspace_create(&api_socket, "api-visible-workspace");
    let created_workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace.create should return workspace_id")
        .to_string();

    // The attached client must receive a frame that includes the recognizable
    // prefix of the new workspace label. Sidebar disclosure controls may
    // truncate the suffix at the negotiated client width; the group header's
    // trailing sort control takes another two cells of that budget.
    let saw_workspace_on_client =
        wait_for_frame_matching(&mut client, Duration::from_secs(3), |frame| {
            frame_contains_text(frame, "api-visible")
        })
        .expect("frame decoding should succeed");
    assert!(
        saw_workspace_on_client,
        "client-side frame should include the newly created workspace label"
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut count_reached = false;
    while Instant::now() < deadline {
        if workspace_count(&api_socket) == before + 1 {
            count_reached = true;
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        count_reached,
        "API workspace list should include the created workspace"
    );

    let listed = workspace_list(&api_socket);
    let listed_workspace_id = workspace_id_by_label(&listed, "api-visible-workspace");
    assert_eq!(
        listed_workspace_id, created_workspace_id,
        "API and client-side state should reference the same created workspace"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_two_clients_shared_view_and_single_detach_stability() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut client_a = UnixStream::connect(&client_socket).expect("client A should connect");
    client_handshake(&mut client_a, CURRENT_PROTOCOL, 110, 30);
    let mut client_b = UnixStream::connect(&client_socket).expect("client B should connect");
    client_handshake(&mut client_b, CURRENT_PROTOCOL, 100, 30);

    assert!(wait_for_frame(&mut client_a, Duration::from_secs(2)));
    assert!(wait_for_frame(&mut client_b, Duration::from_secs(2)));
    drain_server_messages(&mut client_a, Duration::from_millis(250));
    drain_server_messages(&mut client_b, Duration::from_millis(250));

    let created = workspace_create(&api_socket, "shared-view");
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();

    // Input from client A should update shared state visible to client B.
    send_client_input(&mut client_a, b"echo SHARED_VIEW\n");
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(2)),
        "client B should receive update from client A"
    );
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "SHARED_VIEW",
        Duration::from_secs(5)
    ));

    // Detach client A; client B should keep working.
    send_client_detach(&mut client_a);
    drop(client_a);

    send_client_input(&mut client_b, b"echo AFTER_A_DETACH\n");
    assert!(
        wait_for_frame(&mut client_b, Duration::from_secs(2)),
        "remaining client should still receive frames after other client detaches"
    );
    assert!(pane_read_recent_contains(
        &api_socket,
        &pane_id,
        "AFTER_A_DETACH",
        Duration::from_secs(5)
    ));

    let ping = ping_socket(&api_socket);
    assert!(
        ping.contains("pong"),
        "server and remaining client flow should stay healthy: {ping}"
    );

    cleanup_spawned_herdr(server, base);
}

#[test]
fn cross_area_server_kill_then_restart_and_reconnect() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let mut server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    // Attach a real thin client process and prove it reached attached state
    // by observing an incoming frame on its PTY stream.
    let mut thin_client = spawn_client_process(&config_home, &runtime_dir, &api_socket);
    let mut thin_reader = thin_client
        ._master
        .as_ref()
        .expect("thin client master")
        .try_clone_reader()
        .expect("clone thin client reader");

    let attached_before_kill = {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut observed = false;
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            match thin_reader.read(&mut buf) {
                Ok(n) if n > 0 => {
                    let out = String::from_utf8_lossy(&buf[..n]);
                    if out.contains("\x1b[2J")
                        || out.contains("\u{2500}")
                        || out.contains("workspace")
                        || out.contains("pane")
                        || out.contains("terminal")
                    {
                        observed = true;
                        break;
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(30)),
                Err(_) => thread::sleep(Duration::from_millis(30)),
            }
        }
        observed
    };
    assert!(
        attached_before_kill,
        "thin client should complete attach before server SIGKILL"
    );

    // Kill server abruptly and verify thin client exits with lost-connection messaging.
    let server_pid = server.child.process_id().expect("server pid should exist");
    unsafe {
        libc::kill(server_pid as libc::pid_t, libc::SIGKILL);
    }
    server.close_master();
    assert!(
        wait_for_child_exit(&mut server.child, Duration::from_secs(5)),
        "server should exit after SIGKILL"
    );
    drop(server);

    let mut crash_output = String::new();
    let thin_exited = {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut exited = false;
        let mut buf = [0u8; 1024];
        while Instant::now() < deadline {
            if thin_client.child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            if let Ok(n) = thin_reader.read(&mut buf) {
                if n > 0 {
                    crash_output.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        exited
    };
    assert!(thin_exited, "thin client should exit after server SIGKILL");

    let thin_status = thin_client
        .child
        .wait()
        .expect("wait for thin client exit status");
    assert!(
        !thin_status.success(),
        "thin client should exit non-zero after unexpected server crash"
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
        "thin client output must include explicit lost-connection message after server kill; output: {crash_output:?}"
    );

    // Restart server and verify new client can connect (stale socket cleaned).
    let server2 = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let mut reconnect_client =
        UnixStream::connect(&client_socket).expect("new client should connect after restart");
    client_handshake(&mut reconnect_client, CURRENT_PROTOCOL, 80, 24);
    assert!(
        wait_for_frame(&mut reconnect_client, Duration::from_secs(5)),
        "new client should receive frame after restart"
    );

    let ping = ping_socket(&api_socket);
    assert!(
        ping.contains("pong"),
        "restarted server should respond over API: {ping}"
    );

    cleanup_spawned_herdr(server2, base);
}

#[test]
fn pane_group_clear_survives_missing_and_corrupt_group_store_after_restart() {
    let _lock = test_lock();

    for damage in ["missing", "corrupt"] {
        let base = unique_test_dir();
        let config_home = base.join("config");
        let runtime_dir = base.join("runtime");
        let api_socket = runtime_dir.join("herdr.sock");

        let server = spawn_server(&config_home, &runtime_dir, &api_socket);
        wait_for_socket(&api_socket, Duration::from_secs(10));
        let created = workspace_create(&api_socket, damage);
        let pane_id = created["result"]["root_pane"]["pane_id"]
            .as_str()
            .expect("created pane id")
            .to_string();
        let group = send_json_request(
            &api_socket,
            "group_create",
            "group.create",
            json!({"name": "Work", "expected_revision": 0}),
        );
        let group_id = group["result"]["record"]["id"].clone();
        assert!(!group_id.is_null(), "group.create should succeed: {group}");
        let assigned = send_json_request(
            &api_socket,
            "group_assign",
            "pane.group.set",
            json!({
                "pane_id": pane_id,
                "group_id": group_id,
                "expected_revision": 0
            }),
        );
        assert!(
            assigned.get("error").is_none(),
            "initial pane.group.set should succeed: {assigned}"
        );
        drop(server);

        let data_dir = config_home.join("herdr-dev");
        let groups_path = data_dir.join("groups.json");
        if damage == "missing" {
            fs::remove_file(&groups_path).unwrap();
        } else {
            fs::write(&groups_path, "not json").unwrap();
        }

        let restarted = spawn_server(&config_home, &runtime_dir, &api_socket);
        wait_for_socket(&api_socket, Duration::from_secs(10));
        let cleared = send_json_request(
            &api_socket,
            "group_clear",
            "pane.group.set",
            json!({"pane_id": pane_id, "expected_revision": 1}),
        );
        assert!(
            cleared.get("error").is_none(),
            "clearing with a {damage} group store should succeed: {cleared}"
        );
        assert_eq!(cleared["result"]["membership"], json!({"revision": 2}));

        let session: Value = serde_json::from_str(
            &fs::read_to_string(data_dir.join("session.json")).expect("persisted session"),
        )
        .expect("valid persisted session");
        let saved_membership = session["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|workspace| {
                workspace["tabs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|tab| {
                        tab["panes"]
                            .as_object()
                            .into_iter()
                            .flat_map(|panes| panes.values())
                    })
            })
            .find_map(|pane| pane.get("group_membership"))
            .expect("cleared membership remains durable");
        assert_eq!(saved_membership, &json!({"revision": 2}));

        cleanup_spawned_herdr(restarted, base);
    }
}

#[test]
fn rolled_back_group_store_rejects_a_stale_revision_after_restart() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let created = send_json_request(
        &api_socket,
        "group_create_before_rollback",
        "group.create",
        json!({"name": "Before", "expected_revision": 0}),
    );
    assert!(created.get("error").is_none(), "create failed: {created}");
    let group_id = created["result"]["record"]["id"].clone();
    let pre_rename_revision = created["result"]["revision"]
        .as_u64()
        .expect("created authority revision");
    let data_dir = config_home.join("herdr-dev");
    let groups_path = data_dir.join("groups.json");
    let before_rename = fs::read_to_string(&groups_path).expect("pre-rename group store");

    let renamed = send_json_request(
        &api_socket,
        "group_rename_before_rollback",
        "group.rename",
        json!({
            "group_id": group_id,
            "name": "After",
            "expected_revision": pre_rename_revision
        }),
    );
    assert!(renamed.get("error").is_none(), "rename failed: {renamed}");
    let post_rename_revision = renamed["result"]["revision"]
        .as_u64()
        .expect("renamed authority revision");
    assert!(post_rename_revision > pre_rename_revision);
    drop(server);
    fs::write(&groups_path, before_rename).unwrap();

    let restarted = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let snapshot = send_json_request(
        &api_socket,
        "group_snapshot_after_revision_rollback",
        "group.host_snapshot",
        json!({}),
    );
    let repaired_revision = snapshot["result"]["snapshot"]["revision"]
        .as_u64()
        .expect("repaired authority revision");
    assert!(repaired_revision > post_rename_revision);
    let stale = send_json_request(
        &api_socket,
        "group_rename_with_stale_revision",
        "group.rename",
        json!({
            "group_id": group_id,
            "name": "Stale",
            "expected_revision": pre_rename_revision
        }),
    );
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert!(stale["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains(&repaired_revision.to_string())));

    cleanup_spawned_herdr(restarted, base);
}

#[test]
fn rolled_back_group_store_never_reissues_a_retired_identity_after_restart() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let workspace = workspace_create(&api_socket, "rollback-membership");
    let pane_id = workspace["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();
    let first = send_json_request(
        &api_socket,
        "group_create_first",
        "group.create",
        json!({"name": "First", "expected_revision": 0}),
    );
    assert!(first.get("error").is_none(), "first create failed: {first}");
    let data_dir = config_home.join("herdr-dev");
    let groups_path = data_dir.join("groups.json");
    let before_second_group = fs::read_to_string(&groups_path).expect("first group store");

    let second = send_json_request(
        &api_socket,
        "group_create_second",
        "group.create",
        json!({"name": "Second", "expected_revision": 1}),
    );
    let second_id = second["result"]["record"]["id"].clone();
    let second_revision = second["result"]["record"]["revision"]
        .as_u64()
        .expect("second group revision");
    let assigned = send_json_request(
        &api_socket,
        "group_assign_second",
        "pane.group.set",
        json!({
            "pane_id": pane_id,
            "group_id": second_id,
            "expected_revision": 0
        }),
    );
    assert!(
        assigned.get("error").is_none(),
        "group assignment failed: {assigned}"
    );
    let deleted = send_json_request(
        &api_socket,
        "group_delete_second",
        "group.delete",
        json!({
            "group_id": second_id,
            "expected_revision": second_revision
        }),
    );
    assert!(
        deleted.get("error").is_none(),
        "group delete failed: {deleted}"
    );
    drop(server);
    fs::write(&groups_path, before_second_group).unwrap();

    let restarted = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let snapshot = send_json_request(
        &api_socket,
        "group_snapshot_after_rollback",
        "group.host_snapshot",
        json!({}),
    );
    assert!(
        snapshot.get("error").is_none(),
        "rolled-back store should be repaired from the authority ledger: {snapshot}"
    );
    assert!(snapshot["result"]["snapshot"]["groups"]
        .as_array()
        .expect("snapshot groups")
        .iter()
        .any(|record| record["id"] == second_id && record["state"] == "deleted"));
    let membership = snapshot["result"]["snapshot"]["memberships"]
        .as_array()
        .expect("snapshot memberships")
        .iter()
        .find(|membership| membership["pane_id"] == pane_id)
        .expect("restored pane membership");
    assert_eq!(membership["membership"]["group_id"], second_id);
    assert_eq!(membership["membership"]["revision"], 1);

    let revision = snapshot["result"]["snapshot"]["revision"]
        .as_u64()
        .expect("reconstructed authority revision");
    let replacement = send_json_request(
        &api_socket,
        "group_create_after_rollback",
        "group.create",
        json!({"name": "Replacement", "expected_revision": revision}),
    );
    assert!(
        replacement.get("error").is_none(),
        "create after rollback failed: {replacement}"
    );
    assert_eq!(
        replacement["result"]["record"]["id"]["owner"],
        second_id["owner"]
    );
    assert_eq!(replacement["result"]["record"]["id"]["local"], 3);
    assert_ne!(replacement["result"]["record"]["id"], second_id);

    let after_create = send_json_request(
        &api_socket,
        "group_snapshot_after_replacement",
        "group.host_snapshot",
        json!({}),
    );
    let groups = after_create["result"]["snapshot"]["groups"]
        .as_array()
        .expect("snapshot groups after replacement");
    assert!(groups
        .iter()
        .any(|record| record["id"] == second_id && record["state"] == "deleted"));
    let membership = after_create["result"]["snapshot"]["memberships"]
        .as_array()
        .expect("snapshot memberships after replacement")
        .iter()
        .find(|membership| membership["pane_id"] == pane_id)
        .expect("pane membership after replacement");
    assert_eq!(membership["membership"]["group_id"], second_id);
    assert_eq!(membership["membership"]["revision"], 1);

    cleanup_spawned_herdr(restarted, base);
}

fn fleet_config(self_name: &str, own_socket: &Path, peer_name: &str, peer_socket: &Path) -> String {
    format!(
        "onboarding = false\n[ui]\nshow_home_on_start = false\n[remote.fleet]\nself_name = \"{self_name}\"\ntimeout_ms = 500\nrefresh_interval_ms = 100\nheartbeat_stale_ms = 500\n[[remote.fleet.hosts]]\nname = \"{self_name}\"\nlocal = true\nsocket = \"{}\"\n[[remote.fleet.hosts]]\nname = \"{peer_name}\"\nlocal = true\nsocket = \"{}\"\n",
        own_socket.display(),
        peer_socket.display(),
    )
}

fn wait_for_authority_catalog(
    socket: &Path,
    authority: &Value,
    state: &str,
    count: usize,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = send_json_request(socket, "fleet", "fleet.list", json!({}));
        let matching = last["result"]["snapshot"]["authority_catalogs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|catalog| catalog["authority_id"] == *authority && catalog["state"] == state)
            .count();
        if matching == count {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("authority catalog did not reach {state} x{count}: {last}");
}

fn wait_for_authority_group_state(
    socket: &Path,
    authority: &Value,
    catalog_state: &str,
    group_state: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = send_json_request(socket, "fleet_group", "fleet.list", json!({}));
        if last["result"]["snapshot"]["authority_catalogs"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|catalog| {
                catalog["authority_id"] == *authority
                    && catalog["state"] == catalog_state
                    && catalog["snapshot"]["groups"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|record| record["state"] == group_state)
            })
        {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("authority group did not reach {catalog_state}/{group_state}: {last}");
}

fn wait_for_authority_pane(socket: &Path, authority: &Value, pane_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = send_json_request(socket, "fleet_pane", "fleet.list", json!({}));
        if last["result"]["snapshot"]["authority_catalogs"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|catalog| {
                catalog["authority_id"] == *authority
                    && catalog["state"] == "fresh"
                    && catalog["snapshot"]["memberships"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|membership| membership["pane_id"] == pane_id)
            })
        {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("authority pane did not become fresh for {pane_id}: {last}");
}

fn wait_for_authority_catalog_error(
    socket: &Path,
    authority: &Value,
    state: &str,
    error_fragment: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = send_json_request(socket, "fleet_error", "fleet.list", json!({}));
        if last["result"]["snapshot"]["authority_catalogs"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|catalog| {
                catalog["authority_id"] == *authority
                    && catalog["state"] == state
                    && catalog["error"]
                        .as_str()
                        .is_some_and(|error| error.contains(error_fragment))
            })
        {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("authority catalog did not reach {state}/{error_fragment}: {last}");
}

#[test]
fn legacy_v1_writer_cannot_narrow_the_v2_authority_ledger() {
    let _guard = test_lock();
    let base_a = unique_test_dir();
    let base_b = unique_test_dir();
    let config_a = base_a.join("config");
    let config_b = base_b.join("config");
    let runtime_a = base_a.join("runtime");
    let runtime_b = base_b.join("runtime");
    let socket_a = runtime_a.join("api.sock");
    let socket_b = runtime_b.join("api.sock");
    let normal_a = fleet_config("alpha", &socket_a, "beta", &socket_b);
    let normal_b = fleet_config("beta", &socket_b, "alpha", &socket_a);
    let server_a = spawn_server_with_config_text(&config_a, &runtime_a, &socket_a, None, &normal_a);
    let server_b = spawn_server_with_config_text(&config_b, &runtime_b, &socket_b, None, &normal_b);
    wait_for_socket(&socket_a, Duration::from_secs(5));
    wait_for_socket(&socket_b, Duration::from_secs(5));

    let created = send_json_request(
        &socket_a,
        "create_before_legacy_writer",
        "group.create",
        json!({"name": "Work", "expected_revision": 0}),
    );
    assert!(created.get("error").is_none(), "create failed: {created}");
    let group_id = created["result"]["record"]["id"].clone();
    let authority = group_id["owner"].clone();
    wait_for_authority_group_state(&socket_b, &authority, "fresh", "active");
    let active_snapshot = send_json_request(
        &socket_a,
        "active_snapshot_for_legacy_writer",
        "group.host_snapshot",
        json!({}),
    )["result"]["snapshot"]
        .clone();

    let alpha_data = config_a.join("herdr-dev");
    let beta_data = config_b.join("herdr-dev");
    let authority_path = alpha_data.join("group-authority.json");
    let groups_path = alpha_data.join("groups.json");
    let legacy_path = beta_data.join("remote-group-catalogs-v1.json");
    let v2_path = beta_data.join("authority-acceptance-ledger-v2.json");
    let active_authority = fs::read(&authority_path).expect("active authority state");
    let active_groups = fs::read(&groups_path).expect("active group state");
    let legacy_file = |snapshot: Value| {
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "entries": [{
                "target": "legacy-alpha",
                "local": true,
                "session": null,
                "socket": socket_a,
                "snapshot": snapshot
            }]
        }))
        .expect("serialize legacy cache")
    };

    let deleted = send_json_request(
        &socket_a,
        "delete_before_legacy_writer",
        "group.delete",
        json!({"group_id": group_id, "expected_revision": 1}),
    );
    assert_eq!(deleted["result"]["record"]["state"], "deleted", "{deleted}");
    wait_for_authority_group_state(&socket_b, &authority, "fresh", "deleted");
    let deleted_snapshot = send_json_request(
        &socket_a,
        "deleted_snapshot_for_upgrade",
        "group.host_snapshot",
        json!({}),
    )["result"]["snapshot"]
        .clone();

    drop(server_a);
    drop(server_b);
    if v2_path.exists() {
        fs::remove_file(&v2_path).expect("remove v2 written before the upgrade fixture");
    }
    fs::write(&legacy_path, legacy_file(deleted_snapshot)).expect("seed the first upgrade from v1");

    let first_upgrade = spawn_server_with_config_text(
        &config_b,
        &runtime_b,
        &socket_b,
        None,
        "onboarding = false\n[ui]\nshow_home_on_start = false\n",
    );
    wait_for_socket(&socket_b, Duration::from_secs(5));
    ping_socket(&socket_b);
    let migration_deadline = Instant::now() + Duration::from_secs(5);
    while !v2_path.exists() && Instant::now() < migration_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let durable_v2: Value = serde_json::from_slice(
        &fs::read(&v2_path).expect("first upgrade must materialize the v2 ledger"),
    )
    .expect("parse migrated v2 ledger");
    assert_eq!(durable_v2["version"], 2);
    assert_eq!(
        durable_v2["authorities"][0]["groups"][0]["state"],
        "deleted"
    );
    drop(first_upgrade);

    fs::write(&legacy_path, legacy_file(active_snapshot)).expect("simulate the old v1 writer");
    fs::write(&authority_path, active_authority).expect("roll back owner authority");
    fs::write(&groups_path, active_groups).expect("roll back owner group store");

    let restarted_a =
        spawn_server_with_config_text(&config_a, &runtime_a, &socket_a, None, &normal_a);
    let restarted_b =
        spawn_server_with_config_text(&config_b, &runtime_b, &socket_b, None, &normal_b);
    wait_for_socket(&socket_a, Duration::from_secs(5));
    wait_for_socket(&socket_b, Duration::from_secs(5));
    let rejected = wait_for_authority_catalog_error(
        &socket_b,
        &authority,
        "stale",
        "snapshot revision rolled back",
    );
    let catalog = rejected["result"]["snapshot"]["authority_catalogs"]
        .as_array()
        .expect("catalog array")
        .iter()
        .find(|catalog| catalog["authority_id"] == authority)
        .expect("rolled-back authority catalog");
    assert_eq!(catalog["snapshot"]["groups"][0]["state"], "active");

    drop(restarted_a);
    cleanup_spawned_herdr(restarted_b, base_b);
    cleanup_test_base(&base_a);
}

#[test]
fn two_servers_share_groups_route_concurrent_mutations_and_recover_stale_catalogs() {
    let _guard = test_lock();
    let base_a = unique_test_dir();
    let base_b = unique_test_dir();
    let config_a = base_a.join("config");
    let config_b = base_b.join("config");
    let runtime_a = base_a.join("runtime");
    let runtime_b = base_b.join("runtime");
    let socket_a = runtime_a.join("api.sock");
    let socket_b = runtime_b.join("api.sock");
    let normal_a = fleet_config("alpha", &socket_a, "beta", &socket_b);
    let normal_b = fleet_config("beta", &socket_b, "alpha", &socket_a);
    let mut server_a = Some(spawn_server_with_config_text(
        &config_a, &runtime_a, &socket_a, None, &normal_a,
    ));
    let mut server_b = Some(spawn_server_with_config_text(
        &config_b, &runtime_b, &socket_b, None, &normal_b,
    ));
    wait_for_socket(&socket_a, Duration::from_secs(5));
    wait_for_socket(&socket_b, Duration::from_secs(5));

    let created = send_json_request(
        &socket_a,
        "create_empty",
        "group.create",
        json!({"name": "Empty", "expected_revision": 0}),
    );
    let group_id = created["result"]["record"]["id"].clone();
    let authority_a = group_id["owner"].clone();
    assert_eq!(created["result"]["record"]["revision"], 1);
    let owner_snapshot = send_json_request(
        &socket_a,
        "empty_snapshot",
        "group.host_snapshot",
        json!({}),
    );
    assert!(owner_snapshot["result"]["snapshot"]["memberships"]
        .as_array()
        .is_some_and(Vec::is_empty));
    wait_for_authority_catalog(&socket_b, &authority_a, "fresh", 1);

    let workspace_a = workspace_create(&socket_a, "alpha-pane");
    let workspace_b = workspace_create(&socket_b, "beta-pane");
    let pane_a = workspace_a["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("alpha root pane")
        .to_string();
    let pane_b = workspace_b["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("beta root pane")
        .to_string();
    assert_eq!(
        pane_a, pane_b,
        "independent servers should reproduce the collision"
    );
    wait_for_authority_pane(&socket_b, &authority_a, &pane_a);
    let assigned = send_json_request(
        &socket_b,
        "assign_routed",
        "pane.group.set",
        json!({
            "pane_id": pane_a,
            "group_id": group_id,
            "expected_revision": 0,
            "expected_pane_authority": authority_a
        }),
    );
    assert_eq!(
        assigned["result"]["membership"]["revision"], 1,
        "{assigned}"
    );
    let alpha_after_assignment = send_json_request(
        &socket_a,
        "alpha_after_assignment",
        "group.host_snapshot",
        json!({}),
    );
    let beta_after_assignment = send_json_request(
        &socket_b,
        "beta_after_assignment",
        "group.host_snapshot",
        json!({}),
    );
    let alpha_membership = alpha_after_assignment["result"]["snapshot"]["memberships"]
        .as_array()
        .expect("alpha memberships")
        .iter()
        .find(|membership| membership["pane_id"] == pane_a)
        .expect("alpha pane membership");
    let beta_membership = beta_after_assignment["result"]["snapshot"]["memberships"]
        .as_array()
        .expect("beta memberships")
        .iter()
        .find(|membership| membership["pane_id"] == pane_b)
        .expect("beta pane membership");
    assert_eq!(alpha_membership["membership"]["group_id"], group_id);
    assert_eq!(alpha_membership["membership"]["revision"], 1);
    assert!(beta_membership["membership"].get("group_id").is_none());
    assert_eq!(beta_membership["membership"]["revision"], 0);

    let a_socket = socket_a.clone();
    let b_socket = socket_b.clone();
    let a_group = group_id.clone();
    let b_group = group_id.clone();
    let direct = thread::spawn(move || {
        send_json_request(
            &a_socket,
            "rename_direct",
            "group.rename",
            json!({"group_id": a_group, "name": "From alpha", "expected_revision": 1}),
        )
    });
    let routed = thread::spawn(move || {
        send_json_request(
            &b_socket,
            "rename_routed",
            "group.rename",
            json!({"group_id": b_group, "name": "From beta", "expected_revision": 1}),
        )
    });
    let direct = direct.join().expect("direct mutation thread");
    let routed = routed.join().expect("routed mutation thread");
    let successes = [&direct, &routed]
        .into_iter()
        .filter(|response| response.get("result").is_some())
        .count();
    let conflicts = [&direct, &routed]
        .into_iter()
        .filter(|response| response["error"]["code"] == "revision_conflict")
        .count();
    assert_eq!((successes, conflicts), (1, 1), "{direct} / {routed}");

    let after_race = send_json_request(&socket_a, "after_race", "group.host_snapshot", json!({}));
    let winning_record = after_race["result"]["snapshot"]["groups"][0].clone();
    assert_eq!(winning_record["revision"], 2);
    wait_for_authority_catalog(&socket_b, &authority_a, "fresh", 1);
    let alpha_data = config_a.join("herdr-dev");
    let authority_path = alpha_data.join("group-authority.json");
    let groups_path = alpha_data.join("groups.json");
    let rolled_back_authority = fs::read(&authority_path).expect("authority rollback fixture");
    let rolled_back_groups = fs::read(&groups_path).expect("group rollback fixture");

    let collision_b = format!(
        "onboarding = false\n[ui]\nshow_home_on_start = false\n[remote.fleet]\nself_name = \"beta\"\ntimeout_ms = 500\nrefresh_interval_ms = 100\nheartbeat_stale_ms = 500\n[[remote.fleet.hosts]]\nname = \"beta\"\nlocal = true\nsocket = \"{}\"\n[[remote.fleet.hosts]]\nname = \"alpha-primary\"\nlocal = true\nsocket = \"{}\"\n[[remote.fleet.hosts]]\nname = \"alpha-duplicate\"\nlocal = true\nsocket = \"{}\"\n",
        socket_b.display(),
        socket_a.display(),
        socket_a.display(),
    );
    fs::write(config_b.join("herdr/config.toml"), collision_b).unwrap();
    let reloaded = send_json_request(
        &socket_b,
        "reload_collision",
        "server.reload_config",
        json!({}),
    );
    assert!(reloaded.get("result").is_some(), "{reloaded}");
    wait_for_authority_catalog(&socket_b, &authority_a, "identity_conflict", 2);
    let collision_mutation = send_json_request(
        &socket_b,
        "collision_mutation",
        "group.rename",
        json!({"group_id": group_id, "name": "Blocked", "expected_revision": 2}),
    );
    assert_eq!(collision_mutation["error"]["code"], "authority_not_fresh");
    assert!(collision_mutation["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains("identity conflict")));

    fs::write(config_b.join("herdr/config.toml"), &normal_b).unwrap();
    send_json_request(
        &socket_b,
        "reload_normal",
        "server.reload_config",
        json!({}),
    );
    wait_for_authority_catalog(&socket_b, &authority_a, "fresh", 1);

    let collision_a = format!(
        "onboarding = false\n[ui]\nshow_home_on_start = false\n[remote.fleet]\nself_name = \"alpha\"\ntimeout_ms = 500\nrefresh_interval_ms = 100\nheartbeat_stale_ms = 500\n[[remote.fleet.hosts]]\nname = \"alpha-primary\"\nlocal = true\nsocket = \"{}\"\n[[remote.fleet.hosts]]\nname = \"alpha-duplicate\"\nlocal = true\nsocket = \"{}\"\n[[remote.fleet.hosts]]\nname = \"beta\"\nlocal = true\nsocket = \"{}\"\n",
        socket_a.display(),
        socket_a.display(),
        socket_b.display(),
    );
    fs::write(config_a.join("herdr/config.toml"), collision_a).unwrap();
    send_json_request(
        &socket_a,
        "reload_local_collision",
        "server.reload_config",
        json!({}),
    );
    wait_for_authority_catalog(&socket_a, &authority_a, "identity_conflict", 2);
    let local_collision = send_json_request(
        &socket_a,
        "local_collision_create",
        "group.create",
        json!({"name": "Blocked locally", "expected_revision": 2}),
    );
    assert_eq!(local_collision["error"]["code"], "authority_not_fresh");
    assert!(local_collision["error"]["message"]
        .as_str()
        .is_some_and(|message| message.contains(authority_a.as_str().unwrap_or_default())));
    fs::write(config_a.join("herdr/config.toml"), &normal_a).unwrap();
    send_json_request(
        &socket_a,
        "reload_local_normal",
        "server.reload_config",
        json!({}),
    );
    wait_for_authority_catalog(&socket_a, &authority_a, "fresh", 1);

    let deleted = send_json_request(
        &socket_b,
        "delete_routed",
        "group.delete",
        json!({"group_id": group_id, "expected_revision": 2}),
    );
    assert_eq!(deleted["result"]["record"]["state"], "deleted", "{deleted}");
    wait_for_authority_group_state(&socket_b, &authority_a, "fresh", "deleted");
    let deleted_authority = fs::read(&authority_path).expect("deleted authority state");
    let deleted_groups = fs::read(&groups_path).expect("deleted group state");

    drop(server_a.take());
    let stale = wait_for_authority_catalog(&socket_b, &authority_a, "stale", 1);
    let retained = stale["result"]["snapshot"]["authority_catalogs"]
        .as_array()
        .expect("catalog array")
        .iter()
        .find(|catalog| catalog["authority_id"] == authority_a)
        .expect("retained alpha catalog");
    assert_eq!(retained["snapshot"]["groups"][0]["state"], "deleted");

    drop(server_b.take());
    fs::write(&authority_path, &rolled_back_authority).expect("restore older authority ledger");
    fs::write(&groups_path, &rolled_back_groups).expect("restore older group store");
    server_a = Some(spawn_server_with_config_text(
        &config_a, &runtime_a, &socket_a, None, &normal_a,
    ));
    wait_for_socket(&socket_a, Duration::from_secs(5));
    let rolled_back_owner = send_json_request(
        &socket_a,
        "rolled_back_owner",
        "group.host_snapshot",
        json!({}),
    );
    assert_eq!(rolled_back_owner["result"]["snapshot"]["revision"], 2);
    assert_eq!(
        rolled_back_owner["result"]["snapshot"]["groups"][0]["state"],
        "active"
    );
    server_b = Some(spawn_server_with_config_text(
        &config_b, &runtime_b, &socket_b, None, &normal_b,
    ));
    wait_for_socket(&socket_b, Duration::from_secs(5));
    let rejected = wait_for_authority_catalog_error(
        &socket_b,
        &authority_a,
        "stale",
        "snapshot revision rolled back",
    );
    let rejected_catalog = rejected["result"]["snapshot"]["authority_catalogs"]
        .as_array()
        .expect("restarted beta catalogs")
        .iter()
        .find(|catalog| catalog["authority_id"] == authority_a)
        .expect("retained catalog after beta restart");
    assert_eq!(
        rejected_catalog["snapshot"]["groups"][0]["state"], "active",
        "the live catalog reports the owner's current rolled-back answer"
    );

    drop(server_a.take());
    fs::write(&authority_path, deleted_authority).expect("restore deleted authority ledger");
    fs::write(&groups_path, deleted_groups).expect("restore deleted group store");
    server_a = Some(spawn_server_with_config_text(
        &config_a, &runtime_a, &socket_a, None, &normal_a,
    ));
    wait_for_socket(&socket_a, Duration::from_secs(5));
    let recovered = wait_for_authority_catalog(&socket_b, &authority_a, "fresh", 1);
    assert_eq!(
        recovered["result"]["snapshot"]["authority_catalogs"]
            .as_array()
            .expect("catalog array")
            .iter()
            .find(|catalog| catalog["authority_id"] == authority_a)
            .expect("recovered alpha catalog")["snapshot"]["groups"][0]["state"],
        "deleted"
    );

    drop(server_a.take());
    cleanup_spawned_herdr(server_b.take().expect("beta server"), base_b);
    cleanup_test_base(&base_a);
}
