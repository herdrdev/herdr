//! Remote-host side of the SSH stdio bridge.

use interprocess::TryClone as _;
use std::io;
#[cfg(windows)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
const BRIDGE_READ_POLL: Duration = Duration::from_millis(1);

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    ensure_remote_server_running()?;

    let socket_path = crate::server::socket_paths::client_socket_path();
    let stream = crate::ipc::connect_local_stream(&socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to remote Herdr client socket {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    let mut stdout = io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;
    #[cfg(windows)]
    let upload_done = Arc::new(AtomicBool::new(false));
    #[cfg(windows)]
    let upload_done_worker = Arc::clone(&upload_done);

    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let _ = copy_flush(&mut stdin, &mut stdin_to_socket);
        #[cfg(unix)]
        let _ = crate::ipc::shutdown_local_stream_write(&stdin_to_socket);
        #[cfg(windows)]
        upload_done_worker.store(true, Ordering::Release);
    });

    #[cfg(unix)]
    {
        copy_flush(&mut socket_to_stdout, &mut stdout).map(|_| ())
    }
    #[cfg(windows)]
    {
        copy_socket_to_stdout(&mut socket_to_stdout, &mut stdout, &upload_done).map(|_| ())
    }
}

#[cfg(windows)]
fn copy_socket_to_stdout<W: io::Write>(
    stream: &mut crate::ipc::LocalStream,
    stdout: &mut W,
    upload_done: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;
    while !upload_done.load(Ordering::Acquire) {
        match crate::ipc::poll_local_stream_read_count(stream, &mut buffer)? {
            crate::ipc::LocalStreamReadCount::Data(read) => {
                stdout.write_all(&buffer[..read])?;
                stdout.flush()?;
                total += read as u64;
            }
            crate::ipc::LocalStreamReadCount::Pending => thread::sleep(BRIDGE_READ_POLL),
            crate::ipc::LocalStreamReadCount::Closed => break,
        }
    }
    Ok(total)
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        writer.write_all(&buffer[..read])?;
        writer.flush()?;
        total += read as u64;
    }
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation)
            == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
        {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote herdr server needs one final update before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve it",
        ));
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}
