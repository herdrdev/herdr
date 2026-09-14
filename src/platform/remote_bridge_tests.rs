use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_millis(300);

#[test]
fn bridge_child() {
    let Some(path) = std::env::var_os("HERDR_BRIDGE_TEST_SOCKET") else {
        return;
    };
    let stream = crate::ipc::connect_local_stream(&PathBuf::from(path)).unwrap();
    let timeout = (std::env::var_os("HERDR_BRIDGE_TEST_LEGACY").is_none()).then_some(TIMEOUT);
    super::unix_common::forward_remote_bridge_stdio_with_timeout(stream, timeout).unwrap();
}

struct Bridge {
    child: Child,
    stream: UnixStream,
    path: PathBuf,
}

impl Bridge {
    fn start(legacy: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "hbl-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "platform::remote_bridge_tests::bridge_child",
                "--nocapture",
            ])
            .env("HERDR_BRIDGE_TEST_SOCKET", &path)
            .env_remove("HERDR_BRIDGE_TEST_LEGACY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if legacy {
            command.env("HERDR_BRIDGE_TEST_LEGACY", "1");
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = std::fs::remove_file(&path);
                    panic!("bridge did not connect: {error}");
                }
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        Self {
            child,
            stream,
            path,
        }
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "bridge did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(&mut self) -> String {
        drop(self.child.stdin.take());
        let mut input = Vec::new();
        self.stream.read_to_end(&mut input).unwrap();
        self.stream
            .write_all(b"final-output-after-stdin-eof")
            .unwrap();
        self.stream.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(self.wait().success());
        let mut output = String::new();
        self.child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        output
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn bridge_expires_when_silent_or_stdout_is_blocked() {
    for blocked in [false, true] {
        let mut bridge = Bridge::start(false);
        if blocked {
            let buffer = vec![b'x'; 64 * 1024];
            let deadline = Instant::now() + Duration::from_secs(2);
            while bridge.stream.write_all(&buffer).is_ok() {
                assert!(Instant::now() < deadline, "failed to fill bridge stdout");
            }
        }
        assert_eq!(bridge.wait().code(), Some(1));
    }
}

#[test]
fn bridge_preserves_one_way_progress_and_drains_after_stdin_eof() {
    for upload in [false, true] {
        let mut bridge = Bridge::start(false);
        for _ in 0..12 {
            if upload {
                bridge
                    .child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(b"ping")
                    .unwrap();
                bridge.stream.read_exact(&mut [0; 4]).unwrap();
            } else {
                bridge.stream.write_all(b"output").unwrap();
            }
            std::thread::sleep(Duration::from_millis(60));
            assert!(bridge.child.try_wait().unwrap().is_none());
        }
        assert!(bridge.finish().contains("final-output-after-stdin-eof"));
    }
}

#[test]
fn legacy_bridge_has_no_idle_deadline() {
    let mut bridge = Bridge::start(true);
    std::thread::sleep(TIMEOUT * 2);
    assert!(bridge.child.try_wait().unwrap().is_none());
    assert!(bridge.finish().contains("final-output-after-stdin-eof"));
}
