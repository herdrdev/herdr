use std::io::{self, Read, Write};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

fn now() -> io::Result<u64> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // This clock includes suspend, so short maintenance wakes can reap old bridges.
    if unsafe { libc::clock_gettime(super::REMOTE_BRIDGE_CLOCK, &mut time) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((time.tv_sec as u64) * 1_000_000_000 + time.tv_nsec as u64)
}

#[derive(Clone)]
pub(super) struct Activity {
    last: Arc<AtomicU64>,
    _stop: mpsc::Sender<()>,
}

impl Activity {
    pub(super) fn start(timeout: Duration) -> io::Result<Self> {
        let last = Arc::new(AtomicU64::new(now()?));
        let watched = Arc::clone(&last);
        let (stop, stopped) = mpsc::channel();
        std::thread::Builder::new()
            .name("ssh-bridge-liveness".into())
            .spawn(move || {
                loop {
                    match stopped.recv_timeout(timeout.min(Duration::from_secs(1))) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let expired = now().map_or(true, |now| idle_expired(&watched, now, timeout));
                    if expired {
                        // Only the dedicated bridge process owns this watchdog. Returning from a
                        // blocked copy cannot guarantee shutdown, and joining it could hang forever.
                        std::process::exit(1);
                    }
                }
            })?;
        Ok(Self { last, _stop: stop })
    }

    fn record(&self) -> io::Result<()> {
        self.last.fetch_max(now()?, Ordering::Relaxed);
        Ok(())
    }
}

fn idle_expired(last: &AtomicU64, now: u64, timeout: Duration) -> bool {
    Duration::from_nanos(now.saturating_sub(last.load(Ordering::Relaxed))) >= timeout
}

pub(super) struct TrackedIo<T> {
    inner: T,
    activity: Option<Activity>,
}

impl<T> TrackedIo<T> {
    pub(super) fn new(inner: T, activity: Option<Activity>) -> Self {
        Self { inner, activity }
    }

    fn progressed(&self, count: usize) -> io::Result<()> {
        if count > 0 {
            if let Some(activity) = &self.activity {
                activity.record()?;
            }
        }
        Ok(())
    }
}

impl<T: Read> Read for TrackedIo<T> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buffer)?;
        self.progressed(count)?;
        Ok(count)
    }
}

impl<T: Write> Write for TrackedIo<T> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.progressed(count)?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_deadline_counts_elapsed_sleep_and_only_positive_io_renews_it() {
        let (stop, _stopped) = mpsc::channel();
        let last = Arc::new(AtomicU64::new(0));
        let activity = Activity {
            last: Arc::clone(&last),
            _stop: stop,
        };
        assert!(idle_expired(
            &last,
            IDLE_TIMEOUT.as_nanos() as u64,
            IDLE_TIMEOUT
        ));
        let mut reader = TrackedIo::new(&b"output"[..], Some(activity.clone()));
        reader.read_exact(&mut [0; 6]).unwrap();
        let after_read = last.load(Ordering::Relaxed);
        assert!(after_read > 0);
        assert!(!idle_expired(&last, after_read, IDLE_TIMEOUT));
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(last.load(Ordering::Relaxed), after_read);
        let mut writer = TrackedIo::new(Vec::new(), Some(activity));
        writer.write_all(b"ping").unwrap();
        assert!(last.load(Ordering::Relaxed) >= after_read);
        assert!(idle_expired(
            &last,
            last.load(Ordering::Relaxed) + 120_000_000_000,
            IDLE_TIMEOUT
        ));
    }
}
