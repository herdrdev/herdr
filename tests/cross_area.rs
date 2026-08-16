//! Cross-area integration tests for end-to-end persistence flows.

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
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
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
        json!({ "label": label }),
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
    let mut payload = encode_varint_u32(0);
    payload.extend_from_slice(&encode_varint_u32(version));
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

    let server = spawn_server_with_path(
        &config_home,
        &runtime_dir,
        &api_socket,
        Some(Path::new(&path_override)),
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
            frame_contains_colored_symbol(frame, "●", (249, 226, 175))
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
            frame_contains_colored_symbol(frame, "●", (243, 139, 168))
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
    client_handshake(&mut client, CURRENT_PROTOCOL, 100, 30);
    assert!(wait_for_frame(&mut client, Duration::from_secs(2)));
    drain_server_messages(&mut client, Duration::from_millis(300));

    let before = workspace_count(&api_socket);

    // Create a workspace via API while the client is attached.
    let created = workspace_create(&api_socket, "api-visible-workspace");
    let created_workspace_id = created["result"]["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace.create should return workspace_id")
        .to_string();

    // The attached client must receive a frame that includes the new workspace
    // label, proving client-side state reflects the API surface.
    let saw_workspace_on_client =
        wait_for_frame_matching(&mut client, Duration::from_secs(3), |frame| {
            frame_contains_text(frame, "api-visible-workspace")
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
                    if out.contains("\u{2500}")
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

// ---------------------------------------------------------------------------
// Pane context-menu movement and split-orientation TUI acceptance
// ---------------------------------------------------------------------------

/// SGR right-button press at a 0-based cell position.
fn sgr_right_click(col: u16, row: u16) -> Vec<u8> {
    format!("\x1b[<2;{};{}M", col + 1, row + 1).into_bytes()
}

fn layout_export(socket_path: &Path, tab_id: &str) -> Value {
    send_json_request(
        socket_path,
        "layout_export",
        "layout.export",
        json!({ "tab_id": tab_id }),
    )
}

fn pane_split(socket_path: &Path, target_pane_id: &str, direction: &str) -> Value {
    send_json_request(
        socket_path,
        "pane_split",
        "pane.split",
        json!({ "target_pane_id": target_pane_id, "direction": direction }),
    )
}

fn tab_list(socket_path: &Path, workspace_id: &str) -> Value {
    send_json_request(
        socket_path,
        "tab_list",
        "tab.list",
        json!({ "workspace_id": workspace_id }),
    )
}

/// Root split direction of a tab's exported layout, if the root is a split.
fn root_split_direction(layout: &Value) -> Option<String> {
    let root = layout.pointer("/result/layout/root")?;
    if root.get("type")?.as_str()? != "split" {
        return None;
    }
    Some(root.get("direction")?.as_str()?.to_string())
}

fn collect_pane_ids(node: &Value, out: &mut Vec<String>) {
    match node.get("type").and_then(Value::as_str) {
        Some("pane") => {
            if let Some(id) = node.get("pane_id").and_then(Value::as_str) {
                out.push(id.to_string());
            }
        }
        Some("split") => {
            if let Some(first) = node.get("first") {
                collect_pane_ids(first, out);
            }
            if let Some(second) = node.get("second") {
                collect_pane_ids(second, out);
            }
        }
        _ => {}
    }
}

fn tab_pane_ids(socket_path: &Path, tab_id: &str) -> Vec<String> {
    let layout = layout_export(socket_path, tab_id);
    let mut ids = Vec::new();
    if let Some(root) = layout.pointer("/result/layout/root") {
        collect_pane_ids(root, &mut ids);
    }
    ids
}

/// Drives the real TUI over the client socket: right-clicks a pane, walks the
/// context menu with the keyboard, and activates a labeled entry. Asserts the
/// menu actually rendered the entry before selecting it.
fn right_click_and_activate(
    client: &mut UnixStream,
    col: u16,
    row: u16,
    label: &str,
    steps_down: usize,
) {
    drain_server_messages(client, Duration::from_millis(200));
    send_client_input(client, &sgr_right_click(col, row));

    let opened = wait_for_frame_matching(client, Duration::from_secs(5), |frame| {
        frame_contains_text(frame, label)
    })
    .expect("frame decoding should succeed");
    assert!(
        opened,
        "right-click at ({col},{row}) should render a pane context menu containing {label:?}"
    );

    for _ in 0..steps_down {
        send_client_input(client, b"\x1b[B");
        thread::sleep(Duration::from_millis(30));
    }
    send_client_input(client, b"\r");
    thread::sleep(Duration::from_millis(400));
    drain_server_messages(client, Duration::from_millis(300));
}

#[test]
fn pane_context_menu_swaps_split_orientation_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "orientation");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "orientation");

    let tabs = tab_list(&api_socket, &ws_id);
    let tab_id = tabs
        .pointer("/result/tabs/0/tab_id")
        .and_then(Value::as_str)
        .expect("tab id")
        .to_string();

    let root_pane = tab_pane_ids(&api_socket, &tab_id)
        .first()
        .cloned()
        .expect("root pane");

    // Build a real split so the root node is a split with a known direction.
    pane_split(&api_socket, &root_pane, "right");
    let before = layout_export(&api_socket, &tab_id);
    assert_eq!(
        root_split_direction(&before).as_deref(),
        Some("right"),
        "split right should produce a horizontal root split; layout: {before}"
    );

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    // "Swap to vertical" is the entry after "Rename pane" and "Swap to horizontal"
    // for a pane with no manual label and no pending swap source.
    right_click_and_activate(&mut client, 40, 6, "Swap to vertical", 2);

    let after = layout_export(&api_socket, &tab_id);
    assert_eq!(
        root_split_direction(&after).as_deref(),
        Some("down"),
        "context-menu 'Swap to vertical' must flip the real layout to a vertical split; \
         before={before}, after={after}"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

#[test]
fn pane_context_menu_moves_pane_to_new_workspace_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "movement");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "movement");

    let tabs = tab_list(&api_socket, &ws_id);
    let tab_id = tabs
        .pointer("/result/tabs/0/tab_id")
        .and_then(Value::as_str)
        .expect("tab id")
        .to_string();

    let root_pane = tab_pane_ids(&api_socket, &tab_id)
        .first()
        .cloned()
        .expect("root pane");

    // Two panes so moving one away leaves the source tab alive.
    pane_split(&api_socket, &root_pane, "right");
    let panes_before = tab_pane_ids(&api_socket, &tab_id);
    assert_eq!(panes_before.len(), 2, "expected a split source tab");

    // Write a marker into the pane so we can prove the process survived the move.
    let marker = "herdr_move_marker_9137";

    let workspaces_before = workspace_count(&api_socket);

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));
    seed_pane_marker(&api_socket, &root_pane, marker);

    // Rename pane, Swap to horizontal, Swap to vertical, Move to previous tab,
    // Move to next tab, Move to previous workspace, Move to next workspace,
    // Move to new workspace -> index 7.
    right_click_and_activate(&mut client, 40, 6, "Move to new workspace", 7);

    let workspaces_after = workspace_count(&api_socket);
    assert_eq!(
        workspaces_after,
        workspaces_before + 1,
        "'Move to new workspace' must create exactly one new workspace"
    );

    let panes_after = tab_pane_ids(&api_socket, &tab_id);
    assert_eq!(
        panes_after.len(),
        1,
        "the moved pane must leave the source tab; before={panes_before:?}, after={panes_after:?}"
    );

    // The moved pane keeps its identity and its live process/scrollback.
    let moved = panes_before
        .iter()
        .find(|id| !panes_after.contains(id))
        .expect("exactly one pane should have left the source tab");
    assert!(
        pane_read_recent_contains(&api_socket, moved, marker, Duration::from_secs(10)),
        "moved pane {moved} must keep its live terminal contents after the move"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

fn tab_create(socket_path: &Path, workspace_id: &str, focus: bool) -> Value {
    send_json_request(
        socket_path,
        "tab_create",
        "tab.create",
        json!({ "workspace_id": workspace_id, "focus": focus }),
    )
}

fn tab_ids(socket_path: &Path, workspace_id: &str) -> Vec<String> {
    tab_list(socket_path, workspace_id)["result"]["tabs"]
        .as_array()
        .expect("tabs array")
        .iter()
        .filter_map(|tab| tab["tab_id"].as_str().map(str::to_string))
        .collect()
}

/// Right-clicks a pane and returns the rendered frame text of the context menu,
/// without activating anything. Used to assert entry presence and edge state.
fn right_click_menu_text(client: &mut UnixStream, col: u16, row: u16) -> String {
    drain_server_messages(client, Duration::from_millis(200));
    send_client_input(client, &sgr_right_click(col, row));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while Instant::now() < deadline {
        match read_server_message_payload(client, Duration::from_millis(120)) {
            Ok((1, payload)) => {
                if let Ok(frame) = decode_frame_payload(&payload) {
                    let width = frame.width.max(1) as usize;
                    let mut text = String::new();
                    for chunk in frame.cells.chunks(width) {
                        for cell in chunk {
                            text.push_str(&cell.symbol);
                        }
                        text.push('\n');
                    }
                    if text.contains("Move to new workspace") {
                        return text;
                    }
                    last = text;
                }
            }
            Ok(_) => {}
            Err(err) if is_timeout(&err) => {}
            Err(_) => break,
        }
    }
    last
}

fn close_menu(client: &mut UnixStream) {
    send_client_input(client, b"\x1b");
    thread::sleep(Duration::from_millis(200));
    drain_server_messages(client, Duration::from_millis(200));
}

/// Writes a marker into a pane's live shell, retrying until it is readable.
/// A freshly spawned pane may not have its shell ready on the first write,
/// which otherwise makes marker-based identity checks flaky under load.
fn seed_pane_marker(socket_path: &Path, pane_id: &str, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut attempt = 0;
    while Instant::now() < deadline {
        attempt += 1;
        pane_send_text(socket_path, pane_id, &format!("echo {marker}\n"));
        if pane_read_recent_contains(socket_path, pane_id, marker, Duration::from_secs(3)) {
            return;
        }
        thread::sleep(Duration::from_millis(200));
    }
    panic!("marker {marker} never appeared in pane {pane_id} after {attempt} attempts");
}

/// Every new context-menu entry must render for a pane, and the tab/workspace
/// movement entries must be present regardless of whether they are enabled.
#[test]
fn pane_context_menu_renders_all_new_entries_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "entries");

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    let text = right_click_menu_text(&mut client, 40, 6);
    for entry in [
        "Swap to horizontal",
        "Swap to vertical",
        "Move to previous tab",
        "Move to next tab",
        "Move to previous workspace",
        "Move to next workspace",
        "Move to new workspace",
    ] {
        assert!(
            text.contains(entry),
            "pane context menu must render {entry:?}; menu frame was:\n{text}"
        );
    }

    close_menu(&mut client);
    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// Activating a disabled edge entry must be a no-op rather than moving the
