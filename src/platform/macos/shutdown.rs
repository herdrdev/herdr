//! Restart and shutdown detection through loginwindow's notify(3) keys.
//!
//! During a restart or shutdown, loginwindow kills LaunchServices-registered
//! background processes (for example Node-based agents that set
//! `process.title`) right after its point of no return, 15-20 seconds before
//! launchd sends SIGTERM to the server. The server must save its intact session
//! before it applies those exits.
//!
//! loginwindow posts these keys as system notifications. They are undocumented,
//! so a missing key degrades to the previous behavior (save on SIGTERM).

use std::ffi::{c_char, c_int, c_ulong, c_void, CString};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::platform::{HostShutdownIntent, HostShutdownIntentCell};

const LOGINWINDOW_PREFIX: &str = "com.apple.system.loginwindow.";
const NOTIFY_STATUS_OK: u32 = 0;

/// How long a restart or shutdown announcement stays armed without a
/// cancellation. loginwindow can wait indefinitely on an app that blocks the
/// logout (a real software-update restart waited 28 minutes before its point
/// of no return), but it does not always post `logoutcancelled` when the user
/// abandons the restart. Without a limit, a much later plain logout would stop
/// the server, which is meant to outlive logout. A restart held open longer
/// than this falls back to the previous behavior and saves on SIGTERM.
const INTENT_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginwindowEvent {
    /// A restart or shutdown started. The user or an app can still cancel it.
    Intent,
    /// The pending logout, restart or shutdown was cancelled.
    Cancelled,
    /// loginwindow passed its point of no return and starts killing processes.
    NoReturn,
}

const LOGINWINDOW_KEYS: [(&str, LoginwindowEvent); 6] = [
    ("restartinitiated", LoginwindowEvent::Intent),
    ("shutdownInitiated", LoginwindowEvent::Intent),
    ("likelyShutdown", LoginwindowEvent::Intent),
    ("logoutcancelled", LoginwindowEvent::Cancelled),
    ("logoutNoReturn", LoginwindowEvent::NoReturn),
    ("shutdownNoReturn", LoginwindowEvent::NoReturn),
];

/// Applies one loginwindow event to the pending intent and returns true when
/// the server should save and exit now.
///
/// A plain logout also reaches the point of no return, but the server is meant
/// to outlive it, so only a recent restart or shutdown announcement arms the
/// exit. Any point of no return consumes the announcement.
fn observe(intent: &mut Option<HostShutdownIntent>, event: LoginwindowEvent, now_ns: u64) -> bool {
    match event {
        LoginwindowEvent::Intent => {
            *intent = Some(HostShutdownIntent {
                announced_at_ns: now_ns,
            });
            false
        }
        LoginwindowEvent::Cancelled => {
            *intent = None;
            false
        }
        LoginwindowEvent::NoReturn => intent.take().is_some_and(|intent| {
            u128::from(now_ns.saturating_sub(intent.announced_at_ns)) <= INTENT_LIFETIME.as_nanos()
        }),
    }
}

/// Boot-wide monotonic time that keeps counting while the Mac sleeps, so an
/// announcement ages across sleep and compares across server processes.
fn monotonic_now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer. Darwin's CLOCK_MONOTONIC includes sleep.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } != 0 {
        return 0;
    }
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

struct Watcher {
    intent: HostShutdownIntentCell,
    requested: Arc<AtomicBool>,
    wake: Box<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    seen: std::sync::atomic::AtomicUsize,
}

impl Watcher {
    fn handle(&self, event: LoginwindowEvent) {
        tracing::info!(?event, "macos loginwindow notification");
        let mut intent = self.intent.lock();
        let shutdown = observe(&mut intent, event, monotonic_now_ns());
        drop(intent);
        if shutdown {
            tracing::info!("host shutdown requested; preserving session before pane termination");
            self.requested.store(true, Ordering::Release);
            (self.wake)();
        }
        #[cfg(test)]
        self.seen.fetch_add(1, Ordering::Release);
    }
}

pub(crate) fn monitor_host_shutdown(
    requested: Arc<AtomicBool>,
    intent: HostShutdownIntentCell,
    wake: impl Fn() + Send + Sync + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    let watcher = Arc::new(Watcher {
        intent,
        requested,
        wake: Box::new(wake),
        #[cfg(test)]
        seen: Default::default(),
    });
    let registration = match Registration::new(LOGINWINDOW_PREFIX, watcher) {
        Ok(registration) => registration,
        Err(err) => {
            tracing::debug!(err = %err, "host shutdown notification unavailable");
            return None;
        }
    };
    tracing::debug!("host shutdown notification ready");
    // Dropping the monitor aborts this task, which cancels the registration.
    Some(tokio::spawn(async move {
        let _registration = registration;
        std::future::pending::<()>().await;
    }))
}

