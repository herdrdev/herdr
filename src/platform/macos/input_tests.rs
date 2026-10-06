use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Duration;

use bytes::Bytes;

use crate::api::schema::{AgentStartParams, Method, PaneSendInputParams, Request};
use crate::app::{App, AppPolicy, AppState};
use crate::pty::actor::{PtyIoActor, PtyIoActorConfig, PtyReadResult};
use crate::terminal::{TerminalId, TerminalRuntime};

fn pty_pair() -> (OwnedFd, std::fs::File) {
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    (
        unsafe { OwnedFd::from_raw_fd(master) },
        std::fs::File::from(unsafe { OwnedFd::from_raw_fd(slave) }),
    )
}

fn set_canonical(slave: &std::fs::File, canonical: bool) {
    let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) },
        0
    );
    let mut termios = unsafe { termios.assume_init() };
    termios.c_lflag &= !(libc::ICANON | libc::ECHO);
    if canonical {
        termios.c_lflag |= libc::ICANON;
    }
    termios.c_cc[libc::VMIN] = 0;
    termios.c_cc[libc::VTIME] = 20;
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios) },
        0
    );
}

fn read_line(slave: &mut std::fs::File) -> Vec<u8> {
    let mut poll = libc::pollfd {
        fd: slave.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
    let mut received = vec![0; 2048];
    let count = slave.read(&mut received).unwrap();
    received.truncate(count);
    received
}

fn app_with_actor(handle: crate::pty::actor::PtyIoActorHandle) -> (App, String, TerminalId) {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state = AppState::test_with_adversarial_identity_state();
    let pane = app.state.workspaces[0].tabs[0].root_pane;
    let terminal_id = app.state.workspaces[0].terminal_id(pane).unwrap().clone();
    app.terminal_runtimes.insert(
        terminal_id.clone(),
        TerminalRuntime::test_with_actor(handle),
    );
    let pane_id = app.public_pane_id(0, pane).unwrap();
    app.state.assert_invariants_for_test();
    (app, pane_id, terminal_id)
}

fn start_request(pane_id: &str, argument: &str) -> Request {
    Request {
        id: "start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: "worker".into(),
            kind: "codex".into(),
            pane_id: pane_id.into(),
            args: vec!["resume".into(), argument.into()],
            timeout_ms: Some(10_000),
        }),
    }
}

fn pane_request(pane_id: &str, text: &str) -> Request {
    Request {
        id: "input".into(),
        method: Method::PaneSendInput(PaneSendInputParams {
            pane_id: pane_id.into(),
            text: text.into(),
            keys: vec!["enter".into()],
        }),
    }
}

