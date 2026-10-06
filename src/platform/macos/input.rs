//! Preflight for submissions that overflow Darwin's canonical line discipline.

use std::os::fd::RawFd;
use std::sync::{Arc, Weak};

/// Samples the actor's terminal without keeping its master descriptor open.
#[derive(Clone, Default)]
pub(crate) struct PtyInputGuard(Weak<std::fs::File>);

impl PtyInputGuard {
    pub(crate) fn new(master: &Arc<std::fs::File>) -> Self {
        Self(Arc::downgrade(master))
    }

    /// Check the current terminal mode before an API command enters the queue.
    pub(crate) fn validate(&self, bytes: &[u8]) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;

        let master = self.0.upgrade().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
        })?;
        validate_pty_submission(master.as_raw_fd(), bytes, &[])
    }
}

/// Validate encoded input against the terminal mode sampled at admission.
/// This bounds the current submission, not earlier incomplete input or later
/// mode changes while accepted input waits in the actor's existing queue.
pub(crate) fn validate_pty_submission(fd: RawFd, text: &[u8], enter: &[u8]) -> std::io::Result<()> {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) } != 0 {
        let err = std::io::Error::last_os_error();
        // Non-terminal transports have no canonical line discipline.
        return if matches!(err.raw_os_error(), Some(libc::ENOTTY | libc::EOPNOTSUPP)) {
            Ok(())
        } else {
            Err(err)
        };
    }
    let termios = unsafe { termios.assume_init() };
    if termios.c_lflag & libc::ICANON == 0 {
        return Ok(());
    }
    let limit = unsafe { libc::fpathconf(fd, libc::_PC_MAX_CANON) };
    if limit <= 0 {
        return Err(std::io::Error::other(
            "could not determine canonical input capacity",
        ));
    }
    let line_len = longest_canonical_line(&termios, text.iter().chain(enter).copied());
    if line_len > limit as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("input contains a {line_len}-byte line but the terminal is in canonical mode with a {limit}-byte limit; wait for the shell prompt and retry"),
        ));
    }
    Ok(())
}

/// Count an upper bound between effective delimiters. Editing controls do not
/// reduce the bound: this is a preflight, not a shadow of the kernel's queue.
fn longest_canonical_line(termios: &libc::termios, input: impl Iterator<Item = u8>) -> usize {
    let iflag = termios.c_iflag;
    let lflag = termios.c_lflag;
    let extproc = lflag & libc::EXTPROC != 0;
    let extended = lflag & libc::IEXTEN != 0;
    let matches_control =
        |byte: u8, index: usize| byte != libc::_POSIX_VDISABLE && byte == termios.c_cc[index];
    let mut quoted = false;
    let mut line_len = 0usize;
    let mut longest = 0;
    for mut byte in input {
        if iflag & libc::ISTRIP != 0 {
            byte &= 0x7f;
        }
        let was_quoted = std::mem::take(&mut quoted);
        let mut delimiter = false;
        if !extproc && !was_quoted {
            if extended && matches_control(byte, libc::VLNEXT) {
                quoted = true;
                continue;
            }
            // Controls consumed before CR translation / line-break detection
            // cannot become delimiters, even with custom control characters.
            let consumed = (extended && matches_control(byte, libc::VDISCARD))
                || (lflag & libc::ISIG != 0
                    && [libc::VINTR, libc::VQUIT, libc::VSUSP]
                        .into_iter()
                        .any(|index| matches_control(byte, index)))
                || (iflag & libc::IXON != 0
                    && [libc::VSTART, libc::VSTOP]
                        .into_iter()
                        .any(|index| matches_control(byte, index)));
            if consumed {
                continue;
            }
            if byte == b'\r' {
                if iflag & libc::IGNCR != 0 {
                    continue;
                }
                if iflag & libc::ICRNL != 0 {
                    byte = b'\n';
                }
            } else if byte == b'\n' && iflag & libc::INLCR != 0 {
                byte = b'\r';
            }
        }
        if !was_quoted {
            let editing = !extproc
                && ([libc::VERASE, libc::VKILL]
                    .into_iter()
                    .any(|index| matches_control(byte, index))
                    || (extended
                        && [libc::VWERASE, libc::VREPRINT, libc::VSTATUS]
                            .into_iter()
                            .any(|index| matches_control(byte, index))));
            if editing {
                continue;
            }
            delimiter = byte == b'\n'
                || matches_control(byte, libc::VEOF)
                || matches_control(byte, libc::VEOL)
                || (extended && matches_control(byte, libc::VEOL2));
        }
        // Darwin escapes an unstripped 0xff when PARMRK is active.
        let width = if !was_quoted
            && byte == 0xff
            && iflag & libc::PARMRK != 0
            && iflag & libc::ISTRIP == 0
            && iflag & (libc::IGNBRK | libc::IGNPAR) != (libc::IGNBRK | libc::IGNPAR)
        {
            2
        } else {
            1
        };
        line_len = line_len.saturating_add(width);
        longest = longest.max(line_len);
        if delimiter {
            line_len = 0;
        }
    }
    longest
}
