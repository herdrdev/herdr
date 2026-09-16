#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;

#[cfg(windows)]
pub(crate) struct SpawnedPty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub handoff_child: OwnedHandle,
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WindowsPtyHandoff {
    handles: [usize; 6],
    owner: WindowsPtyHandoffOwner,
}

#[cfg(windows)]
#[derive(Debug)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "used by the stacked Windows handoff integration")
)]
enum WindowsPtyHandoffOwner {
    Local,
    Remote(OwnedHandle),
    Disarmed,
}

#[cfg(windows)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "used by the stacked Windows handoff integration")
)]
impl WindowsPtyHandoff {
    fn remote(handles: [usize; 6], target_process: OwnedHandle) -> Self {
        Self {
            handles,
            owner: WindowsPtyHandoffOwner::Remote(target_process),
        }
    }

    pub(crate) fn into_raw_handles(mut self) -> [usize; 6] {
        self.owner = WindowsPtyHandoffOwner::Disarmed;
        std::mem::take(&mut self.handles)
    }

    fn take_master_handles(&mut self) -> [usize; 5] {
        std::array::from_fn(|index| std::mem::take(&mut self.handles[index]))
    }

    fn take_child_handle(&mut self) -> usize {
        std::mem::take(&mut self.handles[5])
    }

