//! Portable session bundles: move a Herdr session between machines and
//! operating systems.
//!
//! A bundle is a reconstruction plan, not a process dump. It carries the
//! saved session layout (`session.json`), the agent session files needed to
//! resume each agent conversation, optional pane scrollback for reference,
//! and git provenance for each project folder. Import writes a new named
//! session and installs the agent files into their local stores; Herdr's
//! existing snapshot restore and native agent resume do the rest on attach.
//!
//! Bundles contain full agent transcripts. They never contain agent
//! credentials or project files. Treat them as private data.

pub(crate) mod export;
pub(crate) mod git_info;
pub(crate) mod import;
pub(crate) mod manifest;
pub(crate) mod stores;

use std::path::{Path, PathBuf};

pub(crate) const BUNDLE_KIND: &str = "herdr-bundle";
pub(crate) const BUNDLE_SCHEMA_VERSION: u32 = 2;
/// Oldest bundle schema import can read (the original script format).
pub(crate) const MIN_SUPPORTED_SCHEMA_VERSION: u32 = 1;
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const SESSION_FILE: &str = "session.json";
pub(crate) const HISTORY_FILE: &str = "session-history.json";
pub(crate) const API_SNAPSHOT_FILE: &str = "api-snapshot.json";
pub(crate) const MAX_SCROLLBACK_BYTES: usize = 2_000_000;

pub(crate) type BundleResult<T> = Result<T, BundleError>;

#[derive(Debug)]
pub(crate) enum BundleError {
    Message(String),
    Io(std::io::Error),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => write!(formatter, "{message}"),
            Self::Io(error) => write!(formatter, "{error}"),
        }
    }
}

impl From<std::io::Error> for BundleError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<BundleError> for std::io::Error {
    fn from(error: BundleError) -> Self {
        match error {
            BundleError::Io(error) => error,
            BundleError::Message(message) => std::io::Error::other(message),
        }
    }
}

pub(crate) fn message_error(message: impl Into<String>) -> BundleError {
    BundleError::Message(message.into())
}

/// The user's home directory. Windows prefers `USERPROFILE`, then
/// `HOMEDRIVE`+`HOMEPATH`, then `HOME`; other systems use `HOME`.
pub(crate) fn home_dir() -> Option<PathBuf> {
    home_dir_from_env(cfg!(windows), |key| std::env::var_os(key))
}

pub(crate) fn home_dir_from_env(
    is_windows: bool,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let non_empty = |value: std::ffi::OsString| {
        let path = PathBuf::from(value);
        (!path.as_os_str().is_empty()).then_some(path)
    };
    if is_windows {
        if let Some(path) = env("USERPROFILE").and_then(non_empty) {
            return Some(path);
        }
        if let (Some(drive), Some(rest)) = (env("HOMEDRIVE"), env("HOMEPATH")) {
            let mut combined = drive;
            combined.push(&rest);
            if let Some(path) = non_empty(combined) {
                return Some(path);
            }
        }
    }
    env("HOME").and_then(non_empty)
}

/// One `--map OLD=NEW` path rewrite. Both sides are kept as strings because
/// the old side uses the source machine's separator and drive conventions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PathMap {
    pub(crate) old: String,
    pub(crate) new: String,
}

impl PathMap {
    pub(crate) fn parse(raw: &str) -> BundleResult<Self> {
        let (old, new) = split_map_assignment(raw)
            .ok_or_else(|| message_error(format!("--map must use OLD=NEW, got '{raw}'")))?;
        if old.is_empty() || new.is_empty() {
            return Err(message_error(format!(
                "--map must use OLD=NEW with both sides set, got '{raw}'"
            )));
        }
        Ok(Self {
            old: old.to_string(),
            new: new.to_string(),
        })
    }
}

/// Split on the `=` that separates two paths. A Windows old path starts with
/// a drive such as `C:\...`, so the separator is the first `=` after it.
fn split_map_assignment(raw: &str) -> Option<(&str, &str)> {
    let bytes = raw.as_bytes();
    let start = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        2
    } else {
        0
    };
    let offset = raw[start..].find('=')? + start;
    Some((&raw[..offset], &raw[offset + 1..]))
}

