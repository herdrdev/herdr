//! `agent start` must deliver a composed command whatever its length.
//!
//! herdr runs an agent by typing its command at the pane's shell. A terminal in
//! canonical mode assembles one input line in a fixed buffer -- 1024 bytes on
//! Darwin, 4096 on Linux, both measured -- and hands the reader nothing until a
//! terminator arrives, so a longer line cannot be delivered: the kernel keeps a
//! prefix, discards the rest, and still reports the write as successful.
//!
//! `dash` is the deterministic case. It has no line editor, so it never leaves
//! canonical mode, and it is `/bin/sh` on Ubuntu. `bash` and `zsh` are only
//! intermittently exposed -- during pane startup before their line editor is
//! up, and for the duration of every foreground command -- which is why the
//! original report looked timing-dependent (refs #2862).

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, wait_for_socket,
};

/// Longer than the canonical line buffer on either platform, so one payload
/// reproduces the failure everywhere.
const OVERSIZED_ARG: usize = 4200;
/// Comfortably typed directly on any platform.
const ORDINARY_ARG: usize = 20;

fn unique_test_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-long-command-test-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Option<Box<dyn MasterPty + Send>>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        drop(self._master.take());
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

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A stand-in for the agent binary that records how many bytes of its argument
/// actually arrived. A command cut by the line discipline never runs it at all.
fn install_fake_agent(bin_dir: &Path, marker: &Path) {
    fs::create_dir_all(bin_dir).unwrap();
    let agent = bin_dir.join("claude");
    fs::write(
        &agent,
        format!(
            "#!/bin/sh\nprintf %s \"${{#1}}\" > '{}'\nsleep 30\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&agent, fs::Permissions::from_mode(0o755)).unwrap();
}

fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    shell: &str,
    bin_dir: &Path,
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
    cmd.env("XDG_STATE_HOME", config_home.join("state"));
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", shell);
    cmd.env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()));
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
    let request = json!({ "id": id, "method": method, "params": params });
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn pane_read(socket_path: &Path, pane_id: &str) -> String {
    let response = send_json_request(
        socket_path,
        "pane_read",
        "pane.read",
        json!({ "pane_id": pane_id, "source": "recent", "strip_ansi": true }),
    );
    response["result"]["read"]["text"]
        .as_str()
        .map(|text| text.trim_end().to_string())
        .unwrap_or_else(|| format!("<pane.read failed: {response}>"))
}

