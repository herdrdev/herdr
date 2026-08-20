#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};

fn temporary_root() -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::path::Path::new("/tmp").join(format!("hw-{}-{nonce:x}", std::process::id()))
}

#[test]
fn generated_bridge_script_resolves_through_the_fork_cli() {
    let root = temporary_root();
    let state_home = root.join("s");
    let config_home = root.join("c");
    let claude_home = root.join("a");
    std::fs::create_dir_all(&claude_home).expect("Claude config directory");

    let output = Command::new(env!("CARGO_BIN_EXE_herdr"))
        .args(["watcher", "claude-bridge", "enable"])
        .env("XDG_STATE_HOME", &state_home)
        .env("XDG_CONFIG_HOME", &config_home)
        .env("CLAUDE_CONFIG_DIR", &claude_home)
        .output()
        .expect("enable bridge through fork CLI");
    assert!(
        output.status.success(),
        "bridge enable failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let settings_path = String::from_utf8(output.stdout)
        .expect("settings path UTF-8")
        .trim()
        .to_string();
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(settings_path).expect("generated Claude settings"),
    )
    .expect("settings JSON");
    let script = settings["statusLine"]["command"]
        .as_str()
        .expect("statusLine command")
        .trim_matches('\'')
        .to_string();

    let watcher_state = state_home
        .join("herdr-dev")
        .join("plugins")
        .join("herdr-agent-watcher");
    let socket = watcher_state.join("herdr-agent-watcher-state.sock");
    let target = root.join("status.json");
    let listener = UnixListener::bind(&socket).expect("fake watcher state socket");
    let target_for_server = target.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("bridge connection");
        let mut request = String::new();
        BufReader::new(stream.try_clone().expect("clone stream"))
            .read_line(&mut request)
            .expect("status-path request");
        assert!(request.contains("status-path"));
        writeln!(
            stream,
            "{}",
            serde_json::json!({ "version": 2, "path": target_for_server })
        )
        .expect("status-path response");
    });

    let mut child = Command::new(&script)
        .env("HERDR_PANE_ID", "w1:p1")
        .stdin(Stdio::piped())
        .spawn()
        .expect("execute generated bridge script");
    child
        .stdin
        .take()
        .expect("script stdin")
        .write_all(br#"{"session_id":"session-1"}"#)
        .expect("write bridge payload");
    assert!(child.wait().expect("bridge script exit").success());
    server.join().expect("fake watcher server");
    assert_eq!(
        std::fs::read_to_string(&target).expect("forwarded status payload"),
        r#"{"session_id":"session-1"}"#
    );

    let _ = std::fs::remove_dir_all(root);
}
