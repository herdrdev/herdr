use std::time::{Duration, Instant};

use herdr_agent_watcher::daemon::{DaemonHandle, DaemonOptions};
use tracing::{error, warn};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DISABLE_COMMAND: &str = "herdr plugin disable herdr-agent-watcher";

pub(super) struct EmbeddedWatcher {
    handle: Option<DaemonHandle>,
    next_poll: Instant,
}

impl EmbeddedWatcher {
    pub(super) fn start(enabled: bool) -> Self {
        Self::start_with_options(
            enabled,
            DaemonOptions::new(crate::plugin_paths::plugin_state_dir("herdr-agent-watcher")),
        )
    }

    fn start_with_options(enabled: bool, options: DaemonOptions) -> Self {
        let handle = enabled
            .then(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    herdr_agent_watcher::daemon::start(options)
                }))
            })
            .and_then(|started| match started {
                Ok(handle) => Some(handle),
                Err(_) => {
                    error!("embedded agent watcher panicked during startup; server continuing");
                    None
                }
            });
        Self {
            handle,
            next_poll: Instant::now() + POLL_INTERVAL,
        }
    }

    #[cfg(test)]
    pub(super) fn disabled() -> Self {
        Self::start_with_options(false, DaemonOptions::new(std::env::temp_dir()))
    }

    pub(super) fn poll(&mut self, now: Instant) -> bool {
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + POLL_INTERVAL;
        if !self.handle.as_ref().is_some_and(DaemonHandle::is_finished) {
            return false;
        }
        let result = self.handle.take().map(DaemonHandle::join);
        if let Some(result) = result {
            log_unexpected_exit(result);
            return true;
        }
        false
    }

    fn shutdown(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        handle.shutdown();
        if handle.join().is_err() {
            error!("embedded agent watcher panicked during shutdown; server continuing");
        }
    }
}

impl Drop for EmbeddedWatcher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn log_unexpected_exit(result: std::thread::Result<i32>) {
    match result {
        Ok(code) => warn!(
            code,
            disable_command = DISABLE_COMMAND,
            "embedded agent watcher exited unexpectedly, likely superseded by the standalone plugin; not restarting. Disable the standalone plugin with `{DISABLE_COMMAND}`"
        ),
        Err(_) => error!("embedded agent watcher panicked; server continuing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct FakeHerdr {
        root: std::path::PathBuf,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        previous_socket: Option<std::ffi::OsString>,
        previous_config: Option<std::ffi::OsString>,
    }

    impl FakeHerdr {
        fn start(root: &std::path::Path) -> Self {
            let socket = root.join("herdr.sock");
            let config = root.join("config");
            std::fs::create_dir_all(&config).expect("config directory");
            std::fs::write(config.join("config.toml"), "[daemon]\ninterval_ms = 25\n")
                .expect("daemon config");
            let listener = UnixListener::bind(&socket).expect("fake Herdr socket");
            listener.set_nonblocking(true).expect("nonblocking socket");
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let thread = std::thread::spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let mut request = String::new();
                            if BufReader::new(stream.try_clone().expect("clone stream"))
                                .read_line(&mut request)
                                .is_err()
                            {
                                continue;
                            }
                            let request: serde_json::Value =
                                serde_json::from_str(&request).expect("request JSON");
                            let mut stream = stream;
                            let response = serde_json::json!({
                                "id": request["id"],
                                "result": { "panes": [] },
                            });
                            let _ = writeln!(stream, "{response}");
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            let previous_socket = std::env::var_os("HERDR_SOCKET_PATH");
            let previous_config = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR");
            std::env::set_var("HERDR_SOCKET_PATH", socket);
            std::env::set_var("HERDR_PLUGIN_CONFIG_DIR", config);
            Self {
                root: root.to_path_buf(),
                stop,
                thread: Some(thread),
                previous_socket,
                previous_config,
            }
        }
    }

    impl Drop for FakeHerdr {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
            restore_env("HERDR_SOCKET_PATH", self.previous_socket.take());
            restore_env("HERDR_PLUGIN_CONFIG_DIR", self.previous_config.take());
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for embedded watcher state");
    }

    fn temporary_root(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        std::path::Path::new("/tmp").join(format!("h-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn normal_and_handoff_paths_start_and_stop() {
        let temporary = temporary_root("watcher-paths");
        let _fake = FakeHerdr::start(&temporary);
        for path in ["normal", "handoff"] {
            let options = DaemonOptions::new(temporary.join(path));
            let mut owner = EmbeddedWatcher::start_with_options(true, options.clone());
            wait_for(|| {
                options.control_socket_path().exists()
                    && options.state_socket_path().exists()
                    && owner
                        .handle
                        .as_ref()
                        .is_some_and(|handle| !handle.is_finished())
            });
            owner.shutdown();
            assert!(!options.control_socket_path().exists());
            assert!(!options.state_socket_path().exists());
        }
    }

    #[test]
    fn superseded_daemon_is_observed_once_without_restart() {
        let temporary = temporary_root("watcher-supersede");
        let _fake = FakeHerdr::start(&temporary);
        let options = DaemonOptions::new(temporary.join("state"));
        let mut owner = EmbeddedWatcher::start_with_options(true, options.clone());
        wait_for(|| options.control_socket_path().exists());

        let replacement = herdr_agent_watcher::daemon::start(options);
        wait_for(|| owner.handle.as_ref().is_some_and(DaemonHandle::is_finished));
        assert!(owner.poll(Instant::now() + POLL_INTERVAL));
        assert!(!owner.poll(Instant::now() + POLL_INTERVAL * 2));
        assert!(owner.handle.is_none(), "superseded daemon must not restart");

        replacement.shutdown();
        assert_eq!(replacement.join().expect("replacement daemon"), 0);
    }

    #[test]
    fn daemon_panic_is_logged_without_unwinding_the_caller() {
        let panic_result: std::thread::Result<i32> = Err(Box::new("test panic"));
        log_unexpected_exit(panic_result);
        assert_eq!(2 + 2, 4, "server-side work continues after the panic");
    }
}
