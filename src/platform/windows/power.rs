use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    LazyLock, Mutex,
};

use tokio::sync::mpsc::UnboundedSender;
use windows_sys::Win32::System::Power::{
    PowerRegisterSuspendResumeNotification, PowerUnregisterSuspendResumeNotification,
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
};

use crate::platform::HostPowerEvent;

static SENDERS: LazyLock<Mutex<HashMap<usize, UnboundedSender<HostPowerEvent>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

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

struct Registration {
    handle: *mut c_void,
    id: usize,
}

// The registration handle can be cancelled from any thread; the callback only sends events.
unsafe impl Send for Registration {}

impl Registration {
    fn new(sender: UnboundedSender<HostPowerEvent>) -> io::Result<Self> {
        let mut registration = Self {
            handle: std::ptr::null_mut(),
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
        };
        SENDERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(registration.id, sender);
        let parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(notify),
            Context: std::ptr::without_provenance_mut(registration.id),
        };
        // The context is an opaque ID, never a dereferenced pointer.
        let error = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                std::ptr::from_ref(&parameters).cast_mut().cast(),
                &mut registration.handle,
            )
        };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        Ok(registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Cancellation does not document callback quiescence; late callbacks find no sender.
        SENDERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
        if !self.handle.is_null() {
            let error = unsafe { PowerUnregisterSuspendResumeNotification(self.handle as isize) };
            if error != 0 {
                tracing::debug!(error, "host power notification unregistration failed");
            }
        }
    }
}

unsafe extern "system" fn notify(
    context: *const c_void,
    kind: u32,
    _setting: *const c_void,
) -> u32 {
    let event = match kind {
        PBT_APMSUSPEND => HostPowerEvent::Suspend,
        PBT_APMRESUMEAUTOMATIC => HostPowerEvent::AutomaticResume,
        PBT_APMRESUMESUSPEND => HostPowerEvent::InteractiveResume,
        _ => return 0,
    };
    let sender = SENDERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&(context as usize))
        .cloned();
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_registration_distinguishes_user_wake_and_releases_receiver() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let registration = Registration::new(sender).unwrap();
        let context = std::ptr::without_provenance(registration.id);
        // Invoke our callback only; never broadcast a power event or suspend the host.
        for kind in [PBT_APMSUSPEND, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND] {
            unsafe { notify(context, kind, std::ptr::null()) };
        }
        assert_eq!(receiver.try_recv().unwrap(), HostPowerEvent::Suspend);
        assert_eq!(
            receiver.try_recv().unwrap(),
            HostPowerEvent::AutomaticResume
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            HostPowerEvent::InteractiveResume
        );
        drop(registration);
        unsafe { notify(context, PBT_APMRESUMESUSPEND, std::ptr::null()) };
        assert!(receiver.is_closed());
    }
}
