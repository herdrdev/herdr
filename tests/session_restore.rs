//! Session restore must not wait for optional repository metadata.
#![cfg(unix)]

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct RestoredServer {
    child: std::process::Child,
    base: PathBuf,
}

impl Drop for RestoredServer {
    fn drop(&mut self) {
        let pid = self.child.id();
        let _ = self.child.kill();
        let _ = self.child.wait();
        support::unregister_spawned_herdr_pid(Some(pid));
        support::cleanup_test_base(&self.base);
    }
}

fn restore_request(socket_path: &Path, method: &str) -> serde_json::Value {
    let response = restore_request_with_params(socket_path, method, serde_json::json!({}));
    assert!(response.get("error").is_none(), "{response}");
    response
}

fn restore_request_with_params(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket_path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        serde_json::json!({"id": "restore", "method": method, "params": params})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .unwrap_or_else(|err| panic!("{method} blocked during session restore: {err}"));
    serde_json::from_str(&line).unwrap()
}

#[test]
fn server_restores_while_git_metadata_is_blocked() {
    use std::os::unix::fs::OpenOptionsExt;

    for saved_membership in [false, true] {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = PathBuf::from(format!("/tmp/herdr-restore-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let base = fs::canonicalize(base).unwrap();
        let config_home = base.join("config");
        let runtime_dir = base.join("runtime");
        let socket_path = runtime_dir.join("herdr.sock");
        let data_dir = config_home.join(if cfg!(debug_assertions) {
            "herdr-dev"
        } else {
            "herdr"
        });
        let repo = base.join("repo");
        fs::create_dir_all(repo.join(".git/objects")).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let config_path = repo.join(".git/config");
        assert!(Command::new("mkfifo")
            .arg(&config_path)
            .status()
            .unwrap()
            .success());
        // Keep the FIFO open without writing: metadata reads cannot finish until
        // this guard drops, so API readiness must not depend on a lucky delay.
        let _blocked_metadata = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&config_path)
            .unwrap();
        let membership = saved_membership.then(|| {
            serde_json::json!({
                "key": "saved-repo", "label": "saved-space", "repo_root": repo,
                "checkout_path": repo, "is_linked_worktree": false
            })
        });
        let healthy = base.join("healthy");
        assert!(Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .arg(&healthy)
            .status()
            .unwrap()
            .success());
        let healthy_membership = serde_json::json!({
            "key": healthy.join(".git"), "label": "healthy", "repo_root": healthy,
            "checkout_path": healthy, "is_linked_worktree": false
        });
        let snapshot = serde_json::json!({
            "version": 3, "active": 0, "selected": 0,
            "workspaces": [{
                "id": "w1", "custom_name": "restored", "identity_cwd": repo,
                "worktree_space": membership,
                "public_pane_numbers": {"7": 3}, "public_tab_numbers": [2],
                "tabs": [{"layout": {"Pane": 7}, "panes": {"7": {"cwd": repo}},
                          "zoomed": false, "focused": 7, "root_pane": 7}]
            }, {
                "id": "w2", "custom_name": "healthy", "identity_cwd": healthy,
                "worktree_space": healthy_membership,
                "tabs": [{"layout": {"Pane": 9}, "panes": {"9": {"cwd": healthy}},
                          "zoomed": false, "focused": 9, "root_pane": 9}]
            }]
        });
        fs::write(data_dir.join("session.json"), snapshot.to_string()).unwrap();
        fs::write(data_dir.join("config.toml"),
            "onboarding = false\n[terminal]\ndefault_shell = \"/bin/sh\"\n[ui.sidebar.spaces]\nrows = [[\"workspace\"]]\n").unwrap();
        support::register_runtime_dir(&runtime_dir);
        let child = Command::new(env!("CARGO_BIN_EXE_herdr"))
            .arg("server")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_RUNTIME_DIR", &runtime_dir)
            .env("HERDR_SOCKET_PATH", &socket_path)
            .env("SHELL", "/bin/sh")
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_STARTUP_CWD")
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_ENV")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        support::register_spawned_herdr_pid(Some(child.id()));
        let mut herdr = RestoredServer { child, base };
        support::wait_for_socket(&socket_path, Duration::from_secs(5));

        let workspaces = restore_request(&socket_path, "workspace.list");
        let restored = &workspaces["result"]["workspaces"][0];
        assert_eq!(restored["workspace_id"], "w1");
        assert_eq!(restored["label"], "restored");
        assert_eq!(restored["active_tab_id"], "w1:t2");
        assert_eq!(restored["pane_count"], 1);
        if saved_membership {
            for (method, params) in [
                (
                    "worktree.create",
                    serde_json::json!({"workspace_id": "w1", "branch": "blocked"}),
                ),
                (
                    "worktree.open",
                    serde_json::json!({"workspace_id": "w1", "branch": "main"}),
                ),
                ("worktree.list", serde_json::json!({"workspace_id": "w1"})),
                (
                    "worktree.remove",
                    serde_json::json!({"workspace_id": "w1", "force": true}),
                ),
            ] {
                let response = restore_request_with_params(&socket_path, method, params);
                assert_eq!(
                    response["error"]["code"], "worktree_operation_in_progress",
                    "{method}: {response}"
                );
            }
        }
        if saved_membership {
            // A blocked saved membership must not keep unrelated saved
            // memberships pending or prevent their worktree operations.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let response = restore_request_with_params(
                    &socket_path,
                    "worktree.list",
                    serde_json::json!({"workspace_id": "w2"}),
                );
                if response.get("error").is_none() {
                    break;
                }
                assert_eq!(response["error"]["code"], "worktree_operation_in_progress");
                assert!(
                    std::time::Instant::now() < deadline,
                    "healthy workspace validation waited for another checkout's blocked metadata"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        restore_request(&socket_path, "session.snapshot");
        restore_request(&socket_path, "agent.list");
        restore_request(&socket_path, "server.stop");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = herdr.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "server shutdown waited for blocked Git metadata"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let saved: serde_json::Value =
            serde_json::from_slice(&fs::read(data_dir.join("session.json")).unwrap()).unwrap();
        assert_eq!(
            saved["workspaces"][0]["identity_cwd"],
            snapshot["workspaces"][0]["identity_cwd"]
        );
        assert_eq!(
            saved["workspaces"][0]["worktree_space"],
            snapshot["workspaces"][0]["worktree_space"]
        );
        drop(herdr);
    }
}