#[tokio::test]
async fn canonical_api_rejection_writes_nothing_and_allows_same_name_retry() {
    for agent_start in [false, true] {
        let (master, mut slave) = pty_pair();
        set_canonical(&slave, true);
        let limit = unsafe { libc::fpathconf(slave.as_raw_fd(), libc::_PC_MAX_CANON) } as usize;
        assert_eq!(limit, 1024);
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            master_fd: master,
            initially_quiesced: false,
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
        })
        .unwrap();
        let (mut app, pane_id, terminal_id) = app_with_actor(handle);
        // Check encoded bytes, including Enter, and reject the entire input even
        // when a preceding line would fit. Agent arguments cannot contain LF.
        let rejected = if agent_start {
            vec!["é".repeat(limit / 2)]
        } else {
            vec![
                "é".repeat(limit / 2),
                "x".repeat(limit),
                format!("first line\n{}", "x".repeat(limit)),
            ]
        };
        for text in rejected {
            let request = if agent_start {
                start_request(&pane_id, &text)
            } else {
                pane_request(&pane_id, &text)
            };
            let response: serde_json::Value =
                serde_json::from_str(&app.handle_api_request(request)).unwrap();
            assert_eq!(
                response["error"]["code"],
                if agent_start {
                    "agent_start_input_failed"
                } else {
                    "pane_send_failed"
                }
            );
            assert!(response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("canonical mode"));
            let terminal = &app.state.terminals[&terminal_id];
            assert!(terminal.agent_name.is_none());
            assert!(terminal.managed_agent_kind().is_none());
            assert!(terminal.persisted_agent_session.is_none());
            app.state.assert_invariants_for_test();
        }
        let request = if agent_start {
            start_request(&pane_id, "retry")
        } else {
            pane_request(&pane_id, "retry")
        };
        let response: serde_json::Value =
            serde_json::from_str(&app.handle_api_request(request)).unwrap();
        assert!(response.get("error").is_none(), "{response}");
        let received = String::from_utf8(read_line(&mut slave)).unwrap();
        assert!(received.contains("retry"));
        assert!(!received.contains("first line") && !received.contains('é'));
        if agent_start {
            assert_eq!(
                app.state.terminals[&terminal_id].agent_name.as_deref(),
                Some("worker")
            );
        } else {
            assert_eq!(received, "retry\n");
            let text = "x".repeat(limit - 1);
            let response: serde_json::Value =
                serde_json::from_str(&app.handle_api_request(pane_request(&pane_id, &text)))
                    .unwrap();
            assert!(response.get("error").is_none(), "{response}");
            assert_eq!(read_line(&mut slave), format!("{text}\n").as_bytes());
        }
        app.state.assert_invariants_for_test();
    }
}

#[tokio::test]
async fn input_admission_does_not_wait_for_actor_and_raw_input_stays_intact() {
    let (master, mut slave) = pty_pair();
    set_canonical(&slave, true);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let mut barrier = Some((entered_tx, resume_rx));
    let handle = PtyIoActor::spawn(PtyIoActorConfig {
        pane_id: 1,
        master_fd: master,
        initially_quiesced: false,
        on_read: Box::new(move |_| {
            if let Some((entered, resume)) = barrier.take() {
                let _ = entered.send(());
                let _ = resume.recv();
            }
            PtyReadResult::empty()
        }),
        on_reader_exit: None,
    })
    .unwrap();
    let (mut app, pane_id, _) = app_with_actor(handle);
    slave.write_all(b"barrier").unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let text = "x".repeat(3017);
    let response: serde_json::Value =
        serde_json::from_str(&app.handle_api_request(pane_request(&pane_id, &text))).unwrap();
    assert_eq!(response["error"]["code"], "pane_send_failed");
    set_canonical(&slave, false);
    let response: serde_json::Value =
        serde_json::from_str(&app.handle_api_request(pane_request(&pane_id, &text))).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    // Both rejection and successful admission return while the actor is held.
    resume_tx.send(()).unwrap();
    let mut received = vec![0; text.len() + 1];
    slave.read_exact(&mut received).unwrap();
    assert_eq!(received, format!("{text}\n").as_bytes());
}

#[test]
fn input_guard_does_not_keep_the_master_open() {
    let (master, slave) = pty_pair();
    set_canonical(&slave, true);
    let owner = std::sync::Arc::new(std::fs::File::from(master));
    let guard = super::PtyInputGuard::new(&owner);
    assert_eq!(std::sync::Arc::strong_count(&owner), 1);
    guard.validate(b"valid\n").unwrap();
    drop(owner);
    assert_eq!(
        guard.validate(b"valid\n").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        guard.clone().validate(b"valid\n").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[test]
fn canonical_multiline_submission_preserves_each_line() {
    for delimiter in [b'\n', b'\r', b'|', 4] {
        let (master, mut slave) = pty_pair();
        set_canonical(&slave, true);
        let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), settings.as_mut_ptr()) },
            0
        );
        let mut settings = unsafe { settings.assume_init() };
        settings.c_cc[libc::VEOL] = b'|';
        settings.c_cc[libc::VEOF] = 4;
        settings.c_iflag |= libc::ICRNL;
        settings.c_iflag &= !(libc::IGNCR | libc::INLCR);
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
            0
        );
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            master_fd: master,
            initially_quiesced: false,
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
        })
        .unwrap();
        let line = [vec![b'x'; 600], vec![delimiter]].concat();
        let bytes = line.repeat(2);
        let expected = bytes
            .iter()
            .copied()
            .filter(|byte| *byte != 4)
            .map(|byte| if byte == b'\r' { b'\n' } else { byte })
            .collect::<Vec<_>>();
        let reader = std::thread::spawn(move || {
            let mut received = Vec::new();
            while received.len() < expected.len() {
                received.extend(read_line(&mut slave));
            }
            assert_eq!(received, expected);
        });
        handle.validate_input_submission(&bytes).unwrap();
        handle.try_write_user_input(Bytes::from(bytes)).unwrap();
        reader.join().unwrap();
        handle.shutdown();
    }
}

