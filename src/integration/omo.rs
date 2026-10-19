use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const INSTALL_NAME: &str = "herdr-omo.js";
const MARKER_PREFIX: &str = "// managed by omo-herdr; sha256=";
const REPORTER: &str = include_str!("assets/omo/herdr-omo.js");
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OmoState {
    NotInstalled,
    Current,
    Outdated,
    Modified,
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OmoStatus {
    pub(crate) path: PathBuf,
    pub(crate) available: bool,
    pub(crate) state: OmoState,
}

fn agent_dir() -> io::Result<PathBuf> {
    for key in [
        "OMO_CODING_AGENT_DIR",
        "SENPI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_DIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            let value = value.to_string_lossy();
            let value = value.trim();
            if !value.is_empty() {
                return std::path::absolute(Path::new(value));
            }
        }
    }
    std::path::absolute(super::env::home_dir()?.join(".omo").join("agent"))
}

fn target() -> io::Result<PathBuf> {
    Ok(agent_dir()?.join("extensions").join(INSTALL_NAME))
}

fn available() -> bool {
    super::registry::command_available("omo")
        || agent_dir().is_ok_and(|directory| directory.is_dir())
}

fn digest(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}

fn managed_body(content: &[u8]) -> Result<Option<&[u8]>, OmoState> {
    let line_end = content.iter().position(|byte| *byte == b'\n');
    let marker = line_end.map_or(content, |index| &content[..index]);
    if !marker.starts_with(MARKER_PREFIX.as_bytes()) {
        return Ok(None);
    }
    let recorded =
        std::str::from_utf8(&marker[MARKER_PREFIX.len()..]).map_err(|_| OmoState::Modified)?;
    let body = line_end.map_or(&[][..], |index| &content[index + 1..]);
    if recorded.len() != 64
        || !recorded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || digest(body) != recorded
    {
        return Err(OmoState::Modified);
    }
    Ok(Some(body))
}

fn inspect(path: &Path) -> io::Result<(OmoState, Option<Vec<u8>>)> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((OmoState::NotInstalled, None))
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok((OmoState::Foreign, None));
    }
    let bytes = fs::read(path)?;
    match managed_body(&bytes) {
        Ok(None) => Ok((OmoState::Foreign, None)),
        Err(state) => Ok((state, None)),
        Ok(Some(body)) if body == REPORTER.as_bytes() => {
            Ok((OmoState::Current, Some(body.to_vec())))
        }
        Ok(Some(body)) => Ok((OmoState::Outdated, Some(body.to_vec()))),
    }
}

fn wrapped_reporter() -> Vec<u8> {
    format!("{MARKER_PREFIX}{}\n{REPORTER}", digest(REPORTER.as_bytes())).into_bytes()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "extension path has no parent")
    })?;
    fs::create_dir_all(directory)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(
        ".{INSTALL_NAME}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn require_owned(path: &Path) -> io::Result<OmoState> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(OmoState::Foreign);
    }
    match managed_body(&fs::read(path)?) {
        Ok(Some(_)) => Ok(OmoState::Current),
        Ok(None) => Ok(OmoState::Foreign),
        Err(_) => Ok(OmoState::Modified),
    }
}

pub(crate) fn status() -> io::Result<OmoStatus> {
    let path = target()?;
    let (state, _) = inspect(&path)?;
    Ok(OmoStatus {
        path,
        available: available(),
        state,
    })
}

pub(crate) fn install() -> io::Result<Vec<String>> {
    let path = target()?;
    let (state, _) = inspect(&path)?;
    match state {
        OmoState::Current => Ok(vec![format!(
            "Omo integration is current at {}",
            path.display()
        )]),
        OmoState::NotInstalled | OmoState::Outdated => {
            write_atomic(&path, &wrapped_reporter())?;
            Ok(vec![format!(
                "Omo integration {} at {}",
                if state == OmoState::NotInstalled {
                    "installed"
                } else {
                    "updated"
                },
                path.display()
            )])
        }
        OmoState::Modified => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing modified managed Omo integration at {}",
                path.display()
            ),
        )),
        OmoState::Foreign => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing foreign Omo integration at {}", path.display()),
        )),
    }
}

