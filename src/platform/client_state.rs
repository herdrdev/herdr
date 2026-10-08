use std::path::Path;

#[cfg(not(windows))]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::create_remote_ssh_config_file(path)
}

#[cfg(windows)]
pub(crate) fn create_private_state_file(path: &Path) -> std::io::Result<std::fs::File> {
    super::windows::create_remote_ssh_config_file(path)
}

#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    super::windows::replace_file(source, destination)
}

#[cfg(not(windows))]
pub(crate) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
pub(crate) fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    // replace_file uses MOVEFILE_WRITE_THROUGH on Windows.
    Ok(())
}

/// The journal requires durable directory entries, unsupported on platforms without them.
pub(crate) const DURABLE_INTERACTION_JOURNAL_SUPPORTED: bool = cfg!(unix);

#[cfg(all(test, unix))]
pub(crate) fn create_private_state_directory(path: &Path) -> std::io::Result<()> {
    super::unix_common::create_remote_private_dir(path)
}

#[cfg(all(test, not(unix)))]
pub(crate) fn create_private_state_directory(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)
}
