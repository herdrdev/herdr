//! Shepherd's durable agent message bus. Shepherd owns terminals; Shepherd owns agent mail and its timeline.

use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs::OpenOptions, io::{BufRead, BufReader, Write}, path::{Path, PathBuf}, sync::Mutex};

pub const KIND_MESSAGE: &str = "msg";
pub const KIND_RESULT: &str = "result";
pub const KIND_SYSTEM: &str = "system";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub ts: String,
    pub from: String,
    pub to: String,
    pub kind: String,
    pub content: String,
}

#[derive(Default)]
struct State {
    all: Vec<Message>,
    pending: HashMap<String, Vec<Message>>,
    read_through: HashMap<String, i64>,
    next: i64,
}

pub struct MessageBus {
    path: PathBuf,
    state: Mutex<State>,
}

impl MessageBus {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let path = path.into();
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) { std::fs::create_dir_all(parent)?; }
        let mut state = State { next: 1, ..State::default() };
        match std::fs::File::open(&path) {
            Ok(file) => {
                let mut reader = BufReader::new(file);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if reader.read_until(b'\n', &mut line)? == 0 { break; }
                    if line.len() > 16 * 1024 * 1024 { return Err("message timeline line exceeds 16 MiB".into()); }
                    if let Ok(message) = serde_json::from_slice::<Message>(&line) {
                        state.next = message.id + 1;
                        state.all.push(message);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        match std::fs::read(path_with_suffix(&path, ".read")) {
            Ok(bytes) => state.read_through = serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for message in &state.all {
            if !message.to.is_empty() && message.kind != KIND_SYSTEM && message.id > *state.read_through.get(&message.to).unwrap_or(&0) {
                state.pending.entry(message.to.clone()).or_default().push(message.clone());
            }
        }
        Ok(Self { path, state: Mutex::new(state) })
    }

    pub fn send(&self, mut message: Message) -> Result<Message, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        message.id = state.next;
        message.ts = super::now_rfc3339();
        state.next += 1;
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.append(true).create(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&self.path)?.write_all(&line)?;
        state.all.push(message.clone());
        if !message.to.is_empty() && message.kind != KIND_SYSTEM {
            state.pending.entry(message.to.clone()).or_default().push(message.clone());
        }
        Ok(message)
    }

    pub fn read(&self, recipient: &str) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        let pending = state.pending.remove(recipient).unwrap_or_default();
        if pending.is_empty() { return Ok(pending); }
        let max_id = pending.iter().map(|message| message.id).max().unwrap_or(0);
        let previous = state.read_through.get(recipient).copied().unwrap_or(0);
        state.read_through.insert(recipient.to_owned(), previous.max(max_id));
        if let Err(error) = save_read_checkpoint(&self.path, &state.read_through) {
            state.read_through.insert(recipient.to_owned(), previous);
            state.pending.insert(recipient.to_owned(), pending.clone());
            return Err(error);
        }
        Ok(pending)
    }

    pub fn since(&self, after: i64) -> Result<Vec<Message>, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        Ok(state.all.iter().filter(|message| message.id > after).cloned().collect())
    }

    pub fn last(&self) -> Result<i64, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.state.lock().map_err(|_| "message bus lock poisoned")?;
        Ok(state.next - 1)
    }
}

fn save_read_checkpoint(path: &Path, checkpoints: &HashMap<String, i64>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".read.tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&tmp)?.write_all(&serde_json::to_vec(checkpoints)?)?;
    std::fs::rename(tmp, path_with_suffix(path, ".read"))?;
    Ok(())
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(from: &str, to: &str, kind: &str, content: &str) -> Message {
        Message { id: 0, ts: String::new(), from: from.into(), to: to.into(), kind: kind.into(), content: content.into() }
    }

    #[test]
    fn persists_timeline_and_restores_only_unread_mail_after_restart() {
        let path = std::env::temp_dir().join(format!("shepherd-messages-{}.jsonl", std::process::id()));
        for suffix in ["", ".read", ".read.tmp"] { let _ = std::fs::remove_file(path_with_suffix(&path, suffix)); }
        {
            let bus = MessageBus::open(&path).unwrap();
            assert_eq!(bus.send(message("brain-1", "coder-1", KIND_MESSAGE, "build the API")).unwrap().id, 1);
            assert_eq!(bus.send(message("coder-1", "", KIND_SYSTEM, "coder started")).unwrap().id, 2);
            assert_eq!(bus.send(message("reviewer-1", "coder-1", KIND_RESULT, "needs changes")).unwrap().id, 3);
            assert_eq!(bus.last().unwrap(), 3);
            let mail = bus.read("coder-1").unwrap();
            assert_eq!(mail.iter().map(|m| m.id).collect::<Vec<_>>(), [1, 3]);
            assert!(bus.read("coder-1").unwrap().is_empty());
        }
        {
            let bus = MessageBus::open(&path).unwrap();
            assert!(bus.read("coder-1").unwrap().is_empty());
            assert_eq!(bus.since(1).unwrap().iter().map(|m| m.id).collect::<Vec<_>>(), [2, 3]);
            assert_eq!(bus.send(message("brain-1", "coder-1", KIND_MESSAGE, "follow up")).unwrap().id, 4);
            assert_eq!(bus.read("coder-1").unwrap()[0].id, 4);
        }
        for suffix in ["", ".read", ".read.tmp"] { let _ = std::fs::remove_file(path_with_suffix(&path, suffix)); }
    }
}
