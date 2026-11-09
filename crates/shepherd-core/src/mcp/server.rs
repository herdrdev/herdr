//! HTTP MCP server implementation for Shepherd agents.

use super::tools::{call_tool, tool_list};
use super::State;
use crate::{config, load_agent_registry, terminals::Terminals};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_BODY: usize = 2 * 1024 * 1024;

pub struct RunningMcpServer {
    addr: std::net::SocketAddr,
    url: String,
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl RunningMcpServer {
    pub fn start(
        port: u16,
        state_dir: PathBuf,
        terminals: Option<Arc<dyn Terminals>>,
        active_vendors: Option<Vec<String>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let addr = listener.local_addr()?;
        let url = format!("http://{addr}/mcp");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let state = Arc::new(Mutex::new(State {
            registry: state_dir.join("agents.json"),
            bus_path: state_dir.join("messages.jsonl"),
            mcp_url: Some(url.clone()),
            active_vendors,
            terminals,
        }));
        let join = std::thread::Builder::new()
            .name("shepherd-mcp-http".into())
            .spawn(move || {
                for incoming in listener.incoming() {
                    if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = incoming {
                        let state = Arc::clone(&state);
                        std::thread::spawn(move || {
                            let _ = handle_http(stream, state);
                        });
                    } else {
                        break;
                    }
                }
            })?;
        Ok(Self {
            addr,
            url,
            stop,
            join: Some(join),
        })
    }

    pub fn addr(&self) -> std::net::SocketAddr {
        self.addr
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn shutdown(&mut self) {
        if !self.stop.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for RunningMcpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn serve(port: u16) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve_with_terminals(port, None)
}

pub fn serve_with_terminals(
    port: u16,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let addr = listener.local_addr()?;
    println!("MCP endpoint: http://{addr}/mcp");
    serve_listener(listener, Some(format!("http://{addr}/mcp")), terminals)
}

fn serve_listener(
    listener: TcpListener,
    mcp_url: Option<String>,
    terminals: Option<Arc<dyn Terminals>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state_dir = config::state_dir();
    let state = Arc::new(Mutex::new(State {
        registry: state_dir.join("agents.json"),
        bus_path: state_dir.join("messages.jsonl"),
        mcp_url,
        active_vendors: None,
        terminals,
    }));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    let _ = handle_http(stream, state);
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn handle_http(mut stream: TcpStream, state: Arc<Mutex<State>>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::with_capacity(4096);
    let mut buf = [0u8; 4096];
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= 16 * 1024 {
            return respond(&mut stream, 413, "text/plain", b"headers too large");
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&buf[..n]);
    };
    let header = std::str::from_utf8(&bytes[..header_end]).unwrap_or("");
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    let mut auth = String::new();
    let mut content_length = None;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("authorization") {
                auth = value.trim().to_owned();
            }
            if key.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
    if method != "POST" || path != "/mcp" {
        return respond(&mut stream, 404, "text/plain", b"not found");
    }
    let Some(token) = auth.strip_prefix("Bearer ").map(str::to_owned) else {
        return respond(&mut stream, 401, "text/plain", b"unauthorized");
    };
    let length = content_length.unwrap_or(0);
    if length == 0 || length > MAX_BODY {
        return respond(&mut stream, 413, "text/plain", b"invalid request size");
    }
    while bytes.len() < header_end + length {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return respond(&mut stream, 400, "text/plain", b"incomplete body");
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let request: Value = match serde_json::from_slice(&bytes[header_end..header_end + length]) {
        Ok(value) => value,
        Err(_) => return respond(&mut stream, 400, "text/plain", b"invalid json"),
    };
    let state_snapshot = state
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    let result = rpc(&state_snapshot, &token, &request);
    match result {
        RpcReply::Notification => respond(&mut stream, 202, "application/json", b""),
        RpcReply::Json(value) => {
            let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
            respond(&mut stream, 200, "application/json", &body)
        }
        RpcReply::Unauthorized => respond(&mut stream, 401, "text/plain", b"unauthorized"),
    }
}

pub(crate) enum RpcReply {
    Json(Value),
    Notification,
    Unauthorized,
}

pub(crate) fn rpc(state: &State, token: &str, request: &Value) -> RpcReply {
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return request
            .get("id")
            .map(|id| rpc_error(id.clone(), -32600, "invalid request"))
            .unwrap_or(RpcReply::Notification);
    };
    let agent = match load_agent_registry(&state.registry) {
        Ok(registry) => match registry.by_token(token) {
            Some(agent) => agent.clone(),
            None => return RpcReply::Unauthorized,
        },
        Err(_) => return RpcReply::Unauthorized,
    };
    let Some(id) = request.get("id") else {
        return RpcReply::Notification;
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2025-03-26",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "shepherd", "version": "0.1.0" }
        }),
        "ping" => json!({}),
        "tools/list" => tool_list(),
        "tools/call" => {
            match call_tool(state, &agent, request.get("params").unwrap_or(&Value::Null)) {
                Ok(result) => result,
                Err(error) => {
                    return RpcReply::Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": error}],
                            "isError": true
                        }
                    }))
                }
            }
        }
        _ => return rpc_error(id.clone(), -32601, "method not found"),
    };
    RpcReply::Json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

pub(crate) fn rpc_error(id: Value, code: i32, message: &str) -> RpcReply {
    RpcReply::Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn respond(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        413 => "Payload Too Large",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}
