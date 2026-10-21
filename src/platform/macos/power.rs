use std::ffi::{c_char, c_void};
use std::io;

use tokio::sync::mpsc::UnboundedSender;

use crate::platform::HostPowerEvent;

const CAN_SLEEP: u32 = 0xe0000270;
const WILL_SLEEP: u32 = 0xe0000280;
const HAS_POWERED_ON: u32 = 0xe0000300;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        context: *mut c_void,
        port: *mut *mut c_void,
        callback: unsafe extern "C" fn(*mut c_void, u32, u32, *mut c_void),
        notifier: *mut u32,
    ) -> u32;
    fn IODeregisterForSystemPower(notifier: *mut u32) -> i32;
    fn IONotificationPortSetDispatchQueue(port: *mut c_void, queue: *mut c_void);
    fn IONotificationPortDestroy(port: *mut c_void);
    fn IOServiceClose(connection: u32) -> i32;
    fn IOAllowPowerChange(connection: u32, notification: isize) -> i32;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_release(object: *mut c_void);
}

pub(crate) fn monitor_host_power(
    sender: UnboundedSender<HostPowerEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
    let registration = match Registration::new(sender) {
        Ok(registration) => registration,
        Err(error) => {
            tracing::debug!(%error, "host power notifications unavailable");
            return None;
        }
    };
    Some(tokio::spawn(async move {
        let _registration = registration;
        std::future::pending::<()>().await;
    }))
}

struct Context {
    sender: UnboundedSender<HostPowerEvent>,
    connection: u32,
}

struct Registration {
    context: Box<Context>,
    port: *mut c_void,
    notifier: u32,
    queue: *mut c_void,
}

// IOKit handles and dispatch queues support teardown from another thread.
unsafe impl Send for Registration {}

impl Registration {
    fn new(sender: UnboundedSender<HostPowerEvent>) -> io::Result<Self> {
        let mut registration = Self {
            context: Box::new(Context {
                sender,
                connection: 0,
            }),
            port: std::ptr::null_mut(),
            notifier: 0,
            queue: std::ptr::null_mut(),
        };
        // No callback is delivered until the notification port is scheduled below.
        registration.context.connection = unsafe {
            IORegisterForSystemPower(
                std::ptr::from_mut(registration.context.as_mut()).cast(),
                &mut registration.port,
                notify,
                &mut registration.notifier,
            )
        };
        if registration.context.connection == 0 {
            return Err(io::Error::other("IORegisterForSystemPower failed"));
        }
        registration.queue =
            unsafe { dispatch_queue_create(c"herdr.host-power".as_ptr(), std::ptr::null()) };
        if registration.queue.is_null() {
            return Err(io::Error::other("dispatch_queue_create failed"));
        }
        unsafe { IONotificationPortSetDispatchQueue(registration.port, registration.queue) };
        Ok(registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Stop native deliveries, then drain callbacks before releasing their context.
        unsafe {
            if self.notifier != 0 {
                IODeregisterForSystemPower(&mut self.notifier);
            }
            if !self.port.is_null() {
                IONotificationPortSetDispatchQueue(self.port, std::ptr::null_mut());
                IONotificationPortDestroy(self.port);
            }
            if !self.queue.is_null() {
                dispatch_sync_f(self.queue, std::ptr::null_mut(), drained);
                dispatch_release(self.queue);
            }
            if self.context.connection != 0 {
                IOServiceClose(self.context.connection);
            }
        }
    }
}

unsafe extern "C" fn drained(_context: *mut c_void) {}

unsafe extern "C" fn notify(
    context: *mut c_void,
    _service: u32,
    message: u32,
    argument: *mut c_void,
) {
    // The boxed context outlives this serial dispatch queue's callbacks.
    let context = unsafe { &*context.cast::<Context>() };
    if matches!(message, CAN_SLEEP | WILL_SLEEP) {
        // Never delay or inhibit the host's sleep for connection recovery.
        unsafe { IOAllowPowerChange(context.connection, argument as isize) };
    }
    let event = match message {
        WILL_SLEEP => HostPowerEvent::Suspend,
        // Legacy application notifications follow full wake, including dark-wake promotion.
        HAS_POWERED_ON => HostPowerEvent::InteractiveResume,
        _ => return,
    };
    let _ = context.sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_wake_callback_delivers_and_registration_releases_receiver() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut registration = Registration::new(sender).unwrap();
        unsafe {
            notify(
                std::ptr::from_mut(registration.context.as_mut()).cast(),
                0,
                HAS_POWERED_ON,
                std::ptr::null_mut(),
            );
        }
        assert_eq!(
            receiver.try_recv().unwrap(),
            HostPowerEvent::InteractiveResume
        );
        drop(registration);
        assert!(receiver.is_closed());
    }
}
