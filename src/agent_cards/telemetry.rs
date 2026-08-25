use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use herdr_agent_watcher::daemon::store::PaneTelemetry;
use herdr_agent_watcher::sidebar::reducer::{apply_line, State};
use herdr_agent_watcher::sidebar::state_stream::{Event, StateStream};

const RETRY_EVERY: Duration = Duration::from_millis(400);
const POLL_EVERY: Duration = Duration::from_millis(100);

enum CacheUpdate {
    Clear,
    Replace(HashMap<String, PaneTelemetry>),
}

pub(crate) struct TelemetryIngest {
    latest: Arc<Mutex<Option<CacheUpdate>>>,
    shutdown: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl TelemetryIngest {
    pub(crate) fn start(socket: PathBuf) -> Self {
        let latest = Arc::new(Mutex::new(Some(CacheUpdate::Clear)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_latest = latest.clone();
        let worker_shutdown = shutdown.clone();
        let worker = std::thread::spawn(move || {
            run(socket, &worker_latest, &worker_shutdown);
        });
        Self {
            latest,
            shutdown,
            worker: Some(worker),
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(true)),
            worker: None,
        }
    }

    pub(crate) fn poll(&self, cache: &mut HashMap<String, PaneTelemetry>) -> bool {
        let Some(update) = self.latest.lock().expect("telemetry cache poisoned").take() else {
            return false;
        };
        match update {
            CacheUpdate::Clear => cache.clear(),
            CacheUpdate::Replace(next) => *cache = next,
        }
        true
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn join(mut self) -> std::thread::Result<()> {
        self.worker.take().map_or(Ok(()), |worker| worker.join())
    }
}

impl Drop for TelemetryIngest {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(latest: &Mutex<Option<CacheUpdate>>, update: CacheUpdate) {
    *latest.lock().expect("telemetry cache poisoned") = Some(update);
}

fn run(socket: PathBuf, latest: &Mutex<Option<CacheUpdate>>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        let stream = match StateStream::start(&socket) {
            Ok(stream) => stream,
            Err(_) => {
                std::thread::sleep(RETRY_EVERY);
                continue;
            }
        };
        let mut state = State::default();
        while !shutdown.load(Ordering::Relaxed) {
            match stream.recv_timeout(POLL_EVERY) {
                Ok(Event::Line(line)) => {
                    if apply_line(&mut state, &line).is_ok() {
                        publish(latest, CacheUpdate::Replace(state.panes.clone()));
                    }
                }
                Ok(Event::Disconnected) => {
                    state = State::default();
                    publish(latest, CacheUpdate::Clear);
                }
                Ok(Event::Reconnected) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        stream.shutdown();
        let _ = stream.join();
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "herdr-agent-cards-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create tempdir");
            Self(path)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("condition not met before timeout");
    }

    fn serve(
        listener: UnixListener,
        line: &'static str,
    ) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Sender<()>) {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept subscriber");
            let mut request = String::new();
            BufReader::new(stream.try_clone().expect("clone stream"))
                .read_line(&mut request)
                .expect("read subscribe");
            assert_eq!(request, "{\"method\":\"subscribe\"}\n");
            stream.write_all(line.as_bytes()).expect("write snapshot");
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        (worker, release_tx)
    }

    #[test]
    fn updates_land_in_the_non_blocking_cache() {
        let dir = TestDir::new();
        let socket = dir.path().join("state.sock");
        let listener = UnixListener::bind(&socket).expect("bind socket");
        let (server, release) = serve(
            listener,
            "{\"version\":2,\"seq\":1,\"panes\":{\"w1:p1\":{\"agent\":\"claude\",\"card_state\":\"running\"}}}\n",
        );
        let ingest = TelemetryIngest::start(socket);
        let mut cache = HashMap::new();
        wait_for(Duration::from_secs(2), || {
            ingest.poll(&mut cache) && cache.contains_key("w1:p1")
        });
        release.send(()).expect("release server");
        ingest.shutdown();
        ingest.join().expect("join ingest");
        server.join().expect("join server");
    }

    #[test]
    fn absent_socket_degrades_to_an_empty_cache() {
        let dir = TestDir::new();
        let ingest = TelemetryIngest::start(dir.path().join("missing.sock"));
        let mut cache = HashMap::from([("stale".to_string(), PaneTelemetry::with_agent("codex"))]);
        assert!(ingest.poll(&mut cache));
        assert!(cache.is_empty());
        ingest.shutdown();
        ingest.join().expect("join ingest");
    }

    #[test]
    fn reconnect_after_restart_replaces_the_cleared_cache() {
        let dir = TestDir::new();
        let socket = dir.path().join("state.sock");
        let first_listener = UnixListener::bind(&socket).expect("bind first socket");
        let (first, release_first) = serve(
            first_listener,
            "{\"version\":2,\"seq\":1,\"panes\":{\"w1:p1\":{\"agent\":\"claude\"}}}\n",
        );
        let ingest = TelemetryIngest::start(socket.clone());
        let mut cache = HashMap::new();
        wait_for(Duration::from_secs(2), || {
            ingest.poll(&mut cache) && cache.contains_key("w1:p1")
        });
        release_first.send(()).expect("release first server");
        first.join().expect("join first server");
        wait_for(Duration::from_secs(2), || {
            ingest.poll(&mut cache) && cache.is_empty()
        });

        std::fs::remove_file(&socket).expect("remove old socket");
        let second_listener = UnixListener::bind(&socket).expect("bind second socket");
        let (second, release_second) = serve(
            second_listener,
            "{\"version\":2,\"seq\":2,\"panes\":{\"w1:p2\":{\"agent\":\"codex\"}}}\n",
        );
        wait_for(Duration::from_secs(3), || {
            ingest.poll(&mut cache) && cache.contains_key("w1:p2")
        });
        assert!(!cache.contains_key("w1:p1"));
        release_second.send(()).expect("release second server");
        ingest.shutdown();
        ingest.join().expect("join ingest");
        second.join().expect("join second server");
    }
}
