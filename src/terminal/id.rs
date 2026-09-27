use std::borrow::Borrow;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Opaque identity for a server-owned terminal.
///
/// During the pane-backed transition this is stored one-to-one beside panes,
/// but callers must not derive it from a pane id or layout position.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TerminalId(String);

static NEXT_TERMINAL_ID: AtomicU64 = AtomicU64::new(1);

impl TerminalId {
    pub fn alloc() -> Self {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros())
            .unwrap_or(0);
        let counter = NEXT_TERMINAL_ID.fetch_add(1, Ordering::Relaxed);
        Self(format!("term_{micros:x}{counter:x}"))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for TerminalId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn terminal_ids_support_borrowed_lookup_without_changing_identity() {
        let id = TerminalId::alloc();
        let serialized = serde_json::to_string(&id).unwrap();
        let restored: TerminalId = serde_json::from_str(&serialized).unwrap();
        let mut terminals = HashMap::from([(id.clone(), 1)]);

        assert_eq!(terminals.get(restored.as_str()), Some(&1));
        assert_eq!(terminals.get("missing-terminal"), None);
        *terminals.get_mut(restored.as_str()).unwrap() = 2;
        assert_eq!(terminals.remove(id.as_str()), Some(2));
        assert!(terminals.is_empty());
    }
}
