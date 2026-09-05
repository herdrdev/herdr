use std::fs;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

#[cfg(unix)]
use interprocess::local_socket::traits::Stream as _;

pub(crate) type LocalListener = interprocess::local_socket::Listener;
pub(crate) type LocalStream = interprocess::local_socket::Stream;

/// Sockets carrying terminal traffic are owner-only.
#[cfg(unix)]
const SOCKET_PERMISSION_MODE: u32 = 0o600;

pub(crate) enum LocalStreamRead {
    Data,
    Pending,
    Closed,
}

pub(crate) enum LocalStreamReadCount {
    Data(usize),
    Pending,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SocketFileIdentity {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    marker: Vec<u8>,
}

pub(crate) fn connect_local_stream(path: &Path) -> io::Result<LocalStream> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{prelude::*, GenericFilePath};

        let name = path.to_fs_name::<GenericFilePath>()?;
        LocalStream::connect(name)
    }

    #[cfg(windows)]
    {
        use interprocess::local_socket::{prelude::*, GenericNamespaced};

        let name = path.to_string_lossy().to_string();
        let name = name.to_ns_name::<GenericNamespaced>()?;
        LocalStream::connect(name)
    }
}

/// Binds a listener without restricting who may connect. Production callers
/// want [`bind_private_local_listener`].
#[cfg(test)]
pub(crate) fn bind_local_listener(path: &Path) -> io::Result<LocalListener> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{prelude::*, GenericFilePath, ListenerOptions};

        let name = path.to_fs_name::<GenericFilePath>()?;
        ListenerOptions::new()
            .name(name)
            .reclaim_name(false)
            .create_sync()
    }

    #[cfg(windows)]
    {
        use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};

        let name = path.to_string_lossy().to_string();
        let name = name.to_ns_name::<GenericNamespaced>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .reclaim_name(false)
            .create_sync()?;
        fs::write(path, windows_socket_marker())?;
        Ok(listener)
    }
}

pub(crate) fn prepare_socket_path(
    path: &Path,
    busy_message: impl FnOnce(&Path) -> String,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    if !path.exists() {
        return Ok(());
    }

    match connect_local_stream(path) {
        Ok(_) => {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, busy_message(path)));
        }
        Err(err) if stale_socket_connect_error(err.kind()) => {}
        Err(err) => return Err(err),
    }

    if let Err(err) = fs::remove_file(path) {
        if err.kind() != io::ErrorKind::NotFound {
            return Err(err);
        }
    }

    Ok(())
}

fn stale_socket_connect_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound | io::ErrorKind::TimedOut
    ) || (cfg!(windows) && kind == io::ErrorKind::WouldBlock)
}

pub(crate) fn local_stream_peer_closed(stream: &mut LocalStream) -> io::Result<bool> {
    probe_stream_closed(stream)
}

