use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::planning_lock::{PlanningLock, Snapshot};

const SSH_OPTIONS: &[&str] = &[
    "-T",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=1",
];

pub(crate) trait Runner: Send + Sync {
    fn execute(&self, target: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, String>;
}

#[derive(Debug, Default)]
pub(crate) struct OpenSshRunner;

impl Runner for OpenSshRunner {
    fn execute(&self, target: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
        if target.is_empty() || target.starts_with('-') {
            return Err("invalid planning-lock SSH target".into());
        }
        let mut child = Command::new("ssh")
            .args(SSH_OPTIONS.iter().copied())
            .args(["--", target, "herdr", "planning-lock"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not start SSH: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(input)
                .map_err(|error| format!("could not write SSH input: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("SSH failed: {error}"))?;
        if !output.status.success() {
            return Err("planning-lock authority command failed".into());
        }
        Ok(output.stdout)
    }
}

pub(crate) fn status(runner: &dyn Runner, target: &str) -> Result<Option<Snapshot>, String> {
    let output = runner.execute(target, &["status", "--json"], &[])?;
    serde_json::from_slice(&output).map_err(|_| "invalid planning-lock status response".into())
}

pub(crate) fn forward_action(
    runner: &dyn Runner,
    target: &str,
    action: &[&str],
    password: &[u8],
) -> Result<Option<Snapshot>, String> {
    runner.execute(target, action, password)?;
    status(runner, target)
}

pub(crate) fn start_polling(
    target: String,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    if target.trim().is_empty() || cfg!(test) {
        return;
    }
    std::thread::Builder::new()
        .name("planning-lock-sync".into())
        .spawn(move || {
            let runner: Arc<dyn Runner> = Arc::new(OpenSshRunner);
            loop {
                match status(runner.as_ref(), &target) {
                    Ok(snapshot) => {
                        if event_tx.blocking_send(crate::events::AppEvent::PlanningLockRemoteSnapshot(snapshot)).is_err() {
                            break;
                        }
                    }
                    Err(error) => tracing::warn!(%error, "planning-lock authority is unreachable; retaining last known state"),
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        })
        .ok();
}

pub(crate) fn watch_authority_file(
    path: std::path::PathBuf,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    if cfg!(test) {
        return;
    }
    std::thread::Builder::new()
        .name("planning-lock-file-watch".into())
        .spawn(move || {
            let mut previous = std::fs::read(&path).ok();
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let current = std::fs::read(&path).ok();
                if current == previous {
                    continue;
                }
                previous = current;
                let lock = PlanningLock::load(&path);
                if event_tx
                    .blocking_send(crate::events::AppEvent::PlanningLockFileChanged(lock))
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

pub(crate) fn run_cli(args: &[String]) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("status") if args.get(1).map(String::as_str) == Some("--json") && args.len() == 2 => {
            let path = crate::config::config_dir().join(crate::planning_lock::CONFIG_FILE_NAME);
            let state = PlanningLock::load(&path);
            let snapshot = state.snapshot(crate::app::settled::unix_seconds(
                std::time::SystemTime::now(),
            ));
            println!(
                "{}",
                serde_json::to_string(&snapshot).unwrap_or_else(|_| "null".into())
            );
            Ok(0)
        }
        Some("unlock")
            if args.get(1).map(String::as_str) == Some("--minutes") && args.len() == 3 =>
        {
            let minutes = args[2].parse::<u8>().unwrap_or_default();
            let allowed = crate::planning_lock::UNLOCK_MINUTES.contains(&minutes);
            if !allowed {
                eprintln!("choose 5, 10, 15, 20 or 30 minutes");
                return Ok(2);
            }
            run_password_action(minutes)
        }
        Some("off") if args.len() == 1 => run_password_action(0),
        _ => {
            eprintln!(
                "usage: herdr planning-lock status --json | unlock --minutes <5|10|15|20|30> | off"
            );
            Ok(2)
        }
    }
}

fn run_password_action(minutes: u8) -> io::Result<i32> {
    let mut password = String::new();
    io::stdin().take(4096).read_to_string(&mut password)?;
    let password = password.trim_end_matches(['\r', '\n']).as_bytes().to_vec();
    let config = crate::config::Config::load().config;
    if let Some(target) = config.planning_lock.authority.as_deref() {
        let action_owned = if minutes > 0 {
            vec![
                "unlock".to_owned(),
                "--minutes".to_owned(),
                minutes.to_string(),
            ]
        } else {
            vec!["off".to_owned()]
        };
        let args = action_owned.iter().map(String::as_str).collect::<Vec<_>>();
        return match forward_action(&OpenSshRunner, target, &args, &password) {
            Ok(_) => Ok(0),
            Err(error) => {
                eprintln!("{error}");
                Ok(1)
            }
        };
    }
    let path = crate::config::config_dir().join(crate::planning_lock::CONFIG_FILE_NAME);
    let mut lock = PlanningLock::load(&path);
    let now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
    let result = if minutes > 0 {
        lock.unlock(
            std::str::from_utf8(&password).unwrap_or_default(),
            minutes,
            now,
        )
    } else {
        lock.disable(
            std::str::from_utf8(&password).unwrap_or_default(),
            "turn off",
        )
    };
    match result {
        Ok(()) => match lock.persist(&path) {
            Ok(()) => Ok(0),
            Err(error) => {
                eprintln!("{error}");
                Ok(1)
            }
        },
        Err(error) => {
            eprintln!("{error}");
            Ok(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type CapturedCommand = (String, Vec<String>, Vec<u8>);

    struct FakeRunner {
        response: Mutex<Result<Vec<u8>, String>>,
        captured: Mutex<Vec<CapturedCommand>>,
    }

    impl Default for FakeRunner {
        fn default() -> Self {
            Self {
                response: Mutex::new(Ok(Vec::new())),
                captured: Mutex::new(Vec::new()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn execute(&self, target: &str, args: &[&str], input: &[u8]) -> Result<Vec<u8>, String> {
            self.captured.lock().unwrap().push((
                target.into(),
                args.iter().map(|arg| (*arg).into()).collect(),
                input.to_vec(),
            ));
            self.response.lock().unwrap().clone()
        }
    }

    #[test]
    fn follower_applies_authority_state_and_unreachable_poll_keeps_it() {
        let snapshot = Snapshot {
            locked: true,
            discussion_tab_id: "discussion".into(),
            unlock_until_unix_s: None,
        };
        let runner = FakeRunner::default();
        *runner.response.lock().unwrap() = Ok(serde_json::to_vec(&Some(snapshot.clone())).unwrap());
        let mut local = PlanningLock::default();
        local.fail_closed_remote();
        local.apply_remote_snapshot(status(&runner, "ubuntu@ubuntu-direct").unwrap());
        assert_eq!(local.snapshot(10), Some(snapshot.clone()));
        *runner.response.lock().unwrap() = Err("offline".into());
        if let Ok(remote) = status(&runner, "ubuntu@ubuntu-direct") {
            local.apply_remote_snapshot(remote);
        }
        assert_eq!(local.snapshot(10), Some(snapshot));
    }

    #[test]
    fn unlock_password_is_forwarded_only_as_stdin() {
        let runner = FakeRunner::default();
        *runner.response.lock().unwrap() = Ok(serde_json::to_vec(&None::<Snapshot>).unwrap());
        let password = b"a secret planning password";
        forward_action(
            &runner,
            "ubuntu@ubuntu-direct",
            &["unlock", "--minutes", "10"],
            password,
        )
        .unwrap();
        let captured = runner.captured.lock().unwrap();
        assert_eq!(captured[0].2, password);
        assert!(!captured[0].1.iter().any(|arg| arg
            .as_bytes()
            .windows(password.len())
            .any(|part| part == password)));
        assert_eq!(captured[1].1, ["status", "--json"]);
    }

    #[test]
    fn ssh_calls_have_connect_and_server_liveness_timeouts() {
        assert!(SSH_OPTIONS
            .windows(2)
            .any(|option| option == ["-o", "ConnectTimeout=5"]));
        assert!(SSH_OPTIONS
            .windows(2)
            .any(|option| option == ["-o", "ServerAliveInterval=5"]));
        assert!(SSH_OPTIONS
            .windows(2)
            .any(|option| option == ["-o", "ServerAliveCountMax=1"]));
    }

    #[test]
    fn forwarded_action_returns_the_authoritys_updated_snapshot() {
        let runner = FakeRunner::default();
        let snapshot = Snapshot {
            locked: false,
            discussion_tab_id: "authority-tab".into(),
            unlock_until_unix_s: Some(900),
        };
        *runner.response.lock().unwrap() = Ok(serde_json::to_vec(&Some(snapshot.clone())).unwrap());
        assert_eq!(
            forward_action(&runner, "host", &["unlock", "--minutes", "5"], b"password"),
            Ok(Some(snapshot))
        );
        assert_eq!(runner.captured.lock().unwrap().len(), 2);
    }

    #[test]
    fn initial_unreachable_authority_is_locked() {
        let mut local = PlanningLock::default();
        local.fail_closed_remote();
        assert!(local.is_locked(10));
    }
}
