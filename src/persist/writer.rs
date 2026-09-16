use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{SessionHistorySnapshot, SessionSnapshot};

/// Shared by autosave, pane-exit checkpoints, and shutdown.
pub(crate) struct SessionWriter {
    path: PathBuf,
    protect_unloaded: bool,
}

impl SessionWriter {
    pub(crate) fn new(protect_unloaded: bool) -> Self {
        Self {
            path: super::io::session_path(),
            protect_unloaded,
        }
    }

    fn preserve_unloaded(&mut self) -> io::Result<()> {
        if self.protect_unloaded && preserve_existing(&self.path)? {
            self.protect_unloaded = false;
        }
        Ok(())
    }

    pub(crate) fn save(
        &mut self,
        snapshot: &SessionSnapshot,
        history: Option<&SessionHistorySnapshot>,
    ) {
        let result = self
            .preserve_unloaded()
            .and_then(|()| super::io::save_to_path(&self.path, snapshot));
        if let Err(err) = result {
            crate::logging::session_save_failed(&self.path, &err.to_string());
            return;
        }
        // Optional history failure must not reclassify our committed layout as unloaded.
        self.protect_unloaded = false;
        let history_path = self.path.with_file_name("session-history.json");
        if let Err(err) = super::io::save_history_to_path(&history_path, history) {
            crate::logging::session_save_failed(&history_path, &err.to_string());
        }
        crate::logging::session_saved(&self.path, snapshot.workspaces.len());
    }

    pub(crate) fn clear(&mut self) {
        let result = self
            .preserve_unloaded()
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

fn preserve_existing(path: &Path) -> io::Result<bool> {
    let mut source = match File::open(path) {
        Ok(file) => file,
        // Recheck on the next mutation until a fresh session is actually saved.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    if !source.metadata()?.is_file() {
        return Err(io::Error::other("session path is not a regular file"));
    }
    let directory = path.with_file_name("session-backups");
    std::fs::create_dir_all(&directory)?;
    let older = recovery_files(&directory)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Keep creation order even when the wall clock moves backwards.
    let timestamp = match older.last() {
        Some((previous, _)) => now.max(
            previous
                .checked_add(1)
                .ok_or_else(|| io::Error::other("session recovery sequence exhausted"))?,
        ),
        None => now,
    };
    for sequence in 0..128 {
        let backup = directory.join(format!(
            "session-{timestamp:039}-{}-{sequence}.json",
            std::process::id()
        ));
        match copy_recovery(&mut source, &backup) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
        tracing::info!(
            event = "persist.backup",
            subsystem = "persist",
            outcome = "ok",
            path = %path.display(),
            backup_path = %backup.display(),
            "preserved unloaded session before replacement"
        );
        if let Err(err) = prune_backups(&older) {
            tracing::warn!(
                event = "persist.backup", subsystem = "persist", outcome = "prune_error",
                path = %directory.display(), err = %err, "failed to prune session recovery copies"
            );
        }
        return Ok(true);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate session recovery copy",
    ))
}

fn copy_recovery(source: &mut impl io::Read, backup: &Path) -> io::Result<()> {
    let directory = backup.parent().unwrap_or_else(|| Path::new("."));
    let pending = backup.with_extension("pending");
    let mut output = crate::platform::create_config_temporary(&pending, true)?;
    let mut published = false;
    let result = (|| {
        match std::fs::symlink_metadata(backup) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "recovery copy already exists",
                ))
            }
        }
        io::copy(source, &mut output)?;
        output.sync_all()?;
        drop(output);
        std::fs::rename(&pending, backup)?;
        published = true;
        crate::platform::sync_parent_directory(directory)?;
        crate::platform::sync_parent_directory(directory.parent().unwrap_or_else(|| Path::new(".")))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(pending);
        if published {
            let _ = std::fs::remove_file(backup);
        }
    }
    result
}

fn recovery_files(directory: &Path) -> io::Result<Vec<(u128, PathBuf)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            if let Some(timestamp) = entry.file_name().to_str().and_then(recovery_timestamp) {
                files.push((timestamp, entry.path()));
            }
        }
    }
    files.sort();
    Ok(files)
}

