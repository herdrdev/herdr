use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    let tmp_path = unique_tmp_path(&target);
    std::fs::write(&tmp_path, &json)?;
    if let Err(err) = std::fs::rename(&tmp_path, &target) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

/// Monotonic per-process counter so successive temp names never collide.
static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Builds a process-unique temporary file path beside `target`. The previous
/// fixed `.json.tmp` suffix collided when another herdr process (or a stalled
/// first save) wrote to the same name, and — because `save_to_paths` writes
/// `session.json` and `session-history.json` in two separate renames — a crash
/// between them could leave a stale/foreign tmp that a live save then
/// clobbered. Including the PID, a per-process counter, and a high-resolution
/// timestamp keeps the name unique, and the tmp stays on the same filesystem
/// as `target` so the subsequent rename remains atomic.
fn unique_tmp_path(target: &Path) -> PathBuf {
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session.json".to_string());
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let seq = TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    parent.join(format!(
        ".{file_name}.{}.{}.{}.tmp",
        std::process::id(),
        seq,
        nanos
    ))
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
        clear_path(history_path)?;
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
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
        return;
    }
    clear_history();
    crate::logging::session_cleared(&path);
}

pub fn clear_history() {
    let path = session_history_path();
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
    }
}

pub fn load() -> Option<SessionSnapshot> {
    let path = session_path();
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session file");
            return None;
        }
    };
    match parse_snapshot(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(err = %err, "failed to parse session file, ignoring");
            None
        }
    }
}

pub fn load_history() -> Option<SessionHistorySnapshot> {
    let path = session_history_path();
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session history file");
            return None;
        }
    };
    match parse_history_snapshot(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session history file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(err = %err, "failed to parse session history file, ignoring");
            None
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
    fn unique_tmp_path_is_unique_per_call() {
        let target = Path::new("/tmp/herdr-session-tests/session.json");
        let a = unique_tmp_path(target);
        let b = unique_tmp_path(target);
        // Two consecutive calls must never collide (the old fixed .json.tmp
        // name could).
        assert_ne!(a, b);
        // Both sit beside the target so rename stays on the same filesystem.
        assert_eq!(a.parent(), target.parent());
        assert_eq!(b.parent(), target.parent());
        assert!(a.to_string_lossy().ends_with(".tmp"));
    }

    #[test]
    fn save_to_paths_leaves_no_tmp_files_behind() {
        let (session_path, history_path) = temp_session_paths("no-tmp-left");

        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("no-tmp-secret")),
        )
        .unwrap();

        assert!(session_path.exists());
        assert!(history_path.exists());
        let dir = session_path.parent().unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(leftovers.is_empty(), "tmp files leaked: {leftovers:?}");
    }
}
