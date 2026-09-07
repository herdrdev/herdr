//! Native ownership tests for the Windows headless handoff boundary.
use crate::{
    ipc,
    protocol::{self, ClientMessage, ServerMessage},
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Session {
    root: PathBuf,
    name: String,
    binary: PathBuf,
    child: Option<Child>,
}

impl Session {
    fn new(mode: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "herdr-native-handoff-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let name = "handoff-owned".to_owned();
        fs::create_dir_all(root.join("herdr-dev")).unwrap();
        fs::write(
            root.join("herdr-dev/config.toml"),
            "onboarding = false\n[terminal]\ndefault_shell = 'pwsh.exe -NoLogo -NoProfile'\n",
        )
        .unwrap();
        let binary =
            PathBuf::from(std::env::var_os("HERDR_HANDOFF_TEST_BIN").expect(
                "set HERDR_HANDOFF_TEST_BIN to the staged debug herdr.exe with conpty bundle",
            ));
        let mut session = Self {
            root,
            name,
            binary,
            child: None,
        };
        let mut command = session.command();
        command.arg("server");
        if mode == "system" {
            command.env("HERDR_WINDOWS_CONPTY", "system");
        } else if !mode.is_empty() {
            command.env("HERDR_TEST_HANDOFF_IMPORT_FAIL", mode);
        }
        session.child = Some(command.spawn().unwrap());
        wait(|| session.api_path().exists());
        assert_eq!(session.request("ping", json!({}))["result"]["type"], "pong");
        session
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("XDG_CONFIG_HOME", &self.root)
            .env("XDG_STATE_HOME", &self.root)
            .env("HERDR_SESSION", &self.name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for key in [
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_CONFIG_PATH",
            "HERDR_STARTUP_CWD",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
            "HERDR_WINDOWS_CONPTY",
            "HERDR_TEST_HANDOFF_IMPORT_FAIL",
        ] {
            command.env_remove(key);
        }
        crate::platform::detach_server_daemon_command(&mut command);
        command
    }

    fn directory(&self) -> PathBuf {
        self.root.join("herdr-dev/sessions").join(&self.name)
    }
    fn api_path(&self) -> PathBuf {
        self.directory().join("herdr.sock")
    }
    fn client_path(&self) -> PathBuf {
        self.directory().join("herdr-client.sock")
    }
    fn pid(&self) -> u32 {
        fs::read_to_string(self.api_path())
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn request(&self, method: &str, params: Value) -> Value {
        let stream = ipc::connect_local_stream(&self.api_path()).unwrap();
        let mut stream =
            crate::platform::WindowsHandoffStream::new(stream, Duration::from_secs(15)).unwrap();
        writeln!(
            stream,
            "{}",
            json!({"id":"native", "method":method, "params":params})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|error| panic!("{method}: {error}: {line}"))
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        let value = self.request(method, params);
        assert!(value.get("error").is_none(), "{method}: {value}");
        value["result"].clone()
    }

    fn panes(&self) -> [String; 2] {
        let first = self.ok("workspace.create", json!({"cwd":self.root,"focus":true}))["root_pane"]
            ["pane_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let second = self.ok(
            "pane.split",
            json!({"target_pane_id":first,"direction":"right","focus":false}),
        )["pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_owned();
        [first, second]
    }

    fn input(&self, pane: &str, text: &str) {
        self.ok(
            "pane.send_input",
            json!({"pane_id":pane,"text":text,"keys":["Enter"]}),
        );
    }

    fn observe_child(&self, pane: &str, label: &str) -> String {
        let path = self.root.join(label);
        self.input(pane, &format!("[IO.File]::WriteAllText('{}', \"$PID|$([Console]::WindowWidth)|$([Console]::WindowHeight)|ü🦀\")", path.to_string_lossy().replace('\'', "''")));
        let mut value = String::new();
        wait(|| {
            value = fs::read_to_string(&path).unwrap_or_default();
            value.ends_with("ü🦀")
        });
        value
    }

    fn tui(&self, cols: u16, rows: u16) -> crate::platform::WindowsHandoffStream {
        let stream = ipc::connect_local_stream(&self.client_path()).unwrap();
        let mut stream =
            crate::platform::WindowsHandoffStream::new(stream, Duration::from_secs(10)).unwrap();
        protocol::write_message(&mut stream, &hello(cols, rows)).unwrap();
        let welcome: ServerMessage =
            protocol::read_message(&mut stream, protocol::MAX_FRAME_SIZE).unwrap();
        assert!(
            matches!(welcome, ServerMessage::Welcome { error: None, .. }),
            "{welcome:?}"
        );
        stream
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
            PROCESS_TERMINATE,
        };
        // The marker belongs to the unique session launched by this test.
        let owner = fs::read_to_string(self.api_path())
            .ok()
            .and_then(|marker| marker.split(':').next()?.parse::<u32>().ok())
            .filter(|pid| *pid != std::process::id())
            .and_then(|pid| {
                let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
                (!raw.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(raw) })
            });
        // Only this test's named session is addressed, including after replacement.
        if owner.is_some() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.request("server.stop", json!({}))
            }));
        }
        if let Some(owner) = owner {
            if unsafe { WaitForSingleObject(owner.as_raw_handle(), 5000) } == 258 {
                unsafe {
                    TerminateProcess(owner.as_raw_handle(), 1);
                    WaitForSingleObject(owner.as_raw_handle(), 5000);
                }
            }
        }
        if let Some(child) = self.child.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        if std::thread::panicking() {
            eprintln!("native handoff evidence: {}", self.root.display());
        } else {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "native handoff condition timed out"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn hello(cols: u16, rows: u16) -> ClientMessage {
    ClientMessage::TerminalHello {
        version: protocol::PROTOCOL_VERSION,
        cols,
        rows,
        cell_width_px: 8,
        cell_height_px: 16,
        pixel_mouse: false,
    }
}

#[test]
#[ignore = "requires HERDR_HANDOFF_TEST_BIN staged with the verified ConPTY bundle"]
fn windows_handoff_preserves_two_children_io_resize_and_exit() {
    for mode in ["", "lost_owned", "activation_ack", "marker_delayed"] {
        let session = Session::new(mode);
        let panes = session.panes();
        let old_tui = session.tui(120, 40);
        let before = [
            session.observe_child(&panes[0], "before0"),
            session.observe_child(&panes[1], "before1"),
        ];
        let source = session.pid();
        session.ok("server.live_handoff", json!({})); // This is the original accepted API connection's full reply.
        if mode == "marker_delayed" {
            // The source exits before publication. Both prepared files must stay linked.
            wait(|| session.pid() != source);
            assert!(session.client_path().exists());
        }
        assert_ne!(source, session.pid());
        if mode == "lost_owned" {
            assert!(
                fs::read_to_string(session.directory().join("herdr-server.log"))
                    .unwrap()
                    .contains("ownership ack was not received")
            );
        }
        if mode == "activation_ack" {
            assert!(
                fs::read_to_string(session.directory().join("herdr-server.log"))
                    .unwrap()
                    .contains("failed to acknowledge handoff activation")
            );
        }
        drop(old_tui);
        let mut tui = session.tui(160, 50);
        protocol::write_message(
            &mut tui,
            &ClientMessage::Resize {
                cols: 160,
                rows: 50,
                cell_width_px: 8,
                cell_height_px: 16,
                pixel_mouse: false,
            },
        )
        .unwrap();
        for (index, pane) in panes.iter().enumerate() {
            let after = session.observe_child(pane, &format!("after{index}"));
            assert_eq!(
                before[index].split('|').next(),
                after.split('|').next(),
                "shell process changed"
            );
            assert_ne!(before[index], after, "ConPTY size did not change");
            let pid = after.split('|').next().unwrap().parse::<u64>().unwrap();
            assert_eq!(
                session.ok("pane.process_info", json!({"pane_id":pane}))["process_info"]
                    ["shell_pid"],
                pid
            );
        }
        tui.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        protocol::write_message(&mut tui, &ClientMessage::Detach).unwrap();
        drop(tui);
        let reconnected = session.tui(140, 45);
        session.input(&panes[1], "exit");
        wait(|| {
            !session.ok("pane.list", json!({}))["panes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|pane| pane["pane_id"] == panes[1])
        });
        drop(reconnected);
        eprintln!("{mode:?}: source {source} -> target {}, two child PIDs and Unicode preserved; resize/reconnect/exit passed", session.pid());
    }
}

#[test]
#[ignore = "requires HERDR_HANDOFF_TEST_BIN staged with the verified ConPTY bundle"]
fn windows_handoff_precommit_failure_resumes_original_children_and_listeners() {
    use std::os::windows::fs::OpenOptionsExt as _;
    for mode in ["after_restored", "marker_locked"] {
        let session = Session::new(mode);
        let panes = session.panes();
        let source = session.pid();
        let before = session.observe_child(&panes[0], "before");
        let marker = fs::read(session.api_path()).unwrap();
        let marker_lock = (mode == "marker_locked").then(|| {
            fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(session.api_path())
                .unwrap()
        });
        let response = session.request("server.live_handoff", json!({}));
        assert!(response.get("error").is_some(), "{response}");
        assert_eq!(source, session.pid());
        assert_eq!(marker, fs::read(session.api_path()).unwrap());
        drop(marker_lock);
        let tui = session.tui(120, 40);
        let after = session.observe_child(&panes[0], "after");
        assert_eq!(before.split('|').next(), after.split('|').next());
        assert_eq!(
            session.ok("pane.list", json!({}))["panes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        drop(tui);
    }
}

#[test]
#[ignore = "requires HERDR_HANDOFF_TEST_BIN staged with the verified ConPTY bundle"]
fn windows_handoff_empty_session_and_system_backend_refusal() {
    let empty = Session::new("");
    let source = empty.pid();
    empty.ok("server.live_handoff", json!({}));
    assert_ne!(empty.pid(), source);
    assert!(empty.ok("pane.list", json!({}))["panes"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(empty.tui(120, 40));
    let system = Session::new("system");
    let source = system.pid();
    assert_eq!(
        system.ok("ping", json!({}))["capabilities"]["live_handoff"],
        false
    );
    assert!(system
        .request("server.live_handoff", json!({}))
        .get("error")
        .is_some());
    assert_eq!(system.pid(), source);
    drop(system.tui(120, 40));
}

#[test]
#[ignore = "requires HERDR_HANDOFF_TEST_BIN staged with the verified ConPTY bundle"]
fn windows_handoff_both_pending_listeners_are_inert_until_commit_and_resume_on_rollback() {
    use crate::platform::{spawn_transferable_listener, TransferableLocalListener};
    use crate::server::handoff;
    for mode in ["rollback", "commit", "marker_publish"] {
        let commit = mode != "rollback";
        let mut session = Session::new("");
        session.ok("server.stop", json!({}));
        session.child.take().unwrap().wait().unwrap();
        let paths = [session.api_path(), session.client_path()];
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let listeners = paths.each_ref().map(|path| {
            TransferableLocalListener::bound(ipc::bind_local_listener(path).unwrap()).unwrap()
        });
        let original_markers = paths.each_ref().map(|path| fs::read(path).unwrap());
        let mut threads = Vec::new();
        let controls = listeners
            .into_iter()
            .enumerate()
            .map(|(index, listener)| {
                let tx = accepted_tx.clone();
                let (thread, control) = spawn_transferable_listener(
                    listener,
                    "handoff test source",
                    || false,
                    move |stream| {
                        tx.send((index, stream)).unwrap();
                    },
                );
                threads.push(thread);
                control
            })
            .collect::<Vec<_>>();
        let old_client = ipc::connect_local_stream(&paths[0]).unwrap();
        let (_, mut old_response) = accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for control in &controls {
            control.pause().unwrap();
        }
        let private_path = session.directory().join("test-handoff.sock");
        let private_listener = handoff::bind_listener(&private_path).unwrap();
        let target = session
            .command()
            .args(["server", "--handoff-import"])
            .arg(&private_path)
            .arg("owned-test")
            .env("HERDR_TEST_HANDOFF_IMPORT_FAIL", mode)
            .spawn()
            .unwrap();
        session.child = Some(target);
        let target = session.child.as_ref().unwrap();
        let resources = [
            controls[0].duplicate_for_handoff(target).unwrap(),
            controls[1].duplicate_for_handoff(target).unwrap(),
        ];
        let snapshot = crate::persist::capture(
            &[],
            &std::collections::HashMap::new(),
            &crate::terminal::TerminalRuntimeRegistry::new(),
            None,
            0,
        );
        let manifest = handoff::manifest_for(snapshot, Vec::new(), None, None, None);
        let mut transaction = handoff::accept_windows_handoff(
            private_listener,
            target,
            "owned-test",
            &manifest,
            Vec::new(),
            resources,
        )
        .unwrap();
        assert_eq!(
            original_markers,
            paths.each_ref().map(|path| fs::read(path).unwrap())
        );
        let mut api = ipc::connect_local_stream(&paths[0]).unwrap();
        let mut tui = ipc::connect_local_stream(&paths[1]).unwrap();
        writeln!(
            api,
            "{}",
            json!({"id":"pending","method":"ping","params":{}})
        )
        .unwrap();
        protocol::write_message(&mut tui, &hello(120, 40)).unwrap();
        for _ in 0..10 {
            for stream in [&mut api, &mut tui] {
                assert!(
                    matches!(
                        ipc::poll_local_stream_read_count(stream, &mut [0; 1]).unwrap(),
                        ipc::LocalStreamReadCount::Pending
                    ),
                    "target answered before COMMIT"
                );
            }
            assert!(
                accepted_rx.try_recv().is_err(),
                "source accepted after idle ACK"
            );
            thread::sleep(Duration::from_millis(10));
        }
        if commit {
            handoff::report_committed(&mut transaction).unwrap();
            for control in &controls {
                control.release_after_commit().unwrap();
            }
            handoff::wait_owned_ack(&mut transaction);
            if mode == "marker_publish" {
                assert!(
                    fs::read_to_string(session.directory().join("herdr-server.log"))
                        .unwrap()
                        .contains("failed to publish handoff socket marker")
                );
                assert_eq!(
                    original_markers,
                    paths.each_ref().map(|path| fs::read(path).unwrap())
                );
                assert!(session
                    .child
                    .as_mut()
                    .unwrap()
                    .try_wait()
                    .unwrap()
                    .is_none());
            }
            let api =
                crate::platform::WindowsHandoffStream::new(api, Duration::from_secs(5)).unwrap();
            let mut line = String::new();
            BufReader::new(api).read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&line).unwrap()["result"]["type"],
                "pong"
            );
            let mut tui =
                crate::platform::WindowsHandoffStream::new(tui, Duration::from_secs(5)).unwrap();
            let welcome: ServerMessage =
                protocol::read_message(&mut tui, protocol::MAX_FRAME_SIZE).unwrap();
            assert!(matches!(
                welcome,
                ServerMessage::Welcome { error: None, .. }
            ));
        } else {
            handoff::cleanup_failed_import_child(session.child.as_mut().unwrap()).unwrap();
            for control in &controls {
                control.resume().unwrap();
            }
            let mut replies = Vec::new();
            for _ in 0..2 {
                let (index, mut stream) = accepted_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                if index == 0 {
                    let mut request = String::new();
                    BufReader::new(&mut stream).read_line(&mut request).unwrap();
                } else {
                    let _: ClientMessage =
                        protocol::read_message(&mut stream, protocol::MAX_FRAME_SIZE).unwrap();
                }
                stream.write_all(b"resumed\n").unwrap();
                replies.push(stream);
            }
            for stream in [api, tui] {
                let stream =
                    crate::platform::WindowsHandoffStream::new(stream, Duration::from_secs(5))
                        .unwrap();
                let mut line = String::new();
                BufReader::new(stream).read_line(&mut line).unwrap();
                assert_eq!(line, "resumed\n");
            }
            for control in &controls {
                control.release_after_commit().unwrap();
            }
            for path in &paths {
                fs::remove_file(path).unwrap();
            }
        }
        // An accepted source connection remains writable after either outcome.
        old_response.write_all(b"original-response\n").unwrap();
        let old_client =
            crate::platform::WindowsHandoffStream::new(old_client, Duration::from_secs(5)).unwrap();
        let mut line = String::new();
        BufReader::new(old_client).read_line(&mut line).unwrap();
        assert_eq!(line, "original-response\n");
        if mode == "marker_publish" {
            session.ok("server.stop", json!({}));
        }
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
