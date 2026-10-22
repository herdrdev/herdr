use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

use crate::platform::HostPowerEvent;

pub(crate) fn monitor_host_power(
    sender: UnboundedSender<HostPowerEvent>,
) -> Option<tokio::task::JoinHandle<()>> {
    Some(tokio::spawn(async move {
        let mut retry = Duration::from_secs(1);
        loop {
            let result =
                async { watch_connection(zbus::Connection::system().await?, &sender).await }.await;
            match result {
                Ok(()) => retry = Duration::from_secs(1),
                Err(error) => tracing::debug!(%error, "host power notifications unavailable"),
            }
            tokio::time::sleep(retry).await;
            retry = (retry * 2).min(Duration::from_secs(60));
        }
    }))
}

async fn watch_connection(
    connection: zbus::Connection,
    sender: &UnboundedSender<HostPowerEvent>,
) -> zbus::Result<()> {
    let mut sleeping = false;
    let result = watch_manager(connection, sender, &mut sleeping).await;
    // A lost source must not leave reconnection inhibited indefinitely. This is
    // uncertain recovery, never permission to start an interactive retry burst.
    if sleeping {
        report_power(false, sender);
    }
    result
}

async fn watch_manager(
    connection: zbus::Connection,
    sender: &UnboundedSender<HostPowerEvent>,
    sleeping: &mut bool,
) -> zbus::Result<()> {
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let mut owners = manager.receive_owner_changed().await?;
    let mut signals = manager.receive_signal("PrepareForSleep").await?;
    let preparing: bool = manager.get_property("PreparingForSleep").await?;
    // Joining while logind prepares to sleep still needs a paired notification.
    if preparing {
        report_power(true, sender);
    }
    *sleeping = preparing;
    loop {
        tokio::select! {
            _ = owners.next() => return Ok(()),
            signal = signals.next() => {
                let Some(signal) = signal else { return Ok(()) };
                let preparing: bool = signal.body().deserialize()?;
                *sleeping = preparing;
                report_power(preparing, sender);
            }
        }
    }
}

fn report_power(sleeping: bool, sender: &UnboundedSender<HostPowerEvent>) {
    let _ = sender.send(if sleeping {
        HostPowerEvent::Suspend
    } else {
        // logind does not distinguish a user wake from maintenance.
        HostPowerEvent::UnclassifiedResume
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::{Child, Command, Stdio};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct PrivateBus(Child);

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct LoginManager {
        sleeping: Arc<AtomicBool>,
        read: Arc<tokio::sync::Notify>,
    }

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl LoginManager {
        #[zbus(property)]
        fn preparing_for_sleep(&self) -> bool {
            self.read.notify_one();
            self.sleeping.load(Ordering::Acquire)
        }
    }

    #[tokio::test]
    #[ignore = "requires dbus-daemon; uses a private bus and never suspends the host"]
    async fn logind_loss_releases_suspend_once_without_initial_or_repeated_false_wakes() {
        let mut bus = PrivateBus(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let sleeping = Arc::new(AtomicBool::new(false));
        let read = Arc::new(tokio::sync::Notify::new());
        let service = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at(
                "/org/freedesktop/login1",
                LoginManager {
                    sleeping: sleeping.clone(),
                    read: read.clone(),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let first_sender = sender.clone();
        let first_client = client.clone();
        let first = tokio::spawn(async move {
            watch_connection(first_client, &first_sender).await.unwrap();
        });
        tokio::time::timeout(Duration::from_secs(3), read.notified())
            .await
            .unwrap();
        assert!(
            receiver.try_recv().is_err(),
            "initial awake property is not a resume"
        );
        sleeping.store(true, Ordering::Release);
        service
            .emit_signal(
                None::<&str>,
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                "PrepareForSleep",
                &true,
            )
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), receiver.recv())
                .await
                .unwrap(),
            Some(HostPowerEvent::Suspend)
        );
        service
            .release_name("org.freedesktop.login1")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), first)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            receiver.try_recv().unwrap(),
            HostPowerEvent::UnclassifiedResume
        );
        sleeping.store(false, Ordering::Release);
        // Consume any property reads from the previous subscription.
        let _ = tokio::time::timeout(Duration::from_millis(10), read.notified()).await;
        service
            .request_name("org.freedesktop.login1")
            .await
            .unwrap();
        let second = tokio::spawn(async move { watch_connection(client, &sender).await });
        tokio::time::timeout(Duration::from_secs(3), read.notified())
            .await
            .unwrap();
        // Receiving the next real suspend also proves the initial false was consumed
        // without emitting a second recovery event.
        for (preparing, expected) in [
            (true, HostPowerEvent::Suspend),
            (false, HostPowerEvent::UnclassifiedResume),
        ] {
            service
                .emit_signal(
                    None::<&str>,
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                    "PrepareForSleep",
                    &preparing,
                )
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), receiver.recv())
                    .await
                    .unwrap(),
                Some(expected)
            );
        }
        second.abort();
        let _ = second.await;
        assert!(receiver.try_recv().is_err());
        assert!(receiver.is_closed());
    }
}
