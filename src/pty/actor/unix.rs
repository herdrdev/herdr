use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc, Arc, Mutex, TryLockError,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::mpsc::{self, error::TryRecvError as DataTryRecvError};
use tracing::{debug, error, warn};

use crate::pty::fd;

// Actor handle methods must call wake_actor() after queuing work. The idle
// timeout is only a fallback for missed wakes; PTY and wake readiness drive
// normal responsiveness.
const ACTOR_IDLE_POLL_MS: i32 = 1000;
const ACTOR_COMMAND_BUFFER: usize = 1024;
const HANDOFF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorState {
    Running,
    Quiesced,
    Released,
}

pub(crate) struct PtyReadResult {
    pub terminal_responses: Vec<Bytes>,
}

impl PtyReadResult {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            terminal_responses: Vec::new(),
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PtyResizeRequest {
    resize: PtyResize,
    terminal_responses: Vec<Bytes>,
}

#[derive(Default)]
struct SharedPtyControls {
    resize: Option<PtyResizeRequest>,
    nudge: Option<PtyResize>,
    terminal_responses: Vec<Bytes>,
}

pub(crate) struct PtyIoActorConfig {
    pub pane_id: u32,
    pub master_fd: OwnedFd,
    pub initially_quiesced: bool,
    pub on_read: ReadCallback,
    pub on_reader_exit: Option<ReaderExitCallback>,
    pub on_user_writes_poisoned: Option<Arc<dyn Fn() + Send + Sync>>,
    pub pending_user_input: Arc<AtomicU64>,
}

enum PtyIoDataCommand {
    WriteUserInput { bytes: Bytes },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlledWriteResult {
    Written,
    DeliveryUnknown { written: usize },
    Refused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteOwnerAcquireResult {
    Acquired,
    AlreadyControlled,
    RefusedForSafety,
}

enum PtyIoControlCommand {
    BeginHandoff(std_mpsc::Sender<std::io::Result<()>>),
    DuplicateForHandoff(std_mpsc::Sender<std::io::Result<RawFd>>),
    ForegroundProcessGroup(std_mpsc::Sender<Option<u32>>),
    RollbackHandoff(std_mpsc::Sender<std::io::Result<()>>),
    ReleaseAfterCommit(std_mpsc::Sender<std::io::Result<()>>),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct PtyIoActorHandle {
    data_tx: mpsc::Sender<PtyIoDataCommand>,
    control_tx: std_mpsc::Sender<PtyIoControlCommand>,
    wake: fd::WakeWriter,
    controlled_write_fd: Option<RawFd>,
    user_writes: Arc<Mutex<UserWriteGate>>,
    // Lock order is user_writes -> pty_write. Terminal-response writes take
    // only pty_write and never user_writes, so no path can form a cycle. The
    // PTY write lock is held only across the individual write(2) syscall.
    pty_write: Arc<Mutex<()>>,
    user_writes_poisoned: Arc<AtomicBool>,
    on_user_writes_poisoned: Option<Arc<dyn Fn() + Send + Sync>>,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
    pending_user_input: Arc<AtomicU64>,
}

#[derive(Debug)]
struct UserWriteGate {
    accepting: bool,
    remote_owner: Option<u64>,
    pending_local_writes: usize,
}

impl PtyIoActorHandle {
    fn mark_user_writes_poisoned(&self) {
        if !self.user_writes_poisoned.swap(true, Ordering::AcqRel) {
            error!("PTY user-write gate was poisoned; refusing local and remote input while keeping the pane alive");
            if let Some(callback) = &self.on_user_writes_poisoned {
                callback();
            }
        }
    }

    fn lock_user_writes(&self) -> Option<std::sync::MutexGuard<'_, UserWriteGate>> {
        if self.user_writes_poisoned.load(Ordering::Acquire) {
            return None;
        }
        match self.user_writes.lock() {
            Ok(guard) => Some(guard),
            Err(_) => {
                self.mark_user_writes_poisoned();
                None
            }
        }
    }

    pub(crate) async fn write_user_input(
        &self,
        bytes: Bytes,
    ) -> Result<(), mpsc::error::SendError<Bytes>> {
        let accepting = {
            let Some(user_writes) = self.lock_user_writes() else {
                return Err(mpsc::error::SendError(bytes));
            };
            user_writes.accepting
        };
        if !accepting {
            return Err(mpsc::error::SendError(bytes));
        }

        let permit = match self.data_tx.reserve().await {
            Ok(permit) => permit,
            Err(_) => return Err(mpsc::error::SendError(bytes)),
        };

        let Some(mut user_writes) = self.lock_user_writes() else {
            return Err(mpsc::error::SendError(bytes));
        };
        if !user_writes.accepting || user_writes.remote_owner.is_some() {
            return Err(mpsc::error::SendError(bytes));
        }
        permit.send(PtyIoDataCommand::WriteUserInput { bytes });
        user_writes.pending_local_writes += 1;
        self.pending_user_input.fetch_add(1, Ordering::AcqRel);
        drop(user_writes);
        self.wake_actor();
        Ok(())
    }

    pub(crate) fn try_write_user_input(
        &self,
        bytes: Bytes,
    ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        let Some(mut user_writes) = self.lock_user_writes() else {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        };
        if !user_writes.accepting {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        if user_writes.remote_owner.is_some() {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        match self
            .data_tx
            .try_send(PtyIoDataCommand::WriteUserInput { bytes })
        {
            Ok(()) => {
                user_writes.pending_local_writes += 1;
                self.pending_user_input.fetch_add(1, Ordering::AcqRel);
                drop(user_writes);
                self.wake_actor();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(PtyIoDataCommand::WriteUserInput { bytes })) => {
                Err(mpsc::error::TrySendError::Full(bytes))
            }
            Err(mpsc::error::TrySendError::Closed(PtyIoDataCommand::WriteUserInput { bytes })) => {
                Err(mpsc::error::TrySendError::Closed(bytes))
            }
        }
    }

    pub(crate) fn try_acquire_remote_owner(&self, owner_id: u64) -> RemoteOwnerAcquireResult {
        if self.user_writes_poisoned.load(Ordering::Acquire) {
            return RemoteOwnerAcquireResult::RefusedForSafety;
        }
        let mut user_writes = match self.user_writes.try_lock() {
            Ok(user_writes) => user_writes,
            // A local writer may hold this gate while it waits for pty_write.
            // Remote acquisition is advisory: refuse rather than blocking in
            // the opposite lock order and deadlocking with an in-flight write.
            Err(TryLockError::WouldBlock) => return RemoteOwnerAcquireResult::RefusedForSafety,
            Err(TryLockError::Poisoned(_)) => {
                self.mark_user_writes_poisoned();
                return RemoteOwnerAcquireResult::RefusedForSafety;
            }
        };
        match user_writes.remote_owner {
            Some(existing) if existing == owner_id => RemoteOwnerAcquireResult::Acquired,
            Some(_) => RemoteOwnerAcquireResult::AlreadyControlled,
            None if user_writes.pending_local_writes != 0 => {
                RemoteOwnerAcquireResult::RefusedForSafety
            }
            None => {
                user_writes.remote_owner = Some(owner_id);
                RemoteOwnerAcquireResult::Acquired
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn acquire_remote_owner(&self, owner_id: u64) -> bool {
        self.try_acquire_remote_owner(owner_id) == RemoteOwnerAcquireResult::Acquired
    }

    pub(crate) fn release_remote_owner(&self, owner_id: u64) {
        let Some(mut user_writes) = self.lock_user_writes() else {
            return;
        };
        if user_writes.remote_owner == Some(owner_id) {
            user_writes.remote_owner = None;
            drop(user_writes);
            self.wake_actor();
        }
    }

    pub(crate) fn try_write_controlled_user_input(
        &self,
        owner_id: u64,
        bytes: &[u8],
    ) -> ControlledWriteResult {
        if bytes.is_empty() {
            return ControlledWriteResult::Written;
        }
        let Some(user_writes) = self.lock_user_writes() else {
            return ControlledWriteResult::Refused;
        };
        if !user_writes.accepting || user_writes.remote_owner != Some(owner_id) {
            return ControlledWriteResult::Refused;
        }
        let Some(controlled_write_fd) = self.controlled_write_fd else {
            return ControlledWriteResult::Refused;
        };

        // The foreground-process-group check and write(2) are not one atomic
        // kernel operation. The kernel can change the tty's foreground group
        // between the server's fresh probe and write(2); no userspace mutex can
        // close that window because it does not serialize with kernel tty state.
        // The window is bounded to this single check-to-write syscall sequence.
        let pty_write = self
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result =
            unsafe { libc::write(controlled_write_fd, bytes.as_ptr().cast(), bytes.len()) };
        drop(pty_write);
        drop(user_writes);
        if result >= 0 {
            let written = result as usize;
            if written == bytes.len() {
                ControlledWriteResult::Written
            } else {
                ControlledWriteResult::DeliveryUnknown { written }
            }
        } else {
            let error = std::io::Error::last_os_error();
            debug!(error = %error, "controlled PTY write did not complete");
            ControlledWriteResult::DeliveryUnknown { written: 0 }
        }
    }

    pub(crate) fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
        let _order = self
            .response_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(bytes) = response() else {
            return;
        };
        if !bytes.is_empty() {
            self.controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .terminal_responses
                .push(bytes);
            self.wake_actor();
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
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.resize = Some(PtyResizeRequest {
                resize: PtyResize {
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                },
                terminal_responses,
            });
        }
        self.wake_actor();
    }

    pub(crate) fn nudge_child_redraw_after_handoff(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.nudge = Some(PtyResize {
                rows,
                cols,
                cell_width_px,
                cell_height_px,
            });
        }
        self.wake_actor();
    }

    pub(crate) fn begin_handoff(&self, timeout: Duration) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate is poisoned; user input is disabled",
                ));
            };
            user_writes.accepting = false;
            if self
                .control_tx
                .send(PtyIoControlCommand::BeginHandoff(reply_tx))
                .is_err()
            {
                user_writes.accepting = true;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "pty actor closed",
                ));
            }
            self.wake_actor();
        }
        match reply_rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                let _ = self.rollback_handoff();
                Err(err)
            }
            Err(_) => {
                let _ = self.rollback_handoff();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for PTY actor to quiesce",
                ))
            }
        }
    }

    pub(crate) fn duplicate_for_handoff(&self) -> std::io::Result<RawFd> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::DuplicateForHandoff(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff duplicate",
            )
        })?
    }

    pub(crate) fn foreground_process_group_id(&self) -> Option<u32> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::ForegroundProcessGroup(reply_tx))
            .ok()?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).ok()?
    }

    pub(crate) fn rollback_handoff(&self) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::RollbackHandoff(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        let result = reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff rollback",
            )
        })?;
        if result.is_ok() {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate is poisoned; user input is disabled",
                ));
            };
            user_writes.accepting = true;
        }
        result
    }

    pub(crate) fn release_after_commit(&self) -> std::io::Result<()> {
        {
            let Some(mut user_writes) = self.lock_user_writes() else {
                return Err(std::io::Error::other(
                    "PTY user-write gate is poisoned; user input is disabled",
                ));
            };
            user_writes.accepting = false;
        }
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .send(PtyIoControlCommand::ReleaseAfterCommit(reply_tx))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed"))?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY actor release",
            )
        })?
    }

    pub(crate) fn shutdown(&self) {
        if let Some(mut user_writes) = self.lock_user_writes() {
            user_writes.accepting = false;
        }
        if self.control_tx.send(PtyIoControlCommand::Shutdown).is_ok() {
            self.wake_actor();
        }
    }

    fn wake_actor(&self) {
        if let Err(err) = self.wake.wake() {
            debug!(err = %err, "failed to wake PTY actor");
        }
    }
}