fn normalize_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Apply the first matching rewrite to a source-machine path and return a
/// path for this machine. Matching ignores separator style so a bundle made
/// on Windows can be mapped on Linux and the reverse.
pub(crate) fn map_path(path: &str, maps: &[PathMap]) -> String {
    let normalized = normalize_separators(path);
    for map in maps {
        let old = normalize_separators(&map.old);
        let old = old.trim_end_matches('/');
        if normalized == old {
            return localize_path(&normalize_separators(&map.new));
        }
        if let Some(rest) = normalized
            .strip_prefix(old)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            let new = normalize_separators(&map.new);
            let new = new.trim_end_matches('/');
            return localize_path(&format!("{new}/{rest}"));
        }
    }
    localize_path(&normalized)
}

/// Rewrite `/` separators to this platform's separator. On Unix this only
/// normalizes a Windows source path's backslashes, which `map_path` has
/// already turned into `/`.
fn localize_path(normalized: &str) -> String {
    if cfg!(windows) {
        normalized.replace('/', "\\")
    } else {
        normalized.to_string()
    }
}

/// True when a path string is absolute under either Unix or Windows rules,
/// so a bundle can be validated on a different OS than it was made on.
pub(crate) fn path_is_absolute_any(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

/// Claude Code's project directory key: every non-alphanumeric character of
/// the working directory becomes `-`.
pub(crate) fn claude_project_key(cwd: &str) -> String {
    cwd.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

/// Strip credentials from a git remote URL before it is written to a bundle.
/// Only `scheme://userinfo@host` forms carry a password or token; SSH forms
/// such as `git@host:path` are returned unchanged.
pub(crate) fn redact_remote_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, tail) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    let Some((_, host)) = authority.rsplit_once('@') else {
        return url.to_string();
    };
    format!("{scheme}://{host}{tail}")
}

/// Drop a torn trailing record from a copied JSONL transcript. An agent may
/// be mid-write during export; keep only the valid prefix so the restored
/// session still loads. Returns `(kept, total)` non-empty line counts.
pub(crate) fn sanitize_jsonl_bytes(bytes: &[u8]) -> (Vec<u8>, usize, usize) {
    let mut kept: Vec<&[u8]> = Vec::new();
    let mut total = 0usize;
    for line in bytes.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        total += 1;
        if serde_json::from_slice::<serde_json::Value>(line).is_err() {
            break;
        }
        kept.push(line);
    }
    let kept_count = kept.len();
    let mut out = Vec::new();
    for line in kept {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    (out, kept_count, total)
}

pub(crate) fn sanitize_jsonl_file(path: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    let (cleaned, kept, total) = sanitize_jsonl_bytes(&bytes);
    if kept != total {
        std::fs::write(path, cleaned)?;
    }
    Ok(())
}

/// Restrict a bundle directory to its owner where the OS supports it.
/// Bundles hold full transcripts; on Windows the default user-profile ACL
/// applies and the docs tell users to keep bundles in a private folder.
pub(crate) fn restrict_dir_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

pub(crate) fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    Ok(())
}