// libSystem: notify(3), libdispatch and the Blocks runtime.
type DispatchQueue = *mut c_void;

unsafe extern "C" {
    fn notify_register_dispatch(
        name: *const c_char,
        out_token: *mut c_int,
        queue: DispatchQueue,
        handler: *mut c_void,
    ) -> u32;
    fn notify_cancel(token: c_int) -> u32;
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> DispatchQueue;
    fn dispatch_release(object: DispatchQueue);
    static _NSConcreteStackBlock: [*const c_void; 32];
}

const BLOCK_HAS_COPY_DISPOSE: c_int = 1 << 25;

/// A Clang block literal `^(int token)` capturing one strong `Watcher`
/// reference and its event. libnotify copies it to the heap with `Block_copy`
/// (which calls `copy_handler`) and releases it after `notify_cancel`.
#[repr(C)]
struct HandlerBlock {
    isa: *const c_void,
    flags: c_int,
    reserved: c_int,
    invoke: unsafe extern "C" fn(*mut HandlerBlock, c_int),
    descriptor: *const HandlerBlockDescriptor,
    watcher: *const Watcher,
    event: LoginwindowEvent,
}

#[repr(C)]
struct HandlerBlockDescriptor {
    reserved: c_ulong,
    size: c_ulong,
    copy: unsafe extern "C" fn(*mut HandlerBlock, *const HandlerBlock),
    dispose: unsafe extern "C" fn(*const HandlerBlock),
}

static HANDLER_BLOCK_DESCRIPTOR: HandlerBlockDescriptor = HandlerBlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<HandlerBlock>() as c_ulong,
    copy: copy_handler,
    dispose: dispose_handler,
};

// The Blocks runtime updates a block's `flags` (its reference count) while other
// threads may hold it, so these helpers never form a reference to the whole
// block. They read only the immutable captured fields through raw pointers.

unsafe extern "C" fn invoke_handler(block: *mut HandlerBlock, _token: c_int) {
    // SAFETY: libnotify invokes a live copy of our block. Its captured fields are
    // written once before registration and never change; the copy owns a
    // Watcher reference, so the Watcher outlives this call.
    let (watcher, event) = unsafe {
        (
            std::ptr::addr_of!((*block).watcher).read(),
            std::ptr::addr_of!((*block).event).read(),
        )
    };
    let watcher = unsafe { &*watcher };
    // A panic must not unwind into libdispatch.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watcher.handle(event)));
}

unsafe extern "C" fn copy_handler(_dst: *mut HandlerBlock, src: *const HandlerBlock) {
    // SAFETY: the Blocks runtime already copied the bytes; the copy takes its own reference.
    unsafe {
        let watcher = std::ptr::addr_of!((*src).watcher).read();
        Arc::increment_strong_count(watcher);
    }
}

unsafe extern "C" fn dispose_handler(block: *const HandlerBlock) {
    // SAFETY: releases the reference taken in `copy_handler`.
    unsafe {
        let watcher = std::ptr::addr_of!((*block).watcher).read();
        Arc::decrement_strong_count(watcher);
    }
}

/// notify(3) registrations handled one at a time on a shared serial queue.
///
/// Ordering is best effort. libnotify reads its notification port from a
/// single dispatch source and forwards each token with `dispatch_async` to the
/// registration's queue, so the serial queue keeps the order in which this
/// process received notifyd's Mach messages. notifyd itself may defer or
/// coalesce deliveries under backpressure, so that order is not guaranteed to
/// match posting order. loginwindow's announcement and point of no return are
/// normally seconds to minutes apart. File-descriptor registrations give
/// weaker ordering still: libnotify writes those from a global concurrent queue.
struct Registration {
    queue: DispatchQueue,
    tokens: Vec<c_int>,
}

// SAFETY: the queue handle and tokens are process-wide identifiers that libdispatch
// and libnotify allow to be used and released from any thread.
unsafe impl Send for Registration {}

