use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use interprocess::local_socket::traits::{ListenerExt as _, Stream as _};
use tracing::{debug, error, info, warn};

use crate::api::schema::{Method, PaneTarget, Request, ResponseResult, SuccessResponse};
use crate::api::ApiRequestSender;
use crate::ipc::LocalStream;

use super::{
    dispatch_to_app_with_timeout, error_response_json, write_text_line_allow_disconnect,
    APP_RESPONSE_TIMEOUT, STREAM_WRITE_TIMEOUT,
};

pub(crate) struct RemoteReportHandle {
    _thread: std::thread::JoinHandle<()>,
    path: PathBuf,
    running: Arc<AtomicBool>,
}

impl Drop for RemoteReportHandle {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Err(err) = std::fs::remove_file(&self.path) {
            if err.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %self.path.display(),
                    err = %err,
                    "failed to remove remote report socket on shutdown"
                );
            }
        }
    }
}

pub(crate) fn start_remote_report_listener(
    api_tx: ApiRequestSender,
) -> io::Result<RemoteReportHandle> {
    let path = crate::session::remote_report_socket_path();
    super::prepare_socket_path(&path)?;

    let listener = crate::ipc::bind_local_listener(&path)?;
    super::restrict_socket_permissions(&path)?;
    info!(path = %path.display(), "remote report listener listening");

    let running = Arc::new(AtomicBool::new(true));
    let listener_running = Arc::clone(&running);
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let api_tx = api_tx.clone();
                    let connection_running = Arc::clone(&listener_running);
                    std::thread::spawn(move || {
                        if let Err(err) = handle_connection(stream, &api_tx, &connection_running) {
                            warn!(err = %err, "remote report connection failed");
                        }
                    });
                }
                Err(err) => {
                    if listener_running.load(Ordering::Relaxed) {
                        error!(err = %err, "remote report listener accept failed");
                    }
                    break;
                }
            }
        }
        debug!("remote report listener thread exiting");
    });

    Ok(RemoteReportHandle {
        _thread: thread,
        path,
        running,
    })
}