/// Copy a directory tree, skipping entries the predicate rejects by file
/// name. Used for agent session folders, which sit beside lock files,
/// databases, and output spools that must never enter a bundle.
pub(crate) fn copy_tree_filtered(
    src: &Path,
    dst: &Path,
    keep: &dyn Fn(&str, bool) -> bool,
) -> std::io::Result<usize> {
    let mut count = 0usize;
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type()?;
            // Never follow symlinks out of the session folder.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if keep(&name, true) {
                    stack.push(entry.path());
                }
            } else if file_type.is_file() && keep(&name, false) {
                let relative = entry
                    .path()
                    .strip_prefix(src)
                    .map_err(std::io::Error::other)?
                    .to_path_buf();
                copy_file(&entry.path(), &dst.join(relative))?;
                count += 1;
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
pub(crate) fn test_temp_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "herdr-bundle-{name}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create test temp dir");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn maps(pairs: &[(&str, &str)]) -> Vec<PathMap> {
        pairs
            .iter()
            .map(|(old, new)| PathMap {
                old: (*old).to_string(),
                new: (*new).to_string(),
            })
            .collect()
    }

    #[test]
    fn map_path_rewrites_prefix_and_exact_match() {
        let maps = maps(&[("/home/ada/work", "/Users/ada/work")]);
        assert_eq!(
            map_path("/home/ada/work/app/src", &maps),
            localize_path("/Users/ada/work/app/src")
        );
        assert_eq!(
            map_path("/home/ada/work", &maps),
            localize_path("/Users/ada/work")
        );
        assert_eq!(
            map_path("/home/ada/workshop", &maps),
            localize_path("/home/ada/workshop")
        );
        assert_eq!(map_path("/elsewhere", &maps), localize_path("/elsewhere"));
    }

    #[test]
    fn map_path_matches_windows_source_separators() {
        let maps = maps(&[("C:\\Users\\ada\\work", "/home/ada/work")]);
        assert_eq!(
            map_path("C:\\Users\\ada\\work\\app", &maps),
            localize_path("/home/ada/work/app")
        );
    }

    #[test]
    fn path_map_parse_keeps_windows_drive_colon() {
        let parsed = PathMap::parse("C:\\Users\\ada=/home/ada").unwrap();
        assert_eq!(parsed.old, "C:\\Users\\ada");
        assert_eq!(parsed.new, "/home/ada");
        assert!(PathMap::parse("no-equals").is_err());
        assert!(PathMap::parse("=only-new").is_err());
        assert!(PathMap::parse("only-old=").is_err());
    }

    #[test]
    fn claude_key_replaces_every_separator() {
        assert_eq!(
            claude_project_key("/home/ada/my app/x.y"),
            "-home-ada-my-app-x-y"
        );
        assert_eq!(
            claude_project_key("C:\\Users\\ada\\app"),
            "C--Users-ada-app"
        );
    }

    #[test]
    fn remote_url_credentials_are_redacted() {
        assert_eq!(
            redact_remote_url("https://user:token@github.com/ada/app.git"),
            "https://github.com/ada/app.git"
        );
        assert_eq!(
            redact_remote_url("https://github.com/ada/app.git"),
            "https://github.com/ada/app.git"
        );
        assert_eq!(
            redact_remote_url("git@github.com:ada/app.git"),
            "git@github.com:ada/app.git"
        );
    }

    #[test]
    fn jsonl_sanitize_drops_torn_tail_only() {
        let (cleaned, kept, total) = sanitize_jsonl_bytes(b"{\"a\":1}\n{\"b\":2}\n{\"c\":");
        assert_eq!((kept, total), (2, 3));
        assert_eq!(cleaned, b"{\"a\":1}\n{\"b\":2}\n");
        let (cleaned, kept, total) = sanitize_jsonl_bytes(b"{\"a\":1}\r\n\n");
        assert_eq!((kept, total), (1, 1));
        assert_eq!(cleaned, b"{\"a\":1}\n");
    }

    #[test]
    fn home_resolution_prefers_windows_profile() {
        let home = home_dir_from_env(true, |key| match key {
            "USERPROFILE" => Some("C:\\Users\\ada".into()),
            "HOME" => Some("/home/ada".into()),
            _ => None,
        });
        assert_eq!(home, Some(PathBuf::from("C:\\Users\\ada")));
        let home = home_dir_from_env(false, |key| match key {
            "HOME" => Some("/home/ada".into()),
            _ => None,
        });
        assert_eq!(home, Some(PathBuf::from("/home/ada")));
    }

    #[test]
    fn absolute_detection_covers_both_os_families() {
        assert!(path_is_absolute_any("/home/ada"));
        assert!(path_is_absolute_any("C:\\Users\\ada"));
        assert!(path_is_absolute_any("C:/Users/ada"));
        assert!(path_is_absolute_any("\\\\server\\share"));
        assert!(!path_is_absolute_any("relative/path"));
        assert!(!path_is_absolute_any("C:relative"));
    }
}