/// pane. This is set up so a broken guard would have somewhere to move: the
/// workspace has a second tab and a second workspace exists, but the pane sits
/// on the FIRST tab of the FIRST workspace, where "previous" is out of range.
/// A guard that clamps instead of refusing would relocate the pane and fail.
#[test]
fn pane_context_menu_edge_entries_are_inert_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "edges");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "edges");
    let first_tab = tab_ids(&api_socket, &ws_id)
        .first()
        .cloned()
        .expect("first tab");

    // Give a clamping bug a real destination: a second tab in this workspace.
    tab_create(&api_socket, &ws_id, false);
    let tabs = tab_ids(&api_socket, &ws_id);
    assert_eq!(tabs.len(), 2, "expected a second tab; got {tabs:?}");
    let second_tab = tabs[1].clone();

    // Split so the pane could legally move without collapsing the tab.
    let root_pane = tab_pane_ids(&api_socket, &first_tab)
        .first()
        .cloned()
        .expect("root pane");
    pane_split(&api_socket, &root_pane, "right");

    // Refocus the first tab and its original pane so the right-click targets it.
    send_json_request(
        &api_socket,
        "tab_focus",
        "tab.focus",
        json!({ "tab_id": first_tab }),
    );
    send_json_request(
        &api_socket,
        "pane_focus",
        "pane.focus",
        json!({ "pane_id": root_pane }),
    );

    let panes_before = tab_pane_ids(&api_socket, &first_tab);
    let second_before = tab_pane_ids(&api_socket, &second_tab);
    let workspaces_before = workspace_count(&api_socket);
    assert_eq!(panes_before.len(), 2, "source tab should hold two panes");

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    // Index 3 = "Move to previous tab", disabled because this is the first tab.
    right_click_and_activate(&mut client, 40, 6, "Move to previous tab", 3);
    assert_eq!(
        tab_pane_ids(&api_socket, &first_tab),
        panes_before,
        "'Move to previous tab' on the first tab must not move the pane"
    );
    assert_eq!(
        tab_pane_ids(&api_socket, &second_tab),
        second_before,
        "'Move to previous tab' on the first tab must not leak the pane into another tab"
    );

    // Index 5 = "Move to previous workspace", disabled on the first workspace.
    right_click_and_activate(&mut client, 40, 6, "Move to previous workspace", 5);
    assert_eq!(
        tab_pane_ids(&api_socket, &first_tab),
        panes_before,
        "'Move to previous workspace' on the first workspace must not move the pane"
    );
    assert_eq!(
        workspace_count(&api_socket),
        workspaces_before,
        "disabled edge entries must not create workspaces"
    );
    assert_eq!(
        tab_ids(&api_socket, &ws_id).len(),
        2,
        "disabled edge entries must not create tabs"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// "Move to next tab" must relocate the pane into the adjacent tab and keep the
/// live shell process attached to it.
#[test]
fn pane_context_menu_moves_pane_to_next_tab_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "nexttab");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "nexttab");
    let first_tab = tab_ids(&api_socket, &ws_id)
        .first()
        .cloned()
        .expect("first tab");

    // A second tab to move into, then return focus to the first tab.
    tab_create(&api_socket, &ws_id, false);
    let tabs = tab_ids(&api_socket, &ws_id);
    assert_eq!(tabs.len(), 2, "expected two tabs; got {tabs:?}");
    let second_tab = tabs[1].clone();

    // Split so the source tab survives losing a pane.
    let root_pane = tab_pane_ids(&api_socket, &first_tab)
        .first()
        .cloned()
        .expect("root pane");
    pane_split(&api_socket, &root_pane, "right");

    let panes_before = tab_pane_ids(&api_socket, &first_tab);
    assert_eq!(panes_before.len(), 2);
    let second_before = tab_pane_ids(&api_socket, &second_tab);

    let marker = "herdr_next_tab_marker_5521";

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));
    seed_pane_marker(&api_socket, &root_pane, marker);

    // Index 4 = "Move to next tab".
    right_click_and_activate(&mut client, 40, 6, "Move to next tab", 4);

    let panes_after = tab_pane_ids(&api_socket, &first_tab);
    let second_after = tab_pane_ids(&api_socket, &second_tab);
    assert_eq!(
        panes_after.len(),
        1,
        "pane must leave the source tab; before={panes_before:?} after={panes_after:?}"
    );
    assert_eq!(
        second_after.len(),
        second_before.len() + 1,
        "destination tab must gain the pane; before={second_before:?} after={second_after:?}"
    );

    let moved = second_after
        .iter()
        .find(|id| !second_before.contains(id))
        .expect("destination tab should have exactly one new pane");
    assert!(
        pane_read_recent_contains(&api_socket, moved, marker, Duration::from_secs(10)),
        "pane moved to the next tab must keep its live terminal contents"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// "Move to next workspace" must relocate the pane across workspaces and keep
/// its live process.
#[test]
fn pane_context_menu_moves_pane_to_next_workspace_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "ws-source");
    workspace_create(&api_socket, "ws-dest");
    let source_id = workspace_id_by_label(&workspace_list(&api_socket), "ws-source");
    let dest_id = workspace_id_by_label(&workspace_list(&api_socket), "ws-dest");

    let source_tab = tab_ids(&api_socket, &source_id)
        .first()
        .cloned()
        .expect("source tab");
    let dest_tab = tab_ids(&api_socket, &dest_id)
        .first()
        .cloned()
        .expect("dest tab");

    let root_pane = tab_pane_ids(&api_socket, &source_tab)
        .first()
        .cloned()
        .expect("root pane");
    pane_split(&api_socket, &root_pane, "down");

    let panes_before = tab_pane_ids(&api_socket, &source_tab);
    assert_eq!(panes_before.len(), 2);
    let dest_before = tab_pane_ids(&api_socket, &dest_tab);

    let marker = "herdr_next_ws_marker_7744";

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));
    seed_pane_marker(&api_socket, &root_pane, marker);

    // Focus the source workspace pane, then index 6 = "Move to next workspace".
    right_click_and_activate(&mut client, 40, 6, "Move to next workspace", 6);

    let panes_after = tab_pane_ids(&api_socket, &source_tab);
    let dest_after = tab_pane_ids(&api_socket, &dest_tab);

    assert_eq!(
        panes_after.len() + dest_after.len(),
        panes_before.len() + dest_before.len(),
        "moving across workspaces must conserve pane count; \
         source before={panes_before:?} after={panes_after:?}, \
         dest before={dest_before:?} after={dest_after:?}"
    );
    assert_eq!(
        panes_after.len(),
        1,
        "pane must leave the source workspace tab"
    );
    assert_eq!(
        dest_after.len(),
        dest_before.len() + 1,
        "destination workspace must gain the pane"
    );

    let moved = dest_after
        .iter()
        .find(|id| !dest_before.contains(id))
        .expect("destination workspace should have exactly one new pane");
    assert!(
        pane_read_recent_contains(&api_socket, moved, marker, Duration::from_secs(10)),
        "pane moved across workspaces must keep its live terminal contents"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// "Swap to horizontal" must flip a vertical split back to horizontal, the
/// mirror of the vertical case.
#[test]
fn pane_context_menu_swaps_to_horizontal_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "horizontal");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "horizontal");
    let tab_id = tab_ids(&api_socket, &ws_id)
        .first()
        .cloned()
        .expect("tab id");
    let root_pane = tab_pane_ids(&api_socket, &tab_id)
        .first()
        .cloned()
        .expect("root pane");

    pane_split(&api_socket, &root_pane, "down");
    let before = layout_export(&api_socket, &tab_id);
    assert_eq!(
        root_split_direction(&before).as_deref(),
        Some("down"),
        "split down should produce a vertical root split; layout: {before}"
    );

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    // Index 1 = "Swap to horizontal".
    right_click_and_activate(&mut client, 40, 6, "Swap to horizontal", 1);

    let after = layout_export(&api_socket, &tab_id);
    assert_eq!(
        root_split_direction(&after).as_deref(),
        Some("right"),
        "context-menu 'Swap to horizontal' must flip the layout to a horizontal split; \
         before={before}, after={after}"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// "Move to previous tab" must relocate the pane into the preceding tab when it
/// is enabled. The pane is placed on the SECOND tab so "previous" is in range.
#[test]
fn pane_context_menu_moves_pane_to_previous_tab_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "prevtab");
    let ws_id = workspace_id_by_label(&workspace_list(&api_socket), "prevtab");
    let first_tab = tab_ids(&api_socket, &ws_id)
        .first()
        .cloned()
        .expect("first tab");

    // Focus a second tab; its pane is the one we move backwards.
    tab_create(&api_socket, &ws_id, true);
    let tabs = tab_ids(&api_socket, &ws_id);
    assert_eq!(tabs.len(), 2, "expected two tabs; got {tabs:?}");
    let second_tab = tabs[1].clone();

    // Split the second tab so it survives losing a pane.
    let second_root = tab_pane_ids(&api_socket, &second_tab)
        .first()
        .cloned()
        .expect("second tab root pane");
    pane_split(&api_socket, &second_root, "right");
    send_json_request(
        &api_socket,
        "pane_focus",
        "pane.focus",
        json!({ "pane_id": second_root }),
    );

    let source_before = tab_pane_ids(&api_socket, &second_tab);
    let dest_before = tab_pane_ids(&api_socket, &first_tab);
    assert_eq!(source_before.len(), 2, "source tab should hold two panes");

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    let marker = "herdr_prev_tab_marker_3310";
    seed_pane_marker(&api_socket, &second_root, marker);

    // Index 3 = "Move to previous tab".
    right_click_and_activate(&mut client, 40, 6, "Move to previous tab", 3);

    let source_after = tab_pane_ids(&api_socket, &second_tab);
    let dest_after = tab_pane_ids(&api_socket, &first_tab);
    assert_eq!(
        source_after.len(),
        1,
        "pane must leave the source tab; before={source_before:?} after={source_after:?}"
    );
    assert_eq!(
        dest_after.len(),
        dest_before.len() + 1,
        "previous tab must gain the pane; before={dest_before:?} after={dest_after:?}"
    );

    let moved = dest_after
        .iter()
        .find(|id| !dest_before.contains(id))
        .expect("previous tab should have exactly one new pane");
    assert!(
        pane_read_recent_contains(&api_socket, moved, marker, Duration::from_secs(10)),
        "pane moved to the previous tab must keep its live terminal contents"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// "Move to previous workspace" must relocate the pane into the preceding
/// workspace when enabled. The pane starts in the SECOND workspace.
#[test]
fn pane_context_menu_moves_pane_to_previous_workspace_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "ws-earlier");
    workspace_create(&api_socket, "ws-later");
    let earlier_id = workspace_id_by_label(&workspace_list(&api_socket), "ws-earlier");
    let later_id = workspace_id_by_label(&workspace_list(&api_socket), "ws-later");

    let earlier_tab = tab_ids(&api_socket, &earlier_id)
        .first()
        .cloned()
        .expect("earlier tab");
    let later_tab = tab_ids(&api_socket, &later_id)
        .first()
        .cloned()
        .expect("later tab");

    let later_root = tab_pane_ids(&api_socket, &later_tab)
        .first()
        .cloned()
        .expect("later root pane");
    pane_split(&api_socket, &later_root, "down");

    // Focus the later workspace and its original pane.
    send_json_request(
        &api_socket,
        "workspace_focus",
        "workspace.focus",
        json!({ "workspace_id": later_id }),
    );
    send_json_request(
        &api_socket,
        "pane_focus",
        "pane.focus",
        json!({ "pane_id": later_root }),
    );

    let source_before = tab_pane_ids(&api_socket, &later_tab);
    let dest_before = tab_pane_ids(&api_socket, &earlier_tab);
    assert_eq!(source_before.len(), 2);

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    let marker = "herdr_prev_ws_marker_8802";
    seed_pane_marker(&api_socket, &later_root, marker);

    // Index 5 = "Move to previous workspace".
    right_click_and_activate(&mut client, 40, 6, "Move to previous workspace", 5);

    let source_after = tab_pane_ids(&api_socket, &later_tab);
    let dest_after = tab_pane_ids(&api_socket, &earlier_tab);

    assert_eq!(
        source_after.len() + dest_after.len(),
        source_before.len() + dest_before.len(),
        "moving to the previous workspace must conserve pane count; \
         source before={source_before:?} after={source_after:?}, \
         dest before={dest_before:?} after={dest_after:?}"
    );
    assert_eq!(
        source_after.len(),
        1,
        "pane must leave the later workspace tab"
    );
    assert_eq!(
        dest_after.len(),
        dest_before.len() + 1,
        "earlier workspace must gain the pane"
    );

    let moved = dest_after
        .iter()
        .find(|id| !dest_before.contains(id))
        .expect("earlier workspace should have exactly one new pane");
    assert!(
        pane_read_recent_contains(&api_socket, moved, marker, Duration::from_secs(10)),
        "pane moved to the previous workspace must keep its live terminal contents"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}

