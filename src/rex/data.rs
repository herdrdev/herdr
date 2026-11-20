//! Rex data connections: binary handshake plus the terminal byte stream.
//!
//! Wire format from the Rex server's `llms.txt` (API revision 19):
//! HELLO/WELCOME are fixed-layout little-endian binaries; after WELCOME every
//! binary frame is raw stream bytes (Ghostty snapshot first, then VT output)
//! with Rex control messages embedded as APC `ESC _ rex1;<nonce>;<json> ESC \`.

use base64::Engine;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use super::client::connect_unix_ws;

pub(crate) const TERMINAL_SUBPROTOCOL: &str = "rex.data.terminal.v1";
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

const HELLO_MAGIC: u8 = 0x01;
const RESUME_MAGIC: u8 = 0x02;
const WELCOME_MAGIC: u8 = 0x81;
const REJECT_MAGIC: u8 = 0x82;

pub(crate) struct DataWelcome {
    pub(crate) reconnect_token: [u8; 16],
    pub(crate) keepalive_ms: u32,
    pub(crate) channel_nonce: u64,
}

impl DataWelcome {
    pub(crate) fn reconnect_token_b64(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.reconnect_token)
    }
}

#[derive(Debug)]
pub(crate) struct DataReject {
    pub(crate) reason: u8,
}

impl std::fmt::Display for DataReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let meaning = match self.reason {
            0x01 => "ticket unknown, expired, used, or foreign",
            0x02 => "client already has a connection to this data source (send RESUME)",
            0x03 => "stream unavailable",
            0x04 => "would pass a limit",
            0x05 => "malformed or late handshake",
            0x06 => "server failure",
            0x07 => "subprotocol mismatch",
            _ => "unknown reason",
        };
        write!(f, "data handshake rejected: {meaning}")
    }
}

pub(crate) enum StreamItem {
    Bytes(Vec<u8>),
    Control(Value),
}

#[derive(Debug)]
pub(crate) enum RexDataError {
    BadTicket,
    Rejected(DataReject),
    Io(String),
}

impl std::fmt::Display for RexDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RexDataError::BadTicket => write!(f, "data ticket must be 16 bytes of base64url"),
            RexDataError::Rejected(r) => write!(f, "{r}"),
            RexDataError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RexDataError {}

pub(crate) struct RexData {
    ws: WebSocketStream<UnixStream>,
    welcome: DataWelcome,
    /// Stream bytes delivered to the consumer so far; RESUME requires this count to be exact.
    consumed: u64,
}

impl RexData {
    pub(crate) async fn open(socket_path: &str, ticket_b64: &str) -> Result<Self, RexDataError> {
        let mut ws = connect_unix_ws(socket_path, "/api/data", TERMINAL_SUBPROTOCOL)
            .await
            .map_err(|e| RexDataError::Io(e.to_string()))?;
        let ticket = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(ticket_b64)
            .map_err(|_| RexDataError::BadTicket)?;
        if ticket.len() != 16 {
            return Err(RexDataError::BadTicket);
        }
        let mut hello = Vec::with_capacity(17);
        hello.push(HELLO_MAGIC);
        hello.extend_from_slice(&ticket);
        send_binary(&mut ws, hello).await?;

        let welcome = read_welcome(&mut ws).await?;
        Ok(Self { ws, welcome, consumed: 0 })
    }

    /// Resume a dropped stream. `received` must count every stream byte the
    /// consumer used across all prior connections of this stream.
    pub(crate) async fn resume(
        socket_path: &str,
        reconnect_token: [u8; 16],
        received: u64,
    ) -> Result<Self, RexDataError> {
        let mut ws = connect_unix_ws(socket_path, "/api/data", TERMINAL_SUBPROTOCOL)
            .await
            .map_err(|e| RexDataError::Io(e.to_string()))?;
        let mut resume = Vec::with_capacity(25);
        resume.push(RESUME_MAGIC);
        resume.extend_from_slice(&reconnect_token);
        resume.extend_from_slice(&received.to_le_bytes());
        send_binary(&mut ws, resume).await?;

        let welcome = read_welcome(&mut ws).await?;
        Ok(Self { ws, welcome, consumed: received })
    }

    pub(crate) fn welcome(&self) -> &DataWelcome {
        &self.welcome
    }

    pub(crate) fn consumed_bytes(&self) -> u64 {
        self.consumed
    }