impl Registration {
    fn new(prefix: &str, watcher: Arc<Watcher>) -> io::Result<Self> {
        // SAFETY: a NULL attribute creates a serial queue; the label is NUL-terminated.
        let queue =
            unsafe { dispatch_queue_create(c"herdr.host-shutdown".as_ptr(), std::ptr::null()) };
        if queue.is_null() {
            return Err(io::Error::other("dispatch_queue_create failed"));
        }
        let mut registration = Self {
            queue,
            tokens: Vec::with_capacity(LOGINWINDOW_KEYS.len()),
        };
        for (key, event) in LOGINWINDOW_KEYS {
            let name = CString::new(format!("{prefix}{key}"))
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
            let mut block = HandlerBlock {
                isa: std::ptr::addr_of!(_NSConcreteStackBlock).cast(),
                flags: BLOCK_HAS_COPY_DISPOSE,
                reserved: 0,
                invoke: invoke_handler,
                descriptor: &HANDLER_BLOCK_DESCRIPTOR,
                watcher: Arc::as_ptr(&watcher),
                event,
            };
            let mut token = -1;
            // SAFETY: `name` is NUL-terminated, `token` is a valid out-pointer, and
            // libnotify copies `block` (taking its own Watcher reference) before returning.
            let status = unsafe {
                notify_register_dispatch(
                    name.as_ptr(),
                    &mut token,
                    queue,
                    std::ptr::addr_of_mut!(block).cast(),
                )
            };
            if status != NOTIFY_STATUS_OK {
                return Err(io::Error::other(format!(
                    "notify_register_dispatch({key}) failed with status {status}"
                )));
            }
            registration.tokens.push(token);
        }
        Ok(registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        for token in &self.tokens {
            // SAFETY: each token came from a successful registration and is cancelled once.
            // libnotify releases its block copy, and the Watcher with it, on the queue.
            unsafe {
                notify_cancel(*token);
            }
        }
        // SAFETY: releases the reference from dispatch_queue_create; registrations
        // hold their own until libnotify is done with them.
        unsafe { dispatch_release(self.queue) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    use LoginwindowEvent::{Cancelled, Intent, NoReturn};

    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
    }

    const MINUTE_NS: u64 = 60 * 1_000_000_000;

    #[test]
    fn point_of_no_return_requires_restart_or_shutdown_intent() {
        let mut intent = None;
        assert!(
            !observe(&mut intent, NoReturn, 0),
            "plain logout keeps the server"
        );

        assert!(!observe(&mut intent, Intent, 0));
        assert!(observe(&mut intent, NoReturn, 28 * MINUTE_NS));
    }

    #[test]
    fn cancelled_logout_disarms_until_the_next_intent() {
        let mut intent = None;
        assert!(!observe(&mut intent, Intent, 0));
        assert!(!observe(&mut intent, Cancelled, 1));
        assert!(!observe(&mut intent, NoReturn, 2));
        assert!(!observe(&mut intent, Intent, 3));
        assert!(observe(&mut intent, NoReturn, 4));
    }

    #[test]
    fn abandoned_intent_expires_before_a_later_logout() {
        let lifetime = INTENT_LIFETIME.as_nanos() as u64;
        let mut intent = None;
        assert!(!observe(&mut intent, Intent, 1_000));
        assert!(
            !observe(&mut intent, NoReturn, 1_000 + lifetime + 1),
            "a logout after the restart was abandoned keeps the server"
        );

        assert!(!observe(&mut intent, Intent, 5_000));
        assert!(observe(&mut intent, NoReturn, 5_000 + lifetime));
    }

    #[test]
    fn point_of_no_return_consumes_the_intent() {
        let mut intent = None;
        assert!(!observe(&mut intent, Intent, 0));
        assert!(observe(&mut intent, NoReturn, 1));
        assert!(!observe(&mut intent, NoReturn, 2));
    }

    #[test]
    fn registers_loginwindow_keys() {
        let key = |name| {
            LOGINWINDOW_KEYS
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, event)| *event)
        };
        assert_eq!(LOGINWINDOW_PREFIX, "com.apple.system.loginwindow.");
        for intent in ["restartinitiated", "shutdownInitiated", "likelyShutdown"] {
            assert_eq!(key(intent), Some(Intent), "{intent}");
        }
        assert_eq!(key("logoutcancelled"), Some(Cancelled));
        assert_eq!(key("logoutNoReturn"), Some(NoReturn));
        assert_eq!(key("shutdownNoReturn"), Some(NoReturn));
        assert_eq!(key("logoutInitiated"), None, "plain logout must not arm");
    }

    // Unprivileged processes cannot post `com.apple.system.*`, so these tests
    // use a private prefix with the same suffixes and never touch the real keys.
    struct Harness {
        prefix: String,
        watcher: Arc<Watcher>,
        _registration: Registration,
        posted: usize,
    }

    impl Harness {
        fn new(case: &str, intent: HostShutdownIntentCell) -> Self {
            let prefix = format!("dev.herdr.test.{}.{case}.", std::process::id());
            let watcher = Arc::new(Watcher {
                intent,
                requested: Arc::new(AtomicBool::new(false)),
                wake: Box::new(|| {}),
                seen: AtomicUsize::new(0),
            });
            let registration = Registration::new(&prefix, watcher.clone()).expect("register");
            Self {
                prefix,
                watcher,
                _registration: registration,
                posted: 0,
            }
        }

