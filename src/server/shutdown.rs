//! Why the server is stopping, recorded where the request arrives and logged
//! once when shutdown begins.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::platform::ServerQuitSignal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShutdownReason {
    Signal(ServerQuitSignal),
    ApiStop { caller: Option<String> },
    HostShutdown,
    Unknown,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(signal) => write!(f, "{signal}"),
            Self::ApiStop {
                caller: Some(caller),
            } => write!(f, "server.stop request from {caller}"),
            Self::ApiStop { caller: None } => f.write_str("server.stop request"),
            Self::HostShutdown => f.write_str("host shutdown"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// The server's stop flag plus the reason recorded by the first stop request.
#[derive(Clone, Default)]
pub(crate) struct ServerStop {
    flag: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    owner: Option<Arc<LifecycleOwner>>,
}

struct LifecycleOwner {
    api_path: std::path::PathBuf,
    epoch: u64,
    retired: AtomicBool,
    stop_guard: Mutex<Option<super::lifecycle::Guard>>,
}

impl ServerStop {
    pub(crate) fn for_epoch(epoch: u64) -> Self {
        Self {
            owner: Some(Arc::new(LifecycleOwner {
                api_path: crate::api::socket_path(),
                epoch,
                retired: AtomicBool::new(false),
                stop_guard: Mutex::new(None),
            })),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_path(api_path: std::path::PathBuf, epoch: u64) -> Self {
        Self {
            owner: Some(Arc::new(LifecycleOwner {
                api_path,
                epoch,
                retired: AtomicBool::new(false),
                stop_guard: Mutex::new(None),
            })),
            ..Self::default()
        }
    }

    pub(crate) fn request_api_stop(&self, caller: Option<String>) -> std::io::Result<()> {
        self.request_api_stop_with_receipt(caller, |guard| guard.commit(true).map(|_| ()))
    }

    fn request_api_stop_with_receipt(
        &self,
        caller: Option<String>,
        persist: impl FnOnce(&mut super::lifecycle::Guard) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        if let Some(owner) = &self.owner {
            let mut held = owner
                .stop_guard
                .lock()
                .map_err(|_| std::io::Error::other("server stop control is unavailable"))?;
            if let Some(guard) = held.as_ref() {
                return guard.confirm_durable().map_err(|error| std::io::Error::other(format!("shutdown was requested, but durable stopped state could not be confirmed: {error}")));
            }
            let mut guard = super::lifecycle::lock(&owner.api_path)?;
            if owner.retired.load(Ordering::Acquire) {
                return Err(std::io::Error::other(
                    "server ownership has transferred; reconnect before stopping",
                ));
            }
            guard.require_epoch(owner.epoch)?;
            let persisted = persist(&mut guard);
            if persisted.is_ok() || guard.receipt.epoch != owner.epoch {
                // Keep Start fenced until pane teardown and persistence finish at process exit.
                *held = Some(guard);
                self.request(ShutdownReason::ApiStop { caller });
            }
            return persisted.map_err(|error| {
                if self.is_requested() && held.is_some() {
                    std::io::Error::other(format!("shutdown was requested, but durable stopped state could not be confirmed: {error}"))
                } else { error }
            });
        } else {
            self.request(ShutdownReason::ApiStop { caller });
        }
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn lock_for_handoff(&self) -> std::io::Result<Option<super::lifecycle::Guard>> {
        let Some(owner) = &self.owner else {
            return Ok(None);
        };
        let guard = super::lifecycle::lock(&owner.api_path)?;
        guard.require_epoch(owner.epoch)?;
        if self.is_requested() || guard.receipt.stopped || owner.retired.load(Ordering::Acquire) {
            return Err(std::io::Error::other("server is stopping"));
        }
        Ok(Some(guard))
    }

    #[cfg(unix)]
    pub(crate) fn retire(&self) {
        if let Some(owner) = &self.owner {
            owner.retired.store(true, Ordering::Release);
        }
    }

    pub(crate) fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self, reason: ShutdownReason) {
        if let Ok(mut recorded) = self.reason.lock() {
            recorded.get_or_insert(reason);
        }
        self.flag.store(true, Ordering::Release);
    }

    pub(crate) fn take_reason(&self) -> Option<ShutdownReason> {
        self.reason.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_owner(check: impl FnOnce(ServerStop, &std::path::Path)) {
        let directory = std::env::temp_dir().join(format!(
            "herdr-lifecycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("server.sock");
        let epoch = super::super::lifecycle::lock(&path)
            .unwrap()
            .commit(false)
            .unwrap();
        let stop = ServerStop::for_test_path(path.clone(), epoch);
        check(stop, &path);
        super::super::lifecycle::remove_test_receipt(&path);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn repeated_stop_retains_shutdown_ownership() {
        with_owner(|stop, path| {
            stop.request_api_stop(None).unwrap();
            let epoch = super::super::lifecycle::read(path).unwrap().epoch;
            stop.request_api_stop(None).unwrap();
            assert_eq!(super::super::lifecycle::read(path).unwrap().epoch, epoch);
            assert!(super::super::lifecycle::lock(path).is_err());
        });
    }

    #[test]
    fn failed_stop_receipt_does_not_acknowledge_or_request_shutdown() {
        with_owner(|stop, path| {
            let temporary = super::super::lifecycle::receipt_path(path)
                .unwrap()
                .with_extension("json.tmp");
            std::fs::create_dir(&temporary).unwrap();
            assert!(stop.request_api_stop(None).is_err());
            assert!(!stop.is_requested());
            assert!(!super::super::lifecycle::read(path).unwrap().stopped);
            std::fs::remove_dir(temporary).unwrap();
        });
    }

    #[test]
    fn stop_reports_uncertain_durability_after_receipt_replacement() {
        with_owner(|stop, path| {
            let error = stop
                .request_api_stop_with_receipt(None, |guard| {
                    guard.commit(true)?;
                    Err(std::io::Error::other("directory sync failed"))
                })
                .unwrap_err();
            assert!(error.to_string().contains("shutdown was requested"));
            assert!(stop.is_requested());
            assert!(super::super::lifecycle::read(path).unwrap().stopped);
            assert!(super::super::lifecycle::lock(path).is_err());
        });
    }

    #[test]
    fn signal_shutdown_does_not_claim_an_api_stop() {
        with_owner(|stop, path| {
            stop.request(ShutdownReason::HostShutdown);
            assert!(!super::super::lifecycle::read(path).unwrap().stopped);
            stop.request_api_stop(None).unwrap();
            assert!(super::super::lifecycle::read(path).unwrap().stopped);
        });
    }

    #[cfg(unix)]
    #[test]
    fn stop_prevents_transfer_and_retired_owner_cannot_stop_replacement() {
        with_owner(|stop, _| {
            stop.request_api_stop(None).unwrap();
            assert!(stop.lock_for_handoff().is_err());
        });
        with_owner(|stop, path| {
            let transfer = stop.lock_for_handoff().unwrap();
            stop.retire();
            drop(transfer);
            assert!(stop.request_api_stop(None).is_err());
            assert!(!super::super::lifecycle::read(path).unwrap().stopped);
        });
    }

    #[test]
    fn first_stop_reason_wins() {
        let stop = ServerStop::default();
        assert!(!stop.is_requested());

        stop.request(ShutdownReason::HostShutdown);
        stop.request(ShutdownReason::ApiStop { caller: None });

        assert!(stop.is_requested());
        assert_eq!(stop.take_reason(), Some(ShutdownReason::HostShutdown));
        assert_eq!(stop.take_reason(), None);
    }

    #[test]
    fn api_stop_reason_names_the_caller() {
        let reason = ShutdownReason::ApiStop {
            caller: Some("pid 42 (herdr), parent pid 7 (zsh)".into()),
        };
        assert_eq!(
            reason.to_string(),
            "server.stop request from pid 42 (herdr), parent pid 7 (zsh)"
        );
    }
}