fn prune_backups(older: &[(u128, PathBuf)]) -> io::Result<()> {
    // The new copy is durable before any of the previous copies are removed.
    for (_, path) in older.iter().take(older.len().saturating_sub(2)) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn recovery_timestamp(name: &str) -> Option<u128> {
    let fields = name.strip_prefix("session-")?.strip_suffix(".json")?;
    let fields: Vec<_> = fields.split('-').collect();
    if fields.len() == 3
        && fields[0].len() == 39
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
    {
        fields[0].parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(protect_unloaded: bool) -> SessionWriter {
        let directory = std::env::temp_dir().join(format!(
            "herdr-session-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        SessionWriter {
            path: directory.join("session.json"),
            protect_unloaded,
        }
    }

    fn snapshot() -> SessionSnapshot {
        serde_json::from_str(include_str!(
            "../../tests/fixtures/session/current-herdr-session.json"
        ))
        .unwrap()
    }

    fn backups(writer: &SessionWriter) -> Vec<Vec<u8>> {
        let directory = writer.path.with_file_name("session-backups");
        if !directory.exists() {
            return Vec::new();
        }
        let mut entries: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        entries
            .into_iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect()
    }

    #[test]
    fn healthy_and_fresh_sessions_save_and_clear_without_backups() {
        for protect_unloaded in [false, true] {
            let mut writer = writer(protect_unloaded);
            if !protect_unloaded {
                super::super::io::save_to_path(&writer.path, &snapshot()).unwrap();
            }
            writer.save(&snapshot(), None);
            assert!(!writer.protect_unloaded);
            assert!(writer.path.exists());
            writer.save(&snapshot(), None);
            writer.clear();
            assert!(!writer.path.exists());
            assert!(backups(&writer).is_empty());
            std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn failed_recovery_blocks_mutations_then_retries_preserving_exact_bytes_once() {
        let original = b"invalid utf8 \xff";
        let mut writer = writer(true);
        std::fs::write(&writer.path, original).unwrap();
        let history_path = writer.path.with_file_name("session-history.json");
        std::fs::write(&history_path, b"history").unwrap();
        let directory = writer.path.with_file_name("session-backups");
        std::fs::write(&directory, b"blocks recovery").unwrap();
        writer.save(&snapshot(), None);
        writer.clear();
        assert!(writer.protect_unloaded);
        assert_eq!(std::fs::read(&writer.path).unwrap(), original);
        assert_eq!(std::fs::read(&history_path).unwrap(), b"history");

        std::fs::remove_file(&directory).unwrap();
        writer.save(&snapshot(), None);
        assert!(!writer.protect_unloaded);
        writer.save(&snapshot(), None);
        writer.clear();
        assert!(!writer.path.exists());
        assert_eq!(backups(&writer), vec![original.to_vec()]);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn optional_history_failure_does_not_block_later_layout_saves() {
        let mut writer = writer(true);
        let history = writer.path.with_file_name("session-history.json");
        std::fs::create_dir(&history).unwrap();
        std::fs::write(
            writer.path.with_file_name("session-backups"),
            b"unavailable",
        )
        .unwrap();
        writer.save(&snapshot(), None);
        assert!(
            !writer.protect_unloaded,
            "structural session was saved successfully"
        );
        let mut changed = snapshot();
        changed.workspaces[0].custom_name = Some("latest layout".into());
        writer.save(&changed, None);
        let saved: SessionSnapshot =
            serde_json::from_slice(&std::fs::read(&writer.path).unwrap()).unwrap();
        assert_eq!(
            saved.workspaces[0].custom_name.as_deref(),
            Some("latest layout")
        );
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn pruning_leaves_user_named_recovery_files_alone() {
        let mut writer = writer(true);
        let directory = writer.path.with_file_name("session-backups");
        std::fs::create_dir(&directory).unwrap();
        let manual = directory.join("session-000-manual.json");
        std::fs::write(&manual, b"manual recovery copy").unwrap();
        for i in 0..5u8 {
            writer.protect_unloaded = true;
            std::fs::write(&writer.path, [i]).unwrap();
            writer.save(&snapshot(), None);
        }
        assert_eq!(std::fs::read(manual).unwrap(), b"manual recovery copy");
        assert_eq!(std::fs::read_dir(directory).unwrap().count(), 4);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn first_clear_preserves_an_unloaded_file_even_after_an_earlier_missing_clear() {
        let mut writer = writer(true);
        writer.clear();
        assert!(writer.protect_unloaded);
        std::fs::write(&writer.path, b"late layout").unwrap();
        writer.clear();
        assert!(!writer.path.exists());
        assert_eq!(backups(&writer), vec![b"late layout".to_vec()]);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn repeated_failed_saves_do_not_replace_a_completed_recovery_copy() {
        let mut writer = writer(true);
        std::fs::write(&writer.path, b"original").unwrap();
        let temporary = writer.path.with_extension("json.tmp");
        std::fs::create_dir(&temporary).unwrap();
        writer.save(&snapshot(), None);
        writer.save(&snapshot(), None);
        assert_eq!(std::fs::read(&writer.path).unwrap(), b"original");
        std::fs::remove_dir(&temporary).unwrap();
        writer.save(&snapshot(), None);
        assert_eq!(backups(&writer), vec![b"original".to_vec()]);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn interrupted_copy_is_not_published_as_a_recovery_file() {
        use std::io::Read;
        struct Interrupted;
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("interrupt the copy after writing its prefix");
            }
        }
        let writer = writer(true);
        let backup = writer
            .path
            .with_file_name("session-000000000000000000000000000000000000001-1-0.json");
        let mut source = io::Cursor::new(b"partial").chain(Interrupted);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            copy_recovery(&mut source, &backup)
        }))
        .is_err());
        assert!(
            !backup.exists(),
            "an interrupted copy must not look complete"
        );
        assert_eq!(
            std::fs::read(backup.with_extension("pending")).unwrap(),
            b"partial"
        );
        assert!(recovery_files(writer.path.parent().unwrap())
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn recovery_order_survives_clock_rollback() {
        let mut writer = writer(true);
        let directory = writer.path.with_file_name("session-backups");
        std::fs::create_dir(&directory).unwrap();
        let future = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            + 1_000_000_000_000_000;
        for i in 0..2u8 {
            std::fs::write(
                directory.join(format!("session-{:039}-1-0.json", future + u128::from(i))),
                [i],
            )
            .unwrap();
        }
        for i in 2..4u8 {
            writer.protect_unloaded = true;
            std::fs::write(&writer.path, [i]).unwrap();
            writer.save(&snapshot(), None);
        }
        assert_eq!(backups(&writer), vec![vec![1], vec![2], vec![3]]);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn recovery_keeps_three_copies_and_healthy_saves_do_not_rotate_them() {
        let mut writer = writer(true);
        for i in 0..5u8 {
            writer.protect_unloaded = true;
            std::fs::write(&writer.path, [i]).unwrap();
            writer.save(&snapshot(), None);
        }
        assert_eq!(backups(&writer), vec![vec![2], vec![3], vec![4]]);
        writer.save(&snapshot(), None);
        writer.clear();
        assert_eq!(backups(&writer), vec![vec![2], vec![3], vec![4]]);
        std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_allows_first_save_and_late_target_is_preserved() {
        use std::os::unix::{fs::symlink, fs::PermissionsExt};
        for late_target in [false, true] {
            let mut writer = writer(true);
            let target = writer.path.with_file_name("target.json");
            symlink("target.json", &writer.path).unwrap();
            if late_target {
                std::fs::write(&target, b"late layout").unwrap();
            }
            writer.save(&snapshot(), None);
            assert!(std::fs::symlink_metadata(&writer.path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(target.exists());
            if late_target {
                assert_eq!(backups(&writer), vec![b"late layout".to_vec()]);
                let backup = std::fs::read_dir(writer.path.with_file_name("session-backups"))
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                assert_eq!(
                    std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            } else {
                assert!(backups(&writer).is_empty());
            }
            writer.clear();
            assert!(target.exists());
            std::fs::remove_dir_all(writer.path.parent().unwrap()).unwrap();
        }
    }
}
