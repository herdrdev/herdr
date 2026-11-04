#![cfg(unix)]

use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output, Stdio};

fn closed_pipe_writer() -> Stdio {
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: pipe initialized both descriptors, and each is assigned to one
    // OwnedFd so it is closed exactly once.
    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    drop(read_fd);
    Stdio::from(write_fd)
}

fn run_with_closed_stdout(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shepherd"))
        .args(args)
        .stdout(closed_pipe_writer())
        .stderr(Stdio::piped())
        .output()
        .expect("run shepherd CLI")
}

fn assert_quiet_sigpipe(output: Output) {
    assert_eq!(output.status.signal(), Some(libc::SIGPIPE));
    assert!(
        output.stderr.is_empty(),
        "closed stdout should not emit an error or panic: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_exits_via_sigpipe_without_panicking_when_stdout_closes() {
    assert_quiet_sigpipe(run_with_closed_stdout(&["api", "schema", "--json"]));
}

#[test]
fn terminal_help_uses_cli_sigpipe_behavior_before_attach_starts() {
    assert_quiet_sigpipe(run_with_closed_stdout(&["terminal", "attach", "--help"]));
}

#[test]
fn completion_direct_writer_uses_cli_sigpipe_behavior() {
    assert_quiet_sigpipe(run_with_closed_stdout(&["completion", "bash"]));
}
