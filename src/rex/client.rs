//! Rex server client: control connection over WebSocket.
//!
//! Protocol source: `GET /llms.txt` served by the Rex server itself (API revision 19).
//! The server documents this wire format for third-party clients.

use std::collections::HashMap;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

pub(crate) const CONTROL_SUBPROTOCOL: &str = "rex.control.v1";
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Revision herdr's client code is written against.
pub(crate) const TARGET_API_REVISION: u64 = 19;
/// Oldest revision with the detached-block, own-block, and data-ticket shapes we use.
pub(crate) const MIN_API_REVISION: u64 = 19;

#[derive(Debug)]
pub(crate) enum RexEvent {
    SessionEvent { session_id: String, event: Value },
    ServerEvent(Value),
}

pub(crate) struct RexControl {
    requests: mpsc::Sender<PendingRequest>,
    events: mpsc::Receiver<RexEvent>,
    pub(crate) client_id: String,
    pub(crate) reconnect_token: Vec<u8>,
}

struct PendingRequest {
    method: &'static str,
    payload: Value,
    reply: oneshot::Sender<Result<Value, RexError>>,
}

#[derive(Debug)]
pub(crate) struct RexError {
    pub(crate) code: String,
    pub(crate) status: u16,
    pub(crate) message: String,
}

impl std::fmt::Display for RexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rex api {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RexError {}

impl RexError {
    fn internal(message: impl Into<String>) -> Self {
        Self { code: "internal".into(), status: 500, message: message.into() }
    }

    fn from_response(value: &Value) -> Self {
        let err = &value["error"];
        Self {
            code: err["code"].as_str().unwrap_or("internal").to_string(),
            status: err["status"].as_u64().unwrap_or(500) as u16,
            message: err["message"].as_str().unwrap_or("unknown error").to_string(),
        }
    }
}

pub(crate) fn unix_request(
    path: &str,
    subprotocol: &str,
) -> std::io::Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut req = format!("ws://localhost{path}")
        .into_client_request()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    req.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        subprotocol.parse().map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid subprotocol header")
        })?,
    );
    Ok(req)
}

pub(crate) async fn connect_unix_ws(
    socket_path: &str,
    path: &str,
    subprotocol: &str,
) -> std::io::Result<WebSocketStream<UnixStream>> {
    let req = unix_request(path, subprotocol)?;
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| std::io::Error::other(format!("connect {socket_path}: {e}")))?;
    let (ws, _resp) = tokio_tungstenite::client_async(req, stream)
        .await
        .map_err(|e| std::io::Error::other(format!("websocket handshake {path}: {e}")))?;
    Ok(ws)
}

