//! The single most recent clipboard entry Herdr served to pane children.
//!
//! Pane children copy with OSC 52 writes and read the clipboard back with an OSC 52
//! query. Herdr answers that query from this one-slot cache, so a yank in a pane can
//! be pasted again by a clipboard-aware program (neovim, `xclip -o`, `wl-paste`)
//! without a round trip to the attached client's real clipboard. Only the newest
//! entry is retained: there is no clipboard history and Herdr never reads the host
//! clipboard on a child's behalf.

use std::sync::{Arc, Mutex, OnceLock};

/// Largest entry served back to a querying child. libghostty-vt already refuses
/// larger OSC 52 writes, so this only bounds what a reply can carry.
const MAX_ENTRY_BYTES: usize = 192 * 1024;

/// One-slot clipboard shared by the panes of a Herdr server process.
///
/// Cheap to clone: the read callback installed on a pane terminal and the pane's
/// PTY reader hold the same slot, and every pane in the process shares it because
/// the attached client has one clipboard.
#[derive(Clone, Default)]
pub(crate) struct ClipboardCache {
    last_entry: Arc<Mutex<Option<Vec<u8>>>>,
}

impl ClipboardCache {
    /// The cache shared by every pane terminal in this process.
    pub(crate) fn shared() -> Self {
        static SHARED: OnceLock<ClipboardCache> = OnceLock::new();
        SHARED.get_or_init(ClipboardCache::default).clone()
    }

    /// Records a clipboard write observed from a pane child. Oversized and empty
    /// writes are ignored so a child cannot grow the retained entry.
    pub(crate) fn record(&self, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() > MAX_ENTRY_BYTES {
            return;
        }
        let mut entry = self
            .last_entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *entry = Some(bytes.to_vec());
    }

    /// The newest recorded entry, if any.
    pub(crate) fn last_entry(&self) -> Option<Vec<u8>> {
        self.last_entry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Whether a read request asks for a text representation Herdr can serve.
/// Mirrors the MIME accepted on the write side, so a Kitty clipboard (OSC 5522)
/// request for another representation is refused instead of answered with text.
pub(crate) fn serves_requested_mime(mimes: &[&str]) -> bool {
    mimes.iter().any(|mime| {
        mime.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("text/plain")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_entry_tracks_the_newest_write() {
        let cache = ClipboardCache::default();
        assert_eq!(cache.last_entry(), None);

        cache.record(b"first");
        assert_eq!(cache.last_entry(), Some(b"first".to_vec()));
        cache.record(b"second");
        assert_eq!(cache.last_entry(), Some(b"second".to_vec()));
    }

    #[test]
    fn empty_and_oversized_writes_do_not_replace_the_entry() {
        let cache = ClipboardCache::default();
        cache.record(b"kept");

        cache.record(&[]);
        cache.record(&[b'x'; MAX_ENTRY_BYTES + 1]);

        assert_eq!(cache.last_entry(), Some(b"kept".to_vec()));
    }

    #[test]
    fn clones_share_the_same_slot() {
        let cache = ClipboardCache::default();
        cache.record(b"shared");

        let reader = cache.clone();
        reader.record(b"from another pane");
        assert_eq!(cache.last_entry(), Some(b"from another pane".to_vec()));
    }

    #[test]
    fn only_text_representations_are_served() {
        assert!(serves_requested_mime(&["text/plain"]));
        assert!(serves_requested_mime(&["text/plain;charset=utf-8"]));
        assert!(serves_requested_mime(&["image/png", "text/plain"]));
        assert!(!serves_requested_mime(&[]));
        assert!(!serves_requested_mime(&["image/png"]));
        assert!(!serves_requested_mime(&["UTF8_STRING"]));
    }
}