#[test]
fn canonical_preflight_respects_quoted_and_translated_delimiters() {
    let (master, slave) = pty_pair();
    set_canonical(&slave, true);
    let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), settings.as_mut_ptr()) },
        0
    );
    let original = unsafe { settings.assume_init() };
    let prefix = vec![b'x'; 600];
    let suffix = vec![b'y'; 600];
    for (middle, setting) in [
        (vec![b'\n'], 0),
        (vec![b'\r'], 1),
        (vec![22, b'\n'], 2),
        (vec![b'\n'], 3),
        (vec![b'\n'], 4),
        (vec![150, b'\n'], 5),
        (vec![b'|'], 6),
    ] {
        let mut settings = original;
        settings.c_lflag |= libc::IEXTEN | libc::ISIG;
        settings.c_cc[libc::VLNEXT] = 22;
        match setting {
            0 => settings.c_iflag |= libc::INLCR,
            1 => settings.c_iflag |= libc::IGNCR,
            2 => {}
            3 => settings.c_cc[libc::VINTR] = b'\n',
            4 => settings.c_cc[libc::VERASE] = b'\n',
            5 => settings.c_iflag |= libc::ISTRIP,
            6 => {
                settings.c_cc[libc::VEOL] = libc::_POSIX_VDISABLE;
                settings.c_cc[libc::VEOL2] = b'|';
                settings.c_lflag &= !libc::IEXTEN;
            }
            _ => unreachable!(),
        }
        assert_eq!(
            unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
            0
        );
        let input = [prefix.clone(), middle, suffix.clone()].concat();
        let result = super::input::validate_pty_submission(master.as_raw_fd(), &input, b"\n");
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    }

    // A quoted newline in the Enter part must not reset a line begun in text.
    let mut settings = original;
    settings.c_lflag |= libc::IEXTEN;
    settings.c_cc[libc::VLNEXT] = 22;
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
        0
    );
    let text = [prefix.clone(), vec![22]].concat();
    let enter = [vec![b'\n'], suffix.clone(), vec![b'\n']].concat();
    assert!(super::input::validate_pty_submission(master.as_raw_fd(), &text, &enter).is_err());
    // EXTPROC bypasses quoting and translations, but still recognizes line breaks.
    let external_processing: libc::c_int = 1;
    assert_eq!(
        unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCEXT, &external_processing) },
        0
    );
    settings.c_iflag |= libc::INLCR;
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
        0
    );
    assert!(super::input::validate_pty_submission(master.as_raw_fd(), &text, &enter).is_ok());
    let external_processing: libc::c_int = 0;
    assert_eq!(
        unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCEXT, &external_processing) },
        0
    );

    let mut settings = original;
    settings.c_iflag |= libc::PARMRK;
    settings.c_iflag &= !(libc::ISTRIP | libc::IGNPAR);
    assert_eq!(
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &settings) },
        0
    );
    assert!(super::input::validate_pty_submission(master.as_raw_fd(), &[0xff; 511], b"\n").is_ok());
    assert!(
        super::input::validate_pty_submission(master.as_raw_fd(), &[0xff; 512], b"\n").is_err()
    );
}