impl RexControl {
    pub(crate) async fn connect(socket_path: &str) -> std::io::Result<Self> {
        let mut ws = connect_unix_ws(socket_path, "/api/control", CONTROL_SUBPROTOCOL).await?;

        let welcome: Value = match tokio::time::timeout(HELLO_TIMEOUT, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str(&t)
                .map_err(|e| std::io::Error::other(format!("welcome parse: {e}")))?,
            Ok(Some(Ok(other))) => {
                return Err(std::io::Error::other(format!(
                    "expected welcome text frame, got {other:?}"
                )))
            }
            Ok(Some(Err(e))) => return Err(std::io::Error::other(format!("welcome read: {e}"))),
            Ok(None) => return Err(std::io::Error::other("control closed before welcome")),
            Err(_) => return Err(std::io::Error::other("timed out waiting for welcome")),
        };
        if welcome["type"] != "welcome" || welcome["protocol"] != CONTROL_SUBPROTOCOL {
            return Err(std::io::Error::other(format!(
                "unexpected welcome: type={} protocol={}",
                welcome["type"], welcome["protocol"]
            )));
        }
        let api_revision = welcome["api_revision"].as_u64().unwrap_or(0);
        if api_revision < MIN_API_REVISION {
            return Err(std::io::Error::other(format!(
                "server control API revision {api_revision} is older than the minimum {MIN_API_REVISION} herdr supports"
            )));
        }
        let client_id = welcome["client"]["id"].as_str().unwrap_or_default().to_string();
        let reconnect_token = welcome["client"]["reconnect_token"]
            .as_str()
            .and_then(|t| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(t).ok())
            .unwrap_or_default();

        let hello = json!({
            "type": "hello",
            "api_revision": TARGET_API_REVISION,
            "api_min_revision": MIN_API_REVISION,
            "client": {"kind": "cli", "name": "herdr", "version": crate::build_info::version()}
        });
        ws.send(Message::Text(hello.to_string()))
            .await
            .map_err(|e| std::io::Error::other(format!("hello send: {e}")))?;

        let (req_tx, mut req_rx) = mpsc::channel::<PendingRequest>(32);
        let (event_tx, event_rx) = mpsc::channel(128);
        tokio::spawn(async move {
            let mut next_id: u64 = 1;
            let mut pending: HashMap<String, oneshot::Sender<Result<Value, RexError>>> =
                HashMap::new();
            loop {
                tokio::select! {
                    req = req_rx.recv() => {
                        let Some(req) = req else { break };
                        let id = next_id.to_string();
                        next_id += 1;
                        let msg = json!({
                            "type": "request",
                            "id": id,
                            "method": req.method,
                            "payload": req.payload
                        });
                        if let Err(e) = ws.send(Message::Text(msg.to_string())).await {
                            let _ = req.reply.send(Err(RexError::internal(format!("send failed: {e}"))));
                            continue;
                        }
                        pending.insert(id, req.reply);
                    }
                    frame = ws.next() => {
                        let frame = match frame {
                            Some(Ok(Message::Text(t))) => t,
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(_)) => continue,
                            Some(Err(e)) => {
                                tracing::debug!(error = %e, "rex control read error");
                                break;
                            }
                        };
                        let Ok(v) = serde_json::from_str::<Value>(&frame) else { continue };
                        match v["type"].as_str().unwrap_or_default() {
                            "response" => {
                                if let Some(reply) = v["id"].as_str().and_then(|id| pending.remove(id)) {
                                    let result = if v.get("error").is_some_and(|e| !e.is_null()) {
                                        Err(RexError::from_response(&v))
                                    } else {
                                        Ok(v["payload"].clone())
                                    };
                                    let _ = reply.send(result);
                                }
                            }
                            "event" => {
                                let payload = v["payload"].clone();
                                let routed = match payload["type"].as_str().unwrap_or_default() {
                                    "session_event" => {
                                        let session_id =
                                            payload["payload"]["session_id"].as_str().unwrap_or_default().to_string();
                                        let event = payload["payload"]["event"].clone();
                                        RexEvent::SessionEvent { session_id, event }
                                    }
                                    _ => RexEvent::ServerEvent(payload),
                                };
                                if event_tx.try_send(routed).is_err() {
                                    tracing::debug!("rex event channel full, dropping event");
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            for (_, reply) in pending.drain() {
                let _ = reply.send(Err(RexError::internal("control connection closed")));
            }
        });

        Ok(Self { requests: req_tx, events: event_rx, client_id, reconnect_token })
    }

    pub(crate) async fn call(
        &self,
        method: &'static str,
        payload: Value,
    ) -> Result<Value, RexError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.requests
            .send(PendingRequest { method, payload, reply: reply_tx })
            .await
            .map_err(|_| RexError::internal("control task gone"))?;
        reply_rx.await.map_err(|_| RexError::internal("control reply dropped"))?
    }

    pub(crate) fn events(&mut self) -> &mut mpsc::Receiver<RexEvent> {
        &mut self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_fields_parse() {
        let welcome = json!({
            "type": "welcome",
            "protocol": "rex.control.v1",
            "api_revision": 19,
            "client": {"id": "client:x", "reconnect_token": "AAAAAAAAAAAAAAAAAAAAAA"}
        });
        assert_eq!(welcome["api_revision"].as_u64(), Some(19));
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(welcome["client"]["reconnect_token"].as_str().unwrap())
            .unwrap();
        assert_eq!(token.len(), 16);
    }
}
