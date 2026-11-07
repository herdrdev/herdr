//! Serialize durable endpoint selection writes without blocking navigation.

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::endpoint::{EndpointCatalog, ProfileId};

#[derive(Default)]
struct Pending {
    catalog: Option<EndpointCatalog>,
    shutdown: bool,
}

type Shared = Arc<(Mutex<Pending>, Condvar)>;

pub(super) struct SelectionWriter {
    shared: Shared,
    worker: Option<JoinHandle<()>>,
    last_selection: Option<ProfileId>,
}

impl SelectionWriter {
    pub(super) fn new(catalog: &EndpointCatalog) -> Self {
        Self {
            shared: Arc::new((Mutex::new(Pending::default()), Condvar::new())),
            worker: None,
            last_selection: catalog.selected_profile.clone(),
        }
    }

    pub(super) fn schedule(&mut self, catalog: &EndpointCatalog) {
        // Workspace/tab navigation on the same machine must not touch the disk.
        if self.last_selection == catalog.selected_profile {
            return;
        }
        if self.worker.is_none() {
            match start_worker(self.shared.clone(), EndpointCatalog::store_selection) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => {
                    tracing::warn!(%error, "failed to start endpoint selection writer");
                    return;
                }
            }
        }
        let (pending, wake) = &*self.shared;
        let mut pending = pending.lock().unwrap_or_else(|error| error.into_inner());
        // Bound the queue to the latest selection while an older write is in flight.
        pending.catalog = Some(catalog.clone());
        self.last_selection = catalog.selected_profile.clone();
        wake.notify_one();
    }
}

impl Drop for SelectionWriter {
    fn drop(&mut self) {
        let (pending, wake) = &*self.shared;
        pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown = true;
        wake.notify_one();
        // Drain the last requested selection on exit, including error returns.
        // Only shutdown waits for disk I/O; navigation never joins the writer.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn start_worker(
    shared: Shared,
    mut store: impl FnMut(&EndpointCatalog) -> Result<(), String> + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("herdr-endpoint-selection".into())
        .spawn(move || loop {
            let catalog = {
                let (pending, wake) = &*shared;
                let mut pending = pending.lock().unwrap_or_else(|error| error.into_inner());
                while pending.catalog.is_none() && !pending.shutdown {
                    pending = wake
                        .wait(pending)
                        .unwrap_or_else(|error| error.into_inner());
                }
                let Some(catalog) = pending.catalog.take() else {
                    return;
                };
                catalog
            };
            // Never hold the queue lock across serialization, fsync, or logging.
            if let Err(error) = store(&catalog) {
                tracing::warn!(%error, "failed to persist desired endpoint selection");
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn catalog() -> EndpointCatalog {
        let mut catalog = EndpointCatalog::default();
        catalog.add_ssh("Build", "build", "default").unwrap();
        catalog.add_ssh("Other", "other", "default").unwrap();
        catalog
    }

    #[test]
    fn same_machine_navigation_does_not_start_a_writer() {
        let mut catalog = catalog();
        for selection in [None, Some(catalog.ssh[0].id.clone())] {
            catalog.selected_profile = selection;
            let mut writer = SelectionWriter::new(&catalog);
            for _ in 0..5 {
                writer.schedule(&catalog);
            }
            // Profile metadata changes do not change the persisted selection either.
            catalog.ssh[0].label = "Renamed".into();
            writer.schedule(&catalog);
            assert!(writer.worker.is_none());
            assert!(writer.shared.0.lock().unwrap().catalog.is_none());
        }
    }

    #[test]
    fn blocked_disk_write_does_not_block_navigation_and_latest_selection_wins() {
        let mut catalog = catalog();
        let mut writer = SelectionWriter::new(&catalog);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let saved = Arc::new(Mutex::new(Vec::new()));
        let observed = saved.clone();
        let mut first = true;
        writer.worker = Some(
            start_worker(writer.shared.clone(), move |catalog| {
                observed
                    .lock()
                    .unwrap()
                    .push(catalog.selected_profile.clone());
                if first {
                    first = false;
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
                Ok(())
            })
            .unwrap(),
        );
        let first_selection = catalog.ssh[0].id.clone();
        let last_selection = catalog.ssh[1].id.clone();
        catalog.selected_profile = Some(first_selection.clone());
        writer.schedule(&catalog);
        started_rx.recv_timeout(Duration::from_secs(10)).unwrap();

        // These requests complete while the first store is still blocked.
        catalog.selected_profile = Some(last_selection.clone());
        writer.schedule(&catalog);
        catalog.select_local();
        writer.schedule(&catalog);
        catalog.selected_profile = Some(last_selection.clone());
        writer.schedule(&catalog);
        release_tx.send(()).unwrap();
        drop(writer);
        assert_eq!(
            *saved.lock().unwrap(),
            vec![Some(first_selection), Some(last_selection)]
        );
    }

    #[test]
    fn shutdown_drains_pending_selection_even_after_a_failed_write() {
        let mut catalog = catalog();
        let mut writer = SelectionWriter::new(&catalog);
        let (stored_tx, stored_rx) = mpsc::channel();
        writer.worker = Some(
            start_worker(writer.shared.clone(), move |catalog| {
                stored_tx.send(catalog.selected_profile.clone()).unwrap();
                Err("disk unavailable".into())
            })
            .unwrap(),
        );
        let id = catalog.ssh[0].id.clone();
        catalog.selected_profile = Some(id.clone());
        writer.schedule(&catalog);
        assert_eq!(
            stored_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            Some(id)
        );
        catalog.select_local();
        writer.schedule(&catalog);
        drop(writer);
        assert_eq!(
            stored_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
            None
        );
    }
}
