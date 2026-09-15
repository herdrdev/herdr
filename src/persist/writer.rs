use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{SessionHistorySnapshot, SessionSnapshot};

static NEXT_BACKUP: AtomicU64 = AtomicU64::new(0);

/// Server-owned writer, shared by autosaves, exit checkpoints, and shutdown.
/// The pre-startup file stays recoverable even if loading or restoring it failed.
pub(crate) struct SessionWriter {
    path: PathBuf,
    startup_preserved: bool,
}

impl SessionWriter {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            startup_preserved: false,
        }
    }

    fn preserve_startup_session(&mut self) -> io::Result<()> {
        if !self.startup_preserved {
            // Recheck at the first mutation: a file missing during startup may
            // have become accessible since then.
            self.startup_preserved = preserve_existing_session(&self.path)?;
        }
        Ok(())
    }

    pub(crate) fn save(
        &mut self,
        snapshot: &SessionSnapshot,
        history: Option<&SessionHistorySnapshot>,
    ) {
        let result = self.preserve_startup_session().and_then(|()| {
            super::io::save_to_paths(
                &self.path,
                &self.path.with_file_name("session-history.json"),
                snapshot,
                history,
            )
        });
        match result {
            Ok(()) => {
                self.startup_preserved = true;
                crate::logging::session_saved(&self.path, snapshot.workspaces.len());
            }
            Err(err) => crate::logging::session_save_failed(&self.path, &err.to_string()),
        }
    }

    pub(crate) fn clear(&mut self) {
        let result = self
            .preserve_startup_session()
            .and_then(|()| super::io::clear_path(&self.path));
        if let Err(err) = result {
            crate::logging::session_clear_failed(&self.path, &err.to_string());
            return;
        }
        let history_path = self.path.with_file_name("session-history.json");
        if let Err(err) = super::io::clear_path(&history_path) {
            crate::logging::session_clear_failed(&history_path, &err.to_string());
        }
        crate::logging::session_cleared(&self.path);
    }
}