        /// Posts each key and waits for it to be handled before the next one.
        /// notifyd may reorder deliveries under load, so ordering semantics are
        /// tested on `Watcher::handle` directly.
        fn post(&mut self, keys: &[&str]) {
            for key in keys {
                self.post_unregistered(key);
                self.posted += 1;
                let deadline = Instant::now() + Duration::from_secs(5);
                while self.watcher.seen.load(Ordering::Acquire) < self.posted {
                    assert!(Instant::now() < deadline, "{key} was not delivered");
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }

        fn post_unregistered(&self, key: &str) {
            let name = CString::new(format!("{}{key}", self.prefix)).unwrap();
            assert_eq!(unsafe { notify_post(name.as_ptr()) }, NOTIFY_STATUS_OK);
        }

        fn requested(&self) -> bool {
            self.watcher.requested.swap(false, Ordering::AcqRel)
        }
    }

    #[test]
    fn watcher_applies_events_in_handling_order() {
        let woken = Arc::new(AtomicUsize::new(0));
        let watcher = Watcher {
            intent: HostShutdownIntentCell::default(),
            requested: Arc::new(AtomicBool::new(false)),
            wake: Box::new({
                let woken = woken.clone();
                move || {
                    woken.fetch_add(1, Ordering::AcqRel);
                }
            }),
            seen: AtomicUsize::new(0),
        };
        let requested = || watcher.requested.swap(false, Ordering::AcqRel);
        for round in 0..3 {
            for event in [Intent, Cancelled, NoReturn] {
                watcher.handle(event);
            }
            assert!(
                !requested(),
                "round {round}: cancel came last before no-return"
            );

            for event in [Intent, NoReturn] {
                watcher.handle(event);
            }
            assert!(requested(), "round {round}: intent preceded no-return");

            watcher.handle(NoReturn);
            assert!(!requested(), "round {round}: no-return consumed the intent");
        }
        assert_eq!(woken.load(Ordering::Acquire), 3);
    }

    #[test]
    fn live_notifications_drive_the_watcher() {
        let mut harness = Harness::new("live", HostShutdownIntentCell::default());
        harness.post(&["restartinitiated", "logoutcancelled", "logoutNoReturn"]);
        assert!(!harness.requested(), "cancelled restart keeps the server");

        harness.post(&["shutdownInitiated", "shutdownNoReturn"]);
        assert!(
            harness.requested(),
            "shutdown reached its point of no return"
        );

        harness.post(&["likelyShutdown", "logoutNoReturn"]);
        assert!(
            harness.requested(),
            "every intent key arms and every no-return key fires"
        );
    }

    #[test]
    fn plain_logout_keeps_server() {
        let mut harness = Harness::new("logout", HostShutdownIntentCell::default());
        // `logoutInitiated` is not registered, so it is never delivered.
        harness.post_unregistered("logoutInitiated");
        harness.post(&["logoutNoReturn", "shutdownNoReturn"]);
        assert!(!harness.requested());
    }

    #[test]
    fn replacement_server_inherits_an_announced_restart() {
        let old_intent = HostShutdownIntentCell::default();
        let mut old = Harness::new("handoff-old", old_intent.clone());
        old.post(&["restartinitiated"]);
        assert!(!old.requested());
        let carried = old_intent.get().expect("restart announced");
        drop(old);

        // The handoff manifest carries the intent as JSON to the new process.
        let carried: HostShutdownIntent =
            serde_json::from_str(&serde_json::to_string(&carried).unwrap()).unwrap();
        let new_intent = HostShutdownIntentCell::default();
        new_intent.set(Some(carried));
        let mut new = Harness::new("handoff-new", new_intent);
        new.post(&["logoutNoReturn"]);
        assert!(new.requested());

        let mut unarmed = Harness::new("handoff-unarmed", HostShutdownIntentCell::default());
        unarmed.post(&["logoutNoReturn"]);
        assert!(!unarmed.requested());
    }

    #[tokio::test]
    async fn monitor_task_releases_its_watcher_when_aborted() {
        let requested = Arc::new(AtomicBool::new(false));
        let task = monitor_host_shutdown(requested.clone(), Default::default(), || {})
            .expect("notify registration");
        task.abort();
        let _ = task.await;
        let deadline = Instant::now() + Duration::from_secs(5);
        // The registration and every block copy release their references.
        while Arc::strong_count(&requested) > 1 {
            assert!(Instant::now() < deadline, "watcher was not released");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