pub(crate) fn set_local_stream_polling(stream: &mut LocalStream, enabled: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        stream.set_nonblocking(enabled)
    }

    #[cfg(windows)]
    {
        let _ = (stream, enabled);
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn shutdown_local_stream_write(stream: &LocalStream) -> io::Result<()> {
    match stream {
        LocalStream::UdSocket(stream) => stream.inner().shutdown(std::net::Shutdown::Write),
    }
}

/// Binds a listener for private terminal traffic. Unix applies the socket mode
/// to the descriptor before binding; Windows must set the named-pipe DACL at
/// creation.
pub(crate) fn bind_private_local_listener(path: &Path) -> io::Result<LocalListener> {
    #[cfg(unix)]
    {
        bind_private_socket(path, bind_local_listener_with_mode)
    }

    #[cfg(windows)]
    {
        use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
        use interprocess::os::windows::local_socket::ListenerOptionsExt as _;
        use interprocess::os::windows::security_descriptor::SecurityDescriptor;
        use widestring::U16CString;

        let sddl = U16CString::from_str("D:P(A;;GA;;;SY)(A;;GA;;;OW)")
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        let security_descriptor = SecurityDescriptor::deserialize(&sddl)?;
        let name = path.to_string_lossy().to_string();
        let name = name.to_ns_name::<GenericNamespaced>()?;
        let listener = ListenerOptions::new()
            .name(name)
            .reclaim_name(false)
            .security_descriptor(security_descriptor)
            .create_sync()?;
        fs::write(path, windows_socket_marker())?;
        Ok(listener)
    }
}

/// Binds a private listener for callers that need the std `UnixListener` type.
#[cfg(unix)]
pub(crate) fn bind_private_unix_listener(
    path: &Path,
) -> io::Result<std::os::unix::net::UnixListener> {
    match bind_private_socket(path, bind_local_listener_with_mode)? {
        // `reclaim_name(false)` is already set, and the conversion forgets the
        // reclaim guard, so unlinking stays the caller's responsibility.
        LocalListener::UdSocket(listener) => Ok(listener.into()),
    }
}

#[cfg(unix)]
fn bind_local_listener_with_mode(path: &Path, mode: Option<u32>) -> io::Result<LocalListener> {
    use interprocess::local_socket::{prelude::*, GenericFilePath, ListenerOptions};
    use interprocess::os::unix::local_socket::ListenerOptionsExt as _;

    let name = path.to_fs_name::<GenericFilePath>()?;
    let options = ListenerOptions::new().name(name).reclaim_name(false);
    match mode {
        Some(mode) => options.mode(mode as libc::mode_t).create_sync(),
        None => options.create_sync(),
    }
}

/// Binds `path` so that only the owner can connect.
///
/// The mode is requested on the socket descriptor before `bind(2)`, so the
/// pathname is never `chmod`ed. Filesystems that reject `fchmod` on a socket
/// (virtiofs, and macOS for any unbound socket) surface `Unsupported`; those
/// fall back to restricting the bound pathname instead. Any other bind error
/// stays fatal.
#[cfg(unix)]
fn bind_private_socket<T>(
    path: &Path,
    bind: impl FnMut(&Path, Option<u32>) -> io::Result<T>,
) -> io::Result<T> {
    bind_private_socket_with(path, bind, |path| {
        restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
    })
}

#[cfg(unix)]
fn bind_private_socket_with<T>(
    path: &Path,
    mut bind: impl FnMut(&Path, Option<u32>) -> io::Result<T>,
    restrict: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<T> {
    match bind(path, Some(SOCKET_PERMISSION_MODE)) {
        Ok(bound) => Ok(bound),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            let bound = bind(path, None)?;
            let identity = socket_file_identity(path)?;
            if let Err(err) = restrict(path) {
                // `reclaim_name(false)` means nothing unlinks the socket for
                // us, so an unrestricted pathname would stay published.
                drop(bound);
                let _ = remove_socket_file_if_owned(path, &identity);
                return Err(err);
            }
            Ok(bound)
        }
        Err(err) => Err(err),
    }
}

pub(crate) fn poll_local_stream_read(
    stream: &mut LocalStream,
    buf: &mut [u8],
) -> io::Result<LocalStreamRead> {
    match poll_local_stream_read_count(stream, buf)? {
        LocalStreamReadCount::Data(read) => {
            let _ = read;
            Ok(LocalStreamRead::Data)
        }
        LocalStreamReadCount::Pending => Ok(LocalStreamRead::Pending),
        LocalStreamReadCount::Closed => Ok(LocalStreamRead::Closed),
    }
}

pub(crate) fn poll_local_stream_read_count(
    stream: &mut LocalStream,
    buf: &mut [u8],
) -> io::Result<LocalStreamReadCount> {
    #[cfg(unix)]
    {
        match stream.read(buf) {
            Ok(0) => Ok(LocalStreamReadCount::Closed),
            Ok(read) => Ok(LocalStreamReadCount::Data(read)),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                Ok(LocalStreamReadCount::Pending)
            }
            Err(err) => Err(err),
        }
    }

    #[cfg(windows)]
    {
        match windows_named_pipe_available(stream)? {
            None => Ok(LocalStreamReadCount::Closed),
            Some(0) => Ok(LocalStreamReadCount::Pending),
            Some(_) => match stream.read(buf) {
                Ok(0) => Ok(LocalStreamReadCount::Closed),
                Ok(read) => Ok(LocalStreamReadCount::Data(read)),
                Err(err) if is_connection_closed_error(&err) => Ok(LocalStreamReadCount::Closed),
                Err(err) => Err(err),
            },
        }
    }
}

#[cfg(unix)]
fn probe_stream_closed(stream: &mut LocalStream) -> io::Result<bool> {
    stream.set_nonblocking(true)?;
    let mut probe = [0u8; 1];
    let status = match stream.read(&mut probe) {
        Ok(0) => Ok(true),
        Ok(_) => Ok(true),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(err) if is_connection_closed_error(&err) => Ok(true),
        Err(err) => Err(err),
    };
    stream.set_nonblocking(false)?;
    status
}

