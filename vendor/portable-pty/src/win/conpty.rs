use crate::cmdbuilder::CommandBuilder;
use crate::win::psuedocon::PsuedoCon;
use crate::{Child, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::Error;
use filedescriptor::{FileDescriptor, Pipe};
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::sync::{Arc, Mutex, Weak};
use winapi::um::handleapi::CloseHandle;
use winapi::um::handleapi::INVALID_HANDLE_VALUE;
use winapi::um::winnt::HANDLE;
use winapi::um::wincon::COORD;

#[derive(Default)]
pub struct ConPtySystem {}

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;

        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable: stdout.read,
                writable: Some(stdin.write),
                writer_taken: false,
                size,
            })),
        };

        let slave = ConPtySlavePty {
            inner: master.inner.clone(),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: PsuedoCon,
    readable: FileDescriptor,
    writable: Option<FileDescriptor>,
    writer_taken: bool,
    size: PtySize,
}

impl Inner {
    pub fn resize(
        &mut self,
        num_rows: u16,
        num_cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), Error> {
        self.con.resize(COORD {
            X: num_cols as i16,
            Y: num_rows as i16,
        })?;
        self.size = PtySize {
            rows: num_rows,
            cols: num_cols,
            pixel_width,
            pixel_height,
        };
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

struct ConPtyWriter {
    writer: FileDescriptor,
    inner: Weak<Mutex<Inner>>,
}

impl Write for ConPtyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for ConPtyWriter {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            // Keep the handoff duplicate source only for the returned writer's lifetime.
            inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .writable
                .take();
        }
    }
}

#[derive(Debug)]
pub struct ConPtyHandoff {
    handles: [usize; 5],
    close_on_drop: bool,
}

impl ConPtyHandoff {
    pub fn into_raw_handles(mut self) -> [usize; 5] {
        self.close_on_drop = false;
        std::mem::take(&mut self.handles)
    }

    fn take(&mut self, index: usize) -> usize {
        std::mem::take(&mut self.handles[index])
    }

    /// # Safety
    ///
    /// Each value must be a live, target-local handle whose ownership is being
    /// transferred to the returned handoff. Unconsumed local handles close on drop.
    pub unsafe fn from_raw_handles(handles: [usize; 5]) -> Self {
        Self {
            handles,
            close_on_drop: true,
        }
    }
}

impl Drop for ConPtyHandoff {
    fn drop(&mut self) {
        if self.close_on_drop {
            for handle in self.handles {
                if handle != 0 && handle != INVALID_HANDLE_VALUE as usize {
                    unsafe { CloseHandle(handle as _) };
                }
            }
        }
    }
}

impl ConPtyMasterPty {
    pub fn supports_handoff() -> bool {
        PsuedoCon::supports_handoff()
    }

    pub fn duplicate_for_handoff(&self, target_process: usize) -> anyhow::Result<ConPtyHandoff> {
        let inner = self.inner.lock().unwrap();
        let target_process = target_process as HANDLE;
        if target_process.is_null() || target_process == INVALID_HANDLE_VALUE {
            anyhow::bail!("target process handle is invalid");
        }
        inner
            .con
            .duplicate_for_handoff(
                target_process,
                inner
                    .writable
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("PTY writer is closed"))?
                    .as_raw_handle() as _,
                inner.readable.as_raw_handle() as _,
            )
            .map(|handles| ConPtyHandoff {
                handles,
                // These handles live in the target process, not this process.
                close_on_drop: false,
            })
    }

    /// # Safety
    ///
    /// The handoff must contain five live handles in the current process and
    /// transfer their ownership exactly once.
    pub unsafe fn from_handoff(mut handoff: ConPtyHandoff, size: PtySize) -> anyhow::Result<Self> {
        let [input, output, ..] = handoff.handles;
        let valid = |handle| handle != 0 && handle != INVALID_HANDLE_VALUE as usize;
        anyhow::ensure!(valid(input) && valid(output), "transferred PTY pipe handle is invalid");
        let writable = unsafe { FileDescriptor::from_raw_handle(handoff.take(0) as _) };
        let readable = unsafe { FileDescriptor::from_raw_handle(handoff.take(1) as _) };
        let con = unsafe {
            PsuedoCon::from_handoff([handoff.take(2), handoff.take(3), handoff.take(4)])
        }?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable,
                writable: Some(writable),
                writer_taken: false,
                size,
            })),
        })
    }
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.resize(size.rows, size.cols, size.pixel_width, size.pixel_height)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.size.clone())
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        let mut inner = self.inner.lock().unwrap();
        if inner.writer_taken {
            anyhow::bail!("writer already taken");
        }
        let writer = inner
            .writable
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("PTY writer is closed"))?
            .try_clone()?;
        inner.writer_taken = true;
        Ok(Box::new(ConPtyWriter {
            writer,
            inner: Arc::downgrade(&self.inner),
        }))
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}
