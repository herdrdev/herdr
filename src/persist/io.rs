use std::path::{Path, PathBuf};

use tracing::warn;

use super::snapshot::{
    parse_history_snapshot, parse_snapshot, snapshot_file_version, SessionHistorySnapshot,
    SessionSnapshot, SNAPSHOT_VERSION,
};

fn session_path() -> PathBuf {
    crate::session::data_dir().join("session.json")
}

fn session_history_path() -> PathBuf {
    crate::session::data_dir().join("session-history.json")
}

// Follow symlinks manually so a write through a (possibly dangling) symlink
// lands on the target. `fs::canonicalize` requires the target to exist, which
// excludes the dangling-symlink case stow users hit on the very first save.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..16 {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(_) => return Ok(current),
        };
        if !meta.file_type().is_symlink() {
            return Ok(current);
        }
        let link = std::fs::read_link(&current)?;
        current = if link.is_absolute() {
            link
        } else {
            current
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(link)
        };
    }
    Ok(current)
}

pub(super) fn save_to_path(path: &Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    save_json_to_path(path, snapshot)
}

fn save_json_to_path<T: serde::Serialize>(path: &Path, snapshot: &T) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(snapshot)?;
    let tmp_path = target.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    if let Err(err) = std::fs::rename(&tmp_path, &target) {
        if is_cross_filesystem_rename_error(err.kind()) {
            return write_fallback(&target, &tmp_path, &json);
        }
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

// Writes directly to `target` when `rename` cannot be used. The temp file at
// `tmp_path` still holds a complete copy of `json`, so it is only removed
// once the direct write has actually landed; if the direct write fails
// partway through, the temp file remains as a recovery copy.
fn write_fallback(target: &Path, tmp_path: &Path, json: &str) -> std::io::Result<()> {
    let result = std::fs::write(target, json);
    if result.is_ok() {
        let _ = std::fs::remove_file(tmp_path);
    }
    result
}

fn is_cross_filesystem_rename_error(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ResourceBusy | std::io::ErrorKind::CrossesDevices
    )
}

pub(super) fn save_to_paths(
    session_path: &Path,
    history_path: &Path,
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
) -> std::io::Result<()> {
    save_to_path(session_path, snapshot)?;
    if let Some(history) = history {
        save_json_to_path(history_path, history)?;
    } else {
        clear_path_and_tmp(history_path)?;
    }
    Ok(())
}

pub(super) fn clear_path(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

// Clears `path` and its `.tmp` recovery file. A stale `.tmp` left behind by
// `write_fallback` must not survive a clear, or a later `load` would recover
// state the user explicitly cleared.
fn clear_path_and_tmp(path: &Path) -> std::io::Result<()> {
    clear_path(path)?;
    clear_path(&path.with_extension("json.tmp"))
}

pub fn save(snapshot: &SessionSnapshot, history: Option<&SessionHistorySnapshot>) {
    let path = session_path();
    let history_path = session_history_path();
    if let Err(err) = save_to_paths(&path, &history_path, snapshot, history) {
        crate::logging::session_save_failed(&path, &err.to_string());
        return;
    }
    crate::logging::session_saved(&path, snapshot.workspaces.len());
}

pub fn clear() {
    let path = session_path();
    if let Err(err) = clear_path_and_tmp(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
        return;
    }
    clear_history();
    crate::logging::session_cleared(&path);
}

pub fn clear_history() {
    let path = session_history_path();
    if let Err(err) = clear_path_and_tmp(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
    }
}

// Outcome of trying to load a snapshot from a single file.
enum LoadOutcome<T> {
    Loaded(T),
    // The file is valid but from a newer herdr version. It must not be
    // treated as recoverable: falling back to a `.tmp` file here could
    // replace a real, valid (if unreadable) snapshot with stale data.
    UnsupportedVersion,
    // The file is missing, unreadable, or invalid.
    Unavailable,
}

// Loads from `path`, falling back to its `.tmp` recovery file only when
// `path` itself is missing or invalid (not when it is merely a newer,
// unsupported version).
fn load_with_recovery<T>(path: &Path, try_load: impl Fn(&Path) -> LoadOutcome<T>) -> Option<T> {
    match try_load(path) {
        LoadOutcome::Loaded(value) => Some(value),
        LoadOutcome::UnsupportedVersion => None,
        LoadOutcome::Unavailable => match try_load(&path.with_extension("json.tmp")) {
            LoadOutcome::Loaded(value) => Some(value),
            LoadOutcome::UnsupportedVersion | LoadOutcome::Unavailable => None,
        },
    }
}

pub fn load() -> Option<SessionSnapshot> {
    load_with_recovery(&session_path(), try_load_snapshot)
}

fn try_load_snapshot(path: &Path) -> LoadOutcome<SessionSnapshot> {
    if !path.exists() {
        return LoadOutcome::Unavailable;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session file");
            return LoadOutcome::Unavailable;
        }
    };
    match parse_snapshot(&content) {
        Ok(snapshot) => LoadOutcome::Loaded(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session file is from a newer herdr version, ignoring"
                    );
                    return LoadOutcome::UnsupportedVersion;
                }
            }
            warn!(err = %err, "failed to parse session file, ignoring");
            LoadOutcome::Unavailable
        }
    }
}

