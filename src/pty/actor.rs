#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows {
    use std::io::{Read, Write};
    use std::sync::{mpsc as std_mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use portable_pty::{MasterPty, PtySize};
    use tokio::sync::{mpsc, oneshot};
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

    pub(crate) struct PtyIoActorConfig {
        pub pane_id: u32,
        pub master: Box<dyn MasterPty + Send>,
        pub initially_quiesced: bool,
        pub on_read: ReadCallback,
        pub on_reader_exit: Option<ReaderExitCallback>,
    }

    enum PtyIoDataCommand {
        WriteUserInput(Bytes),
        SubmitUserInput {
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
    }

    enum PtyIoWriteCommand {
        Write(Bytes),
        SubmissionPart {
            bytes: Bytes,
            deadline: Option<Instant>,
            reply: oneshot::Sender<std::io::Result<()>>,
            enter_completion: Option<std_mpsc::Sender<()>>,
        },
    }

    struct InputAcceptance {
        accepting: bool,
        enter_completion: Option<std_mpsc::Receiver<()>>,
    }

    enum PtyIoControlCommand {
        Resize(PtyResizeRequest),
        Shutdown,
    }

    #[derive(Clone)]
    pub(crate) struct PtyIoActorHandle {
        data_tx: mpsc::Sender<PtyIoDataCommand>,
        control_tx: std_mpsc::Sender<PtyIoControlCommand>,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        response_order: Arc<Mutex<()>>,
        accepting: Arc<Mutex<InputAcceptance>>,
    }

    impl PtyIoActorHandle {
        pub(crate) fn try_write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
            if !self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .accepting
            {
                return Err(mpsc::error::TrySendError::Closed(bytes));
            }
            self.data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(bytes))
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(command) => {
                        let PtyIoDataCommand::WriteUserInput(bytes) = command else {
                            unreachable!("queued write returned another command")
                        };
                        mpsc::error::TrySendError::Full(bytes)
                    }
                    mpsc::error::TrySendError::Closed(command) => {
                        let PtyIoDataCommand::WriteUserInput(bytes) = command else {
                            unreachable!("queued write returned another command")
                        };
                        mpsc::error::TrySendError::Closed(bytes)
                    }
                })
        }

        pub(crate) fn queue_user_input_submission(
            &self,
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
        ) -> std::io::Result<std_mpsc::Receiver<std::io::Result<()>>> {
            let accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !accepting.accepting {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pty actor closed",
                ));
            }
            let (reply_tx, reply_rx) = std_mpsc::channel();
            self.data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text,
                    enter,
                    delay,
                    deadline,
                    reply: reply_tx,
                })
                .map_err(|err| match err {
                    mpsc::error::TrySendError::Full(_) => std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "pty input queue is full",
                    ),
                    mpsc::error::TrySendError::Closed(_) => {
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
                    }
                })?;
            Ok(reply_rx)
        }

        pub(crate) fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
            let _order = self
                .response_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(bytes) = response().filter(|bytes| !bytes.is_empty()) {
                let _ = self.write_tx.send(PtyIoWriteCommand::Write(bytes));
            }
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
            let mut accepting = self
                .accepting
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Give a queued Enter the same grace as process termination. A blocked
            // pipe writer must not prevent teardown from terminating the child.
            if let Some(completion) = accepting.enter_completion.take() {
                let _ = completion.recv_timeout(Duration::from_millis(250));
            }
            accepting.accepting = false;
            drop(accepting);
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
            let mut writer = master
                .take_writer()
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let (data_tx, mut data_rx) = mpsc::channel::<PtyIoDataCommand>(1024);
            let (control_tx, control_rx) = std_mpsc::channel::<PtyIoControlCommand>();
            let (write_tx, write_rx) = std_mpsc::channel::<PtyIoWriteCommand>();
            let response_order = Arc::new(Mutex::new(()));
            let accepting = Arc::new(Mutex::new(InputAcceptance {
                accepting: !initially_quiesced,
                enter_completion: None,
            }));

            crate::thread_spawn::spawn_named("herdr-pty-writer", move || {
                run_writer(&mut writer, write_rx);
                debug!(pane_id, "windows pty writer thread exiting");
            })?;

            {
                let write_tx = write_tx.clone();
                let accepting = Arc::clone(&accepting);
                tokio::spawn(async move {
                    run_input_forwarder(&mut data_rx, write_tx, accepting).await;
                    debug!(pane_id, "windows pty input task exiting");
                });
            }

            {
                let write_tx = write_tx.clone();
                let response_order = Arc::clone(&response_order);
                crate::thread_spawn::spawn_named("herdr-pty-reader", move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                let _order = response_order
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                let result = on_read(&buf[..n]);
                                if result.terminal_responses.into_iter().any(|response| {
                                    write_tx.send(PtyIoWriteCommand::Write(response)).is_err()
                                }) {
                                    break;
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
                })?;
            }

            {
                let write_tx = write_tx.clone();
                crate::thread_spawn::spawn_named("herdr-pty-control", move || {
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
                                if request.terminal_responses.into_iter().any(|response| {
                                    write_tx.send(PtyIoWriteCommand::Write(response)).is_err()
                                }) {
                                    break;
                                }
                            }
                            PtyIoControlCommand::Shutdown => break,
                        }
                    }
                    debug!(pane_id, "windows pty control thread exiting");
                })?;
            }

            Ok(PtyIoActorHandle {
                data_tx,
                control_tx,
                write_tx,
                response_order,
                accepting,
            })
        }
    }

    fn run_writer(writer: &mut impl Write, write_rx: std_mpsc::Receiver<PtyIoWriteCommand>) {
        for command in write_rx {
            let result = match command {
                PtyIoWriteCommand::Write(bytes) => write_and_flush(writer, &bytes),
                PtyIoWriteCommand::SubmissionPart {
                    bytes,
                    deadline,
                    reply,
                    enter_completion,
                } => {
                    let result = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        Err(input_submission_timed_out())
                    } else {
                        write_and_flush(writer, &bytes)
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    // Wake synchronous shutdown directly; it may be blocking the executor
                    // worker that would otherwise receive the async acknowledgement.
                    drop(enter_completion);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                    continue;
                }
            };
            if result.is_err() {
                break;
            }
        }
    }

    async fn run_input_forwarder(
        data_rx: &mut mpsc::Receiver<PtyIoDataCommand>,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        accepting: Arc<Mutex<InputAcceptance>>,
    ) {
        while let Some(command) = data_rx.recv().await {
            match command {
                PtyIoDataCommand::WriteUserInput(bytes) => {
                    if write_tx.send(PtyIoWriteCommand::Write(bytes)).is_err() {
                        break;
                    }
                }
                PtyIoDataCommand::SubmitUserInput {
                    text,
                    enter,
                    delay,
                    deadline,
                    reply,
                } => {
                    let result = if deadline.is_some_and(|deadline| {
                        deadline.saturating_duration_since(Instant::now()) <= delay
                    }) {
                        Err(input_submission_timed_out())
                    } else {
                        let text_deadline =
                            deadline.and_then(|deadline| deadline.checked_sub(delay));
                        async {
                            write_submission_part(&write_tx, text, text_deadline, None).await?;
                            // A started text write is committed. Finish Enter even if the caller
                            // stops waiting so a timeout cannot leave a partial prompt.
                            if !delay.is_zero() {
                                tokio::time::sleep(delay).await;
                            }
                            write_submission_part(&write_tx, enter, None, Some(&accepting)).await
                        }
                        .await
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                }
            }
        }
    }

    async fn write_submission_part(
        write_tx: &std_mpsc::Sender<PtyIoWriteCommand>,
        bytes: Bytes,
        deadline: Option<Instant>,
        accepting: Option<&Arc<Mutex<InputAcceptance>>>,
    ) -> std::io::Result<()> {
        let completion = {
            let (reply, completion) = oneshot::channel();
            let mut accepting = accepting.map(|accepting| {
                accepting
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let enter_completion = if let Some(accepting) = accepting.as_mut() {
                if !accepting.accepting {
                    return Err(pty_actor_closed());
                }
                let (done, completion) = std_mpsc::channel();
                accepting.enter_completion = Some(completion);
                Some(done)
            } else {
                None
            };
            write_tx
                .send(PtyIoWriteCommand::SubmissionPart {
                    bytes,
                    deadline,
                    reply,
                    enter_completion,
                })
                .map_err(|_| pty_actor_closed())?;
            completion
        };
        completion.await.unwrap_or_else(|_| Err(pty_actor_closed()))
    }

    fn pty_actor_closed() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
    }

    fn input_submission_timed_out() -> std::io::Error {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "agent prompt timed out before input submission",
        )
    }

    fn write_and_flush(writer: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
        writer.write_all(bytes)?;
        writer.flush()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn input_acceptance(accepting: bool) -> Arc<Mutex<InputAcceptance>> {
            Arc::new(Mutex::new(InputAcceptance {
                accepting,
                enter_completion: None,
            }))
        }

        fn test_runtime() -> tokio::runtime::Runtime {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
        }

        async fn next_write_command(
            receiver: &std_mpsc::Receiver<PtyIoWriteCommand>,
        ) -> PtyIoWriteCommand {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match receiver.try_recv() {
                        Ok(command) => return command,
                        Err(std_mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                        Err(err) => panic!("writer disconnected: {err}"),
                    }
                }
            })
            .await
            .expect("async forwarder queues writer command")
        }

        struct RecordingWriter {
            writes: Vec<(Vec<u8>, Instant)>,
            flushes: Vec<Instant>,
            fail_after: Option<usize>,
            flushed: std_mpsc::Sender<()>,
        }

        impl Write for RecordingWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.fail_after == Some(self.writes.len()) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "writer closed",
                    ));
                }
                self.writes.push((bytes.to_vec(), Instant::now()));
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes.push(Instant::now());
                let _ = self.flushed.send(());
                Ok(())
            }
        }

        fn run_recorded_submission(
            fail_after: Option<usize>,
            delay: Duration,
            deadline: Option<Instant>,
            during_delay: impl FnOnce(
                &std_mpsc::Sender<PtyIoWriteCommand>,
                &Arc<Mutex<InputAcceptance>>,
            ),
        ) -> (RecordingWriter, std::io::Result<()>) {
            let (flushed_tx, flushed_rx) = std_mpsc::channel();
            let mut writer = RecordingWriter {
                writes: Vec::new(),
                flushes: Vec::new(),
                fail_after,
                flushed: flushed_tx,
            };
            let (data_tx, mut data_rx) = mpsc::channel(2);
            let (write_tx, write_rx) = std_mpsc::channel();
            let (reply_tx, reply_rx) = std_mpsc::channel();
            let accepting = input_acceptance(true);
            data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text: Bytes::from_static(b"prompt"),
                    enter: Bytes::from_static(b"\r"),
                    delay,
                    deadline,
                    reply: reply_tx,
                })
                .unwrap();
            data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(Bytes::from_static(
                    b"user",
                )))
                .unwrap();
            let writer_thread = std::thread::spawn(move || {
                run_writer(&mut writer, write_rx);
                writer
            });
            let input_write_tx = write_tx.clone();
            let input_accepting = Arc::clone(&accepting);
            let runtime = test_runtime();
            let input_task = runtime.spawn(async move {
                run_input_forwarder(&mut data_rx, input_write_tx, input_accepting).await
            });
            flushed_rx.recv().expect("prompt was flushed");
            during_delay(&write_tx, &accepting);
            let result = reply_rx.recv().expect("writer reports submission");
            drop(data_tx);
            runtime.block_on(input_task).expect("input task joins");
            drop(write_tx);
            (writer_thread.join().expect("writer thread joins"), result)
        }

        #[test]
        fn submission_sequences_user_input_but_allows_terminal_responses() {
            let delay = Duration::from_millis(30);
            let (writer, result) = run_recorded_submission(None, delay, None, |write_tx, _| {
                write_tx
                    .send(PtyIoWriteCommand::Write(Bytes::from_static(b"response")))
                    .unwrap();
            });
            result.expect("submission succeeds");

            assert_eq!(writer.writes[0].0, b"prompt");
            assert_eq!(writer.writes[1].0, b"response");
            assert_eq!(writer.writes[2].0, b"\r");
            assert_eq!(writer.writes[3].0, b"user");
            assert!(writer.writes[2].1.duration_since(writer.flushes[0]) >= delay);
        }

        #[test]
        fn submission_returns_enter_write_failure() {
            let (_writer, result) =
                run_recorded_submission(Some(1), Duration::ZERO, None, |_, _| {});
            let err = result.expect_err("enter failure reaches caller");

            assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
        }

        #[test]
        fn shutdown_during_submission_delay_cancels_enter() {
            let (writer, result) =
                run_recorded_submission(None, Duration::from_millis(30), None, |_, accepting| {
                    accepting.lock().unwrap().accepting = false;
                });
            let err = result.expect_err("shutdown cancels enter");
            assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
            assert_eq!(
                writer
                    .writes
                    .iter()
                    .map(|write| write.0.as_slice())
                    .collect::<Vec<_>>(),
                vec![b"prompt"]
            );
        }

        #[test]
        fn expired_queued_submission_is_not_written() {
            let (flushed_tx, _flushed_rx) = std_mpsc::channel();
            let mut writer = RecordingWriter {
                writes: Vec::new(),
                flushes: Vec::new(),
                fail_after: None,
                flushed: flushed_tx,
            };
            let (data_tx, mut data_rx) = mpsc::channel(2);
            let (write_tx, write_rx) = std_mpsc::channel();
            let (first_reply_tx, first_reply_rx) = std_mpsc::channel();
            let (expired_reply_tx, expired_reply_rx) = std_mpsc::channel();
            let accepting = input_acceptance(true);
            data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text: Bytes::from_static(b"first"),
                    enter: Bytes::from_static(b"\r"),
                    delay: Duration::from_millis(30),
                    deadline: None,
                    reply: first_reply_tx,
                })
                .unwrap();
            data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text: Bytes::from_static(b"expired"),
                    enter: Bytes::from_static(b"\r"),
                    delay: Duration::ZERO,
                    deadline: Some(Instant::now() + Duration::from_millis(10)),
                    reply: expired_reply_tx,
                })
                .unwrap();

            let writer_thread = std::thread::spawn(move || {
                run_writer(&mut writer, write_rx);
                writer
            });
            let input_write_tx = write_tx.clone();
            let runtime = test_runtime();
            let input_task = runtime.spawn(async move {
                run_input_forwarder(&mut data_rx, input_write_tx, accepting).await
            });
            first_reply_rx.recv().unwrap().unwrap();
            let err = expired_reply_rx.recv().unwrap().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

            drop(data_tx);
            runtime.block_on(input_task).unwrap();
            drop(write_tx);
            let writer = writer_thread.join().unwrap();
            assert_eq!(
                writer
                    .writes
                    .iter()
                    .map(|write| write.0.as_slice())
                    .collect::<Vec<_>>(),
                vec![b"first".as_slice(), b"\r".as_slice()]
            );
        }

        #[tokio::test]
        async fn abandoned_committed_submission_finishes_without_blocking_executor() {
            let (data_tx, mut data_rx) = mpsc::channel(2);
            let (write_tx, write_rx) = std_mpsc::channel();
            let (reply_tx, reply_rx) = std_mpsc::channel();
            let deadline = Instant::now() + Duration::from_millis(50);
            data_tx
                .try_send(PtyIoDataCommand::SubmitUserInput {
                    text: Bytes::from_static(b"committed"),
                    enter: Bytes::from_static(b"\r"),
                    delay: Duration::from_millis(20),
                    deadline: Some(deadline),
                    reply: reply_tx,
                })
                .unwrap();
            data_tx
                .try_send(PtyIoDataCommand::WriteUserInput(Bytes::from_static(
                    b"after",
                )))
                .unwrap();
            drop(data_tx);
            let forwarder = tokio::spawn(async move {
                run_input_forwarder(&mut data_rx, write_tx, input_acceptance(true)).await
            });

            let PtyIoWriteCommand::SubmissionPart { bytes, reply, .. } =
                next_write_command(&write_rx).await
            else {
                panic!("text must be first");
            };
            assert_eq!(bytes, b"committed".as_slice());
            assert!(reply_rx.try_recv().is_err(), "completion waits for flush");
            // A text write can finish after the caller's deadline. Its acknowledgement
            // must still be consumed, and Enter must follow even after the caller leaves.
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            drop(reply_rx);
            reply.send(Ok(())).unwrap();
            let PtyIoWriteCommand::SubmissionPart {
                bytes,
                deadline,
                reply,
                enter_completion,
            } = next_write_command(&write_rx).await
            else {
                panic!("queued user input cannot overtake Enter");
            };
            assert_eq!(bytes, b"\r".as_slice());
            assert!(deadline.is_none(), "committed Enter cannot expire");
            assert!(
                write_rx.try_recv().is_err(),
                "user input waits for Enter flush"
            );
            drop(enter_completion);
            reply.send(Ok(())).unwrap();
            forwarder.await.unwrap();
            let PtyIoWriteCommand::Write(bytes) = write_rx.try_recv().unwrap() else {
                panic!("ordinary user input follows Enter");
            };
            assert_eq!(bytes, b"after".as_slice());
        }

        #[test]
        fn shutdown_gives_queued_enter_a_bounded_grace() {
            for outcome in ["flushed", "failed", "disconnected", "stalled"] {
                let runtime = test_runtime();
                let accepting = input_acceptance(true);
                let (data_tx, _data_rx) = mpsc::channel(1);
                let (control_tx, control_rx) = std_mpsc::channel();
                let (write_tx, write_rx) = std_mpsc::channel();
                let handle = PtyIoActorHandle {
                    data_tx,
                    control_tx,
                    write_tx: write_tx.clone(),
                    response_order: Arc::new(Mutex::new(())),
                    accepting: Arc::clone(&accepting),
                };
                let input_accepting = Arc::clone(&accepting);
                let input_task = runtime.spawn(async move {
                    write_submission_part(
                        &write_tx,
                        Bytes::from_static(b"\r"),
                        None,
                        Some(&input_accepting),
                    )
                    .await
                });
                let command = write_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                let (shutdown_done_tx, shutdown_done_rx) = std_mpsc::channel();
                let shutdown = std::thread::spawn(move || {
                    handle.shutdown();
                    shutdown_done_tx.send(()).unwrap();
                });
                if outcome == "stalled" {
                    // Keep the queued Enter and its completion sender alive until
                    // shutdown returns, as a blocked pipe writer would do.
                    let result = shutdown_done_rx.recv_timeout(Duration::from_secs(2));
                    drop(command);
                    shutdown.join().unwrap();
                    assert!(result.is_ok(), "stalled Enter cannot prevent teardown");
                    assert!(matches!(
                        control_rx.try_recv(),
                        Ok(PtyIoControlCommand::Shutdown)
                    ));
                    assert!(!accepting.lock().unwrap().accepting);
                    assert!(runtime.block_on(input_task).unwrap().is_err());
                    continue;
                }
                let deadline = Instant::now() + Duration::from_secs(2);
                while accepting.try_lock().is_ok() {
                    assert!(Instant::now() < deadline, "shutdown takes acceptance lock");
                    std::thread::yield_now();
                }
                assert!(
                    shutdown_done_rx.try_recv().is_err(),
                    "shutdown waits for Enter"
                );
                assert!(
                    control_rx.try_recv().is_err(),
                    "master stays open until Enter finishes"
                );

                let (flushed_tx, _flushed_rx) = std_mpsc::channel();
                let mut writer = RecordingWriter {
                    writes: vec![],
                    flushes: vec![],
                    fail_after: (outcome == "failed").then_some(0),
                    flushed: flushed_tx,
                };
                // Shutdown cannot rely on the async receiver running. Writer completion,
                // write failure, and discarded queued commands must all release it.
                if outcome == "disconnected" {
                    drop(command);
                } else {
                    let (writer_tx, writer_rx) = std_mpsc::channel();
                    writer_tx.send(command).unwrap();
                    drop(writer_tx);
                    run_writer(&mut writer, writer_rx);
                }
                shutdown_done_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                shutdown.join().unwrap();
                let result = runtime.block_on(input_task).unwrap();
                assert_eq!(result.is_err(), outcome != "flushed");
                assert_eq!(writer.flushes.len(), usize::from(outcome == "flushed"));
                assert!(!accepting.lock().unwrap().accepting);
            }
        }

        #[tokio::test]
        async fn enter_rejects_quiesced_actor_and_failed_enqueue_releases_shutdown_wait() {
            let (write_tx, write_rx) = std_mpsc::channel();
            let accepting = input_acceptance(false);
            let error =
                write_submission_part(&write_tx, Bytes::from_static(b"\r"), None, Some(&accepting))
                    .await
                    .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            assert!(write_rx.try_recv().is_err());
            accepting.lock().unwrap().accepting = true;
            drop(write_rx);
            let error =
                write_submission_part(&write_tx, Bytes::from_static(b"\r"), None, Some(&accepting))
                    .await
                    .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
            let completion = accepting.lock().unwrap().enter_completion.take().unwrap();
            assert!(matches!(
                completion.try_recv(),
                Err(std_mpsc::TryRecvError::Disconnected)
            ));
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;
