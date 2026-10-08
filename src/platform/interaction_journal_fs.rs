use crate::api::interaction_journal::digest;
use crate::api::schema::InteractionReceipt;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::{
    fs,
    io::{self, Read, Write},
    path::Path,
};
fn name(s: &std::ffi::OsStr) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(s.as_bytes()).map_err(io::Error::other)
}
pub(crate) fn open(
    parent: &fs::File,
    child: &std::ffi::OsStr,
    directory: bool,
    create: bool,
) -> io::Result<fs::File> {
    let child = name(child)?;
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | if directory {
            libc::O_RDONLY | libc::O_DIRECTORY
        } else if create {
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL
        } else {
            libc::O_RDONLY | libc::O_NONBLOCK
        };
    // SAFETY: valid dirfd and NUL-terminated child; exclusive file mode is0600.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), child.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned a new owned descriptor.
    Ok(unsafe { fs::File::from_raw_fd(fd) })
}
pub(crate) fn validate(file: &fs::File, directory: bool, private: bool) -> io::Result<()> {
    let m = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let mode = m.mode() & 0o7777;
    let kind = if directory {
        m.is_dir()
    } else {
        m.is_file() && m.nlink() == 1
    };
    let trusted = if private {
        m.uid() == uid && mode == if directory { 0o700 } else { 0o600 }
    } else {
        (m.uid() == uid || m.uid() == 0)
            && (mode & 0o022 == 0 || (m.uid() == 0 && mode & 0o1000 != 0))
    };
    if !kind || !trusted {
        return Err(io::Error::other("untrusted journal filesystem object"));
    }
    Ok(())
}
pub(crate) fn mkdir(parent: &fs::File, child: &std::ffi::OsStr) -> io::Result<()> {
    let child = name(child)?;
    // SAFETY: valid dirfd and NUL-terminated child.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), child.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    parent.sync_all()
}
pub(crate) fn root(path: &Path, create: bool) -> io::Result<fs::File> {
    if !path.is_absolute() {
        return Err(io::Error::other("journal root must be absolute"));
    }
    let mut dir = fs::File::open("/")?;
    let components: Vec<_> = path.components().collect();
    if components.len() < 2 {
        return Err(io::Error::other("invalid journal root"));
    }
    for (i, component) in components.iter().enumerate().skip(1) {
        let std::path::Component::Normal(child) = component else {
            return Err(io::Error::other("invalid journal path"));
        };
        let last = i + 1 == components.len();
        let next = match open(&dir, child, true, false) {
            Err(e) if last && create && e.kind() == io::ErrorKind::NotFound => {
                mkdir(&dir, child)?;
                open(&dir, child, true, false)?
            }
            other => other?,
        };
        validate(&next, true, last)?;
        dir = next;
    }
    Ok(dir)
}
pub(crate) fn operation(root: &fs::File, id: &str) -> io::Result<fs::File> {
    let dir = open(
        root,
        std::ffi::OsStr::new(&digest(id.as_bytes())),
        true,
        false,
    )?;
    validate(&dir, true, true)?;
    Ok(dir)
}
pub(crate) fn read(dir: &fs::File, filename: &str) -> io::Result<Vec<u8>> {
    let file = open(dir, filename.as_ref(), false, false)?;
    validate(&file, false, true)?;
    if file.metadata()?.len() > 1024 * 1024 {
        return Err(io::Error::other("oversized journal record"));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}
pub(crate) fn write(dir: &fs::File, filename: &str, value: &InteractionReceipt) -> io::Result<()> {
    let mut file = open(dir, filename.as_ref(), false, true)?;
    validate(&file, false, true)?;
    file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
    file.sync_all()?;
    dir.sync_all()
}
