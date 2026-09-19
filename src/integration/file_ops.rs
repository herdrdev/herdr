use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn atomic_replace_asset(path: &Path, text: &str, executable: bool) -> io::Result<()> {
    replace_asset(path, executable, |file| file.write_all(text.as_bytes()))
}

fn replace_asset(
    path: &Path,
    executable: bool,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("asset has no parent directory"))?;
    let previous = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(io::Error::other(format!(
                "integration asset {} is not a regular file",
                path.display()
            )));
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".herdr-asset.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let prepared = (|| {
        write(&mut file)?;
        if let Some(permissions) = previous {
            file.set_permissions(permissions)?;
        }
        if executable {
            make_executable(&temporary)?;
        }
        file.sync_all()
    })();
    drop(file);
    if let Err(error) = prepared.and_then(|_| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = crate::platform::sync_directory_after_replace(parent) {
        tracing::warn!(%error, path = %path.display(), "integration asset replaced but directory sync failed");
    }
    Ok(())
}

pub(crate) fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
pub(crate) fn legacy_bash_hook_path(hook_path: &Path) -> std::path::PathBuf {
    hook_path.with_file_name("herdr-agent-state.sh")
}

#[cfg(windows)]
pub(crate) fn remove_legacy_bash_hook_file(hook_path: &Path) -> io::Result<bool> {
    let legacy_path = legacy_bash_hook_path(hook_path);
    let content = match fs::read_to_string(&legacy_path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    if content.contains("HERDR_INTEGRATION_ID=") {
        fs::remove_file(legacy_path)?;
        return Ok(true);
    }

    Ok(false)
}

#[cfg(not(windows))]
pub(crate) fn remove_legacy_bash_hook_file(_hook_path: &Path) -> io::Result<bool> {
    Ok(false)
}

pub(crate) fn remove_dir_all_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

pub(crate) fn make_executable(_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(_path, perms)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "herdr-asset-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn failed_asset_write_preserves_previous_bytes_and_cleans_temporary_file() {
        let fixture = Fixture::new();
        let path = fixture.0.join("hook");
        fs::write(&path, "previous").unwrap();
        let error = replace_asset(&path, true, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected write failure");
        assert_eq!(fs::read_to_string(&path).unwrap(), "previous");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
        atomic_replace_asset(&path, "complete", true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "complete");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_plugin_permissions_match_ordinary_install_writes() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let ordinary = fixture.0.join("ordinary");
        let atomic = fixture.0.join("atomic");
        fs::write(&ordinary, "plugin").unwrap();
        atomic_replace_asset(&atomic, "plugin", false).unwrap();
        assert_eq!(
            fs::metadata(&ordinary).unwrap().permissions().mode() & 0o777,
            fs::metadata(&atomic).unwrap().permissions().mode() & 0o777
        );
    }

    #[test]
    fn failed_asset_rename_cleans_temporary_file() {
        let fixture = Fixture::new();
        let path = fixture.0.join("hook");
        let error = replace_asset(&path, false, |file| {
            file.write_all(b"complete")?;
            fs::create_dir(&path)?;
            fs::write(path.join("user-data"), "untouched")
        });
        assert!(error.is_err());
        assert_eq!(
            fs::read_to_string(path.join("user-data")).unwrap(),
            "untouched"
        );
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn asset_install_rejects_symlinks_without_touching_their_targets() {
        let fixture = Fixture::new();
        let target = fixture.0.join("user-file");
        let path = fixture.0.join("hook");
        fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(atomic_replace_asset(&path, "replacement", true).is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
    }
}
