//! Refresh when a state file changes: watches the folders the panels read and calls back once per burst of
//! writes, so the panels never need a timer.

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const QUIET: Duration = Duration::from_millis(120);

pub struct StateWatcher {
    watcher: RecommendedWatcher,
    watched: HashSet<PathBuf>,
}

impl StateWatcher {
    pub fn start(on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel::<()>();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok_and(|e| !matches!(e.kind, EventKind::Access(_))) {
                let _ = tx.send(());
            }
        })?;
        std::thread::spawn(move || {
            while rx.recv().is_ok() {
                while rx.recv_timeout(QUIET).is_ok() {} // let the burst finish
                on_change();
            }
        });
        Ok(Self {
            watcher,
            watched: HashSet::new(),
        })
    }

    /// Starts watching `dir` once; a missing folder is skipped. Returns whether it was added.
    pub fn watch(&mut self, dir: &Path, recursive: bool) -> notify::Result<bool> {
        if !dir.is_dir() || self.watched.contains(dir) {
            return Ok(false);
        }
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        self.watcher.watch(dir, mode)?;
        self.watched.insert(dir.to_path_buf());
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn a_burst_of_writes_is_reported_once() {
        let dir = std::env::temp_dir().join(format!("native-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let mut watcher = StateWatcher::start(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        assert!(watcher.watch(&dir, false).unwrap());
        assert!(!watcher.watch(&dir, false).unwrap());
        for i in 0..5 {
            std::fs::write(dir.join("agents.json"), format!("{{\"n\":{i}}}")).unwrap();
        }
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