/// Keep exact bytes, including malformed or newer formats, in an exclusive
/// recovery file. Never rotate away a prior boot's only recoverable snapshot.
fn preserve_existing_session(path: &Path) -> io::Result<bool> {
    let mut source = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    if !source.metadata()?.is_file() {
        return Err(io::Error::other("session path is not a regular file"));
    }

    let directory = path.with_file_name("session-backups");
    std::fs::create_dir_all(&directory)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for _ in 0..128 {
        let sequence = NEXT_BACKUP.fetch_add(1, Ordering::Relaxed);
        let backup = directory.join(format!(
            "session-{timestamp}-{}-{sequence}.json",
            std::process::id()
        ));
        let mut output = match crate::platform::create_config_temporary(&backup, true) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let result = io::copy(&mut source, &mut output)
            .and_then(|_| output.sync_all())
            .and_then(|()| crate::platform::sync_parent_directory(&directory))
            .and_then(|()| {
                crate::platform::sync_parent_directory(
                    directory
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new(".")),
                )
            });
        drop(output);
        if let Err(err) = result {
            let _ = std::fs::remove_file(&backup);
            return Err(err);
        }
        tracing::info!(
            event = "persist.backup",
            subsystem = "persist",
            outcome = "ok",
            path = %path.display(),
            backup_path = %backup.display(),
            "preserved session before first write"
        );
        return Ok(true);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique session backup",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SessionFiles {
        path: PathBuf,
    }

    impl SessionFiles {
        fn new(name: &str) -> Self {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "herdr-session-writer-{name}-{}-{timestamp}",
                std::process::id()
            ));
            std::fs::create_dir_all(&directory).unwrap();
            Self {
                path: directory.join("session.json"),
            }
        }

        fn writer(&self) -> SessionWriter {
            SessionWriter::new(self.path.clone())
        }

        fn backups(&self) -> Vec<Vec<u8>> {
            std::fs::read_dir(self.path.with_file_name("session-backups"))
                .unwrap()
                .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
                .collect()
        }
    }

    impl Drop for SessionFiles {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
        }
    }

    fn snapshot() -> SessionSnapshot {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/session/current-herdr-session.json"
        ))
        .unwrap()
    }

    #[test]
    fn first_save_preserves_original_bytes_across_later_saves_and_clear() {
        for original in [
            b"invalid session \xff".as_slice(),
            br#"{"version":999,"workspaces":{"future_format":true}}"#,
            include_bytes!("../../tests/fixtures/session/current-herdr-session.json"),
        ] {
            let files = SessionFiles::new("first-save");
            std::fs::write(&files.path, original).unwrap();
            let mut writer = files.writer();
            let mut replacement = snapshot();
            replacement.workspaces.truncate(1);

            writer.save(&replacement, None);
            assert_eq!(files.backups(), vec![original.to_vec()]);
            let saved: SessionSnapshot =
                serde_json::from_slice(&std::fs::read(&files.path).unwrap()).unwrap();
            assert_eq!(saved.workspaces.len(), 1);

            writer.save(&replacement, None);
            writer.clear();
            assert!(!files.path.exists(), "intentional clear should still work");
            assert_eq!(files.backups(), vec![original.to_vec()]);
        }
    }

    #[test]
    fn first_clear_preserves_original_without_resurrecting_it() {
        let files = SessionFiles::new("first-clear");
        std::fs::write(&files.path, b"original").unwrap();
        let history = files.path.with_file_name("session-history.json");
        std::fs::write(&history, b"old history").unwrap();
        let mut writer = files.writer();

        writer.clear();
        writer.clear();

        assert!(!files.path.exists());
        assert!(!history.exists());
        assert_eq!(files.backups(), vec![b"original".to_vec()]);
    }

    #[test]
    fn new_writer_keeps_backups_from_previous_startups() {
        let files = SessionFiles::new("repeated-startup");
        std::fs::write(&files.path, b"first boot").unwrap();
        files.writer().save(&snapshot(), None);
        let second_boot = std::fs::read(&files.path).unwrap();

        files.writer().clear();

        let backups = files.backups();
        assert_eq!(backups.len(), 2);
        assert!(backups.contains(&b"first boot".to_vec()));
        assert!(backups.contains(&second_boot));
    }

    #[test]
    fn failed_backup_blocks_save_and_clear_and_can_be_retried() {
        let files = SessionFiles::new("backup-failure");
        let backup_dir = files.path.with_file_name("session-backups");
        let history = files.path.with_file_name("session-history.json");
        std::fs::write(&files.path, b"original").unwrap();
        std::fs::write(&history, b"original history").unwrap();
        std::fs::write(&backup_dir, b"blocking file").unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);
        writer.clear();

        assert!(!writer.startup_preserved);
        assert_eq!(std::fs::read(&files.path).unwrap(), b"original");
        assert_eq!(std::fs::read(&history).unwrap(), b"original history");
        std::fs::remove_file(&backup_dir).unwrap();

        writer.save(&snapshot(), None);

        assert!(writer.startup_preserved);
        assert_eq!(files.backups(), vec![b"original".to_vec()]);
        assert!(!history.exists());
    }

    #[test]
    fn failed_first_save_rechecks_a_previously_missing_session() {
        let files = SessionFiles::new("missing-then-appears");
        let temporary = files.path.with_extension("json.tmp");
        std::fs::create_dir(&temporary).unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);

        assert!(!writer.startup_preserved);
        assert!(!files.path.exists());
        std::fs::remove_dir(&temporary).unwrap();
        std::fs::write(&files.path, b"became available").unwrap();

        writer.save(&snapshot(), None);

        assert_eq!(files.backups(), vec![b"became available".to_vec()]);
    }

    #[test]
    fn failed_save_does_not_replace_a_completed_backup_on_retry() {
        let files = SessionFiles::new("retry");
        std::fs::write(&files.path, b"original").unwrap();
        let temporary = files.path.with_extension("json.tmp");
        std::fs::create_dir(&temporary).unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);
        assert_eq!(files.backups(), vec![b"original".to_vec()]);
        assert_eq!(std::fs::read(&files.path).unwrap(), b"original");
        std::fs::remove_dir(&temporary).unwrap();

        writer.save(&snapshot(), None);
        assert_eq!(files.backups(), vec![b"original".to_vec()]);
    }

    #[test]
    fn fresh_session_does_not_backup_its_own_autosaves() {
        let files = SessionFiles::new("fresh");
        let mut writer = files.writer();
        writer.clear();
        writer.save(&snapshot(), None);
        writer.save(&snapshot(), None);
        writer.clear();

        assert!(!files.path.with_file_name("session-backups").exists());
    }

    #[test]
    fn missing_session_clear_does_not_authorize_overwriting_a_later_file() {
        let files = SessionFiles::new("empty-then-appears");
        let mut writer = files.writer();
        writer.clear();
        std::fs::write(&files.path, b"became available").unwrap();

        writer.save(&snapshot(), None);

        assert_eq!(files.backups(), vec![b"became available".to_vec()]);
    }

    #[test]
    fn non_file_session_path_blocks_persistence() {
        let files = SessionFiles::new("directory");
        std::fs::create_dir(&files.path).unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);
        writer.clear();

        assert!(!writer.startup_preserved);
        assert!(files.path.is_dir());
        assert!(!files.path.with_file_name("session-backups").exists());
    }

    #[cfg(unix)]
    #[test]
    fn backup_follows_symlink_but_clear_only_unlinks_the_configured_path() {
        let files = SessionFiles::new("symlink");
        let target = files.path.with_file_name("target.json");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink("target.json", &files.path).unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);

        assert!(std::fs::symlink_metadata(&files.path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(files.backups(), vec![b"original".to_vec()]);
        writer.clear();
        assert!(target.exists());
        assert!(std::fs::symlink_metadata(&files.path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cyclic_session_symlink_is_never_overwritten_or_cleared() {
        let files = SessionFiles::new("cyclic-symlink");
        std::os::unix::fs::symlink("session.json", &files.path).unwrap();
        let mut writer = files.writer();

        writer.save(&snapshot(), None);
        writer.clear();

        assert!(!writer.startup_preserved);
        assert!(std::fs::symlink_metadata(&files.path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn backup_does_not_broaden_access_to_session_contents() {
        use std::os::unix::fs::PermissionsExt;

        let files = SessionFiles::new("permissions");
        std::fs::write(&files.path, b"original").unwrap();
        files.writer().save(&snapshot(), None);
        let backup = std::fs::read_dir(files.path.with_file_name("session-backups"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();

        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
