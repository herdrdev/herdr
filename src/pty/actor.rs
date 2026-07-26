#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows {
    use std::io::{Read, Write};
    use std::sync::{mpsc as std_mpsc, Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes;
    use portable_pty::{MasterPty, PtySize};
    use tokio::sync::mpsc;
    use tracing::{debug, warn};

    pub(crate) struct PtyReadResult {
        pub terminal_responses: Vec<Bytes>,
    }

    type ReadCallback = Box<dyn FnMut(&[u8]) -> PtyReadResult + Send + 'static>;
    type ReaderExitCallback = Box<dyn FnOnce() + Send + 'static>;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct PtyResize {
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    }

    struct PtyResizeRequest {
        resize: PtyResize,
        terminal_responses: Vec<Bytes>,
    }

    enum PtyIoDataCommand {
        WriteUserInput(Bytes),
        WriteSubmission {
            text: Bytes,
            enter: Bytes,
            delay: Duration,
        },
    }

    pub(crate) struct PtyIoActorConfig {
        pub pane_id: u32,
        pub master: Box<dyn MasterPty + Send>,
        pub initially_quiesced: bool,
        pub on_read: ReadCallback,
        pub on_reader_exit: Option<ReaderExitCallback>,
    }

    enum PtyIoControlCommand {
        Resize(PtyResizeRequest),
        Shutdown,
    }

    #[derive(Clone)]
    pub(crate) struct PtyIoActorHandle {
        data_tx: mpsc::Sender<PtyIoDataCommand>,
        control_tx: std_mpsc::Sender<PtyIoControlCommand>,
        accepting: Arc<Mutex<bool>>,
    }

    impl PtyIoActorHandle {
        pub(crate) async fn write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::SendError<Bytes>> {
            if !*self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                return Err(mpsc::error::SendError(bytes));
            }
            self.data_tx
                .send(PtyIoDataCommand::WriteUserInput(bytes))
                .await
                .map_err(|err| match err.0 {
                    PtyIoDataCommand::WriteUserInput(bytes) => mpsc::error::SendError(bytes),
                    PtyIoDataCommand::WriteSubmission { text, .. } => mpsc::error::SendError(text),
                })
        }

        pub(crate) fn try_write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
            if !*self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                return Err(mpsc::error::TrySendError::Closed(bytes));
            }
            self.data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(bytes))
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput(bytes)) => {
                        mpsc::error::TrySendError::Full(bytes)
                    }
                    mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput(bytes)) => {
                        mpsc::error::TrySendError::Closed(bytes)
                    }
                    _ => unreachable!("user input send returned a different command"),
                })
        }

        pub(crate) fn try_write_submission(
            &self,
            text: Bytes,
            enter: Bytes,
            delay: Duration,
        ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
            if !*self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
            {
                return Err(mpsc::error::TrySendError::Closed(text));
            }
            self.data_tx
                .try_send(PtyIoDataCommand::WriteSubmission { text, enter, delay })
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteSubmission {
                        text,
                        ..
                    }) => mpsc::error::TrySendError::Full(text),
                    mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteSubmission {
                        text,
                        ..
                    }) => mpsc::error::TrySendError::Closed(text),
                    _ => unreachable!("submission send returned a different command"),
                })
        }

        pub(crate) fn resize(
            &self,
            rows: u16,
            cols: u16,
            cell_width_px: u32,
            cell_height_px: u32,
            terminal_responses: Vec<Bytes>,
        ) {
            let _ = self
                .control_tx
                .send(PtyIoControlCommand::Resize(PtyResizeRequest {
                    resize: PtyResize {
                        rows,
                        cols,
                        cell_width_px,
                        cell_height_px,
                    },
                    terminal_responses,
                }));
        }

        pub(crate) fn shutdown(&self) {
            if let Ok(mut accepting) = self.accepting.lock() {
                *accepting = false;
            }
            let _ = self.control_tx.send(PtyIoControlCommand::Shutdown);
        }
    }

    pub(crate) struct PtyIoActor;

    impl PtyIoActor {
        pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
            let PtyIoActorConfig {
                pane_id,
                master,
                initially_quiesced,
                mut on_read,
                on_reader_exit,
            } = config;

            let mut reader = master
                .try_clone_reader()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let writer = master
                .take_writer()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let writer = Arc::new(Mutex::new(writer));
            let (data_tx, mut data_rx) = mpsc::channel::<PtyIoDataCommand>(1024);
            let (control_tx, control_rx) = std_mpsc::channel::<PtyIoControlCommand>();
            let accepting = Arc::new(Mutex::new(!initially_quiesced));

            {
                let writer = Arc::clone(&writer);
                std::thread::spawn(move || {
                    while let Some(command) = data_rx.blocking_recv() {
                        match command {
                            PtyIoDataCommand::WriteUserInput(bytes) => {
                                if write_all_locked(&writer, &bytes).is_err() {
                                    break;
                                }
                            }
                            PtyIoDataCommand::WriteSubmission { text, enter, delay } => {
                                if write_submission_locked(&writer, &text, &enter, delay).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    debug!(pane_id, "windows pty writer thread exiting");
                });
            }

            {
                let writer = Arc::clone(&writer);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let result = on_read(&buf[..n]);
                                for response in result.terminal_responses {
                                    if write_all_locked(&writer, &response).is_err() {
                                        break;
                                    }
                                }
                            }
                            Err(err) => {
                                debug!(pane_id, err = %err, "windows pty reader failed");
                                break;
                            }
                        }
                    }
                    if let Some(on_reader_exit) = on_reader_exit {
                        on_reader_exit();
                    }
                    debug!(pane_id, "windows pty reader thread exiting");
                });
            }

            {
                let writer = Arc::clone(&writer);
                std::thread::spawn(move || {
                    for command in control_rx {
                        match command {
                            PtyIoControlCommand::Resize(request) => {
                                let size = request.resize;
                                if let Err(err) = master.resize(PtySize {
                                    rows: size.rows,
                                    cols: size.cols,
                                    pixel_width: size.cell_width_px.min(u16::MAX as u32) as u16,
                                    pixel_height: size.cell_height_px.min(u16::MAX as u32) as u16,
                                }) {
                                    warn!(pane_id, err = %err, "windows pty resize failed");
                                }
                                for response in request.terminal_responses {
                                    if write_all_locked(&writer, &response).is_err() {
                                        break;
                                    }
                                }
                            }
                            PtyIoControlCommand::Shutdown => break,
                        }
                    }
                    debug!(pane_id, "windows pty control thread exiting");
                });
            }

            Ok(PtyIoActorHandle {
                data_tx,
                control_tx,
                accepting,
            })
        }
    }

    fn write_all_locked(
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        let mut writer = writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(bytes)?;
        writer.flush()
    }

    fn write_submission_locked(
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
        text: &[u8],
        enter: &[u8],
        delay: Duration,
    ) -> std::io::Result<()> {
        let mut writer = writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        writer.write_all(text)?;
        writer.flush()?;
        std::thread::sleep(delay);
        writer.write_all(enter)?;
        writer.flush()
    }

    #[allow(dead_code)]
    fn _assert_duration_send(_: Duration) {}
}

#[cfg(windows)]
pub(crate) use windows::*;
