use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
pub(crate) fn legacy_bash_hook_path(hook_path: &Path) -> std::path::PathBuf {
    hook_path.with_file_name("shepherd-agent-state.sh")
}

#[cfg(windows)]
pub(crate) fn remove_legacy_bash_hook_file(hook_path: &Path) -> io::Result<bool> {
    let legacy_path = legacy_bash_hook_path(hook_path);
    let content = match fs::read_to_string(&legacy_path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    if content.contains("SHEPHERD_INTEGRATION_ID=") {
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
