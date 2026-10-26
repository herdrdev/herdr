use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// Allow slow desktop startup, but never let a clipboard operation wedge the client.
pub(super) const TIMEOUT: Duration = Duration::from_secs(5);

struct Helper {
    child: Child,
    completed: bool,
}

impl Drop for Helper {
    fn drop(&mut self) {
        if !self.completed {
            // Wrappers may leave descendants holding the pipes after their parent exits.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn run(
    mut command: Command,
    input: Option<&[u8]>,
    max_bytes: usize,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let child = command
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(if max_bytes > 0 {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .spawn()?;
    let mut helper = Helper {
        child,
        completed: false,
    };
    let mut stdin = helper.child.stdin.take();
    let mut stdout = helper.child.stdout.take();
    if let Some(pipe) = &stdin {
        nonblocking(pipe.as_raw_fd())?;
    }
    if let Some(pipe) = &stdout {
        nonblocking(pipe.as_raw_fd())?;
    }
    let input = input.unwrap_or_default();
    let mut written = 0;
    let mut bytes = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut progress = false;
        if written == input.len() {
            stdin = None;
        }
        if let Some(pipe) = stdin.as_mut() {
            match pipe.write(&input[written..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => {
                    written += count;
                    progress = true;
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
        if let Some(pipe) = stdout.as_mut() {
            let mut buffer = [0; 8192];
            let limit = buffer
                .len()
                .min(max_bytes.saturating_sub(bytes.len()).saturating_add(1));
            match pipe.read(&mut buffer[..limit]) {
                Ok(0) => stdout = None,
                Ok(count) => {
                    if count > max_bytes - bytes.len() {
                        return Err(io::ErrorKind::FileTooLarge.into());
                    }
                    bytes.extend_from_slice(&buffer[..count]);
                    progress = true;
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(err) => return Err(err),
            }
        }
        if let Some(status) = helper.child.try_wait()? {
            if !status.success() {
                return Err(io::Error::other("clipboard helper failed"));
            }
            if written == input.len() && stdout.is_none() {
                helper.completed = true;
                return Ok(bytes);
            }
        }
        if !progress {
            std::thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}
