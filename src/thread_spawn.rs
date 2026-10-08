//! Thread spawning that reports OS failures instead of panicking.
//!
//! `std::thread::spawn` panics when the OS refuses a new thread (for example
//! EAGAIN at a process or cgroup task limit). In the server that panic either
//! exits the whole process or silently kills a long-lived loop, so server code
//! spawns through these helpers and degrades per call site.

use std::io;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub(crate) fn spawn_named<F, T>(name: &str, f: F) -> io::Result<JoinHandle<T>>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    #[cfg(test)]
    if test_hook::take_injected_failure() {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "injected thread spawn failure",
        ));
    }
    std::thread::Builder::new().name(name.to_owned()).spawn(f)
}

/// Runs `work(payload)` on a new thread. If the thread cannot start, the
/// payload comes back so the caller can still finish the operation it was
/// meant to complete.
pub(crate) fn spawn_named_with<P, F>(name: &str, payload: P, work: F) -> Result<(), (P, io::Error)>
where
    P: Send + 'static,
    F: FnOnce(P) + Send + 'static,
{
    let slot = Arc::new(Mutex::new(Some(payload)));
    let worker_slot = Arc::clone(&slot);
    let spawned = spawn_named(name, move || {
        if let Some(payload) = take_payload(&worker_slot) {
            work(payload);
        }
    });
    match spawned {
        Ok(_) => Ok(()),
        // A failed spawn never runs the closure, so the payload is still here.
        Err(err) => take_payload(&slot).map_or(Ok(()), |payload| Err((payload, err))),
    }
}

fn take_payload<P>(slot: &Mutex<Option<P>>) -> Option<P> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

#[cfg(test)]
pub(crate) mod test_hook {
    use std::cell::Cell;

    thread_local! {
        static INJECTED_FAILURES: Cell<usize> = const { Cell::new(0) };
        static ALLOWED_BEFORE_FAILURES: Cell<usize> = const { Cell::new(0) };
    }

    /// Makes the next `count` spawns from the current thread fail.
    pub(crate) fn fail_next_spawns(count: usize) {
        fail_spawns_after(0, count);
    }

    /// Lets `allowed` spawns from the current thread succeed, then fails `count`.
    pub(crate) fn fail_spawns_after(allowed: usize, count: usize) {
        ALLOWED_BEFORE_FAILURES.with(|cell| cell.set(allowed));
        INJECTED_FAILURES.with(|failures| failures.set(count));
    }

    pub(super) fn take_injected_failure() -> bool {
        let remaining = INJECTED_FAILURES.with(Cell::get);
        if remaining == 0 {
            return false;
        }
        let allowed = ALLOWED_BEFORE_FAILURES.with(Cell::get);
        if allowed > 0 {
            ALLOWED_BEFORE_FAILURES.with(|cell| cell.set(allowed - 1));
            return false;
        }
        INJECTED_FAILURES.with(|failures| failures.set(remaining - 1));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_failure_returns_payload() {
        test_hook::fail_next_spawns(1);
        let result = spawn_named_with("test-payload", 7_u32, |_| {});
        let (payload, err) = result.expect_err("spawn should fail");
        assert_eq!(payload, 7);
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        let (tx, rx) = std::sync::mpsc::channel();
        spawn_named_with("test-payload", 8_u32, move |payload| {
            tx.send(payload).unwrap();
        })
        .expect("spawn should succeed after injected failure");
        assert_eq!(rx.recv().unwrap(), 8);
    }
}
