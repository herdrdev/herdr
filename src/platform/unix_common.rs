use std::path::{Path, PathBuf};

pub(crate) fn remote_ssh_config_paths() -> super::RemoteSshConfigPaths {
    super::RemoteSshConfigPaths {
        user_config: std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".ssh").join("config")),
        system_config: Some(PathBuf::from("/etc/ssh/ssh_config")),
        multiplexing: true,
    }
}

pub(crate) fn create_remote_ssh_config_dir(control_socket_name: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    let mut bases = vec![std::env::temp_dir()];
    let short_tmp = PathBuf::from("/tmp");
    if bases.first() != Some(&short_tmp) {
        bases.push(short_tmp);
    }

    let mut last_error = None;
    let mut path_fits = false;
    for base in bases {
        for attempt in 0..100 {
            let dir = base.join(format!("herdr-ssh-{}-{attempt}", std::process::id()));
            if !fits_unix_socket_path(&dir.join(control_socket_name)) {
                continue;
            }
            path_fits = true;
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(dir),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    last_error = Some(err);
                    break;
                }
            }
        }
    }

    if let Some(err) = last_error {
        return Err(err);
    }
    let message = if path_fits {
        "failed to create private herdr ssh config directory"
    } else {
        "SSH control socket path exceeds the Unix socket length limit"
    };
    Err(std::io::Error::new(
        if path_fits {
            std::io::ErrorKind::AlreadyExists
        } else {
            std::io::ErrorKind::InvalidInput
        },
        message,
    ))
}

pub(crate) fn create_remote_ssh_config_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

pub(crate) fn create_remote_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new().mode(0o700).create(path)
}

pub(crate) fn remote_private_temp_base() -> PathBuf {
    std::env::temp_dir()
}

pub(crate) fn remote_bridge_endpoint_path(readable_name: &str, short_name: &str) -> PathBuf {
    let tmp = std::env::temp_dir();
    let readable = tmp.join(readable_name);
    if fits_unix_socket_path(&readable) {
        return readable;
    }
    let short = tmp.join(short_name);
    if fits_unix_socket_path(&short) {
        return short;
    }
    PathBuf::from("/tmp").join(short_name)
}

pub(crate) fn remote_reattach_program(program: &str) -> String {
    shell_quote(if program.is_empty() { "herdr" } else { program })
}

pub(crate) fn remote_reattach_argument(value: &str) -> String {
    shell_quote(value)
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len() <= 103
}

/// The machine's node name, as shown by tmux's `#h`.
pub(crate) fn hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    let result =
        unsafe { libc::gethostname(buffer.as_mut_ptr().cast::<libc::c_char>(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
    (!name.is_empty()).then_some(name)
}

pub(crate) fn local_datetime() -> Option<time::PrimitiveDateTime> {
    let mut timestamp: libc::time_t = 0;
    if unsafe { libc::time(&mut timestamp) } == -1 {
        return None;
    }
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return None;
    }
    datetime_from_tm(&local)
}

pub(crate) fn status_commands_supported() -> bool {
    true
}

pub(crate) fn configure_status_command(process: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    process.process_group(0);
}

pub(crate) struct StatusCommandGuard {
    process_group_id: Option<i32>,
}

impl StatusCommandGuard {
    pub(crate) fn new(child: &tokio::process::Child) -> std::io::Result<Self> {
        let process_id = child
            .id()
            .ok_or_else(|| std::io::Error::other("status command has no process id"))?;
        let process_group_id = i32::try_from(process_id)
            .map_err(|_| std::io::Error::other("status command process id exceeds i32"))?;
        Ok(Self {
            process_group_id: Some(process_group_id),
        })
    }
}

impl StatusCommandGuard {
    pub(crate) fn terminate(&mut self) {
        if let Some(process_group_id) = self.process_group_id.take() {
            // The command was spawned as this process group's leader. Killing the
            // group also cleans up background descendants on completion/cancellation.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

impl Drop for StatusCommandGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn datetime_from_tm(value: &libc::tm) -> Option<time::PrimitiveDateTime> {
    let month = time::Month::try_from(u8::try_from(value.tm_mon + 1).ok()?).ok()?;
    let date = time::Date::from_calendar_date(
        value.tm_year + 1900,
        month,
        u8::try_from(value.tm_mday).ok()?,
    )
    .ok()?;
    let time = time::Time::from_hms(
        u8::try_from(value.tm_hour).ok()?,
        u8::try_from(value.tm_min).ok()?,
        u8::try_from(value.tm_sec).ok()?,
    )
    .ok()?;
    Some(time::PrimitiveDateTime::new(date, time))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_ssh_config_dir_rejects_overlong_control_socket_name() {
        let err = create_remote_ssh_config_dir(&"x".repeat(200)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