pub(crate) struct PtyIoActor;

impl PtyIoActor {
    pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, None)
    }

    fn spawn_inner(
        config: PtyIoActorConfig,
        poll_observer: Option<std_mpsc::Sender<()>>,
    ) -> std::io::Result<PtyIoActorHandle> {
        fd::set_cloexec(config.master_fd.as_raw_fd())?;
        fd::set_nonblocking(config.master_fd.as_raw_fd())?;
        let controlled_write_fd = config.master_fd.as_raw_fd();

        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe()?;
        let user_writes = Arc::new(Mutex::new(UserWriteGate {
            accepting: !config.initially_quiesced,
            remote_owner: None,
            pending_local_writes: 0,
        }));
        let user_writes_poisoned = Arc::new(AtomicBool::new(false));
        let pty_write = Arc::new(Mutex::new(()));
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            controlled_write_fd: Some(controlled_write_fd),
            user_writes: Arc::clone(&user_writes),
            pty_write: Arc::clone(&pty_write),
            user_writes_poisoned: Arc::clone(&user_writes_poisoned),
            on_user_writes_poisoned: config.on_user_writes_poisoned.clone(),
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
            pending_user_input: Arc::clone(&config.pending_user_input),
        };

        let mut runner = PtyIoActorRunner {
            pane_id: config.pane_id,
            file: std::fs::File::from(config.master_fd),
            data_rx,
            control_rx,
            state: if config.initially_quiesced {
                ActorState::Quiesced
            } else {
                ActorState::Running
            },
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::clone(&user_writes),
            pty_write,
            user_writes_poisoned,
            on_user_writes_poisoned: config.on_user_writes_poisoned,
            controls,
            response_order,
            pending_user_input: config.pending_user_input,
            on_read: config.on_read,
            on_reader_exit: config.on_reader_exit,
            poll_observer,
        };
        std::thread::Builder::new()
            .name(format!("herdr-pty-{}", config.pane_id))
            .spawn(move || runner.run())
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        Ok(handle)
    }

    #[cfg(test)]
    fn spawn_with_poll_observer(
        config: PtyIoActorConfig,
        poll_observer: std_mpsc::Sender<()>,
    ) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, Some(poll_observer))
    }
}

struct PtyIoActorRunner {
    pane_id: u32,
    file: std::fs::File,
    data_rx: mpsc::Receiver<PtyIoDataCommand>,
    control_rx: std_mpsc::Receiver<PtyIoControlCommand>,
    state: ActorState,
    pending_writes: VecDeque<(Bytes, bool)>,
    current_write_offset: usize,
    wake_read_fd: OwnedFd,
    user_writes: Arc<Mutex<UserWriteGate>>,
    pty_write: Arc<Mutex<()>>,
    user_writes_poisoned: Arc<AtomicBool>,
    on_user_writes_poisoned: Option<Arc<dyn Fn() + Send + Sync>>,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
    pending_user_input: Arc<AtomicU64>,
    on_read: ReadCallback,
    on_reader_exit: Option<ReaderExitCallback>,
    poll_observer: Option<std_mpsc::Sender<()>>,
}

impl PtyIoActorRunner {
    fn mark_user_writes_poisoned(&self) {
        if !self.user_writes_poisoned.swap(true, Ordering::AcqRel) {
            error!(
                pane = self.pane_id,
                "PTY user-write gate was poisoned; refusing local and remote input while keeping the pane alive"
            );
            if let Some(callback) = &self.on_user_writes_poisoned {
                callback();
            }
        }
    }

    fn mark_local_write_drained(&self, user_writes: &mut UserWriteGate) {
        self.mark_local_writes_drained(user_writes, 1);
    }

