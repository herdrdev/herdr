//! SSH reverse-forward lifecycle for remote agent reporting.

use crate::detect::RemoteTransport;
#[cfg(not(unix))]
use crate::layout::PaneId;

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TransportKey {
    dest: String,
    port: Option<String>,
    config_path: Option<String>,
    identity_file: Option<String>,
}

#[cfg(unix)]
impl From<&RemoteTransport> for TransportKey {
    fn from(transport: &RemoteTransport) -> Self {
        Self {
            dest: transport.dest.clone(),
            port: transport.port.clone(),
            config_path: transport.config_path.clone(),
            identity_file: transport.identity_file.clone(),
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::{RemoteTransport, TransportKey};
    use crate::layout::PaneId;
    use std::collections::HashMap;
    use std::io::{self, BufRead, Read};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command as ProcessCommand, Stdio};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    const CLAIM_DEBOUNCE: Duration = Duration::from_secs(1);
    const PREP_TIMEOUT: Duration = Duration::from_secs(15);
    const WORKER_TICK: Duration = Duration::from_secs(1);
    const RETRY_BACKOFF: [Duration; 4] = [
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(15),
        Duration::from_secs(60),
    ];

    trait ChildProcess: Send {
        fn try_wait(&mut self) -> io::Result<Option<bool>>;
        fn kill(&mut self) -> io::Result<()>;
        fn wait(&mut self) -> io::Result<()>;
    }

    trait ProcessRunner: Send {
        fn prepare(&mut self, argv: &[String], timeout: Duration) -> io::Result<String>;
        fn spawn_forward(&mut self, argv: &[String]) -> io::Result<Box<dyn ChildProcess>>;
    }

    struct SystemChild {
        child: Child,
    }

    impl ChildProcess for SystemChild {
        fn try_wait(&mut self) -> io::Result<Option<bool>> {
            self.child
                .try_wait()
                .map(|status| status.map(|value| value.success()))
        }

        fn kill(&mut self) -> io::Result<()> {
            self.child.kill()
        }

        fn wait(&mut self) -> io::Result<()> {
            self.child.wait().map(|_| ())
        }
    }

    struct SystemRunner;

    /// Test runner that never reaches a real ssh: preparation fails
    /// immediately, so no forward child is ever spawned from unit tests.
    #[cfg(test)]
    struct FailingRunner;

    #[cfg(test)]
    impl ProcessRunner for FailingRunner {
        fn prepare(&mut self, _argv: &[String], _timeout: Duration) -> io::Result<String> {
            Err(io::Error::other("test runner never runs ssh preparation"))
        }

        fn spawn_forward(&mut self, _argv: &[String]) -> io::Result<Box<dyn ChildProcess>> {
            Err(io::Error::other("test runner never spawns ssh"))
        }
    }

    impl ProcessRunner for SystemRunner {
        fn prepare(&mut self, argv: &[String], timeout: Duration) -> io::Result<String> {
            let Some(program) = argv.first() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty ssh preparation argv",
                ));
            };
            let mut command = ProcessCommand::new(program);
            command
                .args(argv.iter().skip(1))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn()?;
            if let Some(stderr) = child.stderr.take() {
                drain_stderr(stderr, "ssh preparation");
            }

            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        let mut stdout = child.stdout.take().ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "ssh preparation did not provide stdout",
                            )
                        })?;
                        let mut output = Vec::new();
                        stdout.read_to_end(&mut output)?;
                        if !status.success() {
                            return Err(io::Error::other(format!(
                                "ssh preparation exited with {status}"
                            )));
                        }
                        let home = String::from_utf8_lossy(&output).trim().to_string();
                        if home.is_empty() {
                            return Err(io::Error::other(
                                "ssh preparation returned an empty remote home",
                            ));
                        }
                        return Ok(home);
                    }
                    Ok(None) if started.elapsed() >= timeout => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "ssh preparation timed out",
                        ));
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                    Err(err) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(err);
                    }
                }
            }
        }

        fn spawn_forward(&mut self, argv: &[String]) -> io::Result<Box<dyn ChildProcess>> {
            let Some(program) = argv.first() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "empty ssh forward argv",
                ));
            };
            let mut command = ProcessCommand::new(program);
            command
                .args(argv.iter().skip(1))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            let mut child = command.spawn()?;
            if let Some(stderr) = child.stderr.take() {
                drain_stderr(stderr, "ssh reverse forward");
            }
            Ok(Box::new(SystemChild { child }))
        }
    }

    fn drain_stderr<R>(reader: R, context: &'static str)
    where
        R: Read + Send + 'static,
    {
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(reader);
            for line in reader.lines() {
                match line {
                    Ok(line) => tracing::warn!(context, %line, "ssh process reported stderr"),
                    Err(err) => tracing::debug!(context, %err, "ssh stderr reader stopped"),
                }
            }
        });
    }

    #[derive(Debug)]
    enum WorkerCommand {
        Claim(PaneId, RemoteTransport),
        Release(PaneId),
        Reset,
        Terminate,
    }

    struct ForwardState {
        transport: RemoteTransport,
        pane_count: usize,
        remote_socket_path: Option<String>,
        child: Option<Box<dyn ChildProcess>>,
        next_action: Instant,
        backoff_index: usize,
    }

    struct WorkerState {
        pane_keys: HashMap<PaneId, TransportKey>,
        forwards: HashMap<TransportKey, ForwardState>,
    }

    impl WorkerState {
        fn new() -> Self {
            Self {
                pane_keys: HashMap::new(),
                forwards: HashMap::new(),
            }
        }

        fn claim(
            &mut self,
            pane: PaneId,
            transport: RemoteTransport,
            now: Instant,
        ) -> Option<TransportKey> {
            let key = TransportKey::from(&transport);
            if self.pane_keys.get(&pane) == Some(&key) {
                return None;
            }
            let released = if let Some(previous_key) = self.pane_keys.insert(pane, key.clone()) {
                self.release_key(&previous_key).then_some(previous_key)
            } else {
                None
            };
            let state = self.forwards.entry(key).or_insert_with(|| ForwardState {
                transport,
                pane_count: 0,
                remote_socket_path: None,
                child: None,
                next_action: now + CLAIM_DEBOUNCE,
                backoff_index: 0,
            });
            state.pane_count += 1;
            released
        }

        fn release(&mut self, pane: PaneId) -> Option<TransportKey> {
            let key = self.pane_keys.remove(&pane)?;
            self.release_key(&key).then_some(key)
        }

        fn release_key(&mut self, key: &TransportKey) -> bool {
            let remove = if let Some(state) = self.forwards.get_mut(key) {
                state.pane_count = state.pane_count.saturating_sub(1);
                state.pane_count == 0
            } else {
                false
            };
            if remove {
                if let Some(mut state) = self.forwards.remove(key) {
                    stop_child(&mut state.child);
                }
            }
            remove
        }

        fn reset(&mut self) {
            for state in self.forwards.values_mut() {
                stop_child(&mut state.child);
            }
            self.forwards.clear();
            self.pane_keys.clear();
        }

        fn tick(
            &mut self,
            now: Instant,
            runner: &mut dyn ProcessRunner,
            prepared: &Arc<Mutex<HashMap<TransportKey, String>>>,
            socket_name: &str,
        ) {
            let keys = self.forwards.keys().cloned().collect::<Vec<_>>();
            for key in keys {
                if !self.forwards.contains_key(&key) {
                    continue;
                }
                self.reap_child(&key, now);
                self.start_if_due(&key, now, runner, prepared, socket_name);
            }
        }

        fn reap_child(&mut self, key: &TransportKey, now: Instant) {
            let result = self
                .forwards
                .get_mut(key)
                .and_then(|state| state.child.as_mut().map(|child| child.try_wait()));
            let Some(result) = result else {
                return;
            };
            match result {
                Ok(None) => {}
                Ok(Some(success)) => {
                    tracing::warn!(
                        destination = %key.dest,
                        success,
                        "ssh reverse forward exited"
                    );
                    self.schedule_retry(key, now);
                }
                Err(err) => {
                    tracing::warn!(
                        destination = %key.dest,
                        error = %err,
                        "failed to reap ssh reverse forward"
                    );
                    self.schedule_retry(key, now);
                }
            }
        }

        fn schedule_retry(&mut self, key: &TransportKey, now: Instant) {
            let Some(state) = self.forwards.get_mut(key) else {
                return;
            };
            stop_child(&mut state.child);
            let index = state.backoff_index.min(RETRY_BACKOFF.len() - 1);
            state.next_action = now + RETRY_BACKOFF[index];
            state.backoff_index = (index + 1).min(RETRY_BACKOFF.len() - 1);
        }

        fn start_if_due(
            &mut self,
            key: &TransportKey,
            now: Instant,
            runner: &mut dyn ProcessRunner,
            prepared: &Arc<Mutex<HashMap<TransportKey, String>>>,
            socket_name: &str,
        ) {
            let Some(state) = self.forwards.get(key) else {
                return;
            };
            if state.pane_count == 0 || state.child.is_some() || state.next_action > now {
                return;
            }
            let transport = state.transport.clone();
            let mut remote_socket_path = state.remote_socket_path.clone();

            if remote_socket_path.is_none() {
                let argv = build_prep_argv(&transport, socket_name);
                match runner.prepare(&argv, PREP_TIMEOUT) {
                    Ok(remote_home) => {
                        let path = remote_socket_path_for_home(&remote_home, socket_name);
                        remote_socket_path = Some(path.clone());
                        if let Some(state) = self.forwards.get_mut(key) {
                            state.remote_socket_path = Some(path.clone());
                        }
                        lock(prepared).insert(key.clone(), path);
                    }
                    Err(err) => {
                        tracing::warn!(
                            destination = %transport.dest,
                            error = %err,
                            "ssh reverse-forward preparation failed"
                        );
                        self.schedule_retry(key, now);
                        return;
                    }
                }
            }

            let Some(remote_socket_path) = remote_socket_path else {
                self.schedule_retry(key, now);
                return;
            };
            let argv = build_forward_argv(
                &transport,
                &remote_socket_path,
                &crate::session::remote_report_socket_path(),
            );
            match runner.spawn_forward(&argv) {
                Ok(child) => {
                    if let Some(state) = self.forwards.get_mut(key) {
                        state.child = Some(child);
                        state.next_action = now;
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        destination = %transport.dest,
                        error = %err,
                        "ssh reverse-forward spawn failed"
                    );
                    self.schedule_retry(key, now);
                }
            }
        }
    }

    fn stop_child(child: &mut Option<Box<dyn ChildProcess>>) {
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn worker_loop(
        rx: Receiver<WorkerCommand>,
        prepared: Arc<Mutex<HashMap<TransportKey, String>>>,
        mut runner: Box<dyn ProcessRunner>,
        socket_name: String,
    ) {
        let mut state = WorkerState::new();
        loop {
            match rx.recv_timeout(WORKER_TICK) {
                Ok(WorkerCommand::Claim(pane, transport)) => {
                    if let Some(key) = state.claim(pane, transport, Instant::now()) {
                        lock(&prepared).remove(&key);
                    }
                }
                Ok(WorkerCommand::Release(pane)) => {
                    if let Some(key) = state.release(pane) {
                        lock(&prepared).remove(&key);
                    }
                }
                Ok(WorkerCommand::Reset) => {
                    state.reset();
                    lock(&prepared).clear();
                }
                Ok(WorkerCommand::Terminate) => {
                    state.reset();
                    lock(&prepared).clear();
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    state.reset();
                    lock(&prepared).clear();
                    break;
                }
            }
            state.tick(Instant::now(), runner.as_mut(), &prepared, &socket_name);
        }
    }

    pub(crate) struct RemoteForwardManager {
        control: Mutex<Control>,
        prepared: Arc<Mutex<HashMap<TransportKey, String>>>,
        runner: Mutex<Option<Box<dyn ProcessRunner>>>,
    }

    struct Control {
        command_tx: Option<Sender<WorkerCommand>>,
        worker: Option<JoinHandle<()>>,
    }

    impl RemoteForwardManager {
        pub(crate) fn new() -> Self {
            Self::with_runner(Box::new(SystemRunner))
        }

        #[cfg(test)]
        pub(crate) fn for_test() -> Self {
            Self::with_runner(Box::new(FailingRunner))
        }

        fn with_runner(runner: Box<dyn ProcessRunner>) -> Self {
            Self {
                control: Mutex::new(Control {
                    command_tx: None,
                    worker: None,
                }),
                prepared: Arc::new(Mutex::new(HashMap::new())),
                runner: Mutex::new(Some(runner)),
            }
        }

        fn ensure_worker(&self) -> Option<Sender<WorkerCommand>> {
            let mut control = lock(&self.control);
            if let Some(command_tx) = control.command_tx.as_ref() {
                return Some(command_tx.clone());
            }
            let runner = match lock(&self.runner).take() {
                Some(runner) => runner,
                None => Box::new(SystemRunner),
            };
            let (command_tx, command_rx) = mpsc::channel();
            let prepared = Arc::clone(&self.prepared);
            let socket_name = remote_socket_name();
            let worker =
                std::thread::spawn(move || worker_loop(command_rx, prepared, runner, socket_name));
            control.worker = Some(worker);
            control.command_tx = Some(command_tx.clone());
            Some(command_tx)
        }

        pub(crate) fn claim(&self, pane: PaneId, transport: RemoteTransport) {
            let Some(command_tx) = self.ensure_worker() else {
                return;
            };
            if let Err(err) = command_tx.send(WorkerCommand::Claim(pane, transport)) {
                tracing::debug!(%err, "remote forward worker is unavailable");
            }
        }

        pub(crate) fn release(&self, pane: PaneId) {
            let command_tx = lock(&self.control).command_tx.clone();
            let Some(command_tx) = command_tx else {
                return;
            };
            if let Err(err) = command_tx.send(WorkerCommand::Release(pane)) {
                tracing::debug!(%err, "remote forward worker is unavailable");
            }
        }

        pub(crate) fn remote_socket_path_for(&self, transport: &RemoteTransport) -> Option<String> {
            lock(&self.prepared)
                .get(&TransportKey::from(transport))
                .cloned()
        }

        pub(crate) fn shutdown(&self) {
            lock(&self.prepared).clear();
            let command_tx = lock(&self.control).command_tx.clone();
            if let Some(command_tx) = command_tx {
                if let Err(err) = command_tx.send(WorkerCommand::Reset) {
                    tracing::debug!(%err, "remote forward worker is unavailable");
                }
            }
        }
    }

    impl Drop for RemoteForwardManager {
        fn drop(&mut self) {
            self.shutdown();
            let (command_tx, worker) = {
                let mut control = lock(&self.control);
                (control.command_tx.take(), control.worker.take())
            };
            if let Some(command_tx) = command_tx {
                let _ = command_tx.send(WorkerCommand::Terminate);
            }
            if let Some(worker) = worker {
                if worker.join().is_err() {
                    tracing::warn!("remote forward worker thread panicked");
                }
            }
        }
    }

    fn ssh_option_args(transport: &RemoteTransport) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(port) = &transport.port {
            args.extend(["-p".to_string(), port.clone()]);
        }
        if let Some(config_path) = &transport.config_path {
            args.extend(["-F".to_string(), config_path.clone()]);
        }
        if let Some(identity_file) = &transport.identity_file {
            args.extend(["-i".to_string(), identity_file.clone()]);
        }
        args
    }

    fn build_prep_argv(transport: &RemoteTransport, socket_name: &str) -> Vec<String> {
        let mut argv = vec![
            "ssh".to_string(),
            "-T".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ConnectTimeout=8".to_string(),
            // Do not allow a user's multiplexing config to turn this into a mux slave.
            "-o".to_string(),
            "ControlMaster=no".to_string(),
            "-o".to_string(),
            "ControlPath=none".to_string(),
        ];
        argv.extend(ssh_option_args(transport));
        argv.push(transport.dest.clone());
        argv.push(format!(
            "h=$HOME; mkdir -p \"$h/.herdr\" && rm -f \"$h/.herdr/{socket_name}\" && printf %s \"$h\""
        ));
        argv
    }

    fn build_forward_argv(
        transport: &RemoteTransport,
        remote_socket_path: &str,
        local_socket_path: &Path,
    ) -> Vec<String> {
        let mut argv = vec![
            "ssh".to_string(),
            "-N".to_string(),
            "-T".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=15".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-o".to_string(),
            "ConnectTimeout=8".to_string(),
            // Do not allow a user's multiplexing config to turn this into a mux slave.
            "-o".to_string(),
            "ControlMaster=no".to_string(),
            "-o".to_string(),
            "ControlPath=none".to_string(),
        ];
        argv.extend(ssh_option_args(transport));
        argv.extend([
            "-R".to_string(),
            format!("{}:{}", remote_socket_path, local_socket_path.display()),
            transport.dest.clone(),
        ]);
        argv
    }

    fn remote_socket_path_for_home(remote_home: &str, socket_name: &str) -> String {
        format!("{}/.herdr/{socket_name}", remote_home.trim_end_matches('/'))
    }

    fn absolute_path(path: &Path) -> PathBuf {
        if path.is_absolute() {
            return path.to_path_buf();
        }
        match std::env::current_dir() {
            Ok(current_dir) => current_dir.join(path),
            Err(_) => path.to_path_buf(),
        }
    }

    fn remote_socket_name_for_path(path: &Path) -> String {
        use std::hash::{Hash, Hasher};
        let absolute = absolute_path(path);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        absolute.to_string_lossy().hash(&mut hasher);
        let digest = format!("{:016x}", hasher.finish());
        let hash8 = digest.chars().take(8).collect::<String>();
        format!("agent-report-{hash8}.sock")
    }

    fn remote_socket_name() -> String {
        remote_socket_name_for_path(&crate::api::socket_path())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::VecDeque;
        use std::sync::atomic::{AtomicUsize, Ordering};

        fn transport(
            dest: &str,
            port: Option<&str>,
            config_path: Option<&str>,
            identity_file: Option<&str>,
        ) -> RemoteTransport {
            RemoteTransport {
                dest: dest.to_string(),
                port: port.map(str::to_string),
                config_path: config_path.map(str::to_string),
                identity_file: identity_file.map(str::to_string),
            }
        }

        #[test]
        fn prep_argv_covers_transport_option_combinations() {
            let script = "h=$HOME; mkdir -p \"$h/.herdr\" && rm -f \"$h/.herdr/agent-report-test.sock\" && printf %s \"$h\"";
            let cases = [
                (
                    transport("host", None, None, None),
                    vec![
                        "ssh",
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        "-o",
                        "ControlMaster=no",
                        "-o",
                        "ControlPath=none",
                        "host",
                        script,
                    ],
                ),
                (
                    transport("host", Some("2222"), None, None),
                    vec![
                        "ssh",
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        "-o",
                        "ControlMaster=no",
                        "-o",
                        "ControlPath=none",
                        "-p",
                        "2222",
                        "host",
                        script,
                    ],
                ),
                (
                    transport("host", None, Some("cfg"), None),
                    vec![
                        "ssh",
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        "-o",
                        "ControlMaster=no",
                        "-o",
                        "ControlPath=none",
                        "-F",
                        "cfg",
                        "host",
                        script,
                    ],
                ),
                (
                    transport("host", None, None, Some("id")),
                    vec![
                        "ssh",
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        "-o",
                        "ControlMaster=no",
                        "-o",
                        "ControlPath=none",
                        "-i",
                        "id",
                        "host",
                        script,
                    ],
                ),
                (
                    transport("user@host", Some("2200"), Some("cfg"), Some("id")),
                    vec![
                        "ssh",
                        "-T",
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=8",
                        "-o",
                        "ControlMaster=no",
                        "-o",
                        "ControlPath=none",
                        "-p",
                        "2200",
                        "-F",
                        "cfg",
                        "-i",
                        "id",
                        "user@host",
                        script,
                    ],
                ),
            ];
            for (transport, expected) in cases {
                assert_eq!(
                    build_prep_argv(&transport, "agent-report-test.sock"),
                    expected
                        .iter()
                        .map(|arg| (*arg).to_string())
                        .collect::<Vec<_>>()
                );
            }
        }

        #[test]
        fn forward_argv_uses_the_report_socket_and_all_options() {
            let transport = transport("user@host", Some("2200"), Some("cfg"), Some("id"));
            let argv = build_forward_argv(
                &transport,
                "/home/user/.herdr/agent-report-test.sock",
                Path::new("/tmp/herdr/agent-report.sock"),
            );
            assert_eq!(argv[0], "ssh");
            assert!(argv.windows(2).any(|pair| {
                pair[0] == "-R"
                    && pair[1]
                        == "/home/user/.herdr/agent-report-test.sock:/tmp/herdr/agent-report.sock"
            }));
            assert!(argv
                .windows(2)
                .any(|pair| pair[0] == "-p" && pair[1] == "2200"));
            assert!(argv
                .windows(2)
                .any(|pair| pair[0] == "-F" && pair[1] == "cfg"));
            assert!(argv
                .windows(2)
                .any(|pair| pair[0] == "-i" && pair[1] == "id"));
            assert!(argv
                .windows(2)
                .any(|pair| pair[0] == "-o" && pair[1] == "ControlMaster=no"));
            assert!(argv
                .windows(2)
                .any(|pair| pair[0] == "-o" && pair[1] == "ControlPath=none"));
            assert_eq!(argv.last().map(String::as_str), Some("user@host"));
        }

        #[test]
        fn socket_name_is_deterministic_and_session_specific() {
            let first = remote_socket_name_for_path(Path::new("/tmp/herdr/a/herdr.sock"));
            let second = remote_socket_name_for_path(Path::new("/tmp/herdr/a/herdr.sock"));
            let third = remote_socket_name_for_path(Path::new("/tmp/herdr/b/herdr.sock"));
            assert_eq!(first, second);
            assert_ne!(first, third);
            assert!(first.starts_with("agent-report-"));
            assert!(first.ends_with(".sock"));
        }

        struct StubChild {
            polls: VecDeque<io::Result<Option<bool>>>,
            kills: Arc<AtomicUsize>,
        }

        impl ChildProcess for StubChild {
            fn try_wait(&mut self) -> io::Result<Option<bool>> {
                self.polls.pop_front().unwrap_or(Ok(None))
            }

            fn kill(&mut self) -> io::Result<()> {
                self.kills.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }

            fn wait(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        struct StubRunner {
            preparations: usize,
            forwards: usize,
            kills: Arc<AtomicUsize>,
            child_polls: VecDeque<VecDeque<io::Result<Option<bool>>>>,
        }

        impl ProcessRunner for StubRunner {
            fn prepare(&mut self, _argv: &[String], _timeout: Duration) -> io::Result<String> {
                self.preparations += 1;
                Ok("/home/test".to_string())
            }

            fn spawn_forward(&mut self, _argv: &[String]) -> io::Result<Box<dyn ChildProcess>> {
                self.forwards += 1;
                Ok(Box::new(StubChild {
                    polls: self.child_polls.pop_front().unwrap_or_default(),
                    kills: Arc::clone(&self.kills),
                }))
            }
        }

        fn pane(raw: u32) -> PaneId {
            PaneId::from_raw(raw)
        }

        #[test]
        fn claim_release_refcounts_and_stops_only_on_last_release() {
            let kills = Arc::new(AtomicUsize::new(0));
            let mut runner = StubRunner {
                preparations: 0,
                forwards: 0,
                kills: Arc::clone(&kills),
                child_polls: VecDeque::new(),
            };
            let mut state = WorkerState::new();
            let prepared = Arc::new(Mutex::new(HashMap::new()));
            let transport = transport("host", None, None, None);
            let start = Instant::now();
            state.claim(pane(1), transport.clone(), start);
            state.claim(pane(2), transport, start);
            state.tick(
                start + CLAIM_DEBOUNCE,
                &mut runner,
                &prepared,
                "agent-report-test.sock",
            );
            assert_eq!(runner.preparations, 1);
            assert_eq!(runner.forwards, 1);
            assert_eq!(state.forwards.len(), 1);
            assert_eq!(
                state.forwards.values().next().map(|item| item.pane_count),
                Some(2)
            );

            state.release(pane(1));
            assert_eq!(kills.load(Ordering::Relaxed), 0);
            assert_eq!(
                state.forwards.values().next().map(|item| item.pane_count),
                Some(1)
            );
            state.release(pane(2));
            assert_eq!(kills.load(Ordering::Relaxed), 1);
            assert!(state.forwards.is_empty());
        }

        #[test]
        fn child_exit_uses_increasing_capped_backoff() {
            assert_eq!(RETRY_BACKOFF[0], Duration::from_secs(2));
            assert_eq!(RETRY_BACKOFF[1], Duration::from_secs(5));
            assert_eq!(RETRY_BACKOFF[2], Duration::from_secs(15));
            assert_eq!(RETRY_BACKOFF[3], Duration::from_secs(60));

            let kills = Arc::new(AtomicUsize::new(0));
            let mut runner = StubRunner {
                preparations: 0,
                forwards: 0,
                kills,
                child_polls: VecDeque::from([
                    VecDeque::from([Ok(Some(false))]),
                    VecDeque::from([Ok(Some(false))]),
                ]),
            };
            let mut state = WorkerState::new();
            let prepared = Arc::new(Mutex::new(HashMap::new()));
            let transport = transport("host", None, None, None);
            let key = TransportKey::from(&transport);
            let start = Instant::now();
            state.claim(pane(1), transport, start);

            let first_due = start + CLAIM_DEBOUNCE;
            state.tick(first_due, &mut runner, &prepared, "agent-report-test.sock");
            let after_first_exit = first_due + Duration::from_secs(1);
            state.tick(
                after_first_exit,
                &mut runner,
                &prepared,
                "agent-report-test.sock",
            );
            assert_eq!(
                state.forwards.get(&key).map(|item| item.next_action),
                Some(after_first_exit + RETRY_BACKOFF[0])
            );

            let second_due = after_first_exit + RETRY_BACKOFF[0];
            state.tick(second_due, &mut runner, &prepared, "agent-report-test.sock");
            let after_second_exit = second_due + Duration::from_secs(1);
            state.tick(
                after_second_exit,
                &mut runner,
                &prepared,
                "agent-report-test.sock",
            );
            assert_eq!(
                state.forwards.get(&key).map(|item| item.next_action),
                Some(after_second_exit + RETRY_BACKOFF[1])
            );
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::RemoteForwardManager;

#[cfg(not(unix))]
pub(crate) struct RemoteForwardManager;

#[cfg(not(unix))]
impl RemoteForwardManager {
    pub(crate) fn new() -> Self {
        Self
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::new()
    }

    // The local report endpoint is an AF_UNIX path, which cannot be reached through
    // Windows' named-pipe IPC; remote forwarding is intentionally inert there.
    pub(crate) fn claim(&self, _pane: PaneId, _transport: RemoteTransport) {}

    pub(crate) fn release(&self, _pane: PaneId) {}

    pub(crate) fn remote_socket_path_for(&self, _transport: &RemoteTransport) -> Option<String> {
        None
    }

    pub(crate) fn shutdown(&self) {}
}