pub fn load_history() -> Option<SessionHistorySnapshot> {
    load_with_recovery(&session_history_path(), try_load_history)
}

fn try_load_history(path: &Path) -> LoadOutcome<SessionHistorySnapshot> {
    if !path.exists() {
        return LoadOutcome::Unavailable;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session history file");
            return LoadOutcome::Unavailable;
        }
    };
    match parse_history_snapshot(&content) {
        Ok(snapshot) => LoadOutcome::Loaded(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session history file is from a newer herdr version, ignoring"
                    );
                    return LoadOutcome::UnsupportedVersion;
                }
            }
            warn!(err = %err, "failed to parse session history file, ignoring");
            LoadOutcome::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::snapshot::{
        PaneHistorySnapshot, TabHistorySnapshot, WorkspaceHistorySnapshot,
    };

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-session-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("session.json")
    }

    fn temp_session_paths(name: &str) -> (PathBuf, PathBuf) {
        let session = temp_session_path(name);
        let history = session.with_file_name("session-history.json");
        (session, history)
    }

    fn empty_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![],
            active: None,
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: std::collections::HashSet::new(),
        }
    }

    fn history_snapshot(secret: &str) -> SessionHistorySnapshot {
        SessionHistorySnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![TabHistorySnapshot {
                    panes: std::collections::HashMap::from([(
                        0,
                        PaneHistorySnapshot {
                            ansi: secret.to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        }
    }

    #[test]
    fn save_to_paths_writes_pane_history_only_to_history_file() {
        let (session_path, history_path) = temp_session_paths("split-history");

        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("split-secret")),
        )
        .unwrap();

        let session = std::fs::read_to_string(&session_path).unwrap();
        let history = std::fs::read_to_string(&history_path).unwrap();
        assert!(!session.contains("split-secret"));
        assert!(!session.contains("history"));
        assert!(history.contains("split-secret"));
    }

    #[test]
    fn save_to_paths_removes_stale_history_when_history_is_disabled() {
        let (session_path, history_path) = temp_session_paths("clear-history");
        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("stale-secret")),
        )
        .unwrap();

        save_to_paths(&session_path, &history_path, &empty_snapshot(), None).unwrap();

        assert!(session_path.exists());
        assert!(!history_path.exists());
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear-existing");
        save_to_path(&path, &empty_snapshot()).unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("clear-missing");

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_preserves_existing_symlink() {
        let target = temp_session_path("symlink-target");
        let link = target.with_file_name("link.json");
        save_to_path(&target, &empty_snapshot()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut snap = empty_snapshot();
        snap.selected = 7;
        save_to_path(&link, &snap).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let parsed = parse_snapshot(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(parsed.selected, 7);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_writes_through_dangling_symlink() {
        let target = temp_session_path("dangling-target");
        let link = target.with_file_name("link.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_resolves_relative_symlink() {
        let session = temp_session_path("relative-symlink");
        let dir = session.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let target = dir.join("real.json");
        let link = dir.join("link.json");
        std::os::unix::fs::symlink("real.json", &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[test]
    fn cross_filesystem_rename_error_detection() {
        assert!(is_cross_filesystem_rename_error(
            std::io::ErrorKind::ResourceBusy
        ));
        assert!(is_cross_filesystem_rename_error(
            std::io::ErrorKind::CrossesDevices
        ));
        assert!(!is_cross_filesystem_rename_error(
            std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn save_to_path_propagates_unrelated_rename_errors() {
        // Renaming the temp file onto an existing directory fails with an
        // unrelated rename error (the exact kind varies by platform). Herdr
        // must still report the error and clean up the temp file.
        let target = temp_session_path("rename-onto-directory");
        std::fs::create_dir_all(&target).unwrap();

        let err = save_to_path(&target, &empty_snapshot()).unwrap_err();

        assert!(!is_cross_filesystem_rename_error(err.kind()));
        assert!(!target.with_extension("json.tmp").exists());
    }

    #[test]
    fn write_fallback_removes_tmp_file_on_success() {
        let target = temp_session_path("fallback-success");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let tmp_path = target.with_extension("json.tmp");
        std::fs::write(&tmp_path, "{}").unwrap();

        write_fallback(&target, &tmp_path, "{}").unwrap();

        assert!(target.exists());
        assert!(!tmp_path.exists());
    }

    #[test]
    fn write_fallback_keeps_tmp_file_as_recovery_copy_on_failure() {
        // `target`'s parent directory does not exist, so the direct write
        // fails. The temp file must survive so the last known-good snapshot
        // is not lost.
        let target = temp_session_path("fallback-failure");
        let tmp_path = temp_session_path("fallback-failure-tmp");
        std::fs::create_dir_all(tmp_path.parent().unwrap()).unwrap();
        std::fs::write(&tmp_path, "{}").unwrap();

        write_fallback(&target, &tmp_path, "{}").unwrap_err();

        assert!(tmp_path.exists());
    }

    #[test]
    fn load_with_recovery_prefers_a_valid_main_file() {
        let path = temp_session_path("recovery-prefers-main");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut main = empty_snapshot();
        main.selected = 1;
        save_to_path(&path, &main).unwrap();
        let mut stale_tmp = empty_snapshot();
        stale_tmp.selected = 99;
        save_to_path(&path.with_extension("json.tmp"), &stale_tmp).unwrap();

        let loaded = load_with_recovery(&path, try_load_snapshot).unwrap();

        assert_eq!(loaded.selected, 1);
    }

    #[test]
    fn load_with_recovery_recovers_from_tmp_file_when_main_file_is_invalid() {
        // Mirrors what `write_fallback` can leave behind: a corrupt main
        // file next to a complete `.tmp` file.
        let path = temp_session_path("recovery-recovers-from-tmp");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid json").unwrap();
        let mut snap = empty_snapshot();
        snap.selected = 5;
        save_to_path(&path.with_extension("json.tmp"), &snap).unwrap();

        let recovered = load_with_recovery(&path, try_load_snapshot).unwrap();

        assert_eq!(recovered.selected, 5);
    }

    #[test]
    fn load_with_recovery_does_not_recover_a_newer_unsupported_main_file() {
        // A main file from a newer herdr version is valid, just not
        // understood yet. It must not be treated as a recoverable failure,
        // or a stale `.tmp` could silently replace real (if unreadable)
        // data.
        let path = temp_session_path("recovery-skips-unsupported-version");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                r#"{{"version": {}, "workspaces": []}}"#,
                SNAPSHOT_VERSION + 1
            ),
        )
        .unwrap();
        let mut stale_tmp = empty_snapshot();
        stale_tmp.selected = 5;
        save_to_path(&path.with_extension("json.tmp"), &stale_tmp).unwrap();

        assert!(load_with_recovery(&path, try_load_snapshot).is_none());
    }

    #[test]
    fn load_history_with_recovery_recovers_from_tmp_file_when_main_file_is_invalid() {
        let (path, _) = temp_session_paths("history-recovers-from-tmp");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not valid json").unwrap();
        save_json_to_path(
            &path.with_extension("json.tmp"),
            &history_snapshot("secret"),
        )
        .unwrap();

        let recovered = load_with_recovery(&path, try_load_history).unwrap();

        assert!(recovered.workspaces[0].tabs[0].panes[&0]
            .ansi
            .contains("secret"));
    }

    #[test]
    fn clear_path_and_tmp_removes_both_files() {
        let path = temp_session_path("clear-with-tmp");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_to_path(&path, &empty_snapshot()).unwrap();
        std::fs::write(path.with_extension("json.tmp"), "{}").unwrap();

        clear_path_and_tmp(&path).unwrap();

        assert!(!path.exists());
        assert!(!path.with_extension("json.tmp").exists());
    }
}
