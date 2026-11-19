use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use portable_pty::Child;
use tokio::io::unix::AsyncFd;

fn child_pidfd(child: &dyn Child) -> std::io::Result<AsyncFd<OwnedFd>> {
    let pid = child
        .process_id()
        .ok_or_else(|| std::io::Error::other("pane child has no process ID"))?;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    AsyncFd::new(unsafe { OwnedFd::from_raw_fd(fd as _) })
}

pub(crate) async fn wait_for_child(
    mut child: Box<dyn Child + Send + Sync>,
) -> std::io::Result<portable_pty::ExitStatus> {
    let pidfd = match child_pidfd(child.as_ref()) {
        Ok(pidfd) => pidfd,
        Err(err) => {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(%err, "pidfd unavailable; using blocking pane child wait");
            } else {
                tracing::debug!(%err, "pidfd unavailable; using blocking pane child wait");
            }
            return super::wait_for_child_blocking(child).await;
        }
    };
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let mut ready = pidfd.readable().await?;
        ready.clear_ready();
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{mpsc, Notify};

    use crate::events::AppEvent;
    use crate::layout::PaneId;
    use crate::pane::{AgentDetection, PaneLaunchEnv, PaneRuntime};
    use crate::render_signal::RenderSignal;

    fn pidfd_supported() -> bool {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id(), 0) };
        if fd < 0 {
            return false;
        }
        drop(unsafe { OwnedFd::from_raw_fd(fd as _) });
        true
    }

    #[test]
    fn closed_pane_children_are_reaped_when_blocking_pool_is_busy() {
        // Without pidfd the blocking fallback needs the pool this test fills.
        if !pidfd_supported() {
            return;
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let blocker = rt.spawn_blocking(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        started_rx.recv().unwrap();

        let reaped = rt.block_on(async {
            let (events, mut event_rx) = mpsc::channel(32);
            let runtime = PaneRuntime::spawn_shell_command(
                PaneId::from_raw(42),
                24,
                80,
                std::env::temp_dir(),
                "exec sleep 30",
                &PaneLaunchEnv::default(),
                AgentDetection::Disabled,
                0,
                crate::terminal_theme::TerminalTheme::default(),
                None,
                events,
                Arc::new(Notify::new()),
                Arc::new(RenderSignal::new()),
            )
            .unwrap();
            runtime.shutdown();
            tokio::time::timeout(Duration::from_secs(1), async {
                while let Some(event) = event_rx.recv().await {
                    if let AppEvent::PaneDied {
                        pane_id,
                        exit_reason,
                    } = event
                    {
                        return pane_id == PaneId::from_raw(42)
                            && !matches!(
                                exit_reason,
                                crate::platform::ChildExitReason::WaitFailed
                            );
                    }
                }
                false
            })
            .await
        });
        release_tx.send(()).unwrap();
        rt.block_on(blocker).unwrap();
        assert!(matches!(reaped, Ok(true)));
    }
}
