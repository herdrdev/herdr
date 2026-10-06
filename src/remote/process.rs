use std::io::{self, Read as _};
use std::process::Output;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(super) fn wait_with_output_timeout(
    mut child: std::process::Child,
    timeout: Duration,
) -> io::Result<Output> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("SSH command stdout was not captured"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("SSH command stderr was not captured"))?;
    let stdout = thread::spawn(move || {
        let mut stdout = stdout;
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr = thread::spawn(move || {
        let mut stderr = stderr;
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let started = Instant::now();
    // After a kill, detach the readers: descendants such as a ProxyCommand can hold the pipes open.
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                drop((stdout, stderr));
                return Err(error);
            }
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            drop((stdout, stderr));
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "noninteractive SSH command timed out",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    };
    let stdout = stdout
        .join()
        .map_err(|_| io::Error::other("SSH stdout reader panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| io::Error::other("SSH stderr reader panicked"))??;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn timeout_kills_the_child() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("exec sleep 10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let error = wait_with_output_timeout(command.spawn().unwrap(), Duration::from_millis(25))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn timeout_does_not_wait_for_descendants_holding_pipes() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 3 & exec sleep 10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = Instant::now();
        let error = wait_with_output_timeout(command.spawn().unwrap(), Duration::from_millis(25))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
    }
}