#[cfg(windows)]
fn probe_stream_closed(stream: &mut LocalStream) -> io::Result<bool> {
    Ok(windows_named_pipe_available(stream)?.is_none())
}

#[cfg(windows)]
fn windows_named_pipe_available(stream: &mut LocalStream) -> io::Result<Option<u32>> {
    use std::os::windows::io::{AsHandle, AsRawHandle};

    let LocalStream::NamedPipe(pipe) = stream;
    let mut available = 0;
    let ok = unsafe {
        windows_sys::Win32::System::Pipes::PeekNamedPipe(
            pipe.as_handle().as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        return Ok(Some(available));
    }

    let err = io::Error::last_os_error();
    if is_connection_closed_error(&err) || windows_named_pipe_closed_error(&err) {
        return Ok(None);
    }
    Err(err)
}

pub(crate) fn is_connection_closed_error(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::WriteZero
    )
}

#[cfg(windows)]
fn windows_named_pipe_closed_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(6 | 109 | 232 | 233))
}

pub(crate) fn socket_file_identity(path: &Path) -> io::Result<SocketFileIdentity> {
    #[cfg(windows)]
    {
        Ok(SocketFileIdentity {
            marker: fs::read(path)?,
        })
    }

    #[cfg(unix)]
    {
        let metadata = fs::metadata(path)?;
        Ok(SocketFileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
}

pub(crate) fn remove_socket_file_if_owned(
    path: &Path,
    identity: &SocketFileIdentity,
) -> io::Result<()> {
    let current = match socket_file_identity(path) {
        Ok(current) => current,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    if current != *identity {
        return Ok(());
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn windows_socket_marker() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}:{now}", std::process::id())
}

#[cfg(unix)]
fn restrict_socket_permissions(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use interprocess::local_socket::traits::Listener as _;
    #[cfg(windows)]
    use std::path::PathBuf;

    #[test]
    fn stale_socket_connect_errors_keep_unix_would_block_strict() {
        assert!(stale_socket_connect_error(io::ErrorKind::ConnectionRefused));
        assert!(stale_socket_connect_error(io::ErrorKind::NotFound));
        assert!(stale_socket_connect_error(io::ErrorKind::TimedOut));
        assert_eq!(
            stale_socket_connect_error(io::ErrorKind::WouldBlock),
            cfg!(windows)
        );
    }

    #[cfg(windows)]
    #[test]
    fn private_named_pipe_accepts_same_user() {
        use std::io::Write as _;

        let path = temp_socket_marker_path("private-pipe");
        let _ = fs::remove_file(&path);
        let listener = bind_private_local_listener(&path).unwrap();
        let mut client = connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();
        client.write_all(b"remote").unwrap();

        let mut buffer = [0_u8; 16];
        assert!(matches!(
            poll_local_stream_read_count(&mut server, &mut buffer).unwrap(),
            LocalStreamReadCount::Data(6)
        ));
        assert_eq!(&buffer[..6], b"remote");

        drop(client);
        drop(server);
        drop(listener);
        let _ = fs::remove_file(path);
    }

    #[cfg(windows)]
    #[test]
    fn remove_socket_file_if_owned_compares_windows_marker_contents() {
        let path = temp_socket_marker_path("same-len-marker");
        let _ = fs::remove_file(&path);

        fs::write(&path, b"marker-aa").expect("write first marker");
        let identity = socket_file_identity(&path).expect("read first identity");
        fs::write(&path, b"marker-bb").expect("replace with same-length marker");

        remove_socket_file_if_owned(&path, &identity).expect("remove owned marker");

        assert!(path.exists(), "same-length replacement marker must survive");

        let _ = fs::remove_file(&path);
    }

    #[cfg(windows)]
    #[test]
    fn idle_named_pipe_peer_is_not_treated_as_closed() {
        let path = temp_socket_marker_path("idle-pipe");
        let listener = bind_local_listener(&path).unwrap();
        let _client = connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();

        assert!(!local_stream_peer_closed(&mut server).unwrap());

        let _ = fs::remove_file(path);
    }

    #[cfg(windows)]
    #[test]
    fn disconnected_named_pipe_peer_is_treated_as_closed() {
        let path = temp_socket_marker_path("disconnected-pipe");
        let listener = bind_local_listener(&path).unwrap();
        let client = connect_local_stream(&path).unwrap();
        let mut server = listener.accept().unwrap();

        drop(client);

        assert!(local_stream_peer_closed(&mut server).unwrap());

        let _ = fs::remove_file(path);
    }

    #[cfg(windows)]
    fn temp_socket_marker_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("herdr-{name}-{}.sock", std::process::id()))
    }

    #[cfg(unix)]
    mod unix_socket_mode {
        use super::*;
        use std::sync::atomic::{AtomicU32, Ordering};

        fn unique_dir(name: &str) -> std::path::PathBuf {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "herdr-ipc-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn mode_of(path: &Path) -> u32 {
            fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn private_listener_binds_owner_only_without_touching_the_parent() {
            let dir = unique_dir("owner-only");
            let parent_mode = mode_of(&dir);
            let path = dir.join("herdr.sock");

            let listener = bind_private_local_listener(&path).unwrap();

            assert_eq!(mode_of(&path), SOCKET_PERMISSION_MODE);
            assert_eq!(
                mode_of(&dir),
                parent_mode,
                "binding must not modify the parent directory"
            );

            drop(listener);
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn private_unix_listener_binds_owner_only() {
            let dir = unique_dir("owner-only-std");
            let path = dir.join("herdr.sock");

            let listener = bind_private_unix_listener(&path).unwrap();

            assert_eq!(mode_of(&path), SOCKET_PERMISSION_MODE);

            drop(listener);
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn unsupported_creation_mode_falls_back_to_restricting_the_bound_socket() {
            let dir = unique_dir("fallback");
            let path = dir.join("herdr.sock");
            let attempts = std::cell::Cell::new(0);

            let listener = bind_private_socket(&path, |path, mode| {
                attempts.set(attempts.get() + 1);
                if mode.is_some() {
                    // Stand in for virtiofs, and for macOS, where `fchmod` on an
                    // unbound socket always fails this way.
                    return Err(io::Error::from(io::ErrorKind::Unsupported));
                }
                bind_local_listener(path)
            })
            .unwrap();

            assert_eq!(attempts.get(), 2, "the mode attempt must be retried once");
            assert_eq!(mode_of(&path), SOCKET_PERMISSION_MODE);

            drop(listener);
            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn creation_errors_other_than_unsupported_stay_fatal() {
            // `interprocess` maps `fchmod`'s `EINVAL` to `Unsupported`, so no
            // other kind may silently take the fallback path and bind a socket
            // that nothing restricts.
            for kind in [
                io::ErrorKind::InvalidInput,
                io::ErrorKind::PermissionDenied,
                io::ErrorKind::AddrInUse,
            ] {
                let dir = unique_dir("fatal");
                let path = dir.join("herdr.sock");
                let attempts = std::cell::Cell::new(0);

                let err = bind_private_socket(&path, |path, _mode| {
                    attempts.set(attempts.get() + 1);
                    let _ = path;
                    Err::<LocalListener, _>(io::Error::from(kind))
                })
                .unwrap_err();

                assert_eq!(err.kind(), kind);
                assert_eq!(attempts.get(), 1, "{kind:?} must not be retried");
                assert!(!path.exists());

                let _ = fs::remove_dir_all(&dir);
            }
        }

        #[test]
        fn failed_restriction_unpublishes_the_socket_it_bound() {
            let dir = unique_dir("restrict-fails");
            let path = dir.join("herdr.sock");

            let err = bind_private_socket_with(
                &path,
                |path, mode| {
                    if mode.is_some() {
                        return Err(io::Error::from(io::ErrorKind::Unsupported));
                    }
                    bind_local_listener(path)
                },
                |_| Err(io::Error::from(io::ErrorKind::PermissionDenied)),
            )
            .unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert!(
                !path.exists(),
                "an unrestricted socket must not stay published"
            );

            let _ = fs::remove_dir_all(&dir);
        }

        #[test]
        fn failed_restriction_leaves_a_replaced_socket_alone() {
            let dir = unique_dir("replaced");
            let path = dir.join("herdr.sock");
            let mut survivor = None;

            let err = bind_private_socket_with(
                &path,
                |path, mode| {
                    if mode.is_some() {
                        return Err(io::Error::from(io::ErrorKind::Unsupported));
                    }
                    bind_local_listener(path)
                },
                |path| {
                    // Another process replaces the pathname after we bound it,
                    // and then our restriction fails. The replacement is not
                    // ours to unlink.
                    fs::remove_file(path)?;
                    survivor = Some(bind_local_listener(path)?);
                    Err(io::Error::from(io::ErrorKind::PermissionDenied))
                },
            )
            .unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
            assert!(path.exists(), "the replacement socket must survive");

            drop(survivor);
            let _ = fs::remove_dir_all(&dir);
        }
    }
}