fn handle_connection(
    mut stream: LocalStream,
    api_tx: &ApiRequestSender,
    running: &Arc<AtomicBool>,
) -> io::Result<()> {
    if let Err(err) = stream.set_send_timeout(Some(STREAM_WRITE_TIMEOUT)) {
        debug!(err = %err, "remote report connection write timeout unavailable");
    }

    loop {
        if !running.load(Ordering::Relaxed) {
            return Ok(());
        }

        let Some(line) = super::read_initial_request_line(&mut stream)? else {
            return Ok(());
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let request = match serde_json::from_str::<Request>(line) {
            Ok(request) => request,
            Err(err) => {
                write_text_line_allow_disconnect(
                    &mut stream,
                    &error_response_json(
                        String::new(),
                        "invalid_request",
                        format!("invalid request: {err}"),
                    ),
                )?;
                continue;
            }
        };

        let response = dispatch_remote_request(request, api_tx);
        write_text_line_allow_disconnect(&mut stream, &response)?;
    }
}

fn dispatch_remote_request(request: Request, api_tx: &ApiRequestSender) -> String {
    let request_id = request.id.clone();

    if matches!(&request.method, Method::Ping(_)) {
        return super::handle_request(request, api_tx, None, None);
    }

    let Some(pane_id) = report_pane_id(&request.method) else {
        return error_response_json(
            request_id,
            "method_not_allowed",
            "method is not allowed over the remote report socket".into(),
        );
    };

    let validation_response = dispatch_to_app_with_timeout(
        Request {
            id: format!("remote-report:validate:{request_id}"),
            method: Method::PaneGet(PaneTarget {
                pane_id: pane_id.to_owned(),
            }),
        },
        api_tx,
        Some(APP_RESPONSE_TIMEOUT),
    );
    if !pane_response_is_remote(&validation_response) {
        return error_response_json(
            request_id,
            "pane_not_remote",
            "pane is not a remote (ssh) session".into(),
        );
    }

    dispatch_to_app_with_timeout(request, api_tx, Some(APP_RESPONSE_TIMEOUT))
}

fn report_pane_id(method: &Method) -> Option<&str> {
    match method {
        Method::PaneReportAgent(params) => Some(&params.pane_id),
        Method::PaneReportAgentSession(params) => Some(&params.pane_id),
        Method::PaneReportMetadata(params) => Some(&params.pane_id),
        Method::PaneReleaseAgent(params) => Some(&params.pane_id),
        Method::PaneClearAgentAuthority(params) => Some(&params.pane_id),
        _ => None,
    }
}

fn pane_response_is_remote(response: &str) -> bool {
    let Ok(response) = serde_json::from_str::<SuccessResponse>(response) else {
        return false;
    };
    matches!(
        response.result,
        ResponseResult::PaneInfo { pane } if pane.remote_target.is_some()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::api::schema::{AgentStatus, ErrorResponse, PaneInfo};
    use crate::api::ApiRequestMessage;
    use crate::ipc::LocalStream;
    use interprocess::local_socket::traits::Listener as _;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::thread::JoinHandle;
    use tokio::sync::mpsc as tokio_mpsc;

    static NEXT_LOCAL_STREAM_ID: AtomicU64 = AtomicU64::new(1);

    fn local_stream_pair(name: &str) -> (LocalStream, LocalStream, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "herdr-remote-report-{name}-{}-{}.sock",
            std::process::id(),
            NEXT_LOCAL_STREAM_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        (client, server, path)
    }

    fn pane_info(remote_target: Option<String>) -> PaneInfo {
        PaneInfo {
            pane_id: "pane_1".into(),
            terminal_id: "terminal_1".into(),
            workspace_id: "workspace_1".into(),
            tab_id: "tab_1".into(),
            focused: true,
            cwd: None,
            foreground_cwd: None,
            label: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            display_agent: None,
            agent_status: AgentStatus::Idle,
            state_labels: HashMap::new(),
            tokens: HashMap::new(),
            agent_session: None,
            scroll: None,
            revision: 0,
            remote_target,
        }
    }

    fn send_success(message: &ApiRequestMessage, result: ResponseResult) {
        message
            .respond_to
            .send(
                serde_json::to_string(&SuccessResponse {
                    id: message.request.id.clone(),
                    result,
                })
                .unwrap(),
            )
            .unwrap();
    }

    fn spawn_api_responder(
        remote_target: Option<String>,
        seen_tx: Sender<Request>,
    ) -> (ApiRequestSender, JoinHandle<()>) {
        let (api_tx, mut api_rx) = tokio_mpsc::unbounded_channel::<ApiRequestMessage>();
        let responder = std::thread::spawn(move || {
            while let Some(message) = api_rx.blocking_recv() {
                match &message.request.method {
                    Method::PaneGet(_) => send_success(
                        &message,
                        ResponseResult::PaneInfo {
                            pane: pane_info(remote_target.clone()),
                        },
                    ),
                    Method::PaneReportAgent(_)
                    | Method::PaneReportAgentSession(_)
                    | Method::PaneReportMetadata(_)
                    | Method::PaneReleaseAgent(_)
                    | Method::PaneClearAgentAuthority(_) => {
                        seen_tx.send(message.request.clone()).unwrap();
                        send_success(&message, ResponseResult::Ok {});
                    }
                    other => panic!("unexpected request: {other:?}"),
                }
            }
        });
        (api_tx, responder)
    }

    fn run_request(raw_request: &str, remote_target: Option<&str>) -> (String, Vec<Request>) {
        let (seen_tx, seen_rx) = mpsc::channel();
        let (api_tx, responder) = spawn_api_responder(remote_target.map(str::to_owned), seen_tx);
        let (mut client, server, path) = local_stream_pair("request");
        let running = Arc::new(AtomicBool::new(true));
        let server_running = Arc::clone(&running);
        let server_thread =
            std::thread::spawn(move || handle_connection(server, &api_tx, &server_running));

        client.write_all(raw_request.as_bytes()).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        let mut reader = BufReader::new(&mut client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        drop(reader);
        drop(client);
        running.store(false, Ordering::Relaxed);
        assert!(server_thread.join().unwrap().is_ok());
        assert!(responder.join().is_ok());
        let _ = std::fs::remove_file(path);
        (response, seen_rx.try_iter().collect())
    }

    #[cfg(unix)]
    #[test]
    fn remote_report_forwards_allowed_request_verbatim_for_remote_pane() {
        let raw_request = r#"{"id":"report_1","method":"pane.report_agent","params":{"pane_id":"pane_1","source":"herdr:omp","agent":"omp","state":"working"}}"#;
        let (response, seen) = run_request(raw_request, Some("localhost"));
        let response: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.result, ResponseResult::Ok {});
        assert_eq!(seen, vec![serde_json::from_str(raw_request).unwrap()]);
    }

    #[cfg(unix)]
    #[test]
    fn remote_report_rejects_non_remote_pane() {
        let raw_request = r#"{"id":"report_2","method":"pane.report_agent","params":{"pane_id":"pane_1","source":"herdr:omp","agent":"omp","state":"working"}}"#;
        let (response, seen) = run_request(raw_request, None);
        let response: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.error.code, "pane_not_remote");
        assert!(seen.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn remote_report_rejects_pane_send_text_without_dispatching() {
        let raw_request = r#"{"id":"send_1","method":"pane.send_text","params":{"pane_id":"pane_1","text":"uname\n"}}"#;
        let (response, seen) = run_request(raw_request, Some("localhost"));
        let response: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(response.error.code, "method_not_allowed");
        assert!(seen.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn remote_report_forwards_ping() {
        let raw_request = r#"{"id":"ping_1","method":"ping","params":{}}"#;
        let (response, seen) = run_request(raw_request, None);
        let response: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(response.result, ResponseResult::Pong { .. }));
        assert!(seen.is_empty());
    }
}
