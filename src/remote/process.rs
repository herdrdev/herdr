use std::io::{self, Read as _, Write as _};
use std::process::{Child, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "SSH connection attempt cancelled",
    )
}

pub(super) struct OwnedSshChild(pub(super) Child);

impl Drop for OwnedSshChild {
    fn drop(&mut self) {
        // Only this SSH invocation is ours; a multiplex master may serve other sessions.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) struct PipeControl {
    cancelled: Arc<AtomicBool>,
    stopped: AtomicBool,
    finished: AtomicBool,
}

impl PipeControl {
    pub(super) fn new(cancelled: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            cancelled,
            stopped: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        })
    }

    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    pub(super) fn finish_reads(&self) {
        self.finished.store(true, Ordering::Release);
    }

    fn check(&self) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            Err(cancelled())
        } else {
            Ok(())
        }
    }
}

pub(super) struct CancellablePipe {
    pipe: crate::platform::ChildPipe,
    control: Arc<PipeControl>,
}

impl CancellablePipe {
    pub(super) fn new(pipe: crate::platform::ChildPipe, control: &Arc<PipeControl>) -> Self {
        Self {
            pipe,
            control: Arc::clone(control),
        }
    }
}

impl io::Read for CancellablePipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.control.check().is_err() {
                return Ok(0);
            }
            match self.pipe.read(buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // Drain buffered output, then stop even if a descendant kept the pipe.
                    if self.control.finished.load(Ordering::Acquire) {
                        return Ok(0);
                    }
                    self.pipe.wait(false)?;
                }
                result => return result,
            }
        }
    }
}

impl io::Write for CancellablePipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            self.control.check()?;
            if self.control.finished.load(Ordering::Acquire) {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            match self.pipe.write(buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.pipe.wait(true)?,
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn wait_with_output_timeout(
    child: Child,
    input: Option<Vec<u8>>,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Output> {
    wait_with_output(child, input, timeout, cancelled, None)
}

#[cfg(any(unix, test))]
pub(super) fn wait_with_output_limit(
    child: Child,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
    limit: usize,
) -> io::Result<Output> {
    wait_with_output(child, None, timeout, cancelled, Some(limit))
}

fn read_output(mut pipe: CancellablePipe, limit: Option<usize>) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    if let Some(limit) = limit {
        io::Read::by_ref(&mut pipe)
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)?;
        // Drain excess so a full pipe cannot manufacture a command timeout.
        io::copy(&mut pipe, &mut io::sink())?;
    } else {
        pipe.read_to_end(&mut bytes)?;
    }
    Ok(bytes)
}

fn wait_with_output(
    child: Child,
    input: Option<Vec<u8>>,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
    limit: Option<usize>,
) -> io::Result<Output> {
    let mut child = OwnedSshChild(child);
    let control = PipeControl::new(cancelled);
    let stdout = crate::platform::ChildPipe::new_reader(
        child
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("SSH command stdout was not captured"))?,
    )?;
    let stderr = crate::platform::ChildPipe::new_reader(
        child
            .0
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("SSH command stderr was not captured"))?,
    )?;
    let stdin = child
        .0
        .stdin
        .take()
        .map(crate::platform::ChildPipe::new_writer)
        .transpose()?;
    let stdout = CancellablePipe::new(stdout, &control);
    let stderr = CancellablePipe::new(stderr, &control);
    let stdin = stdin.map(|pipe| CancellablePipe::new(pipe, &control));
    thread::scope(|scope| {
        let stdout = scope.spawn(move || read_output(stdout, limit));
        let stderr = scope.spawn(move || read_output(stderr, limit));
        let stdin = scope.spawn(move || match (stdin, input) {
            (Some(mut stdin), Some(input)) => stdin.write_all(&input),
            (None, Some(_)) => Err(io::Error::other("ssh bootstrap stdin missing")),
            _ => Ok(()),
        });
        let deadline = Instant::now() + timeout;
        let mut status = None;
        let result = loop {
            if let Err(error) = control.check() {
                break Err(error);
            }
            if status.is_none() {
                match child.0.try_wait() {
                    Ok(Some(exited)) => {
                        status = Some(exited);
                        control.finish_reads();
                    }
                    Ok(None) => {}
                    Err(error) => break Err(error),
                }
            }
            if let Some(status) = status {
                if stdout.is_finished() && stderr.is_finished() && stdin.is_finished() {
                    break Ok(status);
                }
            }
            if Instant::now() >= deadline {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "noninteractive SSH command timed out",
                ));
            }
            thread::sleep(POLL_INTERVAL);
        };
        if result.is_err() {
            control.stop();
        }
        drop(child);
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("SSH stdout reader panicked"));
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("SSH stderr reader panicked"));
        let stdin = stdin
            .join()
            .map_err(|_| io::Error::other("SSH stdin writer panicked"));
        let status = result?;
        let stdout = stdout??;
        let stderr = stderr??;
        // A rejected command can close stdin early; its SSH error is more useful.
        if status.success() {
            stdin??;
        }
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