pub(crate) fn uninstall() -> io::Result<Vec<String>> {
    let path = target()?;
    match require_owned(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(vec![format!(
            "Omo integration is not installed at {}",
            path.display()
        )]),
        Err(error) => Err(error),
        Ok(OmoState::Foreign) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing foreign Omo integration at {}", path.display()),
        )),
        Ok(OmoState::Modified) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "refusing modified managed Omo integration at {}",
                path.display()
            ),
        )),
        Ok(_) => {
            fs::remove_file(&path)?;
            Ok(vec![format!(
                "Omo integration removed from {}",
                path.display()
            )])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::env::{integration_env_lock, IntegrationEnvLock};
    use super::*;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    const ENV_KEYS: [&str; 3] = [
        "OMO_CODING_AGENT_DIR",
        "SENPI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_DIR",
    ];

    struct TestDir {
        path: PathBuf,
        saved: [Option<OsString>; 3],
        _guard: IntegrationEnvLock,
    }

    impl TestDir {
        fn new() -> Self {
            let guard = integration_env_lock();
            let saved = ENV_KEYS.map(std::env::var_os);
            let path = std::env::temp_dir().join(format!(
                "herdr-omo-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::env::set_var("OMO_CODING_AGENT_DIR", &path);
            std::env::remove_var("SENPI_CODING_AGENT_DIR");
            std::env::remove_var("PI_CODING_AGENT_DIR");
            fs::create_dir_all(path.join("extensions")).expect("create test extensions directory");
            Self {
                path,
                saved,
                _guard: guard,
            }
        }

        fn target(&self) -> PathBuf {
            self.path.join("extensions").join(INSTALL_NAME)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            for (key, value) in ENV_KEYS.into_iter().zip(&self.saved) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn overrides_resolve_relative_to_working_directory_and_follow_precedence() {
        let _dir = TestDir::new();
        for (values, expected) in [
            (
                ["relative-omo", "relative-senpi", "relative-pi"],
                "relative-omo",
            ),
            ([" ", "relative-senpi", "relative-pi"], "relative-senpi"),
            (["", "", "relative-pi"], "relative-pi"),
        ] {
            for (key, value) in ENV_KEYS.into_iter().zip(values) {
                std::env::set_var(key, value);
            }
            assert_eq!(
                agent_dir().unwrap(),
                std::env::current_dir().unwrap().join(expected)
            );
        }
        for key in ENV_KEYS {
            std::env::remove_var(key);
        }
        assert_eq!(
            agent_dir().unwrap(),
            super::super::env::home_dir().unwrap().join(".omo/agent")
        );
    }

    #[test]
    fn status_distinguishes_current_outdated_modified_foreign_and_missing() {
        let dir = TestDir::new();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::NotInstalled);
        fs::write(dir.target(), wrapped_reporter()).unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Current);
        let outdated = format!("{MARKER_PREFIX}{}\nold reporter", digest(b"old reporter"));
        fs::write(dir.target(), outdated).unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Outdated);
        fs::write(
            dir.target(),
            format!("{MARKER_PREFIX}{}\nchanged", digest(b"original")),
        )
        .unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Modified);
        fs::write(dir.target(), "user-owned").unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Foreign);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_is_foreign_and_never_followed() {
        use std::os::unix::fs::symlink;
        let dir = TestDir::new();
        let outside = dir.path.join("outside");
        fs::write(&outside, wrapped_reporter()).unwrap();
        symlink(&outside, dir.target()).unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Foreign);
        assert!(install().is_err());
        assert!(uninstall().is_err());
        assert!(outside.exists());
    }

    #[test]
    fn install_is_idempotent_and_uninstall_removes_only_owned_file() {
        let dir = TestDir::new();
        install().unwrap();
        assert_eq!(inspect(&dir.target()).unwrap().0, OmoState::Current);
        let bytes = fs::read(dir.target()).unwrap();
        install().unwrap();
        assert_eq!(fs::read(dir.target()).unwrap(), bytes);
        fs::write(dir.path.join("extensions/unrelated.txt"), "keep").unwrap();
        uninstall().unwrap();
        assert!(!dir.target().exists());
        assert!(dir.path.join("extensions/unrelated.txt").exists());
    }

    #[test]
    fn modified_and_foreign_files_are_not_overwritten_or_removed() {
        let dir = TestDir::new();
        fs::write(
            dir.target(),
            format!("{MARKER_PREFIX}{}\nchanged", digest(b"original")),
        )
        .unwrap();
        let before = fs::read(dir.target()).unwrap();
        assert!(install().is_err());
        assert!(uninstall().is_err());
        assert_eq!(fs::read(dir.target()).unwrap(), before);
        fs::write(dir.target(), "foreign").unwrap();
        assert!(install().is_err());
        assert!(uninstall().is_err());
        assert_eq!(fs::read(dir.target()).unwrap(), b"foreign");
    }
}
