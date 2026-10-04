#![cfg(target_os = "linux")]

use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct LoginManager {
    peer: Arc<Mutex<Option<UnixStream>>>,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl LoginManager {
    fn inhibit(
        &self,
        what: &str,
        _who: &str,
        _why: &str,
        mode: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        assert_eq!((what, mode), ("shutdown", "delay"));
        let (lock, peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        *self.peer.lock().unwrap() = Some(peer);
        Ok(std::os::fd::OwnedFd::from(lock).into())
    }

    #[zbus(property)]
    fn preparing_for_shutdown(&self) -> bool {
        false
    }
}

fn private_bus(address: &str) -> ChildGuard {
    let mut child = ChildGuard(
        Command::new("dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--print-address=1",
                "--address",
                address,
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut ready = String::new();
    std::io::BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    child
}

async fn login_service(address: &str, peer: Arc<Mutex<Option<UnixStream>>>) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .name("org.freedesktop.login1")
        .unwrap()
        .serve_at("/org/freedesktop/login1", LoginManager { peer })
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn wait_for_inhibitor(peer: &Mutex<Option<UnixStream>>) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while peer.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

fn api(socket: &Path, method: &str, params: serde_json::Value) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        stream,
        "{}",
        serde_json::json!({"id":"test", "method":method, "params":params})
    )
    .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires dbus-daemon; exercises a private bus and disposable named server, not host shutdown"]
async fn host_shutdown_saves_layout_before_releasing_delay_lock() {
    let base = std::path::PathBuf::from(format!(
        "/var/tmp/hhs-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let address = format!("unix:path={}", base.join("bus").display());
    let mut bus = private_bus(&address);
    let peer = Arc::new(Mutex::new(None));
    let mut service = login_service(&address, peer.clone()).await;
    let socket = base.join("herdr-dev/sessions/shutdown/herdr.sock");
    let config = base.join("config.toml");
    std::fs::write(&config, "onboarding = false\n[experimental]\nallow_nested = true\n[terminal]\ndefault_shell = \"/bin/sh\"\n").unwrap();
    let mut server = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_herdr"))
            .args(["--session", "shutdown", "server"])
            .env("XDG_CONFIG_HOME", &base)
            .env("XDG_STATE_HOME", &base)
            .env("XDG_RUNTIME_DIR", &base)
            .env("HERDR_CONFIG_PATH", &config)
            .env_remove("HERDR_SOCKET_PATH")
            .env("DBUS_SYSTEM_BUS_ADDRESS", address.trim())
            .env_remove("HERDR_CLIENT_SOCKET_PATH")
            .env_remove("HERDR_STARTUP_CWD")
            .env_remove("HERDR_SESSION")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_PANE_ID")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !socket.exists() || peer.lock().unwrap().is_none() {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "server exited during startup"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // Neither a login1 owner change nor a bus restart may leave a stale inhibitor.
    peer.lock().unwrap().take();
    service
        .release_name("org.freedesktop.login1")
        .await
        .unwrap();
    drop(service);
    service = login_service(&address, peer.clone()).await;
    wait_for_inhibitor(&peer).await;
    peer.lock().unwrap().take();
    drop(service);
    drop(bus);
    if base.join("bus").exists() {
        std::fs::remove_file(base.join("bus")).unwrap();
    }
    bus = private_bus(&address);
    service = login_service(&address, peer.clone()).await;
    wait_for_inhibitor(&peer).await;

    for label in ["one", "two", "three"] {
        api(
            &socket,
            "workspace.create",
            serde_json::json!({"cwd":base,"label":label,"focus":true}),
        );
    }
    service
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "PrepareForShutdown",
            &true,
        )
        .await
        .unwrap();
    let mut peer = peer.lock().unwrap().take().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match peer.read(&mut [0]) {
                Ok(0) => break,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                other => panic!("unexpected inhibitor state: {other:?}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let saved = base.join("herdr-dev/sessions/shutdown/session.json");
    let layout: serde_json::Value = serde_json::from_slice(&std::fs::read(saved).unwrap()).unwrap();
    assert_eq!(layout["workspaces"].as_array().unwrap().len(), 3);
    assert_eq!(layout["workspaces"][2]["custom_name"], "three");
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.0.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(server);
    drop(service);
    drop(bus);
    std::fs::remove_dir_all(base).unwrap();
}