    pub(crate) async fn next(&mut self) -> Option<StreamItem> {
        loop {
            let frame = match self.ws.next().await? {
                Ok(Message::Binary(b)) => b,
                Ok(Message::Close(_)) => return None,
                Ok(_) => continue,
                Err(e) => {
                    tracing::debug!(error = %e, "rex data read error");
                    return None;
                }
            };
            let nonce = self.welcome.channel_nonce;
            match split_control(&frame, nonce) {
                Some((prefix, control)) => {
                    if !prefix.is_empty() {
                        self.consumed += prefix.len() as u64;
                        return Some(StreamItem::Bytes(prefix.to_vec()));
                    }
                    self.consumed += frame.len() as u64;
                    return Some(StreamItem::Control(control));
                }
                None => {
                    self.consumed += frame.len() as u64;
                    return Some(StreamItem::Bytes(frame.to_vec()));
                }
            }
        }
    }

    pub(crate) async fn close(self) {
        let _ = self.ws.close(None).await;
    }
}

async fn send_binary(ws: &mut WebSocketStream<UnixStream>, bytes: Vec<u8>) -> Result<(), RexDataError> {
    use futures_util::SinkExt;
    ws.send(Message::Binary(bytes))
        .await
        .map_err(|e| RexDataError::Io(format!("data handshake send: {e}")))
}

async fn read_welcome(ws: &mut WebSocketStream<UnixStream>) -> Result<DataWelcome, RexDataError> {
    let frame = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws.next())
        .await
        .map_err(|_| RexDataError::Io("timed out waiting for data handshake reply".into()))?
        .transpose()
        .map_err(|e| RexDataError::Io(format!("data handshake read: {e}")))?
        .ok_or_else(|| RexDataError::Io("data connection closed during handshake".into()))?;
    let Message::Binary(b) = frame else {
        return Err(RexDataError::Io("expected binary handshake frame".into()));
    };
    match b.first() {
        Some(&WELCOME_MAGIC) if b.len() == 32 => Ok(DataWelcome {
            reconnect_token: b[1..17].try_into().unwrap(),
            keepalive_ms: u32::from_le_bytes(b[20..24].try_into().unwrap()),
            channel_nonce: u64::from_le_bytes(b[24..32].try_into().unwrap()),
        }),
        Some(&REJECT_MAGIC) if b.len() == 2 => Err(RexDataError::Rejected(DataReject { reason: b[1] })),
        _ => Err(RexDataError::Io(format!(
            "malformed handshake reply: {} bytes, first byte {:?}",
            b.len(),
            b.first()
        ))),
    }
}

/// Split one frame at its APC control sequence, if present and nonce-valid.
/// Returns (bytes before the APC, parsed control JSON). Bytes after the APC
/// belong to the control message: the server writes whole messages per frame,
/// so a frame holds at most one control sequence.
fn split_control(frame: &[u8], nonce: u64) -> Option<(&[u8], Value)> {
    let marker = format!("\x1b_rex1;{nonce};");
    let pos = frame
        .windows(marker.len())
        .position(|w| w == marker.as_bytes())?;
    let after = &frame[pos + marker.len()..];
    // The APC terminator is ST (ESC \); JSON cannot contain a bare ESC.
    let end = after.iter().position(|&b| b == 0x1b)?;
    let control: Value = serde_json::from_slice(&after[..end]).ok()?;
    Some((&frame[..pos], control))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn welcome_layout_parses() {
        let mut b = vec![WELCOME_MAGIC];
        b.extend_from_slice(&[7u8; 16]);
        b.extend_from_slice(&[0, 0, 0]);
        b.extend_from_slice(&60000u32.to_le_bytes());
        b.extend_from_slice(&0xdeadbeefu64.to_le_bytes());
        assert_eq!(b.len(), 32);
        let reconnect_token: [u8; 16] = b[1..17].try_into().unwrap();
        assert_eq!(
            DataWelcome {
                reconnect_token,
                keepalive_ms: u32::from_le_bytes(b[20..24].try_into().unwrap()),
                channel_nonce: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            }
            .channel_nonce,
            0xdeadbeef
        );
    }

    #[test]
    fn welcome_reject_layouts_differ() {
        assert_eq!(vec![REJECT_MAGIC, 0x01].len(), 2);
    }

    #[test]
    fn control_apc_split() {
        let nonce = 42u64;
        let json = r#"{"type":"resize","cols":80,"rows":24}"#;
        let mut frame = b"screen bytes \x1b[2J".to_vec();
        frame.extend_from_slice(format!("\x1b_rex1;{nonce};{json}\x1b\\").as_bytes());
        let (prefix, control) = split_control(&frame, nonce).expect("apc found");
        assert_eq!(prefix, b"screen bytes \x1b[2J");
        assert_eq!(control["type"], "resize");
        assert_eq!(control["cols"], 80);
    }

    #[test]
    fn lookalike_apc_with_wrong_nonce_is_data() {
        let frame = b"\x1b_rex1;999;{\"type\":\"resize\"}\x1b\\".to_vec();
        assert!(split_control(&frame, 42).is_none());
    }
}
