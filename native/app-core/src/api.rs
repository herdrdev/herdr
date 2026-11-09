//! The server's JSON API (one request per connection over a unix socket), used for actions the endpoint
//! does not carry: focusing a space, tab or agent, and the Shepherd-layer methods later on.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `…/shepherd-client.sock` → `…/shepherd.sock`; `…/s-client.sock` → `…/s.sock`.
pub fn api_socket_for(client_socket: &Path) -> PathBuf {
    let name = client_socket
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("shepherd-client.sock");
    let api = name
        .strip_suffix("-client.sock")
        .map(|stem| format!("{stem}.sock"))
        .unwrap_or_else(|| "shepherd.sock".into());
    client_socket.with_file_name(api)
}

pub fn call(socket: &Path, method: &str, params: Value) -> Result<Value, String> {
    let mut stream =
        UnixStream::connect(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let mut line =
        serde_json::to_vec(&json!({ "id": "native", "method": method, "params": params }))
            .map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    match value.get("error") {
        Some(error) => Err(error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_owned()),
        None => Ok(value["result"].clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_socket_is_found_next_to_the_client_socket() {
        assert_eq!(
            api_socket_for(Path::new("/c/shepherd-client.sock")),
            Path::new("/c/shepherd.sock")
        );
        assert_eq!(
            api_socket_for(Path::new("/t/s-client.sock")),
            Path::new("/t/s.sock")
        );
        assert_eq!(
            api_socket_for(Path::new("/t/odd.sock")),
            Path::new("/t/shepherd.sock")
        );
    }
}