    /// # Safety
    ///
    /// Each value must be a live handle in the current process and must be
    /// adopted exactly once.
    pub(crate) unsafe fn from_raw_handles(handles: [usize; 6]) -> Self {
        Self {
            handles,
            owner: WindowsPtyHandoffOwner::Local,
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsPtyHandoff {
    fn drop(&mut self) {
        match &self.owner {
            WindowsPtyHandoffOwner::Local => {
                for handle in self.handles {
                    if handle != 0 && handle != INVALID_HANDLE_VALUE as usize {
                        unsafe { drop(OwnedHandle::from_raw_handle(handle as _)) };
                    }
                }
            }
            WindowsPtyHandoffOwner::Remote(process) => {
                let process = process.as_raw_handle() as usize;
                for handle in self.handles {
                    if handle != 0 && handle != INVALID_HANDLE_VALUE as usize {
                        let _ = crate::platform::close_handle_in_process(handle, process);
                    }
                }
            }
            WindowsPtyHandoffOwner::Disarmed => {}
        }
    }
}

#[cfg(windows)]
pub(crate) fn spawn_with_portable_pty(
    rows: u16,
    cols: u16,
    cmd: CommandBuilder,
) -> std::io::Result<SpawnedPty> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let handoff_child = duplicate_child_handle(child.as_ref())?;

    Ok(SpawnedPty {
        master: pair.master,
        child,
        handoff_child,
    })
}

#[cfg(windows)]
fn duplicate_child_handle(child: &dyn Child) -> std::io::Result<OwnedHandle> {
    let raw = child
        .as_raw_handle()
        .ok_or_else(|| std::io::Error::other("PTY child process handle is unavailable"))?;
    unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()
}

#[cfg(windows)]
pub(crate) fn windows_handoff_available() -> bool {
    portable_pty::win::conpty::ConPtyMasterPty::supports_handoff()
}

#[cfg(windows)]
pub(crate) fn windows_handoff_supported(master: &dyn MasterPty) -> bool {
    master
        .downcast_ref::<portable_pty::win::conpty::ConPtyMasterPty>()
        .is_some_and(|_| windows_handoff_available())
}

#[cfg(windows)]
pub(crate) fn duplicate_windows_handoff(
    master: &dyn MasterPty,
    child: &OwnedHandle,
    target_process: OwnedHandle,
) -> std::io::Result<WindowsPtyHandoff> {
    let master = master
        .downcast_ref::<portable_pty::win::conpty::ConPtyMasterPty>()
        .ok_or_else(|| std::io::Error::other("Windows PTY backend cannot be transferred"))?;
    let target = target_process.as_raw_handle() as usize;
    let child =
        crate::platform::duplicate_handle_into_process(child.as_raw_handle() as usize, target)?;
    let mut handoff = WindowsPtyHandoff::remote([0, 0, 0, 0, 0, child], target_process);
    let [input, output, signal, reference, process] = master
        .duplicate_for_handoff(target)
        .map_err(|err| std::io::Error::other(err.to_string()))?
        .into_raw_handles();
    handoff.handles[..5].copy_from_slice(&[input, output, signal, reference, process]);
    Ok(handoff)
}

#[cfg(windows)]
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "used by the stacked Windows handoff integration")
)]
pub(crate) unsafe fn adopt_windows_handoff(
    mut handoff: WindowsPtyHandoff,
    size: PtySize,
) -> std::io::Result<SpawnedPty> {
    let [input, output, signal, reference, process] = handoff.take_master_handles();
    let master = unsafe {
        portable_pty::win::conpty::ConPtyMasterPty::from_handoff(
            portable_pty::win::conpty::ConPtyHandoff::from_raw_handles([
                input, output, signal, reference, process,
            ]),
            size,
        )
    }
    .map_err(|err| std::io::Error::other(err.to_string()))?;
    let child = handoff.take_child_handle();
    let child = unsafe { portable_pty::win::WinChild::from_handoff_handle(child) }?;
    let handoff_child = duplicate_child_handle(&child)?;
    Ok(SpawnedPty {
        master: Box::new(master),
        child: Box::new(child),
        handoff_child,
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::{
        io::{AsHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    };
    use std::process::{ChildStdin, ChildStdout, Command, Stdio};

    const TEST_NAME: &str =
        "pty::backend::tests::bundled_conpty_handoff_transfers_six_handles_between_processes";
    const ROLE: &str = "HERDR_CONPTY_HANDOFF_TEST_ROLE";
    const MODE: &str = "HERDR_CONPTY_HANDOFF_TEST_MODE";
    const EXPECTED_PID: &str = "HERDR_CONPTY_HANDOFF_TEST_CHILD_PID";

    struct Target {
        child: std::process::Child,
        input: ChildStdin,
        output: BufReader<ChildStdout>,
    }

    impl Target {
        fn spawn(
            master: &dyn MasterPty,
            handoff_child: &OwnedHandle,
            mode: &str,
            expected_pid: u32,
        ) -> std::io::Result<Self> {
            let mut child = Command::new(std::env::current_exe()?)
                .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
                .env(ROLE, "target")
                .env(MODE, mode)
                .env(EXPECTED_PID, expected_pid.to_string())
                .env(
                    "HERDR_WINDOWS_CONPTY",
                    if mode == "unsupported" {
                        "system"
                    } else {
                        "bundled"
                    },
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                .spawn()?;
            let target_process = child.as_handle().try_clone_to_owned()?;
            let handles = duplicate_windows_handoff(master, handoff_child, target_process)?
                .into_raw_handles();
            let mut input = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("target stdin is unavailable"))?;
            writeln!(
                input,
                "{} {} {} {} {} {}",
                handles[0], handles[1], handles[2], handles[3], handles[4], handles[5]
            )?;
            input.flush()?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| std::io::Error::other("target stdout is unavailable"))?;
            Ok(Self {
                child,
                input,
                output: BufReader::new(output),
            })
        }

        fn read(&mut self, prefix: &str) -> std::io::Result<String> {
            loop {
                let mut line = String::new();
                if self.output.read_line(&mut line)? == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("target exited before {prefix}"),
                    ));
                }
                if let Some(start) = line.find(prefix) {
                    return Ok(line[start..].trim().to_string());
                }
            }
        }

        fn send(&mut self, instruction: &str) -> std::io::Result<()> {
            writeln!(self.input, "{instruction}")?;
            self.input.flush()
        }

        fn finish(mut self) -> std::io::Result<()> {
            drop(self.input);
            let status = self.child.wait()?;
            if status.success() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "target exited with {status}"
                )))
            }
        }
    }

    fn read_pty_until(reader: &mut dyn Read, needle: &str) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        while !bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
        {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("PTY closed before {needle}"),
                ));
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        Ok(bytes)
    }

    fn run_pane_child() -> anyhow::Result<()> {
        let pid = std::process::id();
        println!("PANE_READY:{pid}");
        std::io::stdout().flush()?;
        for line in std::io::stdin().lock().lines() {
            let line = line?;
            let line = line.trim_end_matches('\r');
            if line == "size" {
                let (cols, rows) = crossterm::terminal::size()?;
                println!("SIZE:{pid}:{cols}x{rows}");
            } else {
                println!("ECHO:{pid}:{line}");
            }
            std::io::stdout().flush()?;
            if line == "exit-7" {
                std::process::exit(7);
            }
        }
        Ok(())
    }

    fn read_transferred_handles() -> std::io::Result<[usize; 6]> {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let values = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<usize>, _>>()
            .map_err(std::io::Error::other)?;
        values.try_into().map_err(|values: Vec<usize>| {
            std::io::Error::other(format!("expected six handles, got {}", values.len()))
        })
    }

    fn run_target() -> anyhow::Result<()> {
        let mode = std::env::var(MODE).map_err(std::io::Error::other)?;
        let expected_pid = std::env::var(EXPECTED_PID)
            .map_err(std::io::Error::other)?
            .parse::<u32>()
            .map_err(std::io::Error::other)?;
        let mut handles = read_transferred_handles()?;
        if mode == "partial" || mode == "unsupported" {
            let original = handles;
            let is_open = |handle| {
                let mut flags = 0;
                unsafe {
                    windows_sys::Win32::Foundation::GetHandleInformation(handle as _, &mut flags)
                        != 0
                }
            };
            assert!(original.into_iter().all(is_open));
            if mode == "partial" {
                unsafe { drop(OwnedHandle::from_raw_handle(handles[0] as _)) };
                handles[0] = 0;
            }
            let handoff = unsafe { WindowsPtyHandoff::from_raw_handles(handles) };
            assert!(unsafe { adopt_windows_handoff(handoff, PtySize::default()) }.is_err());
            assert!(original.into_iter().all(|handle| !is_open(handle)));
            println!("ADOPTION_CLEAN:{mode}");
            std::io::stdout().flush()?;
            return Ok(());
        }

        let handoff = unsafe { WindowsPtyHandoff::from_raw_handles(handles) };
        let mut pty = unsafe {
            adopt_windows_handoff(
                handoff,
                PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            )
        }?;
        assert_eq!(pty.child.process_id(), Some(expected_pid));
        println!("ADOPTED:{expected_pid}");
        std::io::stdout().flush()?;

        let mut instruction = String::new();
        std::io::stdin().read_line(&mut instruction)?;
        if mode == "rollback" {
            assert_eq!(instruction.trim(), "rollback");
            drop(pty);
            println!("ROLLED_BACK");
            std::io::stdout().flush()?;
            return Ok(());
        }

        assert_eq!(mode, "commit");
        assert_eq!(instruction.trim(), "commit");
        pty.master.resize(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut reader = pty.master.try_clone_reader()?;
        let mut writer = pty.master.take_writer()?;
        writer.write_all("size\r\ncommit-ü-🦀\r\nexit-7\r\n".as_bytes())?;
        writer.flush()?;
        let output = read_pty_until(reader.as_mut(), &format!("ECHO:{expected_pid}:exit-7"))?;
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains(&format!("SIZE:{expected_pid}:100x30")));
        assert!(output.contains(&format!("ECHO:{expected_pid}:commit-ü-🦀")));
        assert_eq!(pty.child.wait()?.exit_code(), 7);
        println!("COMMITTED:{expected_pid}:7");
        std::io::stdout().flush()?;
        Ok(())
    }

    fn write_and_expect(
        writer: &mut dyn Write,
        reader: &mut dyn Read,
        pid: u32,
        text: &str,
    ) -> std::io::Result<()> {
        writer.write_all(format!("{text}\r\n").as_bytes())?;
        writer.flush()?;
        read_pty_until(reader, &format!("ECHO:{pid}:{text}"))?;
        Ok(())
    }

    fn run_source() -> anyhow::Result<()> {
        assert!(windows_handoff_available());
        let executable = std::env::current_exe()?;
        let mut command = CommandBuilder::new(&executable);
        command.args(["--ignored", "--exact", TEST_NAME, "--nocapture"]);
        command.env(ROLE, "pane");
        let pty = spawn_with_portable_pty(24, 80, command)?;
        assert!(windows_handoff_supported(pty.master.as_ref()));
        let pid = pty
            .child
            .process_id()
            .ok_or_else(|| std::io::Error::other("pane child PID is unavailable"))?;
        let mut reader = pty.master.try_clone_reader()?;
        let mut writer = pty.master.take_writer()?;
        read_pty_until(reader.as_mut(), &format!("PANE_READY:{pid}"))?;
        write_and_expect(writer.as_mut(), reader.as_mut(), pid, "before-ü-🦀")?;

        for mode in ["partial", "unsupported"] {
            let mut partial = Target::spawn(pty.master.as_ref(), &pty.handoff_child, mode, pid)?;
            assert_eq!(
                partial.read("ADOPTION_CLEAN:")?,
                format!("ADOPTION_CLEAN:{mode}")
            );
            partial.finish()?;
            write_and_expect(writer.as_mut(), reader.as_mut(), pid, "after-partial-ü-🦀")?;
        }

        let mut rollback = Target::spawn(pty.master.as_ref(), &pty.handoff_child, "rollback", pid)?;
        assert_eq!(rollback.read("ADOPTED:")?, format!("ADOPTED:{pid}"));
        rollback.send("rollback")?;
        assert_eq!(rollback.read("ROLLED_BACK")?, "ROLLED_BACK");
        rollback.finish()?;
        write_and_expect(writer.as_mut(), reader.as_mut(), pid, "after-rollback-ü-🦀")?;

        let mut commit = Target::spawn(pty.master.as_ref(), &pty.handoff_child, "commit", pid)?;
        assert_eq!(commit.read("ADOPTED:")?, format!("ADOPTED:{pid}"));
        drop(writer);
        drop(reader);
        drop(pty);
        commit.send("commit")?;
        assert_eq!(commit.read("COMMITTED:")?, format!("COMMITTED:{pid}:7"));
        Ok(commit.finish()?)
    }

    fn run_coordinator() -> anyhow::Result<()> {
        let package_dir = std::env::var_os("HERDR_CONPTY_PACKAGE_DIR")
            .ok_or_else(|| std::io::Error::other("HERDR_CONPTY_PACKAGE_DIR is required"))?;
        let executable = std::path::PathBuf::from(package_dir).join(format!(
            "herdr-conpty-handoff-test-{}.exe",
            std::process::id()
        ));
        std::fs::copy(std::env::current_exe()?, &executable)?;
        let statuses = [
            ("system", "system"),
            ("writer-drop", "bundled"),
            ("source", "bundled"),
        ]
        .map(|(role, backend)| {
            Command::new(&executable)
                .args(["--ignored", "--exact", TEST_NAME, "--nocapture"])
                .env(ROLE, role)
                .env("HERDR_WINDOWS_CONPTY", backend)
                .status()
        });
        let _ = std::fs::remove_file(&executable);
        for status in statuses {
            let status = status?;
            anyhow::ensure!(status.success(), "handoff test child exited with {status}");
        }
        Ok(())
    }

    fn run_writer_drop() -> anyhow::Result<()> {
        let mut command = CommandBuilder::new(std::env::current_exe()?);
        command.args(["--ignored", "--exact", TEST_NAME, "--nocapture"]);
        command.env(ROLE, "pane");
        let mut pty = spawn_with_portable_pty(24, 80, command)?;
        let pid = pty.child.process_id().expect("pane child PID");
        let mut reader = pty.master.try_clone_reader()?;
        let writer = pty.master.take_writer()?;
        read_pty_until(reader.as_mut(), &format!("PANE_READY:{pid}"))?;
        drop(writer);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let exited = loop {
            if pty.child.try_wait()?.is_some() {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        if !exited {
            pty.child.kill()?;
            pty.child.wait()?;
        }
        anyhow::ensure!(
            exited,
            "dropping the writer must close input while the master is still alive"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires HERDR_CONPTY_PACKAGE_DIR with the verified bundled runtime"]
    fn bundled_conpty_handoff_transfers_six_handles_between_processes() {
        let result = match std::env::var(ROLE).as_deref() {
            Ok("system") => {
                assert!(!windows_handoff_available());
                Ok(())
            }
            Ok("pane") => run_pane_child(),
            Ok("target") => run_target(),
            Ok("source") => run_source(),
            Ok("writer-drop") => run_writer_drop(),
            _ => run_coordinator(),
        };
        result.unwrap();
    }
}