#[cfg(test)]
pub(in crate::remote) mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    pub(in crate::remote) struct PipeFixture {
        root: std::path::PathBuf,
        mode: &'static str,
    }

    impl PipeFixture {
        pub(in crate::remote) fn new(mode: &'static str) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "herdr-pipes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root).unwrap();
            Self { root, mode }
        }

        pub(in crate::remote) fn command(&self) -> Command {
            let mut command = fixture_command(self.mode);
            command.env("HERDR_PIPE_FIXTURE_ROOT", &self.root);
            command
        }

        pub(in crate::remote) fn ready(&self) -> u32 {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(pid) = std::fs::read_to_string(self.root.join("pid"))
                    .ok()
                    .and_then(|pid| pid.parse().ok())
                {
                    return pid;
                }
                assert!(Instant::now() < deadline, "pipe fixture did not start");
                thread::sleep(POLL_INTERVAL);
            }
        }
    }

    impl Drop for PipeFixture {
        fn drop(&mut self) {
            let _ = std::fs::write(self.root.join("release"), b"");
            if let Some(pid) = std::fs::read_to_string(self.root.join("pid"))
                .ok()
                .and_then(|pid| pid.parse().ok())
            {
                let deadline = Instant::now() + Duration::from_secs(5);
                while crate::platform::process_exists(pid) && Instant::now() < deadline {
                    thread::sleep(POLL_INTERVAL);
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(in crate::remote) fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "remote::process::tests::pipe_fixture",
                "--nocapture",
            ])
            .env("HERDR_PIPE_FIXTURE", mode);
        crate::platform::configure_background_command(&mut command);
        command
    }

    #[test]
    // This bounded fixture deliberately leaves inherited pipes open after its parent exits.
    #[allow(clippy::zombie_processes)]
    fn pipe_fixture() {
        let Ok(mode) = std::env::var("HERDR_PIPE_FIXTURE") else {
            return;
        };
        let root = std::path::PathBuf::from(std::env::var_os("HERDR_PIPE_FIXTURE_ROOT").unwrap());
        let release = root.join("release");
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !release.exists() && Instant::now() < deadline {
                thread::sleep(POLL_INTERVAL);
            }
            std::process::exit(0);
        });
        if mode == "holder" {
            // Acknowledge only after libtest's startup output, before the parent can close
            // the captured pipes. Otherwise the harness itself can exit on a broken pipe.
            std::fs::write(root.join("pid"), std::process::id().to_string()).unwrap();
        } else {
            fixture_command("holder").spawn().unwrap();
            // Keep the parent alive and the pipe empty until the holder is actually ready.
            while !root.join("pid").exists() {
                thread::sleep(POLL_INTERVAL);
            }
            if mode.starts_with("bridge-") && mode != "bridge-preamble" {
                println!("herdr-remote-output-ready:1");
            }
            if mode == "bridge-output" {
                io::stdout()
                    .write_all(&vec![b'x'; 2 * 1024 * 1024])
                    .unwrap();
            }
            if mode == "output-limit" {
                eprintln!("Permission denied (publickey)");
                io::stdout().write_all(&vec![b'x'; 1024 * 1024]).unwrap();
                io::stderr().write_all(&vec![b'y'; 1024 * 1024]).unwrap();
            }
            println!("final child output");
            eprintln!("Permission denied (publickey)");
        }
        if mode != "exit" && mode != "bridge-final" && mode != "output-limit" {
            thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn diagnostic_capture_drains_flooded_pipes_with_bounded_memory() {
        let fixture = PipeFixture::new("output-limit");
        let child = fixture
            .command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let descendant = fixture.ready();
        let limit = 16 * 1024;
        let output = wait_with_output_limit(
            child,
            Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
            limit,
        )
        .unwrap();
        assert!(
            output.status.success(),
            "capture must drain instead of blocking the child"
        );
        assert_eq!(output.stdout.len(), limit + 1);
        assert_eq!(output.stderr.len(), limit + 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains("Permission denied (publickey)"));
        assert!(crate::platform::process_exists(descendant));
    }

    #[test]
    fn child_exit_drains_output_without_waiting_for_inherited_pipes() {
        let fixture = PipeFixture::new("exit");
        let child = fixture
            .command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let descendant = fixture.ready();
        let output = wait_with_output_timeout(
            child,
            None,
            Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(output.status.success());
        assert!(
            crate::platform::process_exists(descendant),
            "cleanup must not kill a shared descendant"
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("final child output"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Permission denied (publickey)"));
    }

    #[test]
    fn cancellation_and_timeout_reap_child_with_blocked_pipes() {
        for cancel in [false, true] {
            let fixture = PipeFixture::new("wait");
            let child = fixture
                .command()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let descendant = fixture.ready();
            let stopped = Arc::new(AtomicBool::new(false));
            let pid = child.id();
            let cancel_stop = Arc::clone(&stopped);
            let cancel_thread = thread::spawn(move || {
                if cancel {
                    thread::sleep(Duration::from_millis(100));
                    cancel_stop.store(true, Ordering::Release);
                }
            });
            let error = wait_with_output_timeout(
                child,
                Some(vec![b'x'; 1024 * 1024]),
                // Test cancellation independently of the scheduler's ability
                // to run its thread within the timeout branch's 100 ms margin.
                if cancel {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(200)
                },
                stopped,
            )
            .unwrap_err();
            cancel_thread.join().unwrap();
            assert!(
                !crate::platform::process_exists(pid),
                "owned child was not reaped"
            );
            assert_eq!(
                error.kind(),
                if cancel {
                    io::ErrorKind::ConnectionAborted
                } else {
                    io::ErrorKind::TimedOut
                }
            );
            assert!(
                crate::platform::process_exists(descendant),
                "cleanup waited for or killed the independent holder"
            );
        }
    }
}
