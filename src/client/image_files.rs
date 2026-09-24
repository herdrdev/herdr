//! Client-owned Kitty `t=t` files, prepared only at the final output boundary.
//!
//! A returned path is published: it must stay readable until the terminal unlinks
//! it, or the client shuts down. Never use `cleanup` to cancel a frame. This ledger
//! does not encode pixels or accept paths from a server. A silent raw-file query
//! gates conversion for the local Ghostty allowlist; an explicit kill switch
//! always wins. Local writes alone do not prove support. Non-consumption disables
//! new files for this client, without deleting files the terminal may still open.

use std::path::PathBuf;

#[derive(Default)]
pub(crate) struct FileTransport {
    #[cfg(unix)]
    inner: Option<crate::platform::unix_image_files::Ledger>,
}

impl FileTransport {
    #[cfg(all(test, unix))]
    pub(crate) fn for_test(root: PathBuf) -> Self {
        Self {
            inner: Some(crate::platform::unix_image_files::Ledger::for_test(root)),
        }
    }

    pub(crate) fn from_environment() -> Self {
        Self {
            #[cfg(unix)]
            inner: environment_allows().then(crate::platform::unix_image_files::Ledger::new),
        }
    }

    /// Once per client, return a private 1x1 RGBA query payload. The output
    /// boundary must emit this as `a=q,t=t,f=32,s=1,v=1,q=2` before inline image
    /// output. No real upload is converted until Ghostty consumes this file.
    pub(crate) fn probe(&mut self) -> Option<PathBuf> {
        #[cfg(unix)]
        {
            self.inner.as_mut()?.probe()
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Returns a complete, closed file containing exactly `data`, or requests
    /// inline fallback. The caller must preserve output ordering and encode the
    /// returned local path as Kitty's base64 filename payload with `t=t`.
    pub(crate) fn prepare(&mut self, data: &[u8]) -> Option<PathBuf> {
        #[cfg(unix)]
        {
            self.inner.as_mut()?.prepare(data)
        }
        #[cfg(not(unix))]
        {
            let _ = data;
            None
        }
    }
}

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::io::IsTerminal;

#[cfg(unix)]
fn environment_allows() -> bool {
    allowed(
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
        |key| std::env::var_os(key),
    )
}

#[cfg(unix)]
fn allowed(stdin_tty: bool, stdout_tty: bool, env: impl Fn(&str) -> Option<OsString>) -> bool {
    stdin_tty
        && stdout_tty
        && env("TERM_PROGRAM").as_deref() == Some(std::ffi::OsStr::new("ghostty"))
        && [
            "SSH_CONNECTION",
            "SSH_TTY",
            "SSH_CLIENT",
            "TMUX",
            "STY",
            "HERDR_PANE_ID",
            "HERDR_PANE_RUNTIME_ID",
        ]
        .iter()
        .all(|key| env(key).is_none())
    // Named-session selection (HERDR_SESSION) and HERDR_REMOTE_KEYBINDINGS
    // deliberately do not affect client locality.
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn conservative_gate_without_process_environment_mutation() {
        let base = |key: &str| match key {
            "TERM_PROGRAM" => Some(OsString::from("ghostty")),
            "HERDR_SESSION" => Some(OsString::from("named-session")),
            "HERDR_REMOTE_KEYBINDINGS" => Some(OsString::from("1")),
            _ => None,
        };
        assert!(allowed(true, true, base));
        assert!(!allowed(false, true, base));
        assert!(!allowed(true, false, base));
        for key in [
            "SSH_CONNECTION",
            "SSH_TTY",
            "SSH_CLIENT",
            "TMUX",
            "STY",
            "HERDR_PANE_ID",
            "HERDR_PANE_RUNTIME_ID",
        ] {
            assert!(!allowed(true, true, |name| if name == key {
                Some(OsString::new())
            } else {
                base(name)
            }));
        }
        assert!(!allowed(true, true, |key| if key == "TERM_PROGRAM" {
            Some(OsString::from("kitty"))
        } else {
            base(key)
        }));
    }
    #[test]
    fn default_is_disabled() {
        let mut transport = FileTransport::default();
        assert!(transport.probe().is_none());
        assert!(transport.prepare(b"inline").is_none());
    }
}