fn wait_for_marker(marker: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(contents) = fs::read_to_string(marker) {
            if !contents.is_empty() {
                return Some(contents);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

struct Fixture {
    _spawned: SpawnedHerdr,
    base: PathBuf,
    api_socket: PathBuf,
    pane_id: String,
    marker: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup_test_base(&self.base);
    }
}

fn start_pane(label: &str, shell: &str) -> Fixture {
    let base = unique_test_dir();
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = base.join("api.sock");
    let bin_dir = base.join("bin");
    let marker = base.join("agent-argv-len");
    install_fake_agent(&bin_dir, &marker);
    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, shell, &bin_dir);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    let created = send_json_request(
        &api_socket,
        "workspace_create",
        "workspace.create",
        json!({ "label": label }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane id")
        .to_string();
    // Let the shell settle so the pane starts from its steady state rather than
    // the canonical window every pane is born in.
    thread::sleep(Duration::from_millis(1500));
    Fixture {
        _spawned: spawned,
        base,
        api_socket,
        pane_id,
        marker,
    }
}

/// Start an agent and assert herdr accepted the request, which is the report
/// the issue says cannot be trusted.
fn agent_start(fixture: &Fixture, arg_len: usize) {
    let response = send_json_request(
        &fixture.api_socket,
        "agent_start",
        "agent.start",
        json!({
            "name": "probe",
            "kind": "claude",
            "pane_id": fixture.pane_id,
            "args": ["A".repeat(arg_len)],
            "timeout_ms": 20000,
        }),
    );
    assert!(
        response.get("error").is_none(),
        "agent.start should be accepted: {response}"
    );
}

fn dash_path() -> Option<String> {
    ["/bin/dash", "/usr/bin/dash"]
        .into_iter()
        .find(|candidate| Path::new(candidate).exists())
        .map(str::to_string)
}

/// refs #2862: the reported failure, made deterministic. `dash` never leaves
/// canonical mode, so the composed command meets the line cap every time
/// rather than only inside a startup window.
#[test]
fn agent_start_delivers_long_args_to_a_shell_that_never_leaves_canonical_mode() {
    let _guard = test_lock();
    let Some(dash) = dash_path() else {
        eprintln!("skipping: dash is not installed on this host");
        return;
    };
    let fixture = start_pane("long-canonical", &dash);

    agent_start(&fixture, OVERSIZED_ARG);

    assert_eq!(
        wait_for_marker(&fixture.marker, Duration::from_secs(15)).as_deref(),
        Some(&OVERSIZED_ARG.to_string()[..]),
        "herdr reported the agent as started but its command never ran intact; pane showed:\n{}",
        pane_read(&fixture.api_socket, &fixture.pane_id)
    );
}

/// Control: an ordinary command reaches the same always-canonical shell, so a
/// failure above is about length and not about `dash` or the harness.
#[test]
fn agent_start_delivers_ordinary_args_to_a_shell_that_never_leaves_canonical_mode() {
    let _guard = test_lock();
    let Some(dash) = dash_path() else {
        eprintln!("skipping: dash is not installed on this host");
        return;
    };
    let fixture = start_pane("ordinary-canonical", &dash);

    agent_start(&fixture, ORDINARY_ARG);

    assert_eq!(
        wait_for_marker(&fixture.marker, Duration::from_secs(15)).as_deref(),
        Some(&ORDINARY_ARG.to_string()[..]),
        "an ordinary agent command must reach the pane; pane showed:\n{}",
        pane_read(&fixture.api_socket, &fixture.pane_id)
    );
}

/// Control: a shell whose line editor holds the terminal in raw mode has no
/// per-line cap and delivered long commands before this fix. It must keep
/// doing so.
#[test]
fn agent_start_delivers_long_args_at_a_raw_mode_prompt() {
    let _guard = test_lock();
    let fixture = start_pane("long-raw", "/bin/sh");

    agent_start(&fixture, OVERSIZED_ARG);

    assert_eq!(
        wait_for_marker(&fixture.marker, Duration::from_secs(15)).as_deref(),
        Some(&OVERSIZED_ARG.to_string()[..]),
        "a raw-mode prompt must still receive long commands; pane showed:\n{}",
        pane_read(&fixture.api_socket, &fixture.pane_id)
    );
}

/// Every payload herdr has staged anywhere under a fixture's state directory.
fn staged_payloads(base: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path
                .file_name()
                .is_some_and(|name| name == "staged-commands")
            {
                found.extend(
                    fs::read_dir(&path)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .map(|payload| payload.path()),
                );
            } else {
                stack.push(path);
            }
        }
    }
    found
}

/// refs #2862: staging writes the composed command, system prompt and all, to
/// disk. A request rejected after that point must take the file with it rather
/// than leave it readable in the state directory.
#[test]
fn a_rejected_agent_start_leaves_no_staged_payload() {
    let _guard = test_lock();
    let fixture = start_pane("rejected", "/bin/sh");
    let response = send_json_request(
        &fixture.api_socket,
        "agent_start",
        "agent.start",
        json!({
            "name": "probe",
            "kind": "claude",
            "pane_id": fixture.pane_id,
            "args": ["A".repeat(OVERSIZED_ARG)],
            "timeout_ms": 1,
        }),
    );
    assert_eq!(
        response["error"]["code"].as_str(),
        Some("invalid_agent_timeout"),
        "an out-of-range timeout must be rejected: {response}"
    );
    assert!(
        staged_payloads(&fixture.base).is_empty(),
        "a rejected request must not leave a staged payload: {:?}",
        staged_payloads(&fixture.base)
    );
}
