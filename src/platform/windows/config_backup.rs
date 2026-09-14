//! Recovery-backed writes for existing Windows integration configs.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
use std::path::{Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, MoveFileExW, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_ENCRYPTED,
};

#[cfg(test)]
mod tests;

fn backup_paths(target: &Path) -> (PathBuf, PathBuf) {
    let mut name = target.as_os_str().to_os_string();
    name.push(".herdr-backup");
    let complete = PathBuf::from(&name);
    name.push(".pending");
    (complete, PathBuf::from(name))
}

pub(super) fn check_recovery(target: &Path) -> io::Result<()> {
    let (complete, pending) = backup_paths(target);
    for (path, description) in [
        (&complete, "recovery copy"),
        (&pending, "unfinished backup"),
    ] {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "cannot update {}: {description} exists at {}; inspect the config and resolve this backup before retrying",
                    target.display(), path.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct PendingBackup(PathBuf);
impl Drop for PendingBackup {
    fn drop(&mut self) {
        // Only the incomplete name is owned by this cleanup. The completed
        // recovery copy must survive errors and must never be removed here.
        if let Err(error) = fs::remove_file(&self.0) {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(path = %self.0.display(), %error, "failed to remove incomplete config backup");
            }
        }
    }
}

// Internal observation points let native tests terminate a child at actual I/O
// boundaries. The production callback is a no-op; there is no runtime failpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    BackupChunk,
    BackupReady,
    Truncated,
    Committed,
}

pub(super) fn write_existing(target: &Path, contents: &[u8]) -> io::Result<bool> {
    write_observed(target, contents, |_| Ok(()))
}

fn check_source(file: &File, target: &Path) -> io::Result<()> {
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_ENCRYPTED != 0 {
        return Err(io::Error::other(format!(
            "cannot update {}: encrypted configs are not supported by recovery-backed writes; the original was not changed",
            target.display()
        )));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.nNumberOfLinks > 1 {
        return Err(io::Error::other(format!(
            "cannot update {}: config has multiple hard links; use a separate file or a symlink before retrying",
            target.display()
        )));
    }
    Ok(())
}

fn write_observed(
    target: &Path,
    contents: &[u8],
    mut observe: impl FnMut(Phase) -> io::Result<()>,
) -> io::Result<bool> {
    check_recovery(target)?;
    // Keep this handle for both backup reads and the destructive write. Do not
    // reopen the pathname after saving the original contents.
    let mut source = match OpenOptions::new().read(true).write(true).open(target) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    check_source(&source, target)?;
    let (complete, pending) = backup_paths(target);
    let mut backup = super::create_config_temporary(&pending, true)?;
    let pending = PendingBackup(pending);
    let preparation = (|| {
        let mut remaining = source.metadata()?.len();
        let mut buffer = [0_u8; 8192];
        while remaining != 0 {
            let requested = remaining.min(buffer.len() as u64) as usize;
            let count = source.read(&mut buffer[..requested])?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "config shortened during backup",
                ));
            }
            backup.write_all(&buffer[..count])?;
            remaining -= count as u64;
            observe(Phase::BackupChunk)?;
        }
        backup.sync_all()?;
        drop(backup);
        let from = super::extended_length_path(&pending.0)?;
        let to = super::extended_length_path(&complete)?;
        // No REPLACE_EXISTING: never overwrite a previous recovery copy.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok::<_, io::Error>(())
    })();
    preparation.map_err(|error| {
        io::Error::other(format!(
            "cannot back up {}; original not changed (unfinished backup: {}): {error}",
            target.display(),
            pending.0.display()
        ))
    })?;

    let update = (|| {
        observe(Phase::BackupReady)?;
        check_source(&source, target)?;
        source.rewind()?;
        source.set_len(0)?;
        observe(Phase::Truncated)?;
        source.write_all(contents)?;
        source.sync_all()?;
        Ok::<_, io::Error>(())
    })();
    update.map_err(|error| {
        io::Error::other(format!(
            "could not update {}; config may be incomplete; original contents retained at {}: {error}",
            target.display(), complete.display()
        ))
    })?;
    // Success here is after the new contents were synced. A cleanup failure is
    // not an unchanged-original failure, and must not trigger automatic rollback.
    observe(Phase::Committed).and_then(|()| fs::remove_file(&complete)).map_err(|error| {
        io::Error::other(format!(
            "updated {}, but recovery copy remains at {}; verify the config and remove that copy before retrying: {error}",
            target.display(), complete.display()
        ))
    })?;
    Ok(true)
}
