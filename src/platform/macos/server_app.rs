//! A stable app identity for servers started outside a GUI terminal (herdr#808).

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(super) const BUNDLE_ID: &str = "dev.herdr.server";
const EXECUTABLE: &str = "Contents/MacOS/herdr-server-launcher";
const PLIST: &[u8] = include_bytes!("server_app/Info.plist");
const LAUNCHER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/herdr-server-launcher"));

pub(super) fn daemon_command(command: &Command) -> io::Result<Command> {
    let root = data_home()?.join("herdr");
    // Keep the installed helper across CLI updates: replacing its ad hoc
    // signature invalidates privacy grants. Change this version only when
    // updating the helper itself, with a documented permission migration.
    let app = root.join("server-app-v1/Herdr Server.app");
    ensure_app(&app)?;
    register_app(&app)?;
    let mut wrapped = wrap_command(command, &app.join(EXECUTABLE));
    crate::platform::detach_server_daemon_command(&mut wrapped);
    Ok(wrapped)
}

fn data_home() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        if path.is_absolute() {
            return Ok(path);
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| path.join(".local/share"))
        .ok_or_else(|| io::Error::other("cannot locate the macOS server app data directory"))
}

fn wrap_command(command: &Command, helper: &Path) -> Command {
    let mut wrapped = Command::new(helper);
    wrapped.arg(command.get_program()).args(command.get_args());
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            wrapped.env(key, value);
        } else {
            wrapped.env_remove(key);
        }
    }
    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    wrapped
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    wrapped
}

fn ensure_app(app: &Path) -> io::Result<()> {
    if app.exists() {
        return verify_app(app);
    }
    let parent = app
        .parent()
        .ok_or_else(|| io::Error::other("invalid server app path"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let staging = super::launchd_spawn::PrivateDir::create_in(parent)?;
    let staged_app = staging.path().join("Herdr Server.app");
    std::fs::create_dir_all(staged_app.join("Contents/MacOS"))?;
    std::fs::write(staged_app.join("Contents/Info.plist"), PLIST)?;
    let mut executable = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(staged_app.join(EXECUTABLE))?;
    std::io::Write::write_all(&mut executable, LAUNCHER)?;
    drop(executable);
    let status = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--identifier", BUNDLE_ID])
        .arg(&staged_app)
        .output()?;
    if !status.status.success() {
        return Err(io::Error::other(format!(
            "could not sign Herdr Server.app: {}",
            String::from_utf8_lossy(&status.stderr)
        )));
    }
    // Publish a complete bundle once. A concurrent startup may have won;
    // never replace an installed helper or its existing privacy identity.
    if let Err(error) = std::fs::rename(&staged_app, app) {
        if !app.exists() {
            return Err(error);
        }
    }
    verify_app(app)
}

fn verify_app(app: &Path) -> io::Result<()> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict"])
        .arg(app)
        .output()?;
    if output.status.success()
        && app.join(EXECUTABLE).is_file()
        && std::fs::read(app.join("Contents/Info.plist"))? == PLIST
    {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "invalid macOS server app at {}; restore the bundle before starting the server",
            app.display()
        )))
    }
}

fn register_app(app: &Path) -> io::Result<()> {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            bytes: *const u8,
            len: isize,
            directory: u8,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }
    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSRegisterURL(url: *const c_void, update: u8) -> i32;
    }
    let bytes = app.as_os_str().as_bytes();
    // SAFETY: bytes remain alive during URL creation; the returned owned CF
    // reference is released after the synchronous Launch Services call.
    unsafe {
        let url = CFURLCreateFromFileSystemRepresentation(
            std::ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            1,
        );
        if url.is_null() {
            return Err(io::Error::other("could not create server app URL"));
        }
        let status = LSRegisterURL(url, 1);
        CFRelease(url);
        if status != 0 {
            return Err(io::Error::other(format!(
                "could not register server app (OSStatus {status})"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "server_app_tests.rs"]
mod tests;
