#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
mod windows {
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, BorrowedHandle, OwnedHandle};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc as std_mpsc, Arc, Condvar, Mutex,
    };
    use std::time::{Duration, Instant};

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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ActorState {
        Running,
        Quiescing,
        Quiesced,
        Released,
        Shutdown,
    }

    struct SharedActorState {
        state: Mutex<ActorState>,
        resumed: Condvar,
        shutdown: AtomicBool,
    }

    impl SharedActorState {
        fn new(initially_quiesced: bool) -> Self {
            Self {
                state: Mutex::new(if initially_quiesced {
                    ActorState::Quiesced
                } else {
                    ActorState::Running
                }),
                resumed: Condvar::new(),
                shutdown: AtomicBool::new(false),
            }
        }

        fn wait_until_resumed(&self, pause_active: &AtomicBool) -> ActorState {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while pause_active.load(Ordering::Acquire)
                && matches!(*state, ActorState::Quiescing | ActorState::Quiesced)
            {
                state = self
                    .resumed
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            *state
        }

        fn wait_to_read(&self, pause: &ReaderPause) -> ActorState {
            loop {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if !matches!(*state, ActorState::Quiescing | ActorState::Quiesced) {
                    return *state;
                }
                let Some(pause_active) = pause.acknowledge() else {
                    state = self
                        .resumed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if !matches!(*state, ActorState::Quiescing | ActorState::Quiesced) {
                        return *state;
                    }
                    continue;
                };
                while pause_active.load(Ordering::Acquire)
                    && matches!(*state, ActorState::Quiescing | ActorState::Quiesced)
                {
                    state = self
                        .resumed
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                if !matches!(*state, ActorState::Quiescing | ActorState::Quiesced) {
                    return *state;
                }
            }
        }

        fn resume(&self, state: ActorState) {
            *self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
            self.resumed.notify_all();
        }
    }

    #[derive(Default)]
    struct ReaderPause {
        reply: Mutex<Option<(Arc<AtomicBool>, std_mpsc::Sender<()>)>>,
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by the stacked Windows handoff integration")
    )]
    impl ReaderPause {
        fn request(&self, active: Arc<AtomicBool>) -> std_mpsc::Receiver<()> {
            let (reply, completion) = std_mpsc::channel();
            *self
                .reply
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((active, reply));
            completion
        }

        fn acknowledge(&self) -> Option<Arc<AtomicBool>> {
            self.reply
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .map(|(active, reply)| {
                    let _ = reply.send(());
                    active
                })
        }

        fn clear(&self) {
            self.reply
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
    }

    pub(crate) struct PtyIoActorConfig {
        pub pane_id: u32,
        pub master: Box<dyn MasterPty + Send>,
        pub handoff_child: OwnedHandle,
        pub initially_quiesced: bool,
        pub on_read: ReadCallback,
        pub on_reader_exit: Option<ReaderExitCallback>,
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by the stacked Windows handoff integration")
    )]
    enum PtyIoDataCommand {
        WriteUserInput(Bytes),
        SubmitUserInput {
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
        Pause {
            active: Arc<AtomicBool>,
            reply: std_mpsc::Sender<()>,
        },
    }

    enum PtyIoWriteCommand {
        Write(Bytes),
        SubmissionPart {
            bytes: Bytes,
            deadline: Option<Instant>,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
        Barrier(std_mpsc::Sender<std::io::Result<()>>),
        Release,
    }

    #[allow(dead_code, reason = "used by the stacked Windows handoff integration")]
    enum PtyIoControlCommand {
        Resize(PtyResizeRequest),
        BeginHandoff {
            active: Arc<AtomicBool>,
            input_paused: std_mpsc::Receiver<()>,
            reader_paused: std_mpsc::Receiver<()>,
            deadline: Instant,
            reply: std_mpsc::Sender<std::io::Result<()>>,
        },
        DuplicateForHandoff {
            target_process: OwnedHandle,
            reply: std_mpsc::Sender<std::io::Result<crate::pty::backend::WindowsPtyHandoff>>,
        },
        RollbackHandoff(std_mpsc::Sender<std::io::Result<()>>),
        ActivateAfterHandoff(std_mpsc::Sender<std::io::Result<()>>),
        ReleaseAfterCommit(std_mpsc::Sender<std::io::Result<()>>),
        Shutdown,
    }

    #[derive(Clone)]
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by the stacked Windows handoff integration")
    )]
    pub(crate) struct PtyIoActorHandle {
        data_tx: mpsc::Sender<PtyIoDataCommand>,
        control_tx: std_mpsc::Sender<PtyIoControlCommand>,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        response_order: Arc<Mutex<()>>,
        state: Arc<SharedActorState>,
        reader_pause: Arc<ReaderPause>,
        handoff_supported: bool,
    }

    #[allow(dead_code, reason = "used by the stacked Windows handoff integration")]
    impl PtyIoActorHandle {
        pub(crate) fn try_write_user_input(
            &self,
            bytes: Bytes,
        ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
            let state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Running {
                return Err(mpsc::error::TrySendError::Closed(bytes));
            }
            let result = self
                .data_tx
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
                });
            drop(state);
            result
        }

        pub(crate) fn queue_user_input_submission(
            &self,
            text: Bytes,
            enter: Bytes,
            delay: Duration,
            deadline: Option<Instant>,
        ) -> std::io::Result<std_mpsc::Receiver<std::io::Result<()>>> {
            let state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Running {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pty actor closed",
                ));
            }
            let (reply_tx, reply_rx) = std_mpsc::channel();
            let result = self
                .data_tx
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
                });
            drop(state);
            result?;
            Ok(reply_rx)
        }

        pub(crate) fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
            let _order = self
                .response_order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Running {
                return;
            }
            if let Some(bytes) = response().filter(|bytes| !bytes.is_empty()) {
                let _ = self.write_tx.send(PtyIoWriteCommand::Write(bytes));
            }
            drop(state);
        }

        pub(crate) fn resize(
            &self,
            rows: u16,
            cols: u16,
            cell_width_px: u32,
            cell_height_px: u32,
            terminal_responses: Vec<Bytes>,
        ) {
            let state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Running {
                return;
            }
            let result = self
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
            drop(state);
            let _ = result;
        }

        pub(crate) fn supports_handoff(&self) -> bool {
            self.handoff_supported
        }

        pub(crate) fn begin_handoff(&self, timeout: Duration) -> std::io::Result<()> {
            if !self.handoff_supported {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "Windows PTY backend cannot be transferred",
                ));
            }
            let deadline = Instant::now() + timeout;
            let mut state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Running {
                return Err(std::io::Error::other("PTY actor is not running"));
            }
            *state = ActorState::Quiescing;
            let active = Arc::new(AtomicBool::new(true));
            let reader_paused = self.reader_pause.request(Arc::clone(&active));
            let (input_paused_tx, input_paused) = std_mpsc::channel();
            if let Err(error) = send_data_before_deadline(
                &self.data_tx,
                PtyIoDataCommand::Pause {
                    active: Arc::clone(&active),
                    reply: input_paused_tx,
                },
                deadline,
            ) {
                active.store(false, Ordering::Release);
                *state = ActorState::Running;
                self.reader_pause.clear();
                self.state.resumed.notify_all();
                return Err(error);
            }
            let (reply, completion) = std_mpsc::channel();
            if self
                .control_tx
                .send(PtyIoControlCommand::BeginHandoff {
                    active: Arc::clone(&active),
                    input_paused,
                    reader_paused,
                    deadline,
                    reply,
                })
                .is_err()
            {
                active.store(false, Ordering::Release);
                *state = ActorState::Shutdown;
                self.reader_pause.clear();
                self.state.resumed.notify_all();
                return Err(pty_actor_closed());
            }
            drop(state);
            match completion.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(result) => result,
                Err(_) => {
                    let _ = self.rollback_handoff();
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out waiting for PTY actor to quiesce",
                    ))
                }
            }
        }

        pub(crate) fn duplicate_for_handoff(
            &self,
            target: &std::process::Child,
        ) -> std::io::Result<crate::pty::backend::WindowsPtyHandoff> {
            let target_process = unsafe { BorrowedHandle::borrow_raw(target.as_raw_handle()) }
                .try_clone_to_owned()?;
            let (reply, completion) = std_mpsc::channel();
            self.control_tx
                .send(PtyIoControlCommand::DuplicateForHandoff {
                    target_process,
                    reply,
                })
                .map_err(|_| pty_actor_closed())?;
            completion
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "timed out waiting for PTY handoff duplicate",
                    )
                })?
        }

        pub(crate) fn rollback_handoff(&self) -> std::io::Result<()> {
            self.request_state_change(
                PtyIoControlCommand::RollbackHandoff,
                "timed out waiting for PTY handoff rollback",
            )
        }

        pub(crate) fn activate_after_handoff(&self) -> std::io::Result<()> {
            self.request_state_change(
                PtyIoControlCommand::ActivateAfterHandoff,
                "timed out waiting for PTY handoff activation",
            )
        }

        pub(crate) fn release_after_commit(&self) -> std::io::Result<()> {
            self.request_state_change(
                PtyIoControlCommand::ReleaseAfterCommit,
                "timed out waiting for PTY actor release",
            )
        }

        fn request_state_change(
            &self,
            command: impl FnOnce(std_mpsc::Sender<std::io::Result<()>>) -> PtyIoControlCommand,
            timeout_message: &'static str,
        ) -> std::io::Result<()> {
            let (reply, completion) = std_mpsc::channel();
            self.control_tx
                .send(command(reply))
                .map_err(|_| pty_actor_closed())?;
            completion
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, timeout_message))?
        }

        pub(crate) fn shutdown(&self) {
            self.state.shutdown.store(true, Ordering::Release);
            let mut state = self
                .state
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *state != ActorState::Released {
                *state = ActorState::Shutdown;
            }
            drop(state);
            self.state.resumed.notify_all();
            let _ = self.control_tx.send(PtyIoControlCommand::Shutdown);
        }
    }

    pub(crate) struct PtyIoActor;

    impl PtyIoActor {
        pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
            Self::spawn_inner(config, None)
        }

        #[cfg(test)]
        fn spawn_with_handoff_support(
            config: PtyIoActorConfig,
            handoff_supported: bool,
        ) -> std::io::Result<PtyIoActorHandle> {
            Self::spawn_inner(config, Some(handoff_supported))
        }

        fn spawn_inner(
            config: PtyIoActorConfig,
            handoff_supported_override: Option<bool>,
        ) -> std::io::Result<PtyIoActorHandle> {
            let PtyIoActorConfig {
                pane_id,
                master,
                handoff_child,
                initially_quiesced,
                on_read,
                on_reader_exit,
            } = config;

            let handoff_supported = handoff_supported_override
                .unwrap_or_else(|| crate::pty::backend::windows_handoff_supported(master.as_ref()));
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
            let state = Arc::new(SharedActorState::new(initially_quiesced));
            let reader_pause = Arc::new(ReaderPause::default());

            std::thread::spawn(move || {
                run_writer(&mut writer, write_rx);
                debug!(pane_id, "windows pty writer thread exiting");
            });

            {
                let write_tx = write_tx.clone();
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    run_input_forwarder(&mut data_rx, write_tx, state);
                    debug!(pane_id, "windows pty input thread exiting");
                });
            }

            let reader_cancel = {
                let write_tx = write_tx.clone();
                let response_order = Arc::clone(&response_order);
                let state = Arc::clone(&state);
                let reader_pause = Arc::clone(&reader_pause);
                let (cancel_tx, cancel_rx) = std_mpsc::channel();
                std::thread::spawn(move || {
                    let cancel = crate::platform::SynchronousIoCancel::for_current_thread();
                    if cancel_tx.send(cancel).is_err() {
                        return;
                    }
                    let notify_exit = run_reader(
                        pane_id,
                        &mut reader,
                        on_read,
                        write_tx,
                        response_order,
                        state,
                        reader_pause,
                    );
                    if notify_exit {
                        if let Some(on_reader_exit) = on_reader_exit {
                            on_reader_exit();
                        }
                    }
                    debug!(pane_id, "windows pty reader thread exiting");
                });
                cancel_rx
                    .recv()
                    .map_err(|_| std::io::Error::other("PTY reader failed to start"))??
            };

            {
                let write_tx = write_tx.clone();
                let state = Arc::clone(&state);
                let reader_pause = Arc::clone(&reader_pause);
                std::thread::spawn(move || {
                    let mut handoff_active: Option<Arc<AtomicBool>> = None;
                    for command in control_rx {
                        let should_exit = match command {
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
                                request.terminal_responses.into_iter().any(|response| {
                                    write_tx.send(PtyIoWriteCommand::Write(response)).is_err()
                                })
                            }
                            PtyIoControlCommand::BeginHandoff {
                                active,
                                input_paused,
                                reader_paused,
                                deadline,
                                reply,
                            } => {
                                let result = quiesce(
                                    &input_paused,
                                    &reader_paused,
                                    &reader_cancel,
                                    &write_tx,
                                    deadline,
                                );
                                if result.is_ok() {
                                    handoff_active = Some(active);
                                    *state
                                        .state
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                                        ActorState::Quiesced;
                                } else {
                                    active.store(false, Ordering::Release);
                                    reader_pause.clear();
                                    state.resume(ActorState::Running);
                                }
                                let _ = reply.send(result);
                                false
                            }
                            PtyIoControlCommand::DuplicateForHandoff {
                                target_process,
                                reply,
                            } => {
                                let result = if *state
                                    .state
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                                    == ActorState::Quiesced
                                {
                                    crate::pty::backend::duplicate_windows_handoff(
                                        master.as_ref(),
                                        &handoff_child,
                                        target_process,
                                    )
                                } else {
                                    Err(std::io::Error::other(
                                        "PTY actor must be quiesced before handoff duplication",
                                    ))
                                };
                                let _ = reply.send(result);
                                false
                            }
                            PtyIoControlCommand::RollbackHandoff(reply) => {
                                let current = *state
                                    .state
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                let result = if matches!(
                                    current,
                                    ActorState::Released | ActorState::Shutdown
                                ) {
                                    Err(std::io::Error::new(
                                        std::io::ErrorKind::BrokenPipe,
                                        "PTY actor cannot resume",
                                    ))
                                } else {
                                    if let Some(active) = handoff_active.take() {
                                        active.store(false, Ordering::Release);
                                    }
                                    reader_pause.clear();
                                    state.resume(ActorState::Running);
                                    Ok(())
                                };
                                let _ = reply.send(result);
                                false
                            }
                            PtyIoControlCommand::ActivateAfterHandoff(reply) => {
                                let current = *state
                                    .state
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                let result = if current == ActorState::Quiesced {
                                    if let Some(active) = handoff_active.take() {
                                        active.store(false, Ordering::Release);
                                    }
                                    reader_pause.clear();
                                    state.resume(ActorState::Running);
                                    Ok(())
                                } else {
                                    Err(std::io::Error::other(
                                        "PTY actor must be quiesced before activation",
                                    ))
                                };
                                let _ = reply.send(result);
                                false
                            }
                            PtyIoControlCommand::ReleaseAfterCommit(reply) => {
                                let current = *state
                                    .state
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                let result = if current == ActorState::Quiesced {
                                    if let Some(active) = handoff_active.take() {
                                        active.store(false, Ordering::Release);
                                    }
                                    reader_pause.clear();
                                    state.resume(ActorState::Released);
                                    write_tx
                                        .send(PtyIoWriteCommand::Release)
                                        .map_err(|_| pty_actor_closed())
                                } else {
                                    Err(std::io::Error::other(
                                        "PTY actor must be quiesced before release",
                                    ))
                                };
                                let released = result.is_ok();
                                let _ = reply.send(result);
                                released
                            }
                            PtyIoControlCommand::Shutdown => true,
                        };
                        if should_exit {
                            break;
                        }
                    }
                    debug!(pane_id, "windows pty control thread exiting");
                });
            }

            Ok(PtyIoActorHandle {
                data_tx,
                control_tx,
                write_tx,
                response_order,
                state,
                reader_pause,
                handoff_supported,
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
                } => {
                    let result = if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        Err(input_submission_timed_out())
                    } else {
                        write_and_flush(writer, &bytes)
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                    continue;
                }
                PtyIoWriteCommand::Barrier(reply) => {
                    let result = writer.flush();
                    let failed = result.is_err();
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                    continue;
                }
                PtyIoWriteCommand::Release => break,
            };
            if result.is_err() {
                break;
            }
        }
    }

    fn run_input_forwarder(
        data_rx: &mut mpsc::Receiver<PtyIoDataCommand>,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        state: Arc<SharedActorState>,
    ) {
        while let Some(command) = data_rx.blocking_recv() {
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
                        write_submission_part(&write_tx, text, text_deadline).and_then(|()| {
                            // A started text write is committed. Finish Enter even if the caller
                            // stops waiting so a timeout cannot leave a partial prompt.
                            std::thread::sleep(delay);
                            if state.shutdown.load(Ordering::Acquire) {
                                return Err(pty_actor_closed());
                            }
                            write_submission_part(&write_tx, enter, None)
                        })
                    };
                    let failed = result
                        .as_ref()
                        .is_err_and(|err| err.kind() != std::io::ErrorKind::TimedOut);
                    let _ = reply.send(result);
                    if failed {
                        break;
                    }
                }
                PtyIoDataCommand::Pause { active, reply } => {
                    let _ = reply.send(());
                    if matches!(
                        state.wait_until_resumed(&active),
                        ActorState::Released | ActorState::Shutdown
                    ) {
                        break;
                    }
                }
            }
        }
    }

    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "used by the stacked Windows handoff integration")
    )]
    fn send_data_before_deadline(
        data_tx: &mpsc::Sender<PtyIoDataCommand>,
        mut command: PtyIoDataCommand,
        deadline: Instant,
    ) -> std::io::Result<()> {
        loop {
            match data_tx.try_send(command) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Full(returned)) => command = returned,
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(pty_actor_closed()),
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out closing PTY input admission",
                ));
            }
            std::thread::yield_now();
        }
    }

    fn run_reader(
        pane_id: u32,
        reader: &mut impl Read,
        mut on_read: ReadCallback,
        write_tx: std_mpsc::Sender<PtyIoWriteCommand>,
        response_order: Arc<Mutex<()>>,
        state: Arc<SharedActorState>,
        reader_pause: Arc<ReaderPause>,
    ) -> bool {
        let mut buf = [0u8; 8192];
        loop {
            if matches!(
                state.wait_to_read(&reader_pause),
                ActorState::Released | ActorState::Shutdown
            ) {
                return false;
            }
            match reader.read(&mut buf) {
                Ok(0) => return true,
                Ok(n) => {
                    let _order = response_order
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let _state = state
                        .state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let result = on_read(&buf[..n]);
                    if result
                        .terminal_responses
                        .into_iter()
                        .any(|response| write_tx.send(PtyIoWriteCommand::Write(response)).is_err())
                    {
                        return true;
                    }
                }
                Err(err)
                    if err.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32) =>
                {
                    // Cancellation can complete after a timed-out pause has rolled back.
                    // Recheck the lifecycle state before reading again.
                }
                Err(err) => {
                    debug!(pane_id, err = %err, "windows pty reader failed");
                    return true;
                }
            }
        }
    }

    fn quiesce(
        input_paused: &std_mpsc::Receiver<()>,
        reader_paused: &std_mpsc::Receiver<()>,
        reader_cancel: &crate::platform::SynchronousIoCancel,
        write_tx: &std_mpsc::Sender<PtyIoWriteCommand>,
        deadline: Instant,
    ) -> std::io::Result<()> {
        let mut input_done = false;
        let mut reader_done = false;
        while !input_done || !reader_done {
            if !input_done {
                input_done = receive_pause_ack(input_paused, "PTY input thread")?;
            }
            if !reader_done {
                reader_done = receive_pause_ack(reader_paused, "PTY reader thread")?;
            }
            if input_done && reader_done {
                break;
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out pausing PTY workers",
                ));
            }
            if !reader_done {
                reader_cancel.cancel()?;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        let (reply, completion) = std_mpsc::channel();
        write_tx
            .send(PtyIoWriteCommand::Barrier(reply))
            .map_err(|_| pty_actor_closed())?;
        completion
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out draining PTY writes",
                )
            })?
    }

    fn receive_pause_ack(
        receiver: &std_mpsc::Receiver<()>,
        worker: &'static str,
    ) -> std::io::Result<bool> {
        match receiver.try_recv() {
            Ok(()) => Ok(true),
            Err(std_mpsc::TryRecvError::Empty) => Ok(false),
            Err(std_mpsc::TryRecvError::Disconnected) => Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("{worker} stopped before handoff pause"),
            )),
        }
    }

    fn write_submission_part(
        write_tx: &std_mpsc::Sender<PtyIoWriteCommand>,
        bytes: Bytes,
        deadline: Option<Instant>,
    ) -> std::io::Result<()> {
        let (reply, completion) = std_mpsc::channel();
        write_tx
            .send(PtyIoWriteCommand::SubmissionPart {
                bytes,
                deadline,
                reply,
            })
            .map_err(|_| pty_actor_closed())?;
        completion
            .recv()
            .unwrap_or_else(|_| Err(pty_actor_closed()))
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
        use std::fs::File;
        use std::os::windows::io::{AsRawHandle, FromRawHandle};

        struct PipeReader(File);

        impl Read for PipeReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let mut read = 0;
                let result = unsafe {
                    windows_sys::Win32::Storage::FileSystem::ReadFile(
                        self.0.as_raw_handle(),
                        bytes.as_mut_ptr(),
                        bytes.len().min(u32::MAX as usize) as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if result == 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(read as usize)
                }
            }
        }

        struct RecordingWriter {
            writes: Vec<(Vec<u8>, Instant)>,
            flushes: Vec<Instant>,
            fail_after: Option<usize>,
            flushed: std_mpsc::Sender<()>,
        }

        #[derive(Default)]
        struct SharedWriterState {
            writes: Vec<Vec<u8>>,
            flushes: usize,
        }

        struct SharedWriter {
            state: Arc<Mutex<SharedWriterState>>,
            flushed: std_mpsc::Sender<()>,
        }

        impl Write for SharedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.state.lock().unwrap().writes.push(bytes.to_vec());
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.state.lock().unwrap().flushes += 1;
                let _ = self.flushed.send(());
                Ok(())
            }
        }

        struct TestMaster {
            reader: PipeReader,
            writer: Mutex<Option<SharedWriter>>,
            resizes: Arc<Mutex<Vec<PtySize>>>,
        }

        impl MasterPty for TestMaster {
            fn resize(&self, size: PtySize) -> anyhow::Result<()> {
                self.resizes.lock().unwrap().push(size);
                Ok(())
            }

            fn get_size(&self) -> anyhow::Result<PtySize> {
                Ok(PtySize::default())
            }

            fn try_clone_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
                Ok(Box::new(PipeReader(self.reader.0.try_clone()?)))
            }

            fn take_writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
                self.writer
                    .lock()
                    .unwrap()
                    .take()
                    .map(|writer| Box::new(writer) as Box<dyn Write + Send>)
                    .ok_or_else(|| anyhow::anyhow!("writer already taken"))
            }
        }

        struct ActorHarness {
            handle: PtyIoActorHandle,
            output: File,
            writer: Arc<Mutex<SharedWriterState>>,
            resizes: Arc<Mutex<Vec<PtySize>>>,
            flushed: std_mpsc::Receiver<()>,
        }

        fn actor_harness(
            initially_quiesced: bool,
            handoff_supported: bool,
            on_read: ReadCallback,
            on_reader_exit: Option<ReaderExitCallback>,
        ) -> ActorHarness {
            let mut read = std::ptr::null_mut();
            let mut write = std::ptr::null_mut();
            assert_ne!(
                unsafe {
                    windows_sys::Win32::System::Pipes::CreatePipe(
                        &mut read,
                        &mut write,
                        std::ptr::null(),
                        0,
                    )
                },
                0
            );
            let reader = PipeReader(unsafe { File::from_raw_handle(read.cast()) });
            let output = unsafe { File::from_raw_handle(write.cast()) };
            let writer = Arc::new(Mutex::new(SharedWriterState::default()));
            let resizes = Arc::new(Mutex::new(Vec::new()));
            let (flushed_tx, flushed) = std_mpsc::channel();
            let master = TestMaster {
                reader,
                writer: Mutex::new(Some(SharedWriter {
                    state: Arc::clone(&writer),
                    flushed: flushed_tx,
                })),
                resizes: Arc::clone(&resizes),
            };
            let handoff_child = File::open("NUL").unwrap().into();
            let handle = PtyIoActor::spawn_with_handoff_support(
                PtyIoActorConfig {
                    pane_id: 1,
                    master: Box::new(master),
                    handoff_child,
                    initially_quiesced,
                    on_read,
                    on_reader_exit,
                },
                handoff_supported,
            )
            .unwrap();
            ActorHarness {
                handle,
                output,
                writer,
                resizes,
                flushed,
            }
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
            during_delay: impl FnOnce(&std_mpsc::Sender<PtyIoWriteCommand>, &Arc<SharedActorState>),
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
            let state = Arc::new(SharedActorState::new(false));
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
            let input_state = Arc::clone(&state);
            let input_thread = std::thread::spawn(move || {
                run_input_forwarder(&mut data_rx, input_write_tx, input_state)
            });
            flushed_rx.recv().expect("prompt was flushed");
            during_delay(&write_tx, &state);
            let result = reply_rx.recv().expect("writer reports submission");
            drop(data_tx);
            input_thread.join().expect("input thread joins");
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
                run_recorded_submission(None, Duration::from_millis(30), None, |_, state| {
                    state.shutdown.store(true, Ordering::Release);
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
            let state = Arc::new(SharedActorState::new(false));
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
            let input_thread = std::thread::spawn(move || {
                run_input_forwarder(&mut data_rx, input_write_tx, state)
            });
            first_reply_rx.recv().unwrap().unwrap();
            let err = expired_reply_rx.recv().unwrap().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);

            drop(data_tx);
            input_thread.join().unwrap();
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

        #[test]
        fn shutdown_wakes_paused_workers_without_resuming_io() {
            for terminal_state in [ActorState::Quiesced, ActorState::Released] {
                let state = Arc::new(SharedActorState::new(true));
                let active = Arc::new(AtomicBool::new(true));
                let reader_pause = Arc::new(ReaderPause::default());
                let reader_paused = reader_pause.request(Arc::clone(&active));
                let (data_tx, mut data_rx) = mpsc::channel(2);
                let (input_paused_tx, input_paused) = std_mpsc::channel();
                data_tx
                    .try_send(PtyIoDataCommand::Pause {
                        active,
                        reply: input_paused_tx,
                    })
                    .unwrap();
                data_tx
                    .try_send(PtyIoDataCommand::WriteUserInput(Bytes::from_static(
                        b"must not write",
                    )))
                    .unwrap();
                let (write_tx, write_rx) = std_mpsc::channel();
                let (control_tx, _control_rx) = std_mpsc::channel();
                let handle = PtyIoActorHandle {
                    data_tx,
                    control_tx,
                    write_tx,
                    response_order: Arc::new(Mutex::new(())),
                    state,
                    reader_pause,
                    handoff_supported: true,
                };
                let input_handle = handle.clone();
                let (input_done_tx, input_done) = std_mpsc::channel();
                let input_thread = std::thread::spawn(move || {
                    run_input_forwarder(&mut data_rx, input_handle.write_tx, input_handle.state);
                    input_done_tx.send(()).unwrap();
                });
                let reader_handle = handle.clone();
                let (reader_done_tx, reader_done) = std_mpsc::channel();
                let reader_thread = std::thread::spawn(move || {
                    let mut reader = std::io::Cursor::new(b"replacement output");
                    let publish_exit = run_reader(
                        1,
                        &mut reader,
                        Box::new(|_| panic!("shutdown must not consume replacement output")),
                        reader_handle.write_tx,
                        reader_handle.response_order,
                        reader_handle.state,
                        reader_handle.reader_pause,
                    );
                    reader_done_tx
                        .send((reader.position(), publish_exit))
                        .unwrap();
                });
                input_paused.recv_timeout(Duration::from_secs(1)).unwrap();
                reader_paused.recv_timeout(Duration::from_secs(1)).unwrap();
                // Model release followed by Drop before paused workers reacquire the lock.
                *handle.state.state.lock().unwrap() = terminal_state;
                let poisoned = Arc::clone(&handle.state);
                assert!(std::thread::spawn(move || {
                    let _state = poisoned.state.lock().unwrap();
                    panic!("a reader callback can poison the state lock");
                })
                .join()
                .is_err());
                handle.shutdown();
                assert_eq!(
                    reader_done.recv_timeout(Duration::from_secs(1)).unwrap(),
                    (0, false)
                );
                input_done.recv_timeout(Duration::from_secs(1)).unwrap();
                assert!(write_rx.try_recv().is_err());
                reader_thread.join().unwrap();
                input_thread.join().unwrap();
            }
        }

        #[test]
        fn reader_retries_a_pause_cancellation_after_rollback() {
            struct LateCancellation(std::io::Cursor<&'static [u8]>, bool);
            impl Read for LateCancellation {
                fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                    if !std::mem::replace(&mut self.1, true) {
                        return Err(std::io::Error::from_raw_os_error(
                            windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32,
                        ));
                    }
                    self.0.read(bytes)
                }
            }
            let mut reader = LateCancellation(std::io::Cursor::new(b"after rollback"), false);
            let (write_tx, _write_rx) = std_mpsc::channel();
            let (read_tx, read_rx) = std_mpsc::channel();
            assert!(run_reader(
                1,
                &mut reader,
                Box::new(move |bytes| {
                    read_tx.send(bytes.to_vec()).unwrap();
                    PtyReadResult {
                        terminal_responses: Vec::new(),
                    }
                }),
                write_tx,
                Arc::new(Mutex::new(())),
                Arc::new(SharedActorState::new(false)),
                Arc::new(ReaderPause::default()),
            ));
            assert_eq!(read_rx.try_recv().unwrap(), b"after rollback");
        }

        #[test]
        fn handoff_drains_all_accepted_work_and_rollback_reuses_reader() {
            let (read_tx, read_rx) = std_mpsc::channel();
            let reader_exits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let exit_count = Arc::clone(&reader_exits);
            let mut harness = actor_harness(
                false,
                true,
                Box::new(move |bytes| {
                    read_tx.send(bytes.to_vec()).unwrap();
                    PtyReadResult {
                        terminal_responses: vec![Bytes::from_static(b"read-response")],
                    }
                }),
                Some(Box::new(move || {
                    exit_count.fetch_add(1, Ordering::Release);
                })),
            );

            let submission = harness
                .handle
                .queue_user_input_submission(
                    Bytes::from_static(b"prompt"),
                    Bytes::from_static(b"\r"),
                    Duration::from_millis(30),
                    None,
                )
                .unwrap();
            harness
                .handle
                .try_write_user_input(Bytes::from_static(b"user"))
                .unwrap();
            harness.flushed.recv().unwrap();
            harness
                .handle
                .write_terminal_response(|| Some(Bytes::from_static(b"appearance-response")));
            harness
                .handle
                .resize(40, 120, 9, 18, vec![Bytes::from_static(b"resize-response")]);
            harness.output.write_all(b"output").unwrap();
            assert_eq!(read_rx.recv().unwrap(), b"output");

            harness
                .handle
                .begin_handoff(Duration::from_secs(1))
                .unwrap();
            submission.recv().unwrap().unwrap();
            assert!(harness
                .handle
                .try_write_user_input(Bytes::from_static(b"rejected"))
                .is_err());
            assert_eq!(reader_exits.load(Ordering::Acquire), 0);

            let writer = harness.writer.lock().unwrap();
            for expected in [
                b"prompt".as_slice(),
                b"\r".as_slice(),
                b"user".as_slice(),
                b"appearance-response".as_slice(),
                b"resize-response".as_slice(),
                b"read-response".as_slice(),
            ] {
                assert_eq!(
                    writer
                        .writes
                        .iter()
                        .filter(|write| write.as_slice() == expected)
                        .count(),
                    1,
                    "{expected:?} must be written exactly once"
                );
            }
            let position = |needle: &[u8]| {
                writer
                    .writes
                    .iter()
                    .position(|write| write == needle)
                    .unwrap()
            };
            assert!(position(b"prompt") < position(b"\r"));
            assert!(position(b"\r") < position(b"user"));
            assert_eq!(writer.flushes, writer.writes.len() + 1);
            drop(writer);
            assert_eq!(
                harness.resizes.lock().unwrap().as_slice(),
                &[PtySize {
                    rows: 40,
                    cols: 120,
                    pixel_width: 9,
                    pixel_height: 18,
                }]
            );

            harness.handle.rollback_handoff().unwrap();
            while harness.flushed.try_recv().is_ok() {}
            harness
                .handle
                .try_write_user_input(Bytes::from_static(b"after-rollback"))
                .unwrap();
            harness.flushed.recv().unwrap();
            harness.output.write_all(b"after-cancel").unwrap();
            assert_eq!(read_rx.recv().unwrap(), b"after-cancel");
            harness.flushed.recv().unwrap();
            assert_eq!(reader_exits.load(Ordering::Acquire), 0);

            harness.handle.shutdown();
            drop(harness.handle);
            drop(harness.output);
        }

        #[test]
        fn handoff_timeout_does_not_satisfy_the_next_pause_with_a_stale_ack() {
            let harness = actor_harness(
                false,
                true,
                Box::new(|_| PtyReadResult {
                    terminal_responses: Vec::new(),
                }),
                None,
            );
            let submission = harness
                .handle
                .queue_user_input_submission(
                    Bytes::from_static(b"slow"),
                    Bytes::from_static(b"\r"),
                    Duration::from_millis(80),
                    None,
                )
                .unwrap();
            harness.flushed.recv().unwrap();

            let error = harness
                .handle
                .begin_handoff(Duration::from_millis(5))
                .unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::TimedOut,
                "unexpected quiesce error: {error}"
            );
            harness
                .handle
                .begin_handoff(Duration::from_secs(1))
                .unwrap();
            submission.recv().unwrap().unwrap();
            harness.handle.rollback_handoff().unwrap();
            harness
                .handle
                .try_write_user_input(Bytes::from_static(b"resumed"))
                .unwrap();

            harness.handle.shutdown();
            drop(harness.handle);
            drop(harness.output);
        }

        #[test]
        fn user_write_admission_is_atomic_with_the_handoff_pause_barrier() {
            let harness = actor_harness(
                false,
                true,
                Box::new(|_| PtyReadResult {
                    terminal_responses: Vec::new(),
                }),
                None,
            );

            for index in 0..100 {
                let start = Arc::new(std::sync::Barrier::new(3));
                let write_start = Arc::clone(&start);
                let write_handle = harness.handle.clone();
                let marker = Bytes::from(format!("race-{index}"));
                let expected = marker.clone();
                let write = std::thread::spawn(move || {
                    write_start.wait();
                    write_handle.try_write_user_input(marker)
                });
                let pause_start = Arc::clone(&start);
                let pause_handle = harness.handle.clone();
                let pause = std::thread::spawn(move || {
                    pause_start.wait();
                    pause_handle.begin_handoff(Duration::from_secs(1))
                });
                start.wait();

                let write_result = write.join().unwrap();
                pause.join().unwrap().unwrap();
                let was_written = harness
                    .writer
                    .lock()
                    .unwrap()
                    .writes
                    .iter()
                    .any(|write| write.as_slice() == expected.as_ref());
                assert_eq!(
                    write_result.is_ok(),
                    was_written,
                    "an admitted write must precede the completed pause barrier"
                );
                harness.handle.rollback_handoff().unwrap();
            }

            harness.handle.shutdown();
            drop(harness.handle);
            drop(harness.output);
        }

        #[test]
        fn unsupported_and_imported_actors_do_not_quiesce_or_start_io_early() {
            let unsupported = actor_harness(
                false,
                false,
                Box::new(|_| PtyReadResult {
                    terminal_responses: Vec::new(),
                }),
                None,
            );
            let error = unsupported
                .handle
                .begin_handoff(Duration::from_millis(20))
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
            unsupported
                .handle
                .try_write_user_input(Bytes::from_static(b"still-running"))
                .unwrap();
            unsupported.flushed.recv().unwrap();
            unsupported.handle.shutdown();
            drop(unsupported.handle);
            drop(unsupported.output);

            let (read_tx, read_rx) = std_mpsc::channel();
            let mut imported = actor_harness(
                true,
                true,
                Box::new(move |bytes| {
                    read_tx.send(bytes.to_vec()).unwrap();
                    PtyReadResult {
                        terminal_responses: Vec::new(),
                    }
                }),
                None,
            );
            assert!(imported
                .handle
                .try_write_user_input(Bytes::from_static(b"too-early"))
                .is_err());
            imported.handle.resize(30, 90, 0, 0, Vec::new());
            imported.output.write_all(b"waiting").unwrap();
            assert!(read_rx.recv_timeout(Duration::from_millis(20)).is_err());
            assert!(imported.resizes.lock().unwrap().is_empty());

            imported.handle.activate_after_handoff().unwrap();
            assert_eq!(
                read_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
                b"waiting"
            );
            imported
                .handle
                .try_write_user_input(Bytes::from_static(b"active"))
                .unwrap();
            imported.flushed.recv().unwrap();
            imported.handle.shutdown();
            drop(imported.handle);
            drop(imported.output);
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::*;
