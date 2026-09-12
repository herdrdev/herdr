use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::Stream as _;

use super::EndpointTransport;
use crate::ipc::LocalStream;
use crate::protocol::ClientMessage;

const MAX_QUEUED_MESSAGES: usize = 256;
const MAX_QUEUED_BYTES: usize = 2 * crate::protocol::MAX_GRAPHICS_FRAME_SIZE;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const IO_POLL_INTERVAL: Duration = Duration::from_millis(2);

enum WriterCommand {
    Frame(Vec<u8>),
    Flush(mpsc::Sender<()>),
}

/// The UI only enqueues complete frames. A worker owns partial writes, cancellation, and the
/// bridge lifetime, so neither socket backpressure nor bridge teardown can block other endpoints.
pub(crate) struct NativeEndpointTransport {
    sender: mpsc::SyncSender<WriterCommand>,
    queued_bytes: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    error: Arc<Mutex<Option<io::Error>>>,
}

impl NativeEndpointTransport {
    pub(crate) fn with_lifetime(
        mut stream: LocalStream,
        lifetime: impl Send + 'static,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        let (sender, receiver) = mpsc::sync_channel::<WriterCommand>(MAX_QUEUED_MESSAGES);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let error = Arc::new(Mutex::new(None));
        let worker_bytes = queued_bytes.clone();
        let worker_stop = stopped.clone();
        let worker_error = error.clone();
        std::thread::Builder::new()
            .name("endpoint-writer".into())
            .spawn(move || {
                let _lifetime = lifetime;
                while let Ok(command) = receiver.recv() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let frame = match command {
                        WriterCommand::Frame(frame) => frame,
                        WriterCommand::Flush(done) => {
                            let _ = done.send(());
                            continue;
                        }
                    };
                    let result = write_frame(&mut stream, &frame, &worker_stop);
                    worker_bytes.fetch_sub(frame.len(), Ordering::AcqRel);
                    if let Err(error) = result {
                        if let Ok(mut slot) = worker_error.lock() {
                            *slot = Some(error);
                        }
                        worker_stop.store(true, Ordering::Release);
                        break;
                    }
                }
            })?;
        Ok(Self {
            sender,
            queued_bytes,
            stopped,
            error,
        })
    }

    pub(crate) fn stop_handle(&self) -> Arc<AtomicBool> {
        self.stopped.clone()
    }
}

impl EndpointTransport for NativeEndpointTransport {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "endpoint writer stopped",
            ));
        }
        let mut frame = Vec::new();
        crate::protocol::write_message(&mut frame, message)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let len = frame.len();
        if self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bytes| {
                bytes
                    .checked_add(len)
                    .filter(|total| *total <= MAX_QUEUED_BYTES)
            })
            .is_err()
        {
            return Err(queue_full());
        }
        self.sender
            .try_send(WriterCommand::Frame(frame))
            .map_err(|error| {
                self.queued_bytes.fetch_sub(len, Ordering::AcqRel);
                match error {
                    mpsc::TrySendError::Full(_) => queue_full(),
                    mpsc::TrySendError::Disconnected(_) => {
                        io::Error::new(io::ErrorKind::BrokenPipe, "endpoint writer stopped")
                    }
                }
            })
    }

    fn disconnect(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }

    fn flush(&mut self, deadline: Instant) -> io::Result<()> {
        let (done, completion) = mpsc::channel();
        self.sender
            .try_send(WriterCommand::Flush(done))
            .map_err(|_| queue_full())?;
        completion
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    io::Error::new(io::ErrorKind::TimedOut, "endpoint flush timed out")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "endpoint writer stopped")
                }
            })
    }

    fn take_error(&mut self) -> Option<io::Error> {
        self.error.lock().ok()?.take()
    }
}

impl Drop for NativeEndpointTransport {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn queue_full() -> io::Error {
    // A message may already be partially written. Retrying or dropping only this message would
    // lose input ordering; revoke the connection and recover through the normal lifecycle.
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "endpoint output queue is full",
    )
}