/// The NEXT-side edges must also refuse rather than clamp. The pane sits on the
/// LAST tab of the LAST workspace, with an earlier tab and an earlier workspace
/// present so a clamping bug would have a visible destination to move into.
#[test]
fn pane_context_menu_next_edge_entries_are_inert_in_live_tui() {
    let _lock = test_lock();
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &runtime_dir, &api_socket);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    workspace_create(&api_socket, "ws-first");
    workspace_create(&api_socket, "ws-last");
    let first_ws = workspace_id_by_label(&workspace_list(&api_socket), "ws-first");
    let last_ws = workspace_id_by_label(&workspace_list(&api_socket), "ws-last");

    let first_ws_tab = tab_ids(&api_socket, &first_ws)
        .first()
        .cloned()
        .expect("first workspace tab");

    // Two tabs in the last workspace; the pane goes on the LAST one.
    tab_create(&api_socket, &last_ws, true);
    let tabs = tab_ids(&api_socket, &last_ws);
    assert_eq!(tabs.len(), 2, "expected two tabs; got {tabs:?}");
    let earlier_tab = tabs[0].clone();
    let last_tab = tabs[1].clone();

    // Split so a move would be legal if the guard wrongly allowed it.
    let last_root = tab_pane_ids(&api_socket, &last_tab)
        .first()
        .cloned()
        .expect("last tab root pane");
    pane_split(&api_socket, &last_root, "right");

    send_json_request(
        &api_socket,
        "workspace_focus",
        "workspace.focus",
        json!({ "workspace_id": last_ws }),
    );
    send_json_request(
        &api_socket,
        "tab_focus",
        "tab.focus",
        json!({ "tab_id": last_tab }),
    );
    send_json_request(
        &api_socket,
        "pane_focus",
        "pane.focus",
        json!({ "pane_id": last_root }),
    );

    let source_before = tab_pane_ids(&api_socket, &last_tab);
    let earlier_tab_before = tab_pane_ids(&api_socket, &earlier_tab);
    let first_ws_before = tab_pane_ids(&api_socket, &first_ws_tab);
    let workspaces_before = workspace_count(&api_socket);
    assert_eq!(source_before.len(), 2, "source tab should hold two panes");

    let mut client = UnixStream::connect(&client_socket).expect("client should connect");
    client_handshake(&mut client, CURRENT_PROTOCOL, 120, 32);
    assert!(wait_for_frame(&mut client, Duration::from_secs(5)));

    // Index 4 = "Move to next tab", disabled because this is the last tab.
    right_click_and_activate(&mut client, 40, 6, "Move to next tab", 4);
    assert_eq!(
        tab_pane_ids(&api_socket, &last_tab),
        source_before,
        "'Move to next tab' on the last tab must not move the pane"
    );
    assert_eq!(
        tab_pane_ids(&api_socket, &earlier_tab),
        earlier_tab_before,
        "'Move to next tab' on the last tab must not clamp back into an earlier tab"
    );

    // Index 6 = "Move to next workspace", disabled on the last workspace.
    right_click_and_activate(&mut client, 40, 6, "Move to next workspace", 6);
    assert_eq!(
        tab_pane_ids(&api_socket, &last_tab),
        source_before,
        "'Move to next workspace' on the last workspace must not move the pane"
    );
    assert_eq!(
        tab_pane_ids(&api_socket, &first_ws_tab),
        first_ws_before,
        "'Move to next workspace' on the last workspace must not clamp into an earlier workspace"
    );
    assert_eq!(
        workspace_count(&api_socket),
        workspaces_before,
        "disabled next-side entries must not create workspaces"
    );

    send_client_detach(&mut client);
    drop(client);
    cleanup_spawned_herdr(server, base);
}