    fn mark_local_writes_drained(&self, user_writes: &mut UserWriteGate, count: usize) {
        if count == 0 {
            return;
        }
        if user_writes.pending_local_writes < count {
            warn!(
                pane = self.pane_id,
                pending = user_writes.pending_local_writes,
                count,
                "PTY actor completed or discarded local writes without enough pending-write reservations"
            );
            user_writes.pending_local_writes = 0;
        } else {
            user_writes.pending_local_writes -= count;
        }
        let count = count as u64;
        let _ =
            self.pending_user_input
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                    Some(pending.saturating_sub(count))
                });
    }

    fn discard_local_write(&self) {
        match self.user_writes.lock() {
            Ok(mut user_writes) => self.mark_local_write_drained(&mut user_writes),
            Err(poisoned) => {
                self.mark_user_writes_poisoned();
                let mut user_writes = poisoned.into_inner();
                self.mark_local_write_drained(&mut user_writes);
            }
        }
    }

    fn discard_pending_local_writes(&mut self) {
        let pending_queue_count = self
            .pending_writes
            .iter()
            .filter(|(_, user_input)| *user_input)
            .count();
        self.pending_writes.clear();
        self.current_write_offset = 0;

        let mut data_queue_count = 0;
        while let Ok(command) = self.data_rx.try_recv() {
            match command {
                PtyIoDataCommand::WriteUserInput { .. } => data_queue_count += 1,
            }
        }
        self.mark_local_writes_discarded(pending_queue_count + data_queue_count);
    }

    fn mark_local_writes_discarded(&self, count: usize) {
        if count == 0 {
            return;
        }
        match self.user_writes.lock() {
            Ok(mut user_writes) => self.mark_local_writes_drained(&mut user_writes, count),
            Err(poisoned) => {
                self.mark_user_writes_poisoned();
                let mut user_writes = poisoned.into_inner();
                self.mark_local_writes_drained(&mut user_writes, count);
            }
        }
    }

    fn enqueue_write(&mut self, bytes: Bytes) {
        if !bytes.is_empty() {
            self.pending_writes.push_back((bytes, false));
        }
    }

    fn run(&mut self) {
        let mut should_exit = false;
        while !should_exit {
            should_exit = self.drain_commands();
            if should_exit || self.state == ActorState::Released {
                break;
            }

            self.apply_pending_controls();

            if !self.pending_writes.is_empty() {
                self.flush_pending_writes_once();
                if self.state == ActorState::Released {
                    break;
                }
            }

            if let Some(poll_observer) = &self.poll_observer {
                let _ = poll_observer.send(());
            }

            match fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                self.state == ActorState::Running,
                !self.pending_writes.is_empty(),
                ACTOR_IDLE_POLL_MS,
            ) {
                Ok(readiness) => {
                    if readiness.wake_ready {
                        if let Err(err) = fd::drain_wake_fd(self.wake_read_fd.as_raw_fd()) {
                            debug!(pane = self.pane_id, err = %err, "PTY actor wake drain failed");
                            break;
                        }
                        continue;
                    }
                    if self.state == ActorState::Running
                        && readiness.pty_read_ready
                        && !self.read_once()
                    {
                        break;
                    }
                    if readiness.pty_write_ready && !self.pending_writes.is_empty() {
                        self.flush_pending_writes_once();
                        if self.state == ActorState::Released {
                            break;
                        }
                    }
                }
                Err(err) => {
                    debug!(pane = self.pane_id, err = %err, "PTY actor poll failed");
                    break;
                }
            }
        }

        // A handle can retain the actor-owned raw fd after the actor exits.
        // Close the acceptance gate before `file` is dropped so a later direct
        // controlled-write attempt cannot use a recycled descriptor.
        match self.user_writes.lock() {
            Ok(mut user_writes) => {
                user_writes.accepting = false;
                user_writes.remote_owner = None;
            }
            Err(_) => self.mark_user_writes_poisoned(),
        }
        self.discard_pending_local_writes();

        if let Some(on_reader_exit) = self.on_reader_exit.take() {
            on_reader_exit();
        }
        debug!(pane = self.pane_id, "PTY actor exiting");
    }

    fn drain_commands(&mut self) -> bool {
        if self.drain_control_commands() {
            return true;
        }
        self.drain_data_commands()
    }

    fn drain_control_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.control_rx.try_recv() {
                Ok(command) => {
                    if self.handle_control_command(command) {
                        should_exit = true;
                        break;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn drain_data_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.data_rx.try_recv() {
                Ok(command) => {
                    if self.handle_data_command(command) {
                        should_exit = true;
                        break;
                    }
                }
                Err(DataTryRecvError::Empty) => break,
                Err(DataTryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn handle_data_command(&mut self, command: PtyIoDataCommand) -> bool {
        match command {
            PtyIoDataCommand::WriteUserInput { bytes } => {
                if self.state == ActorState::Running && !bytes.is_empty() {
                    self.pending_writes.push_back((bytes, true));
                } else {
                    self.discard_local_write();
                }
            }
        }
        false
    }

    fn handle_control_command(&mut self, command: PtyIoControlCommand) -> bool {
        match command {
            PtyIoControlCommand::BeginHandoff(reply) => {
                let result = self.begin_handoff();
                let _ = reply.send(result);
            }
            PtyIoControlCommand::DuplicateForHandoff(reply) => {
                let result = if self.state == ActorState::Quiesced {
                    fd::duplicate_cloexec_fd(self.file.as_raw_fd())
                } else {
                    Err(std::io::Error::other(
                        "PTY actor must be quiesced before handoff duplication",
                    ))
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ForegroundProcessGroup(reply) => {
                let result =
                    crate::platform::foreground_process_group_id_for_tty_fd(self.file.as_raw_fd());
                let _ = reply.send(result);
            }
            PtyIoControlCommand::RollbackHandoff(reply) => {
                let result = if self.state == ActorState::Released {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "PTY actor was released before handoff rollback",
                    ))
                } else {
                    self.state = ActorState::Running;
                    Ok(())
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ReleaseAfterCommit(reply) => {
                self.state = ActorState::Released;
                self.discard_pending_local_writes();
                let _ = reply.send(Ok(()));
                return true;
            }
            PtyIoControlCommand::Shutdown => return true,
        }
        false
    }

    fn begin_handoff(&mut self) -> std::io::Result<()> {
        self.drain_pre_quiesce_commands();
        self.apply_pending_controls();
        if self.state == ActorState::Released {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY actor was released before handoff quiesce",
            ));
        }
        let deadline = Instant::now() + HANDOFF_DRAIN_TIMEOUT;
        self.flush_pending_writes_once();
        while !self.pending_writes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out draining PTY writes before handoff",
                ));
            }
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let readiness = fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                true,
                true,
                timeout_ms,
            )?;
            if readiness.wake_ready {
                fd::drain_wake_fd(self.wake_read_fd.as_raw_fd())?;
            }
            if readiness.pty_read_ready && !self.read_once() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY closed while draining writes before handoff",
                ));
            }
            if readiness.pty_write_ready {
                self.flush_pending_writes_once();
            }
        }
        self.state = ActorState::Quiesced;
        Ok(())
    }

    fn drain_pre_quiesce_commands(&mut self) {
        while let Ok(command) = self.data_rx.try_recv() {
            let _ = self.handle_data_command(command);
        }
    }

    fn apply_pending_controls(&mut self) {
        let (resize, nudge, terminal_responses) = {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                controls.resize.take(),
                controls.nudge.take(),
                std::mem::take(&mut controls.terminal_responses),
            )
        };
        if self.state == ActorState::Released {
            return;
        }
        if let Some(request) = resize {
            self.resize(request.resize);
            self.enqueue_terminal_responses(request.terminal_responses);
        }
        if let Some(nudge) = nudge {
            self.nudge(nudge);
        }
        self.enqueue_terminal_responses(terminal_responses);
    }

    fn read_once(&mut self) -> bool {
        let mut buf = [0u8; 8192];
        match self.file.read(&mut buf) {
            Ok(0) => false,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
            Err(err) => {
                debug!(pane = self.pane_id, err = %err, "PTY actor read failed");
                false
            }
            Ok(n) => {
                let response_order = Arc::clone(&self.response_order);
                let _order = response_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = (self.on_read)(&buf[..n]);
                self.controls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .terminal_responses
                    .extend(result.terminal_responses);
                drop(_order);
                let terminal_responses = std::mem::take(
                    &mut self
                        .controls
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .terminal_responses,
                );
                self.enqueue_terminal_responses(terminal_responses);
                true
            }
        }
    }

    fn enqueue_terminal_responses(&mut self, terminal_responses: Vec<Bytes>) {
        if self.state == ActorState::Released {
            return;
        }
        for bytes in terminal_responses {
            self.enqueue_write(bytes);
        }
    }

    fn flush_pending_writes_once(&mut self) {
        while let Some((bytes, user_input)) = self.pending_writes.front() {
            let bytes = bytes.clone();
            let user_input = *user_input;
            let mut user_writes = if user_input {
                match self.user_writes.lock() {
                    Ok(user_writes) => Some(user_writes),
                    Err(_) => {
                        self.mark_user_writes_poisoned();
                        self.pending_writes.pop_front();
                        self.current_write_offset = 0;
                        self.discard_local_write();
                        continue;
                    }
                }
            } else {
                None
            };
            if user_input
                && user_writes
                    .as_ref()
                    .is_some_and(|user_writes| user_writes.remote_owner.is_some())
            {
                warn!(
                    pane = self.pane_id,
                    "refusing to drop pending local PTY input while remote ownership is held"
                );
                return;
            }
            let chunk = &bytes[self.current_write_offset..];
            let pty_write = self
                .pty_write
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let write_result = self.file.write(chunk);
            drop(pty_write);
            match write_result {
                Ok(0) => {
                    warn!(pane = self.pane_id, "PTY actor write returned zero bytes");
                    return;
                }
                Ok(written) => {
                    self.current_write_offset += written;
                    if self.current_write_offset >= bytes.len() {
                        self.pending_writes.pop_front();
                        self.current_write_offset = 0;
                        if let Some(user_writes) = user_writes.as_mut() {
                            self.mark_local_write_drained(user_writes);
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => return,
                Err(err) => {
                    warn!(pane = self.pane_id, err = %err, "PTY actor write failed");
                    drop(user_writes);
                    self.discard_pending_local_writes();
                    return;
                }
            }
        }
        let _ = self.file.flush();
    }

    fn resize(&self, resize: PtyResize) {
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn nudge(&mut self, resize: PtyResize) {
        if self.state == ActorState::Released {
            return;
        }
        let nudge = if resize.rows > 2 {
            (
                resize.rows - 1,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        } else {
            (
                resize.rows,
                resize.cols.saturating_sub(1).max(4),
                resize.cell_width_px,
                resize.cell_height_px,
            )
        };
        if nudge
            == (
                resize.rows,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        {
            return;
        }
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            nudge.0,
            nudge.1,
            nudge.2,
            nudge.3,
        ));
        std::thread::sleep(Duration::from_millis(30));
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn log_resize_result(&self, result: std::io::Result<()>) {
        if let Err(err) = result {
            debug!(pane = self.pane_id, err = %err, "PTY resize failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::Shutdown,
        os::fd::{AsRawFd, FromRawFd, IntoRawFd},
        os::unix::net::UnixStream,
        sync::atomic::{AtomicBool, Ordering},
    };

    fn test_wake_pair() -> (fd::WakeWriter, OwnedFd) {
        let pipe = fd::create_wake_pipe().expect("wake pipe");
        (pipe.writer, pipe.read_fd)
    }

    fn actor_with_socket_pair(
        initially_quiesced: bool,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        actor_with_socket_pair_and_poll_observer(initially_quiesced, None)
    }

    fn actor_with_socket_pair_and_poll_observer(
        initially_quiesced: bool,
        poll_observer: Option<std_mpsc::Sender<()>>,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let config = PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };
        let handle = if let Some(poll_observer) = poll_observer {
            PtyIoActor::spawn_with_poll_observer(config, poll_observer)
        } else {
            PtyIoActor::spawn(config)
        }
        .expect("actor spawn");
        (handle, peer, read_rx)
    }

    fn actor_runner_for_unit_test() -> (PtyIoActorRunner, UnixStream) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (_data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(owned),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 1,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(1)),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            poll_observer: None,
        };
        (runner, peer)
    }

    #[cfg(target_os = "linux")]
    fn reap_test_pty(child: libc::pid_t) {
        // SAFETY: child is the process created by actor_handle_for_real_pty.
        unsafe {
            libc::kill(child, libc::SIGKILL);
            libc::waitpid(child, std::ptr::null_mut(), 0);
        }
    }

    #[cfg(target_os = "linux")]
    fn actor_handle_for_real_pty(
        cat_child: bool,
    ) -> (
        PtyIoActorHandle,
        std_mpsc::Receiver<Bytes>,
        libc::pid_t,
        u32,
    ) {
        actor_handle_for_real_pty_with_echo_disabled(cat_child, false)
    }

    #[cfg(target_os = "linux")]
    fn actor_handle_for_real_pty_with_echo_disabled(
        cat_child: bool,
        prefill_input: bool,
    ) -> (
        PtyIoActorHandle,
        std_mpsc::Receiver<Bytes>,
        libc::pid_t,
        u32,
    ) {
        let mut master = -1;
        let mut slave_name = [0 as libc::c_char; 128];
        // SAFETY: forkpty initializes a real PTY pair and returns the child pid
        // in the parent. The child immediately execs below.
        let child = unsafe {
            libc::forkpty(
                &mut master,
                slave_name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert!(
            child >= 0,
            "forkpty failed: {}",
            std::io::Error::last_os_error()
        );
        if child == 0 {
            // SAFETY: the forkpty child owns stdin/stdout/stderr and execs now.
            unsafe {
                if cat_child {
                    libc::execl(
                        c"/bin/cat".as_ptr(),
                        c"cat".as_ptr(),
                        std::ptr::null::<libc::c_char>(),
                    );
                } else {
                    libc::execl(
                        c"/bin/sleep".as_ptr(),
                        c"sleep".as_ptr(),
                        c"60".as_ptr(),
                        std::ptr::null::<libc::c_char>(),
                    );
                }
                libc::_exit(127);
            }
        }
        let foreground_group = (0..100).find_map(|_| {
            let group = crate::platform::foreground_process_group_id_for_tty_fd(master);
            if group.is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
            group
        });
        let foreground_group = foreground_group.expect("child did not become PTY foreground");
        // SAFETY: configure the PTY before handing its duplicated master to the actor.
        let mut termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(master, &mut termios) },
            0,
            "PTY tcgetattr failed"
        );
        if cat_child {
            unsafe {
                libc::cfmakeraw(&mut termios);
                libc::tcsetattr(master, libc::TCSANOW, &termios);
            }
        }
        let actor_fd = fd::duplicate_cloexec_fd(master).expect("duplicate PTY master");
        unsafe {
            libc::close(master);
        }
        let (read_tx, read_rx) = std_mpsc::channel();
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            // SAFETY: ownership of the duplicated master descriptor is transferred once.
            master_fd: unsafe { OwnedFd::from_raw_fd(actor_fd) },
            initially_quiesced: false,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("real PTY read receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            pending_user_input: Arc::new(AtomicU64::new(0)),
        })
        .expect("real PTY actor spawn");
        if prefill_input {
            let controlled_write_fd = handle.controlled_write_fd.expect("PTY master fd");
            // Keep the actor from consuming echoed input while the slave-side
            // input queue is being filled to its nonblocking limit.
            let mut termios = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(controlled_write_fd, &mut termios) },
                0,
                "PTY tcgetattr failed while preparing EAGAIN"
            );
            termios.c_lflag &= !libc::ECHO;
            assert_eq!(
                unsafe { libc::tcsetattr(controlled_write_fd, libc::TCSANOW, &termios) },
                0,
                "PTY tcsetattr failed while preparing EAGAIN"
            );
        }
        (handle, read_rx, child, foreground_group)
    }

    #[cfg(target_os = "linux")]
    fn controlled_context() -> crate::api::schema::RemoteControlContext {
        crate::api::schema::RemoteControlContext {
            host: "buildbox".into(),
            user: "operator".into(),
            workspace_id: "workspace".into(),
            tab_id: "tab".into(),
            pane_id: "pane".into(),
            terminal_id: "terminal".into(),
            cwd: "/work".into(),
            foreground_cwd: "/work".into(),
            tty: "/dev/pts/test".into(),
            foreground_process: crate::api::schema::RemoteForegroundProcess {
                pid: 1234,
                process_group_id: 1234,
                name: "agent".into(),
                argv: vec!["agent".into(), "run".into()],
                cwd: "/work".into(),
            },
            detected_agent: "agent".into(),
            interactive_ready: true,
            human_draft: false,
            state_change_seq: 1,
            revision: 2,
            context_epoch: 3,
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_context_change_writes_zero_bytes(
        mutate: impl FnOnce(&mut crate::api::schema::RemoteControlContext),
    ) {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(false);
        assert!(handle.acquire_remote_owner(7));
        let expected = controlled_context();
        let mut current = expected.clone();
        mutate(&mut current);
        let validation = crate::server::remote_control::validate_context(
            "buildbox", "operator", &expected, &current,
        );
        assert_eq!(
            validation.as_ref().err().map(|error| error.code.as_str()),
            Some("refused_for_safety")
        );
        if validation.is_ok() {
            let _ = handle.try_write_controlled_user_input(7, b"must-not-arrive");
        }
        assert!(read_rx.recv_timeout(Duration::from_millis(100)).is_err());
        handle.shutdown();
        reap_test_pty(child);
    }

    #[test]
    fn actor_ignores_empty_user_input_write() {
        let (mut runner, _peer) = actor_runner_for_unit_test();

        assert!(
            !runner.handle_data_command(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::new(),
            })
        );

        assert!(runner.pending_writes.is_empty());
        assert_eq!(
            runner
                .user_writes
                .lock()
                .expect("user-write gate lock")
                .pending_local_writes,
            0
        );
    }

    #[test]
    fn empty_local_input_allows_later_remote_lease() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);

        handle
            .try_write_user_input(Bytes::new())
            .expect("empty local input is accepted before the actor discards it");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match handle.try_acquire_remote_owner(7) {
                RemoteOwnerAcquireResult::Acquired => break,
                RemoteOwnerAcquireResult::AlreadyControlled => {
                    panic!("remote lease unexpectedly already controlled")
                }
                RemoteOwnerAcquireResult::RefusedForSafety => {
                    assert!(
                        Instant::now() < deadline,
                        "empty local input reservation was not released"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        handle.release_remote_owner(7);
        handle.shutdown();
    }

    #[test]
    fn failed_local_write_does_not_block_later_remote_lease() {
        let (handle, peer, _read_rx) = actor_with_socket_pair(false);
        let write_guard = handle
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        handle
            .try_write_user_input(Bytes::from_static(b"failed-local"))
            .expect("local input is accepted before the write fails");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );

        peer.shutdown(Shutdown::Read)
            .expect("peer read shutdown makes the actor write fail");
        drop(write_guard);

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match handle.try_acquire_remote_owner(7) {
                RemoteOwnerAcquireResult::Acquired => break,
                RemoteOwnerAcquireResult::AlreadyControlled => {
                    panic!("remote lease unexpectedly already controlled")
                }
                RemoteOwnerAcquireResult::RefusedForSafety => {
                    assert!(
                        Instant::now() < deadline,
                        "failed local write reservation was not released"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        handle.release_remote_owner(7);
        handle.shutdown();
    }

    #[test]
    fn release_discards_local_write_for_later_remote_lease() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);
        // Larger than the socket buffer and never read by the peer, so part
        // of the write is still pending when release discards it.
        let payload = vec![b'l'; 4 * 1024 * 1024];
        handle
            .try_write_user_input(Bytes::from(payload))
            .expect("local input is accepted before release");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );

        handle
            .release_after_commit()
            .expect("release should complete");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::Acquired
        );
        handle.release_remote_owner(7);
        handle.shutdown();
    }

    #[test]
    fn actor_exit_discards_local_write_for_later_remote_lease() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);
        // Larger than the socket buffer and never read by the peer, so part
        // of the write is still pending when the actor exit discards it.
        let payload = vec![b'l'; 4 * 1024 * 1024];
        handle
            .try_write_user_input(Bytes::from(payload))
            .expect("local input is accepted before actor exit");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );

        handle.shutdown();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match handle.try_acquire_remote_owner(7) {
                RemoteOwnerAcquireResult::Acquired => break,
                RemoteOwnerAcquireResult::AlreadyControlled => {
                    panic!("remote lease unexpectedly already controlled")
                }
                RemoteOwnerAcquireResult::RefusedForSafety => {
                    assert!(
                        Instant::now() < deadline,
                        "actor exit did not release the local write reservation"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn context_epoch_change_before_batch_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| context.context_epoch += 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn claude_subagents_refreshed_revision_before_batch_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| context.revision += 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn metadata_expiry_before_batch_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| context.state_change_seq += 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn terminal_title_sync_before_batch_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| {
            context.revision += 1;
            context.context_epoch = context.revision;
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_pid_change_with_same_process_group_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| context.foreground_process.pid += 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_argv_change_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| {
            context.foreground_process.argv.push("changed".into())
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_cwd_change_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| {
            context.foreground_process.cwd = "/other".into()
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn matching_context_writes_exact_bytes_once_to_real_pty() {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(true);
        assert!(handle.acquire_remote_owner(7));
        let expected = controlled_context();
        assert!(crate::server::remote_control::validate_context(
            "buildbox", "operator", &expected, &expected,
        )
        .is_ok());
        let bytes = b"exact-once";
        assert_eq!(
            handle.try_write_controlled_user_input(7, bytes),
            ControlledWriteResult::Written
        );
        assert_eq!(
            read_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("cat echoes the direct PTY write"),
            Bytes::copy_from_slice(bytes)
        );
        assert!(read_rx.recv_timeout(Duration::from_millis(100)).is_err());
        handle.shutdown();
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn controlled_and_terminal_response_writes_are_contiguous_on_real_pty() {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(true);
        assert!(handle.acquire_remote_owner(7));

        let controlled = b"controlled-payload".to_vec();
        let response = Bytes::from_static(b"terminal-response");
        let total_len = controlled.len() + response.len();
        let write_guard = handle
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (started_tx, started_rx) = std_mpsc::channel();
        let write_handle = handle.clone();
        let controlled_for_thread = controlled.clone();
        let controlled_write = std::thread::spawn(move || {
            started_tx.send(()).expect("controlled writer started");
            write_handle.try_write_controlled_user_input(7, &controlled_for_thread)
        });
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("controlled writer entered the concurrent write");

        handle.write_terminal_response(|| Some(response.clone()));
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !controlled_write.is_finished(),
            "controlled write bypassed the PTY lock"
        );
        assert!(
            read_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "a PTY write escaped while the shared lock was held"
        );
        drop(write_guard);

        assert_eq!(
            controlled_write.join().expect("controlled writer joins"),
            ControlledWriteResult::Written
        );
        let mut observed = Vec::with_capacity(total_len);
        while observed.len() < total_len {
            observed.extend_from_slice(
                &read_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("cat echoes both PTY writes"),
            );
        }
        observed.truncate(total_len);
        let controlled_then_response = [controlled.as_slice(), response.as_ref()].concat();
        let response_then_controlled = [response.as_ref(), controlled.as_slice()].concat();
        assert!(
            observed == controlled_then_response || observed == response_then_controlled,
            "concurrent PTY writes must each remain contiguous"
        );
        handle.shutdown();
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    // AC5: local and controlled writes to one real PTY stay whole under lease and write-lock contention.
    fn local_and_controlled_writes_are_whole_on_one_real_pty_under_contention() {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(true);
        let local = Bytes::from_static(b"local-payload");
        let controlled = b"controlled-payload";

        handle
            .try_write_user_input(local.clone())
            .expect("local input reaches the real PTY actor");
        assert_eq!(
            read_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("cat echoes the local PTY write"),
            local
        );

        assert!(handle.acquire_remote_owner(7));
        let write_guard = handle
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (started_tx, started_rx) = std_mpsc::channel();
        let controlled_handle = handle.clone();
        let controlled_write = std::thread::spawn(move || {
            started_tx.send(()).expect("controlled writer started");
            controlled_handle.try_write_controlled_user_input(7, controlled)
        });
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("controlled writer entered the concurrent write");

        let local_during_control_handle = handle.clone();
        let local_during_control = std::thread::spawn(move || {
            local_during_control_handle
                .try_write_user_input(Bytes::from_static(b"local-during-control"))
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !controlled_write.is_finished(),
            "controlled write bypassed the shared PTY lock"
        );
        assert!(
            read_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "a PTY write escaped while the shared lock was held"
        );
        drop(write_guard);

        assert_eq!(
            controlled_write.join().expect("controlled writer joins"),
            ControlledWriteResult::Written
        );
        assert!(
            local_during_control
                .join()
                .expect("local writer joins")
                .is_err(),
            "local input must be refused while the controlled lease is held"
        );
        assert_eq!(
            read_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("cat echoes the controlled PTY write"),
            Bytes::copy_from_slice(controlled)
        );
        handle.shutdown();
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn foreground_agent_change_with_same_process_group_writes_zero_bytes() {
        assert_context_change_writes_zero_bytes(|context| {
            context.foreground_process.name = "other-agent".into()
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_write_delivers_prefix_once_without_retry() {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(false);
        assert!(handle.acquire_remote_owner(7));
        let bytes = vec![b'x'; 1024 * 1024];
        let result = handle.try_write_controlled_user_input(7, &bytes);
        let ControlledWriteResult::DeliveryUnknown { written } = result else {
            panic!("large nonblocking PTY write should be partial");
        };
        assert!(written > 0 && written < bytes.len());
        let mut observed = Vec::new();
        while observed.len() < written {
            observed.extend_from_slice(
                &read_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("real PTY echoes the written prefix"),
            );
        }
        observed.truncate(written);
        assert_eq!(observed.len(), written);
        assert!(observed.iter().all(|byte| *byte == b'x'));
        handle.release_remote_owner(7);
        assert_eq!(
            handle.try_write_controlled_user_input(7, b"remainder"),
            ControlledWriteResult::Refused
        );
        handle.shutdown();
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn eagain_reports_unknown_without_retry() {
        let (handle, _read_rx, child, _foreground_group) =
            actor_handle_for_real_pty_with_echo_disabled(false, true);
        assert!(handle.acquire_remote_owner(7));
        let filler = vec![b'f'; 64 * 1024];
        let mut reached_eagain = false;
        for _ in 0..4096 {
            match handle.try_write_controlled_user_input(7, &filler) {
                ControlledWriteResult::Written => {}
                ControlledWriteResult::DeliveryUnknown { written: 0 } => {
                    reached_eagain = true;
                    break;
                }
                ControlledWriteResult::DeliveryUnknown { written } => {
                    assert!(written < filler.len());
                }
                ControlledWriteResult::Refused => panic!("controlled owner unexpectedly refused"),
            }
        }
        assert!(reached_eagain, "real PTY input buffer did not reach EAGAIN");
        // The server ends the lease on DeliveryUnknown; EAGAIN is not a
        // persistent promise across independent syscalls, so do not retry it
        // here.
        handle.release_remote_owner(7);
        assert!(handle.acquire_remote_owner(8));
        handle.shutdown();
        reap_test_pty(child);
    }

    #[test]
    fn poisoned_user_write_gate_refuses_local_and_controlled_writes_without_shutdown() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);
        assert!(handle.acquire_remote_owner(7));
        let user_writes = Arc::clone(&handle.user_writes);
        let poison = std::thread::spawn(move || {
            let _guard = user_writes.lock().expect("poisoning lock is healthy");
            panic!("intentional test panic");
        });
        assert!(poison.join().is_err());

        assert!(handle
            .try_write_user_input(Bytes::from_static(b"local"))
            .is_err());
        assert_eq!(
            handle.try_write_controlled_user_input(7, b"controlled"),
            ControlledWriteResult::Refused
        );
        assert!(handle.user_writes_poisoned.load(Ordering::Acquire));
        assert_eq!(
            handle.try_acquire_remote_owner(8),
            RemoteOwnerAcquireResult::RefusedForSafety
        );
        assert_eq!(
            handle
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remote_owner,
            Some(7),
            "refusal must not overwrite the existing remote owner"
        );
        handle.shutdown();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn poisoned_real_pty_gate_keeps_child_alive() {
        let (handle, _read_rx, child, _foreground_group) = actor_handle_for_real_pty(false);
        assert!(handle.acquire_remote_owner(7));
        let user_writes = Arc::clone(&handle.user_writes);
        let poison = std::thread::spawn(move || {
            let _guard = user_writes.lock().expect("poisoning lock is healthy");
            panic!("intentional test panic");
        });
        assert!(poison.join().is_err());

        assert!(handle
            .try_write_user_input(Bytes::from_static(b"local"))
            .is_err());
        assert_eq!(
            handle.try_write_controlled_user_input(7, b"controlled"),
            ControlledWriteResult::Refused
        );
        // A poisoned ownership gate must not terminate the PTY actor or its child.
        assert_eq!(unsafe { libc::kill(child, 0) }, 0);
        handle.shutdown();
        reap_test_pty(child);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unchanged_periodic_context_probes_keep_lease_healthy() {
        let (handle, read_rx, child, _foreground_group) = actor_handle_for_real_pty(true);
        assert!(handle.acquire_remote_owner(7));
        let expected = controlled_context();
        for _ in 0..4 {
            assert!(crate::server::remote_control::validate_context(
                "buildbox", "operator", &expected, &expected,
            )
            .is_ok());
        }
        assert_eq!(
            handle.try_write_controlled_user_input(7, b"healthy"),
            ControlledWriteResult::Written
        );
        assert_eq!(
            read_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("cat echoes the healthy direct PTY write"),
            Bytes::from_static(b"healthy")
        );
        handle.shutdown();
        reap_test_pty(child);
    }

    #[test]
    fn remote_owner_excludes_local_and_api_writes_until_release() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);

        assert!(handle.acquire_remote_owner(7));
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"local-or-api"))
            .is_err());
        handle.release_remote_owner(7);
        handle
            .try_write_user_input(Bytes::from_static(b"after-release"))
            .expect("writes resume after lease release");

        let mut received = [0u8; 13];
        peer.read_exact(&mut received)
            .expect("post-release write reaches the actor fd");
        assert_eq!(&received, b"after-release");
        handle.shutdown();
    }

    #[test]
    fn remote_owner_acquire_refuses_without_waiting_for_contended_write_gate() {
        let (handle, _peer, _read_rx) = actor_with_socket_pair(false);
        let user_writes = Arc::clone(&handle.user_writes);
        let (locked_tx, locked_rx) = std_mpsc::sync_channel(0);
        let (release_tx, release_rx) = std_mpsc::sync_channel(0);
        let gate_holder = std::thread::spawn(move || {
            let _guard = user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            locked_tx.send(()).expect("test observes held gate");
            release_rx.recv().expect("test releases held gate");
        });
        locked_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("test thread holds the user-write gate");

        let (started_tx, started_rx) = std_mpsc::sync_channel(0);
        let (result_tx, result_rx) = std_mpsc::sync_channel(0);
        let acquire_handle = handle.clone();
        let acquire = std::thread::spawn(move || {
            started_tx
                .send(())
                .expect("test observes acquisition start");
            result_tx
                .send(acquire_handle.try_acquire_remote_owner(7))
                .expect("test receives acquisition result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("acquisition thread reached the gate operation");
        let while_contended = result_rx.recv_timeout(Duration::from_millis(250));

        release_tx.send(()).expect("release test gate holder");
        gate_holder.join().expect("gate holder exits");
        let after_release = match while_contended {
            Ok(result) => Some(result),
            Err(_) => result_rx.recv_timeout(Duration::from_secs(1)).ok(),
        };
        acquire.join().expect("acquisition thread exits");
        handle.shutdown();

        assert_eq!(
            while_contended,
            Ok(RemoteOwnerAcquireResult::RefusedForSafety),
            "remote acquisition must fail closed immediately while a local writer owns the gate"
        );
        assert_eq!(
            after_release,
            Some(RemoteOwnerAcquireResult::RefusedForSafety)
        );
        assert_eq!(
            handle
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remote_owner,
            None,
            "contention refusal must not acquire or replace ownership"
        );
    }

    #[test]
    fn remote_owner_refuses_while_local_input_is_queued_then_acquires_after_drain() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let write_guard = handle
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        handle
            .try_write_user_input(Bytes::from_static(b"queued-local"))
            .expect("local input is accepted before remote acquisition");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );
        drop(write_guard);

        let mut delivered = [0u8; 12];
        peer.read_exact(&mut delivered)
            .expect("queued local input reaches the PTY in full");
        assert_eq!(&delivered, b"queued-local");
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match handle.try_acquire_remote_owner(7) {
                RemoteOwnerAcquireResult::Acquired => break,
                RemoteOwnerAcquireResult::RefusedForSafety if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                result => {
                    panic!("remote ownership did not become available after drain: {result:?}")
                }
            }
        }
        handle.shutdown();
    }

    #[test]
    fn remote_owner_refuses_while_partial_local_input_remains_then_acquires_after_drain() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let payload = vec![b'l'; 4 * 1024 * 1024];
        handle
            .try_write_user_input(Bytes::from(payload.clone()))
            .expect("large local input is accepted");

        peer.set_nonblocking(true)
            .expect("peer becomes nonblocking for partial observation");
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut prefix = Vec::new();
        while prefix.is_empty() {
            let mut chunk = vec![0u8; 64 * 1024];
            match peer.read(&mut chunk) {
                Ok(0) => panic!("PTY socket closed before local input was delivered"),
                Ok(written) => prefix.extend_from_slice(&chunk[..written]),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "actor did not start writing the partial local buffer"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("failed to observe partial local input: {err}"),
            }
        }
        assert!(
            prefix.len() < payload.len(),
            "the test must observe a partially written local buffer"
        );
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );

        peer.set_nonblocking(false)
            .expect("peer returns to blocking mode");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer read timeout");
        let mut remainder = vec![0u8; payload.len() - prefix.len()];
        peer.read_exact(&mut remainder)
            .expect("partial local input remainder reaches the PTY");
        prefix.extend_from_slice(&remainder);
        assert_eq!(prefix, payload);
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match handle.try_acquire_remote_owner(7) {
                RemoteOwnerAcquireResult::Acquired => break,
                RemoteOwnerAcquireResult::RefusedForSafety if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                result => {
                    panic!("remote ownership did not become available after drain: {result:?}")
                }
            }
        }
        handle.shutdown();
    }

    #[test]
    fn actor_writes_user_input_to_owned_fd() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);

        handle
            .try_write_user_input(Bytes::from_static(b"hello"))
            .expect("write command accepted");

        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives write");
        assert_eq!(&buf, b"hello");
        handle.shutdown();
    }

    #[test]
    fn actor_wakes_idle_poll_for_user_input() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, mut peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        peer.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("peer timeout");
        poll_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("actor entered idle poll");

        let start = Instant::now();
        handle
            .try_write_user_input(Bytes::from_static(b"x"))
            .expect("write command accepted");

        let mut buf = [0u8; 1];
        peer.read_exact(&mut buf)
            .expect("peer receives write without waiting for actor poll timeout");
        assert_eq!(&buf, b"x");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "actor write should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn actor_reads_output_while_input_is_backpressured() {
        let (mut actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");

        let fill = [0xAA; 8192];
        let mut prefilled = 0;
        loop {
            match actor_socket.write(&fill) {
                Ok(written) => prefilled += written,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to fill actor write buffer: {err}"),
            }
        }
        assert!(prefilled > 0, "actor write buffer should accept some bytes");

        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced: false,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            pending_user_input: Arc::new(AtomicU64::new(0)),
        })
        .expect("actor spawn");

        let marker = Bytes::from_static(b"queued-input");
        handle
            .try_write_user_input(marker.clone())
            .expect("write command accepted");

        const OUTPUT_LEN: usize = 128 * 1024;
        let mut peer_writer = peer.try_clone().expect("clone peer writer");
        let output_writer = std::thread::spawn(move || {
            peer_writer
                .write_all(&vec![0xBB; OUTPUT_LEN])
                .expect("peer writes sustained output");
        });
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut output_len = 0;
        while output_len < OUTPUT_LEN {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "actor did not keep reading blocked peer output"
            );
            let output = read_rx
                .recv_timeout(remaining)
                .expect("actor keeps reading while input remains blocked");
            assert!(output.iter().all(|byte| *byte == 0xBB));
            output_len += output.len();
        }
        assert_eq!(output_len, OUTPUT_LEN);
        output_writer.join().expect("output writer joins");

        let mut received_input = vec![0; prefilled + marker.len()];
        peer.read_exact(&mut received_input)
            .expect("peer receives prefill and queued input");
        assert!(received_input[..prefilled].iter().all(|byte| *byte == 0xAA));
        assert_eq!(&received_input[prefilled..], marker.as_ref());
        handle.shutdown();
    }

    #[test]
    fn actor_wakes_idle_poll_for_handoff_control() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, _peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        poll_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor entered idle poll");

        let start = Instant::now();
        let handoff_handle = handle.clone();
        let handoff =
            std::thread::spawn(move || handoff_handle.begin_handoff(Duration::from_secs(1)));

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff control should wake idle actor");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "handoff control should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn poll_ignores_pty_hup_without_pty_interest() {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        drop(peer);
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");

        let readiness = fd::poll_pty_and_wake(
            actor_socket.as_raw_fd(),
            wake_pipe.read_fd.as_raw_fd(),
            false,
            false,
            10,
        )
        .expect("poll succeeds");

        assert!(!readiness.pty_read_ready);
        assert!(!readiness.pty_write_ready);
        assert!(!readiness.wake_ready);
    }

    #[test]
    fn actor_delivers_fd_reads_to_callback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        peer.write_all(b"from-peer").expect("peer write");

        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor read callback");
        assert_eq!(read, Bytes::from_static(b"from-peer"));
        handle.shutdown();
    }

    #[test]
    fn begin_handoff_stops_reads_and_rejects_user_writes_until_rollback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        peer.write_all(b"held").expect("peer write during quiesce");
        assert!(
            read_rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "actor must not read while quiesced"
        );

        handle.rollback_handoff().expect("rollback resumes actor");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reads held bytes after rollback");
        assert_eq!(read, Bytes::from_static(b"held"));

        handle
            .try_write_user_input(Bytes::from_static(b"after"))
            .expect("write accepted after rollback");
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives after");
        assert_eq!(&buf, b"after");
        handle.shutdown();
    }

    #[test]
    fn handoff_drains_local_write_for_later_remote_lease() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let local = Bytes::from_static(b"handoff-local");
        let write_guard = handle
            .pty_write
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        handle
            .try_write_user_input(local.clone())
            .expect("local input is accepted before handoff");
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::RefusedForSafety
        );
        drop(write_guard);

        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff should drain local input");
        let mut received = vec![0; local.len()];
        peer.read_exact(&mut received)
            .expect("handoff-drained local input reaches the PTY");
        assert_eq!(received, local.as_ref());
        assert_eq!(
            handle.try_acquire_remote_owner(7),
            RemoteOwnerAcquireResult::Acquired
        );
        handle.release_remote_owner(7);
        handle.shutdown();
    }

    #[test]
    fn duplicate_for_handoff_requires_quiesced_actor() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        assert!(handle.duplicate_for_handoff().is_err());
        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        let duplicate = handle
            .duplicate_for_handoff()
            .expect("handoff duplicate created");
        assert!(duplicate >= 0);
        unsafe {
            libc::close(duplicate);
        }
        handle.rollback_handoff().expect("rollback resumes actor");

        peer.write_all(b"still-live").expect("peer write");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor still reads after duplicate closes");
        assert_eq!(read, Bytes::from_static(b"still-live"));
        handle.shutdown();
    }

    #[test]
    fn resize_and_nudge_keep_latest_request_when_command_queue_is_full() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, _control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
            })
            .expect("fill command queue");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            controlled_write_fd: None,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            on_user_writes_poisoned: None,
            controls: Arc::clone(&controls),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };

        handle.resize(20, 80, 8, 16, vec![Bytes::from_static(b"old")]);
        handle.resize(40, 120, 9, 18, vec![Bytes::from_static(b"new")]);
        handle.nudge_child_redraw_after_handoff(41, 121, 10, 20);
        handle.write_terminal_response(|| Some(Bytes::from_static(b"response")));

        let controls = controls.lock().expect("controls lock");
        assert_eq!(
            controls.resize,
            Some(PtyResizeRequest {
                resize: PtyResize {
                    rows: 40,
                    cols: 120,
                    cell_width_px: 9,
                    cell_height_px: 18,
                },
                terminal_responses: vec![Bytes::from_static(b"new")],
            })
        );
        assert_eq!(
            controls.nudge,
            Some(PtyResize {
                rows: 41,
                cols: 121,
                cell_width_px: 10,
                cell_height_px: 20,
            })
        );
        assert_eq!(
            controls.terminal_responses,
            vec![Bytes::from_static(b"response")]
        );
    }

    #[test]
    fn appearance_transition_report_precedes_query_of_new_scheme() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::channel();
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let light = Arc::new(AtomicBool::new(false));
        let query_light = Arc::clone(&light);
        let runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(owned),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: wake_pipe.read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
            pending_user_input: Arc::new(AtomicU64::new(0)),
            on_read: Box::new(move |_| PtyReadResult {
                terminal_responses: vec![if query_light.load(Ordering::Acquire) {
                    Bytes::from_static(b"query-light")
                } else {
                    Bytes::from_static(b"query-dark")
                }],
            }),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            poll_observer: None,
        };
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            controlled_write_fd: None,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            on_user_writes_poisoned: None,
            controls,
            response_order,
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };
        let (changed_tx, changed_rx) = std_mpsc::channel();
        let (continue_tx, continue_rx) = std_mpsc::channel();

        let appearance = std::thread::spawn(move || {
            handle.write_terminal_response(|| {
                light.store(true, Ordering::Release);
                changed_tx.send(()).expect("notify appearance change");
                continue_rx.recv().expect("continue appearance report");
                Some(Bytes::from_static(b"live-light"))
            });
        });
        changed_rx.recv().expect("appearance changed");
        peer.write_all(b"query").expect("write query");
        let reader = std::thread::spawn(move || {
            let mut runner = runner;
            assert!(runner.read_once());
            runner
        });
        continue_tx.send(()).expect("release appearance report");
        appearance.join().expect("appearance thread joins");
        let runner = reader.join().expect("reader thread joins");

        assert_eq!(runner.pending_writes.len(), 2);
        assert_eq!(
            runner.pending_writes[0].0,
            Bytes::from_static(b"live-light")
        );
        assert_eq!(
            runner.pending_writes[1].0,
            Bytes::from_static(b"query-light")
        );
        assert!(runner
            .pending_writes
            .iter()
            .all(|(_, user_input)| !user_input));
    }

    #[test]
    fn resize_writes_terminal_responses_while_remote_lease_is_held() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let response = Bytes::from_static(b"\x1B[48;40;100;720;900t");

        assert!(handle.acquire_remote_owner(7));
        handle.resize(40, 100, 9, 18, vec![response.clone()]);

        let mut buf = vec![0; response.len()];
        peer.read_exact(&mut buf)
            .expect("peer receives resize response");
        assert_eq!(Bytes::from(buf), response);
        handle.shutdown();
    }

    #[tokio::test]
    async fn async_user_input_waits_for_queue_capacity() {
        let (data_tx, mut data_rx) = mpsc::channel(1);
        let (control_tx, _control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            controlled_write_fd: None,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            on_user_writes_poisoned: None,
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };

        let write = tokio::spawn(async move {
            handle
                .write_user_input(Bytes::from_static(b"wait-for-capacity"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !write.is_finished(),
            "async input should wait for queue capacity"
        );

        assert!(matches!(
            data_rx.recv().await,
            Some(PtyIoDataCommand::WriteUserInput { .. })
        ));
        write
            .await
            .expect("write task joins")
            .expect("write succeeds after capacity opens");
        match data_rx.recv().await {
            Some(PtyIoDataCommand::WriteUserInput { bytes, .. }) => {
                assert_eq!(bytes, Bytes::from_static(b"wait-for-capacity"));
            }
            _ => panic!("expected queued user input"),
        }
    }

    #[tokio::test]
    async fn async_user_input_waiting_for_capacity_is_rejected_after_handoff_begins() {
        let (data_tx, mut data_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            controlled_write_fd: None,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            on_user_writes_poisoned: None,
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };
        let write_handle = handle.clone();
        let write = tokio::spawn(async move {
            write_handle
                .write_user_input(Bytes::from_static(b"after-handoff-start"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let handoff = std::thread::spawn(move || handle.begin_handoff(Duration::from_secs(1)));
        match control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff control command")
        {
            PtyIoControlCommand::BeginHandoff(reply) => {
                reply.send(Ok(())).expect("handoff waiter alive");
            }
            _ => panic!("expected begin handoff command"),
        }
        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff succeeds");
        assert!(matches!(
            data_rx.recv().await,
            Some(PtyIoDataCommand::WriteUserInput { .. })
        ));

        let err = write.await.expect("write task joins").expect_err(
            "write waiting for capacity must be rejected after handoff closes the input gate",
        );
        assert_eq!(err.0, Bytes::from_static(b"after-handoff-start"));
        match tokio::time::timeout(Duration::from_millis(50), data_rx.recv()).await {
            Err(_) | Ok(None) => {}
            Ok(Some(_)) => panic!("rejected write must not be queued"),
        }
    }

    #[test]
    fn handoff_control_is_not_blocked_by_full_data_queue() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"fill"),
            })
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            data_tx,
            control_tx,
            wake,
            controlled_write_fd: None,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 0,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            on_user_writes_poisoned: None,
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(0)),
        };

        let handoff = std::thread::spawn(move || handle.begin_handoff(Duration::from_secs(1)));
        match control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff control command")
        {
            PtyIoControlCommand::BeginHandoff(reply) => {
                reply.send(Ok(())).expect("handoff waiter alive");
            }
            _ => panic!("expected begin handoff command"),
        }

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff succeeds despite full data queue");
    }

    #[test]
    fn begin_handoff_drains_user_writes_already_in_command_queue() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput {
                bytes: Bytes::from_static(b"queued-before-ack"),
            })
            .expect("queued write");
        let mut runner = PtyIoActorRunner {
            pane_id: 1,
            file: std::fs::File::from(unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) }),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            wake_read_fd: fd::create_wake_pipe().expect("wake pipe").read_fd,
            user_writes: Arc::new(Mutex::new(UserWriteGate {
                accepting: true,
                remote_owner: None,
                pending_local_writes: 1,
            })),
            pty_write: Arc::new(Mutex::new(())),
            user_writes_poisoned: Arc::new(AtomicBool::new(false)),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            pending_user_input: Arc::new(AtomicU64::new(1)),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            on_user_writes_poisoned: None,
            poll_observer: None,
        };

        runner.begin_handoff().expect("handoff drains queued write");

        let mut buf = [0u8; 17];
        peer.read_exact(&mut buf)
            .expect("queued write reaches peer before quiesce ack");
        assert_eq!(&buf, b"queued-before-ack");
        assert_eq!(runner.state, ActorState::Quiesced);
        assert_eq!(
            runner
                .user_writes
                .lock()
                .expect("user-write gate lock")
                .pending_local_writes,
            0
        );
    }

    #[test]
    fn release_after_commit_prevents_further_io() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle.release_after_commit().expect("actor released");
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        let _ = peer.write_all(b"ignored");
        assert!(read_rx.recv_timeout(Duration::from_millis(150)).is_err());
    }
}