fn write_frame(
    writer: &mut impl io::Write,
    mut frame: &[u8],
    stopped: &AtomicBool,
) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    #[cfg(windows)]
    let mut deadline = deadline;
    while !frame.is_empty() && !stopped.load(Ordering::Acquire) {
        // Match interprocess's 512-byte pipe buffer hint: larger nonblocking Windows
        // writes can make no progress when the peer polls instead of blocking on read.
        #[cfg(windows)]
        let chunk = &frame[..frame.len().min(512)];
        #[cfg(not(windows))]
        let chunk = frame;
        match writer.write(chunk) {
            Ok(0) => {}
            Ok(written) => {
                frame = &frame[written..];
                #[cfg(windows)]
                {
                    deadline = Instant::now() + WRITE_TIMEOUT;
                }
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "endpoint write timed out",
            ));
        }
        std::thread::sleep(IO_POLL_INTERVAL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streams() -> (LocalStream, LocalStream, std::path::PathBuf) {
        use interprocess::local_socket::traits::Listener as _;
        let path = std::env::temp_dir().join(format!(
            "herdr-writer-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let accepting = std::thread::spawn(move || listener.accept().unwrap());
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        (client, accepting.join().unwrap(), path)
    }

    #[test]
    fn native_endpoint_writer_delivers_ordered_protocol_frames() {
        let (stream, mut peer, path) = streams();
        let mut transport = NativeEndpointTransport::with_lifetime(stream, ()).unwrap();
        let (done, received) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let first: ClientMessage =
                crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE).unwrap();
            let second: ClientMessage =
                crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE).unwrap();
            done.send((first, second)).unwrap();
        });
        transport
            .send(&ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        transport
            .send(&ClientMessage::ClientShellFocus { focused: false })
            .unwrap();
        let messages = received.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            messages,
            (
                ClientMessage::ClientShellFocus { focused: true },
                ClientMessage::ClientShellFocus { focused: false }
            )
        );
        reader.join().unwrap();
        drop(transport);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registry_exit_flushes_queued_input_and_a_complete_detach() {
        let (stream, mut peer, path) = streams();
        let transport = NativeEndpointTransport::with_lifetime(stream, ()).unwrap();
        let (done, received) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let result = (|| {
                let first: ClientMessage =
                    crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE)?;
                let second: ClientMessage =
                    crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE)?;
                Ok::<_, crate::protocol::FramingError>((first, second))
            })();
            done.send(result).unwrap();
        });
        let mut registry = super::super::EndpointRegistry::new(
            transport,
            1,
            super::super::EndpointNegotiation::default(),
        );
        let input = ClientMessage::Input {
            data: b"queued input".to_vec(),
        };
        assert_eq!(
            registry.send(&input),
            super::super::EndpointSendOutcome::Sent
        );
        drop(registry);
        let (first, second) = received
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .expect("clean exit must flush complete frames before closing");
        assert_eq!(first, input);
        assert_eq!(second, ClientMessage::Detach);
        reader.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_endpoint_flush_drains_large_frames_before_detach() {
        // The SSH bridge polls for available bytes instead of posting a blocking read.
        struct PollingPeer(LocalStream);
        impl io::Read for PollingPeer {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                loop {
                    match crate::ipc::poll_local_stream_read_count(&mut self.0, buffer)? {
                        crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                        crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                        crate::ipc::LocalStreamReadCount::Pending => {
                            std::thread::sleep(IO_POLL_INTERVAL);
                        }
                    }
                }
            }
        }
        let (stream, peer, path) = streams();
        peer.set_nonblocking(true).unwrap();
        let mut peer = PollingPeer(peer);
        let mut transport = NativeEndpointTransport::with_lifetime(stream, ()).unwrap();
        let (done, received) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let first: ClientMessage =
                crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE).unwrap();
            let second: ClientMessage =
                crate::protocol::read_message(&mut peer, crate::protocol::MAX_FRAME_SIZE).unwrap();
            done.send((first, second)).unwrap();
        });
        let input = ClientMessage::Input {
            data: vec![b'x'; 1024 * 1024],
        };
        transport.send(&input).unwrap();
        transport.send(&ClientMessage::Detach).unwrap();
        // Large-frame correctness must not depend on the registry's short exit grace period.
        transport
            .flush(Instant::now() + Duration::from_secs(10))
            .unwrap();
        drop(transport);
        let (first, second) = received.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(first, input);
        assert_eq!(second, ClientMessage::Detach);
        reader.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_endpoint_teardown_does_not_wait_for_a_stalled_peer() {
        struct Lifetime(mpsc::Sender<()>);
        impl Drop for Lifetime {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (stream, peer, path) = streams();
        let (done, dropped) = mpsc::channel();
        let mut transport = NativeEndpointTransport::with_lifetime(stream, Lifetime(done)).unwrap();
        transport
            .send(&ClientMessage::Input {
                data: vec![b'x'; 2 * 1024 * 1024],
            })
            .unwrap();
        drop(transport);
        dropped
            .recv_timeout(Duration::from_secs(3))
            .expect("worker lifetime is released without peer reads");
        drop(peer);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn partial_writes_preserve_frame_bytes() {
        struct PartialWriter(Vec<u8>);
        impl io::Write for PartialWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(3);
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut writer = PartialWriter(Vec::new());
        write_frame(&mut writer, b"first frame", &AtomicBool::new(false)).unwrap();
        write_frame(&mut writer, b"second frame", &AtomicBool::new(false)).unwrap();
        assert_eq!(writer.0, b"first framesecond frame");
    }

    #[test]
    fn a_stalled_write_is_cancellable_without_peer_progress() {
        struct StalledWriter(mpsc::Sender<()>);
        impl io::Write for StalledWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                let _ = self.0.send(());
                Err(io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (attempted, attempts) = mpsc::channel();
        let (done, completion) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = write_frame(&mut StalledWriter(attempted), b"input", &worker_stop);
            done.send(result).unwrap();
        });
        attempts.recv_timeout(Duration::from_secs(2)).unwrap();
        stop.store(true, Ordering::Release);
        completion
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn a_full_queue_is_a_connection_failure_not_silent_input_loss() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        let mut transport = NativeEndpointTransport {
            sender,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            stopped: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
        };
        transport.send(&ClientMessage::Detach).unwrap();
        assert_eq!(
            transport.send(&ClientMessage::Detach).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        transport
            .queued_bytes
            .store(MAX_QUEUED_BYTES, Ordering::Release);
        assert_eq!(
            transport.send(&ClientMessage::Detach).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
