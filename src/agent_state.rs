use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::sync::Mutex;
#[cfg(test)]
use std::sync::{Arc, Barrier};
use std::time::SystemTime;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::api::schema::AgentStatus;
use crate::layout::PaneId;

pub(crate) const MAX_LINKS: usize = 200;

#[cfg(test)]
const MAX_OPEN_SEQUENCE_BYTES: usize = 8 * 1024;
// Publish complete URLs through 8 KiB. Once a URL exceeds this bound, discard the
// entire open sequence until its terminator so no printed prefix can escape.
const MAX_URL_BYTES: usize = 8 * 1024;
// RFC schemes are much smaller in practice; cap lookbehind and reject the whole
// URL when the scheme before `://` is longer than 64 bytes.
const MAX_SCHEME_BYTES: usize = 64;
const MAX_MARKER_TAIL_BYTES: usize = MAX_SCHEME_BYTES + 2;
const MAX_SCHEME_CANDIDATE_BYTES: usize = MAX_SCHEME_BYTES + 3;
#[cfg(test)]
const LARGE_TEST_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
#[cfg(test)]
const OLD_STAGING_BOUNDARY_BYTES: usize = LARGE_TEST_OUTPUT_BYTES / 2;

#[derive(Debug)]
pub(crate) struct LinkExtractionGate {
    processing: Mutex<()>,
    pending: Mutex<PendingLinkBytes>,
    extracting: AtomicBool,
    active: AtomicBool,
    marker_tail: AtomicMarkerTail,
    extractions: AtomicU64,
    #[cfg(test)]
    lock_acquisitions: AtomicU64,
    #[cfg(test)]
    scanned_bytes: AtomicU64,
    #[cfg(test)]
    parsed_events: AtomicU64,
    #[cfg(test)]
    parsed_event_bytes: AtomicU64,
    #[cfg(test)]
    observe_race_hook: Mutex<Option<(Arc<Barrier>, Arc<Barrier>)>>,
}

impl Default for LinkExtractionGate {
    fn default() -> Self {
        Self {
            processing: Mutex::new(()),
            pending: Mutex::default(),
            extracting: AtomicBool::new(false),
            active: AtomicBool::new(false),
            marker_tail: AtomicMarkerTail::default(),
            extractions: AtomicU64::new(0),
            #[cfg(test)]
            lock_acquisitions: AtomicU64::new(0),
            #[cfg(test)]
            scanned_bytes: AtomicU64::new(0),
            #[cfg(test)]
            parsed_events: AtomicU64::new(0),
            #[cfg(test)]
            parsed_event_bytes: AtomicU64::new(0),
            #[cfg(test)]
            observe_race_hook: Mutex::new(None),
        }
    }
}

#[derive(Debug)]
struct AtomicMarkerTail {
    words: [AtomicU64; MAX_MARKER_TAIL_BYTES.div_ceil(8)],
    len: AtomicU64,
    sequence: AtomicU64,
}

impl Default for AtomicMarkerTail {
    fn default() -> Self {
        Self {
            words: std::array::from_fn(|_| AtomicU64::new(0)),
            len: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
        }
    }
}

impl AtomicMarkerTail {
    fn load(&self) -> (ScannerPrefix, u64) {
        loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if !sequence.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            let len = (self.len.load(Ordering::Relaxed) as usize).min(MAX_MARKER_TAIL_BYTES);
            let mut bytes = [0; MAX_MARKER_TAIL_BYTES];
            for (word_index, word) in self.words.iter().enumerate() {
                let unpacked = word.load(Ordering::Relaxed).to_le_bytes();
                let start = word_index * 8;
                let end = (start + 8).min(MAX_MARKER_TAIL_BYTES);
                bytes[start..end].copy_from_slice(&unpacked[..end - start]);
            }
            if self.sequence.load(Ordering::Acquire) == sequence {
                return (decode_scanner_prefix(&bytes[..len]), sequence);
            }
        }
    }

    fn store(&self, bytes: &[u8]) {
        let prefix = ScannerPrefix::from_visible_bytes(bytes);
        self.store_prefix(&prefix);
    }

    fn store_prefix(&self, prefix: &ScannerPrefix) {
        loop {
            let (_, sequence) = self.load();
            if self.store_prefix_if_sequence(prefix, sequence) {
                return;
            }
        }
    }

    fn store_prefix_if_sequence(&self, prefix: &ScannerPrefix, sequence: u64) -> bool {
        let bytes = encode_scanner_prefix(prefix);
        debug_assert!(bytes.len() <= MAX_MARKER_TAIL_BYTES);
        if self
            .sequence
            .compare_exchange(
                sequence,
                sequence.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return false;
        }
        for (word_index, word) in self.words.iter().enumerate() {
            let start = word_index * 8;
            let end = (start + 8).min(bytes.len());
            let mut packed = [0; 8];
            if start < end {
                packed[..end - start].copy_from_slice(&bytes[start..end]);
            }
            word.store(u64::from_le_bytes(packed), Ordering::Relaxed);
        }
        self.len.store(bytes.len() as u64, Ordering::Relaxed);
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::Release);
        true
    }
}

#[derive(Debug, Default)]
struct PendingLinkBytes {
    scanner: LinkStreamScanner,
    dirty: bool,
    queued_links: VecDeque<DetectedAgentLink>,
}

#[derive(Debug, Default)]
struct LinkStreamScanner {
    visible: VisibleLinkState,
}

#[derive(Debug, Default)]
enum VisibleLinkState {
    #[default]
    Empty,
    Scheme(SchemeCandidate),
    AfterColon(SchemeCandidate),
    AfterSlash(SchemeCandidate),
    Url(UrlCandidate),
    InvalidScheme,
    DiscardUrl(Vec<u8>),
}

#[derive(Debug, Clone)]
struct SchemeCandidate {
    bytes: [u8; MAX_SCHEME_CANDIDATE_BYTES],
    len: usize,
}

#[derive(Debug, Default)]
struct UrlCandidate {
    bytes: Vec<u8>,
    trim_suffix: Vec<u8>,
    trim_suffix_overflowed: bool,
    pending_utf8: Vec<u8>,
    open_delimiters: [usize; 3],
    close_delimiters: [usize; 3],
}

#[derive(Debug, Clone, Default)]
struct ScannerPrefix {
    visible: PrefixVisibleState,
}

#[derive(Debug, Clone, Default)]
enum PrefixVisibleState {
    #[default]
    Empty,
    Scheme(SchemeCandidate),
    AfterColon(SchemeCandidate),
    AfterSlash(SchemeCandidate),
    InvalidScheme,
}

enum PrefixScanOutcome {
    Quiescent(ScannerPrefix),
    Activate {
        prefix: ScannerPrefix,
        offset: usize,
    },
}

impl ScannerPrefix {
    fn from_visible_bytes(bytes: &[u8]) -> Self {
        let mut prefix = Self::default();
        for byte in bytes.iter().copied() {
            if prefix.would_activate(byte) {
                break;
            }
            prefix.scan_byte(byte);
        }
        prefix
    }

    fn scan(mut self, bytes: &[u8]) -> PrefixScanOutcome {
        for (offset, byte) in bytes.iter().copied().enumerate() {
            if self.would_activate(byte) {
                return PrefixScanOutcome::Activate {
                    prefix: self,
                    offset,
                };
            }
            self.scan_byte(byte);
        }
        PrefixScanOutcome::Quiescent(self)
    }

    fn would_activate(&self, byte: u8) -> bool {
        matches!(
            (&self.visible, byte),
            (PrefixVisibleState::AfterSlash(_), b'/')
        )
    }

    fn scan_byte(&mut self, byte: u8) {
        self.scan_visible(byte);
    }

    fn scan_visible(&mut self, byte: u8) {
        let state = std::mem::take(&mut self.visible);
        self.visible = match state {
            PrefixVisibleState::Empty => start_prefix_scheme(byte),
            PrefixVisibleState::Scheme(mut scheme) if is_scheme_byte(byte) => {
                if scheme.len < MAX_SCHEME_BYTES {
                    scheme.push(byte);
                    PrefixVisibleState::Scheme(scheme)
                } else {
                    PrefixVisibleState::InvalidScheme
                }
            }
            PrefixVisibleState::Scheme(mut scheme) if byte == b':' => {
                scheme.push(byte);
                PrefixVisibleState::AfterColon(scheme)
            }
            PrefixVisibleState::Scheme(_) => start_prefix_scheme(byte),
            PrefixVisibleState::AfterColon(mut scheme) if byte == b'/' => {
                scheme.push(byte);
                PrefixVisibleState::AfterSlash(scheme)
            }
            PrefixVisibleState::AfterColon(_) | PrefixVisibleState::AfterSlash(_) => {
                start_prefix_scheme(byte)
            }
            PrefixVisibleState::InvalidScheme if is_scheme_byte(byte) => {
                PrefixVisibleState::InvalidScheme
            }
            PrefixVisibleState::InvalidScheme => start_prefix_scheme(byte),
        };
    }
}

fn start_prefix_scheme(byte: u8) -> PrefixVisibleState {
    if byte.is_ascii_alphabetic() {
        PrefixVisibleState::Scheme(SchemeCandidate::new(byte))
    } else if is_scheme_byte(byte) {
        PrefixVisibleState::InvalidScheme
    } else {
        PrefixVisibleState::Empty
    }
}

fn encode_scanner_prefix(prefix: &ScannerPrefix) -> &[u8] {
    match &prefix.visible {
        PrefixVisibleState::Scheme(candidate)
        | PrefixVisibleState::AfterColon(candidate)
        | PrefixVisibleState::AfterSlash(candidate) => &candidate.bytes[..candidate.len],
        PrefixVisibleState::InvalidScheme => b"\0",
        PrefixVisibleState::Empty => &[],
    }
}

fn decode_scanner_prefix(bytes: &[u8]) -> ScannerPrefix {
    let visible = if bytes == b"\0" {
        PrefixVisibleState::InvalidScheme
    } else if bytes.is_empty() {
        PrefixVisibleState::Empty
    } else if bytes.ends_with(b":/") {
        PrefixVisibleState::AfterSlash(SchemeCandidate::from_bytes(bytes))
    } else if bytes.ends_with(b":") {
        PrefixVisibleState::AfterColon(SchemeCandidate::from_bytes(bytes))
    } else {
        PrefixVisibleState::Scheme(SchemeCandidate::from_bytes(bytes))
    };
    ScannerPrefix { visible }
}

impl SchemeCandidate {
    fn new(byte: u8) -> Self {
        let mut bytes = [0; MAX_SCHEME_CANDIDATE_BYTES];
        bytes[0] = byte;
        Self { bytes, len: 1 }
    }

    fn push(&mut self, byte: u8) {
        debug_assert!(self.len < self.bytes.len());
        self.bytes[self.len] = byte;
        self.len += 1;
    }

    fn from_bytes(source: &[u8]) -> Self {
        debug_assert!(!source.is_empty());
        let mut candidate = Self::new(source[0]);
        for byte in source.iter().copied().skip(1) {
            candidate.push(byte);
        }
        candidate
    }

    fn into_vec(self) -> Vec<u8> {
        self.bytes[..self.len].to_vec()
    }
}

enum UrlScanOutcome {
    Continue,
    Terminated,
    Discard,
}

impl UrlCandidate {
    fn new(bytes: Vec<u8>) -> Self {
        let mut candidate = Self {
            bytes,
            ..Self::default()
        };
        for byte in candidate.bytes.iter().copied() {
            update_delimiter_counts(
                byte,
                &mut candidate.open_delimiters,
                &mut candidate.close_delimiters,
            );
        }
        candidate
    }

    fn scan_byte(&mut self, byte: u8) -> UrlScanOutcome {
        if byte.is_ascii() {
            if !self.flush_pending_utf8() {
                return UrlScanOutcome::Discard;
            }
            return if is_url_terminator(byte) {
                UrlScanOutcome::Terminated
            } else if self.push_ascii(byte) {
                UrlScanOutcome::Continue
            } else {
                UrlScanOutcome::Discard
            };
        }

        self.pending_utf8.push(byte);
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(value) => {
                let is_whitespace = value.chars().all(char::is_whitespace);
                let bytes = std::mem::take(&mut self.pending_utf8);
                if is_whitespace {
                    UrlScanOutcome::Terminated
                } else if bytes.into_iter().all(|byte| self.push_substantive(byte)) {
                    UrlScanOutcome::Continue
                } else {
                    UrlScanOutcome::Discard
                }
            }
            Err(error) if error.error_len().is_none() && self.pending_utf8.len() < 4 => {
                UrlScanOutcome::Continue
            }
            Err(_) => {
                let bytes = std::mem::take(&mut self.pending_utf8);
                if bytes.into_iter().all(|byte| self.push_substantive(byte)) {
                    UrlScanOutcome::Continue
                } else {
                    UrlScanOutcome::Discard
                }
            }
        }
    }

    fn finish(mut self) -> Option<Vec<u8>> {
        self.flush_pending_utf8().then_some(self.bytes)
    }

    fn flush_pending_utf8(&mut self) -> bool {
        let bytes = std::mem::take(&mut self.pending_utf8);
        bytes.into_iter().all(|byte| self.push_substantive(byte))
    }

    fn push_ascii(&mut self, byte: u8) -> bool {
        if is_always_trimmed_suffix(byte)
            || closing_delimiter_index(byte)
                .is_some_and(|index| self.close_delimiters[index] >= self.open_delimiters[index])
        {
            self.push_trim_suffix(byte);
            return true;
        }
        self.push_substantive(byte)
    }

    fn push_trim_suffix(&mut self, byte: u8) {
        if self.bytes.len() + self.trim_suffix.len() < MAX_URL_BYTES {
            self.trim_suffix.push(byte);
        } else {
            self.trim_suffix_overflowed = true;
        }
    }

    fn push_substantive(&mut self, byte: u8) -> bool {
        if self.trim_suffix_overflowed || self.bytes.len() + self.trim_suffix.len() >= MAX_URL_BYTES
        {
            return false;
        }
        for suffix_byte in self.trim_suffix.drain(..) {
            update_delimiter_counts(
                suffix_byte,
                &mut self.open_delimiters,
                &mut self.close_delimiters,
            );
            self.bytes.push(suffix_byte);
        }
        self.bytes.push(byte);
        update_delimiter_counts(byte, &mut self.open_delimiters, &mut self.close_delimiters);
        true
    }
}

fn is_always_trimmed_suffix(byte: u8) -> bool {
    matches!(byte, b'.' | b',' | b';' | b':' | b'!' | b'?')
}

fn opening_delimiter_index(byte: u8) -> Option<usize> {
    match byte {
        b'(' => Some(0),
        b'[' => Some(1),
        b'{' => Some(2),
        _ => None,
    }
}

fn closing_delimiter_index(byte: u8) -> Option<usize> {
    match byte {
        b')' => Some(0),
        b']' => Some(1),
        b'}' => Some(2),
        _ => None,
    }
}

fn update_delimiter_counts(byte: u8, open: &mut [usize; 3], close: &mut [usize; 3]) {
    if let Some(index) = opening_delimiter_index(byte) {
        open[index] += 1;
    } else if let Some(index) = closing_delimiter_index(byte) {
        close[index] += 1;
    }
}

#[derive(Debug)]
pub(crate) struct ExtractedAgentLinks {
    pub(crate) ordered_links: Vec<DetectedAgentLink>,
    #[cfg(test)]
    pub(crate) output_urls: Vec<String>,
    #[cfg(test)]
    pub(crate) osc8_urls: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedAgentLink {
    pub(crate) url: String,
    pub(crate) source: AgentLinkSource,
}

impl LinkExtractionGate {
    /// Receives only text Ghostty's VT parser classified as printable.
    pub(crate) fn observe_parsed_text(&self, bytes: &[u8]) {
        self.record_parsed_event(bytes.len());
        self.observe_chunk(bytes);
    }

    /// Separates printable runs without exposing a private terminal parser.
    pub(crate) fn observe_parsed_separator(&self) {
        self.record_parsed_event(0);
        self.observe_chunk(b"\n");
    }

    /// Receives a complete OSC 8 target from Ghostty's parser.
    pub(crate) fn observe_parsed_hyperlink(&self, bytes: &[u8]) {
        self.record_parsed_event(bytes.len());
        if bytes.len() > MAX_URL_BYTES {
            return;
        }
        let urls = extract_urls(&String::from_utf8_lossy(bytes));
        if urls.is_empty() {
            return;
        }
        let Ok(_processing) = self.processing.lock() else {
            return;
        };
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if !self.active.load(Ordering::Acquire) {
            let (prefix, _) = self.marker_tail.load();
            pending.scanner.restore_prefix(prefix);
            self.marker_tail.store(&[]);
        }
        for url in urls {
            queue_link(&mut pending.queued_links, url, AgentLinkSource::Osc8);
        }
        pending.dirty = true;
        self.active.store(true, Ordering::Release);
    }

    /// Applies a parser-confirmed OSC boundary to lexical prefix state.
    pub(crate) fn observe_parsed_boundary(&self) {
        self.record_parsed_event(0);
        let Ok(_processing) = self.processing.lock() else {
            return;
        };
        if self.active.load(Ordering::Acquire) {
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            pending.scanner.observe_control_boundary();
        } else {
            let (mut prefix, _) = self.marker_tail.load();
            prefix.observe_control_boundary();
            self.marker_tail.store_prefix(&prefix);
        }
    }

    /// Scans one already-visible byte run. Unit tests use this directly;
    /// production calls it only through Ghostty's parsed-output callback.
    pub(crate) fn observe_chunk(&self, bytes: &[u8]) {
        let mut fast_scan = None;
        if !self.active.load(Ordering::Acquire) {
            let (prefix, sequence) = self.marker_tail.load();
            let outcome = prefix.scan(bytes);
            if let PrefixScanOutcome::Quiescent(prefix) = &outcome {
                #[cfg(test)]
                if let Some((arrived, resume)) = self
                    .observe_race_hook
                    .lock()
                    .ok()
                    .and_then(|hook| hook.clone())
                {
                    arrived.wait();
                    resume.wait();
                }
                if self.marker_tail.store_prefix_if_sequence(prefix, sequence) {
                    self.record_scanned_bytes(bytes.len());
                    return;
                }
            } else {
                fast_scan = Some((outcome, sequence));
            }
        }

        let Ok(_processing) = self.processing.lock() else {
            return;
        };
        if self.active.load(Ordering::Acquire) {
            #[cfg(test)]
            self.lock_acquisitions.fetch_add(1, Ordering::Relaxed);
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            pending.scan(bytes);
            self.record_scanned_bytes(bytes.len());
            pending.dirty = true;
            self.active.store(true, Ordering::Release);
            return;
        }

        let (current_prefix, current_sequence) = self.marker_tail.load();
        let outcome = match fast_scan {
            Some((outcome, sequence)) if sequence == current_sequence => outcome,
            _ => current_prefix.scan(bytes),
        };
        let PrefixScanOutcome::Activate { prefix, offset } = outcome else {
            let PrefixScanOutcome::Quiescent(prefix) = outcome else {
                unreachable!();
            };
            self.marker_tail.store_prefix(&prefix);
            self.record_scanned_bytes(bytes.len());
            return;
        };

        #[cfg(test)]
        self.lock_acquisitions.fetch_add(1, Ordering::Relaxed);
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        pending.scanner.restore_prefix(prefix);
        pending.scan(&bytes[offset..]);
        self.record_scanned_bytes(bytes.len());
        pending.dirty = true;
        self.active.store(true, Ordering::Release);
        self.marker_tail.store(&[]);
    }

    #[cfg(test)]
    pub(crate) fn take_dirty(&self) -> bool {
        self.take_links().is_some()
    }

    pub(crate) fn take_links(&self) -> Option<ExtractedAgentLinks> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        if self
            .extracting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let _claim = ExtractionClaim(&self.extracting);
        let ordered_links = {
            let _processing = self.processing.lock().ok()?;
            let mut pending = self.pending.lock().ok()?;
            if !pending.dirty {
                return None;
            }
            pending.dirty = false;
            let links = std::mem::take(&mut pending.queued_links);
            if let Some(prefix) = pending.scanner.take_prefix() {
                self.marker_tail.store_prefix(&prefix);
                self.active.store(false, Ordering::Release);
            } else {
                self.active.store(true, Ordering::Release);
            }
            links.into_iter().collect::<Vec<_>>()
        };
        #[cfg(test)]
        let mut output_urls = ordered_links
            .iter()
            .filter(|link| link.source == AgentLinkSource::Output)
            .map(|link| link.url.clone())
            .collect::<Vec<_>>();
        #[cfg(test)]
        let mut osc8_urls = ordered_links
            .iter()
            .filter(|link| link.source == AgentLinkSource::Osc8)
            .map(|link| link.url.clone())
            .collect::<Vec<_>>();
        #[cfg(test)]
        output_urls.sort_unstable();
        #[cfg(test)]
        osc8_urls.sort_unstable();
        if ordered_links.is_empty() {
            return None;
        }
        self.extractions.fetch_add(1, Ordering::Relaxed);
        Some(ExtractedAgentLinks {
            ordered_links,
            #[cfg(test)]
            output_urls,
            #[cfg(test)]
            osc8_urls,
        })
    }

    #[cfg(test)]
    pub(crate) fn extraction_count(&self) -> u64 {
        self.extractions.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn lock_acquisition_count(&self) -> u64 {
        self.lock_acquisitions.load(Ordering::Relaxed)
    }

    fn record_scanned_bytes(&self, bytes: usize) {
        let _ = bytes;
        #[cfg(test)]
        self.scanned_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }

    fn record_parsed_event(&self, bytes: usize) {
        let _ = bytes;
        #[cfg(test)]
        {
            self.parsed_events.fetch_add(1, Ordering::Relaxed);
            self.parsed_event_bytes
                .fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub(crate) fn parsed_event_measurement(&self) -> (u64, u64) {
        (
            self.parsed_events.load(Ordering::Relaxed),
            self.parsed_event_bytes.load(Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    fn retained_byte_count(&self) -> usize {
        self.pending
            .lock()
            .map(|pending| pending.scanner.retained_byte_count())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn set_observe_race_hook(&self, arrived: Arc<Barrier>, resume: Arc<Barrier>) {
        if let Ok(mut hook) = self.observe_race_hook.lock() {
            *hook = Some((arrived, resume));
        }
    }
}

impl PendingLinkBytes {
    fn scan(&mut self, bytes: &[u8]) {
        for byte in bytes.iter().copied() {
            self.scanner.scan_byte(byte, &mut self.queued_links);
        }
    }
}

impl LinkStreamScanner {
    fn observe_control_boundary(&mut self) {
        if matches!(self.visible, VisibleLinkState::InvalidScheme) {
            self.visible = VisibleLinkState::Empty;
        }
    }

    fn scan_byte(&mut self, byte: u8, links: &mut VecDeque<DetectedAgentLink>) {
        self.scan_visible(byte, links);
    }

    fn scan_visible(&mut self, byte: u8, links: &mut VecDeque<DetectedAgentLink>) {
        let state = std::mem::take(&mut self.visible);
        self.visible = match state {
            VisibleLinkState::Empty => start_scheme(byte),
            VisibleLinkState::Scheme(mut scheme) if is_scheme_byte(byte) => {
                if scheme.len < MAX_SCHEME_BYTES {
                    scheme.push(byte);
                    VisibleLinkState::Scheme(scheme)
                } else {
                    VisibleLinkState::InvalidScheme
                }
            }
            VisibleLinkState::Scheme(mut scheme) if byte == b':' => {
                scheme.push(byte);
                VisibleLinkState::AfterColon(scheme)
            }
            VisibleLinkState::Scheme(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, links);
                return;
            }
            VisibleLinkState::AfterColon(mut scheme) if byte == b'/' => {
                scheme.push(byte);
                VisibleLinkState::AfterSlash(scheme)
            }
            VisibleLinkState::AfterColon(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, links);
                return;
            }
            VisibleLinkState::AfterSlash(mut scheme) if byte == b'/' => {
                scheme.push(byte);
                VisibleLinkState::Url(UrlCandidate::new(scheme.into_vec()))
            }
            VisibleLinkState::AfterSlash(_) => {
                self.visible = VisibleLinkState::Empty;
                self.scan_visible(byte, links);
                return;
            }
            VisibleLinkState::Url(mut url) => match url.scan_byte(byte) {
                UrlScanOutcome::Continue => VisibleLinkState::Url(url),
                UrlScanOutcome::Terminated => {
                    publish_output_url(url, links);
                    VisibleLinkState::Empty
                }
                UrlScanOutcome::Discard => VisibleLinkState::DiscardUrl(Vec::new()),
            },
            VisibleLinkState::DiscardUrl(mut pending_utf8) => {
                if discarded_url_terminates(&mut pending_utf8, byte) {
                    VisibleLinkState::Empty
                } else {
                    VisibleLinkState::DiscardUrl(pending_utf8)
                }
            }
            VisibleLinkState::InvalidScheme if is_scheme_byte(byte) => {
                VisibleLinkState::InvalidScheme
            }
            VisibleLinkState::InvalidScheme => start_scheme(byte),
        };
    }

    fn restore_prefix(&mut self, prefix: ScannerPrefix) {
        self.visible = match prefix.visible {
            PrefixVisibleState::Empty => VisibleLinkState::Empty,
            PrefixVisibleState::Scheme(candidate) => VisibleLinkState::Scheme(candidate),
            PrefixVisibleState::AfterColon(candidate) => VisibleLinkState::AfterColon(candidate),
            PrefixVisibleState::AfterSlash(candidate) => VisibleLinkState::AfterSlash(candidate),
            PrefixVisibleState::InvalidScheme => VisibleLinkState::InvalidScheme,
        };
    }

    fn take_prefix(&mut self) -> Option<ScannerPrefix> {
        if matches!(
            self.visible,
            VisibleLinkState::Url(_) | VisibleLinkState::DiscardUrl(_)
        ) {
            return None;
        }
        let visible = match std::mem::take(&mut self.visible) {
            VisibleLinkState::Empty => PrefixVisibleState::Empty,
            VisibleLinkState::Scheme(candidate) => PrefixVisibleState::Scheme(candidate),
            VisibleLinkState::AfterColon(candidate) => PrefixVisibleState::AfterColon(candidate),
            VisibleLinkState::AfterSlash(candidate) => PrefixVisibleState::AfterSlash(candidate),
            VisibleLinkState::InvalidScheme => PrefixVisibleState::InvalidScheme,
            VisibleLinkState::Url(_) | VisibleLinkState::DiscardUrl(_) => unreachable!(),
        };
        Some(ScannerPrefix { visible })
    }

    #[cfg(test)]
    fn retained_byte_count(&self) -> usize {
        match &self.visible {
            VisibleLinkState::Scheme(bytes)
            | VisibleLinkState::AfterColon(bytes)
            | VisibleLinkState::AfterSlash(bytes) => bytes.len,
            VisibleLinkState::Url(url) => {
                url.bytes.len() + url.trim_suffix.len() + url.pending_utf8.len()
            }
            VisibleLinkState::Empty
            | VisibleLinkState::InvalidScheme
            | VisibleLinkState::DiscardUrl(_) => 0,
        }
    }
}

impl ScannerPrefix {
    fn observe_control_boundary(&mut self) {
        if matches!(self.visible, PrefixVisibleState::InvalidScheme) {
            self.visible = PrefixVisibleState::Empty;
        }
    }
}

fn start_scheme(byte: u8) -> VisibleLinkState {
    if byte.is_ascii_alphabetic() {
        VisibleLinkState::Scheme(SchemeCandidate::new(byte))
    } else if is_scheme_byte(byte) {
        VisibleLinkState::InvalidScheme
    } else {
        VisibleLinkState::Empty
    }
}

fn publish_output_url(candidate: UrlCandidate, links: &mut VecDeque<DetectedAgentLink>) {
    let Some(bytes) = candidate.finish() else {
        return;
    };
    let url = String::from_utf8_lossy(&bytes);
    if url_within_bounds(&url) && url_domain(&url).is_some() {
        queue_link(links, url.into_owned(), AgentLinkSource::Output);
    }
}

struct ExtractionClaim<'a>(&'a AtomicBool);

impl Drop for ExtractionClaim<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn is_url_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(byte, b'"' | b'\'' | b'<' | b'>')
}

fn discarded_url_terminates(pending_utf8: &mut Vec<u8>, byte: u8) -> bool {
    if byte.is_ascii() {
        pending_utf8.clear();
        return is_url_terminator(byte);
    }
    pending_utf8.push(byte);
    match std::str::from_utf8(pending_utf8) {
        Ok(value) => {
            let whitespace = value.chars().all(char::is_whitespace);
            pending_utf8.clear();
            whitespace
        }
        Err(error) if error.error_len().is_none() && pending_utf8.len() < 4 => false,
        Err(_) => {
            pending_utf8.clear();
            false
        }
    }
}

/// Queues one finished URL, keeping the first source that saw it and dropping
/// the oldest entry past `MAX_LINKS`. The dedup scan is linear in the queue, so
/// it costs at most `MAX_LINKS` string compares, and it runs once per URL that
/// visibly ended rather than per parsed byte; `just bench-parse-scale` covers
/// the OSC 8-heavy case this path is reached from.
fn queue_link(queue: &mut VecDeque<DetectedAgentLink>, url: String, source: AgentLinkSource) {
    let source = queue
        .iter()
        .position(|queued| queued.url == url)
        .and_then(|index| queue.remove(index))
        .map_or(source, |queued| queued.source);
    queue.push_back(DetectedAgentLink { url, source });
    if queue.len() > MAX_LINKS {
        queue.pop_front();
    }
}

fn is_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentTask {
    pub text: String,
    pub status: AgentTaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSubagentSource {
    Observed,
    Reported,
}

fn reported_source() -> AgentSubagentSource {
    AgentSubagentSource::Reported
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentSubagent {
    pub name: String,
    pub status: AgentStatus,
    #[serde(default)]
    pub last_active_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default = "reported_source")]
    pub source: AgentSubagentSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentLinkSource {
    Output,
    Osc8,
    Transcript,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentLink {
    pub url: String,
    pub domain: String,
    pub first_seen: String,
    pub last_seen: String,
    pub source: AgentLinkSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedAgentLink {
    pub(crate) url: String,
    pub(crate) first_seen: SystemTime,
    pub(crate) last_seen: SystemTime,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentReportPayload {
    /// Absent keeps the stored value, `null` clears it.
    #[serde(default, deserialize_with = "deserialize_present")]
    pub status_text: Option<Option<String>>,
    /// Absent keeps the stored value, `null` clears it.
    #[serde(default, deserialize_with = "deserialize_present")]
    pub goal: Option<Option<String>>,
    /// Absent keeps the stored list, `[]` clears it.
    #[serde(default)]
    pub tasks: Option<Vec<AgentTask>>,
    /// Absent keeps the stored list, `[]` clears it.
    #[serde(default)]
    pub subagents: Option<Vec<AgentSubagent>>,
}

/// Distinguishes an absent field from an explicit `null`.
fn deserialize_present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AgentStateSnapshot {
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(default)]
    pub goal: Option<String>,
    #[serde(default)]
    pub last_acted_at: Option<String>,
    pub tasks: Vec<AgentTask>,
    pub subagents: Vec<AgentSubagent>,
    pub links: Vec<AgentLink>,
}

#[derive(Debug, Clone, Default)]
struct PaneAgentState {
    status_text: Option<String>,
    goal: Option<String>,
    last_report_at: Option<SystemTime>,
    last_working_at: Option<SystemTime>,
    last_transcript_at: Option<SystemTime>,
    reported_tasks: Vec<AgentTask>,
    reported_tasks_at: Option<SystemTime>,
    transcript_session_id: Option<String>,
    transcript_identity: Option<(u64, u64)>,
    transcript_tasks: Option<Vec<AgentTask>>,
    transcript_tasks_at: Option<SystemTime>,
    reported_subagents: Vec<AgentSubagent>,
    observed_subagents: Vec<AgentSubagent>,
    links: Vec<StoredLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredLink {
    url: String,
    domain: String,
    first_seen: SystemTime,
    last_seen: SystemTime,
    source: AgentLinkSource,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentStateStore {
    panes: HashMap<PaneId, PaneAgentState>,
}

impl AgentStateStore {
    pub(crate) fn remove(&mut self, pane_id: PaneId) {
        self.panes.remove(&pane_id);
    }

    pub(crate) fn report(
        &mut self,
        pane_id: PaneId,
        payload: AgentReportPayload,
        observed_at: SystemTime,
    ) -> Result<(), String> {
        validate_report(&payload)?;

        let pane = self.panes.entry(pane_id).or_default();
        if let Some(status_text) = payload.status_text {
            pane.status_text = status_text;
        }
        if let Some(goal) = payload.goal {
            pane.goal = goal;
        }
        pane.last_report_at = Some(observed_at);
        if let Some(tasks) = payload.tasks {
            pane.reported_tasks = tasks;
            pane.reported_tasks_at = Some(observed_at);
        }
        if let Some(subagents) = payload.subagents {
            pane.reported_subagents = subagents
                .into_iter()
                .map(|mut subagent| {
                    subagent.source = AgentSubagentSource::Reported;
                    subagent
                })
                .collect();
        }
        Ok(())
    }

    pub(crate) fn snapshot(&self, pane_id: PaneId, status: AgentStatus) -> AgentStateSnapshot {
        let Some(pane) = self.panes.get(&pane_id) else {
            return AgentStateSnapshot {
                status,
                status_text: None,
                goal: None,
                last_acted_at: None,
                tasks: Vec::new(),
                subagents: Vec::new(),
                links: Vec::new(),
            };
        };
        let last_acted_at = [
            pane.last_report_at,
            pane.last_working_at,
            pane.last_transcript_at,
        ]
        .into_iter()
        .flatten()
        .max()
        .and_then(format_rfc3339);
        let mut subagents = pane.observed_subagents.clone();
        subagents.extend(pane.reported_subagents.clone());
        AgentStateSnapshot {
            status,
            status_text: pane.status_text.clone(),
            goal: pane.goal.clone(),
            last_acted_at,
            tasks: if pane.transcript_tasks_at > pane.reported_tasks_at {
                pane.transcript_tasks.clone().unwrap_or_default()
            } else {
                pane.reported_tasks.clone()
            },
            subagents,
            links: pane
                .links
                .iter()
                .filter_map(|link| {
                    Some(AgentLink {
                        url: link.url.clone(),
                        domain: link.domain.clone(),
                        first_seen: format_rfc3339(link.first_seen)?,
                        last_seen: format_rfc3339(link.last_seen)?,
                        source: link.source,
                    })
                })
                .collect(),
        }
    }

    pub(crate) fn observe_working(&mut self, pane_id: PaneId, observed_at: SystemTime) {
        self.panes.entry(pane_id).or_default().last_working_at = Some(observed_at);
    }

    pub(crate) fn observe_transcript(
        &mut self,
        pane_id: PaneId,
        observed_at: Option<SystemTime>,
        tasks: Option<Vec<AgentTask>>,
        tasks_observed_at: Option<SystemTime>,
        subagents: Vec<AgentSubagent>,
        links: Vec<ObservedAgentLink>,
    ) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        let previous_last = pane.last_transcript_at;
        let previous_tasks = pane.transcript_tasks.clone();
        let previous_tasks_at = pane.transcript_tasks_at;
        let previous_subagents = pane.observed_subagents.clone();
        if let Some(observed_at) = observed_at {
            pane.last_transcript_at = Some(
                pane.last_transcript_at
                    .map_or(observed_at, |current| current.max(observed_at)),
            );
        }
        if let (Some(tasks), Some(tasks_observed_at)) = (tasks, tasks_observed_at) {
            pane.transcript_tasks = Some(tasks);
            pane.transcript_tasks_at = Some(tasks_observed_at);
        }
        pane.observed_subagents = subagents;
        let links_changed = observe_transcript_links_in_pane(pane, links);
        previous_last != pane.last_transcript_at
            || previous_tasks != pane.transcript_tasks
            || previous_tasks_at != pane.transcript_tasks_at
            || previous_subagents != pane.observed_subagents
            || links_changed
    }

    pub(crate) fn begin_transcript_session(&mut self, pane_id: PaneId, session_id: &str) {
        let pane = self.panes.entry(pane_id).or_default();
        if pane.transcript_session_id.as_deref() != Some(session_id) {
            *pane = PaneAgentState {
                transcript_session_id: Some(session_id.to_owned()),
                ..PaneAgentState::default()
            };
        }
    }

    pub(crate) fn observe_transcript_session(
        &mut self,
        pane_id: PaneId,
        session_id: &str,
        identity: (u64, u64),
    ) -> bool {
        let pane = self.panes.entry(pane_id).or_default();
        if pane.transcript_session_id.is_none() {
            // Legacy/unscoped transcript observations cannot belong to the
            // newly attached session. Native reports remain independently owned.
            pane.last_transcript_at = None;
            pane.transcript_tasks = Some(Vec::new());
            pane.transcript_tasks_at = None;
            pane.observed_subagents.clear();
        }
        // SessionStart already reset the scope. Attaching its first file cursor
        // must preserve reports received between the hook and the next scan.
        // Restored panes can also attach their first transcript without a new
        // hook: no previous transcript identity exists to invalidate reports.
        if (pane.transcript_session_id.is_none()
            || pane.transcript_session_id.as_deref() == Some(session_id))
            && pane.transcript_identity.is_none()
        {
            pane.transcript_session_id = Some(session_id.to_owned());
            pane.transcript_identity = Some(identity);
            return true;
        }
        if pane.transcript_session_id.as_deref() == Some(session_id)
            && pane.transcript_identity == Some(identity)
        {
            return false;
        }
        *pane = PaneAgentState {
            transcript_session_id: Some(session_id.to_owned()),
            transcript_identity: Some(identity),
            ..PaneAgentState::default()
        };
        true
    }

    #[cfg(test)]
    pub(crate) fn observe_links(
        &mut self,
        pane_id: PaneId,
        urls: impl IntoIterator<Item = String>,
        source: AgentLinkSource,
        observed_at: SystemTime,
    ) {
        let _ = observe_links_in_pane(
            self.panes.entry(pane_id).or_default(),
            urls,
            source,
            observed_at,
        );
    }

    pub(crate) fn observe_detected_links(
        &mut self,
        pane_id: PaneId,
        links: impl IntoIterator<Item = DetectedAgentLink>,
        observed_at: SystemTime,
    ) {
        let _ = observe_sourced_links_in_pane(
            self.panes.entry(pane_id).or_default(),
            links.into_iter().map(|link| (link.url, link.source)),
            observed_at,
        );
    }
}

fn validate_report(payload: &AgentReportPayload) -> Result<(), String> {
    if payload
        .tasks
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|task| task.text.trim().is_empty() || task.text.chars().any(char::is_control))
    {
        return Err("task text must not be empty or contain control characters".into());
    }
    let subagents = payload.subagents.as_deref().unwrap_or_default();
    if subagents.iter().any(|subagent| {
        subagent.name.trim().is_empty() || subagent.name.chars().any(char::is_control)
    }) {
        return Err("subagent name must not be empty or contain control characters".into());
    }
    if subagents.iter().any(|subagent| {
        subagent
            .last_active_at
            .as_ref()
            .is_some_and(|value| parse_rfc3339(value).is_none())
    }) {
        return Err("subagent last_active_at must be an RFC3339 timestamp".into());
    }
    Ok(())
}

pub(crate) fn format_rfc3339(value: SystemTime) -> Option<String> {
    time::OffsetDateTime::from(value)
        .format(&time::format_description::well_known::Rfc3339)
        .ok()
}

pub(crate) fn parse_rfc3339(value: &str) -> Option<SystemTime> {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(SystemTime::from)
}

#[cfg(test)]
fn observe_links_in_pane(
    pane: &mut PaneAgentState,
    urls: impl IntoIterator<Item = String>,
    source: AgentLinkSource,
    observed_at: SystemTime,
) -> bool {
    observe_sourced_links_in_pane(pane, urls.into_iter().map(|url| (url, source)), observed_at)
}

fn observe_sourced_links_in_pane(
    pane: &mut PaneAgentState,
    links: impl IntoIterator<Item = (String, AgentLinkSource)>,
    observed_at: SystemTime,
) -> bool {
    let before = pane.links.clone();
    for (url, source) in links {
        let Some(domain) = url_domain(&url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|link| link.url == url) {
            existing.last_seen = existing.last_seen.max(observed_at);
            continue;
        }
        pane.links.push(StoredLink {
            url,
            domain,
            first_seen: observed_at,
            last_seen: observed_at,
            source,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

fn observe_transcript_links_in_pane(
    pane: &mut PaneAgentState,
    links: Vec<ObservedAgentLink>,
) -> bool {
    let before = pane.links.clone();
    for link in links {
        let Some(domain) = url_domain(&link.url) else {
            continue;
        };
        if let Some(existing) = pane.links.iter_mut().find(|stored| stored.url == link.url) {
            existing.first_seen = existing.first_seen.min(link.first_seen);
            existing.last_seen = existing.last_seen.max(link.last_seen);
            continue;
        }
        pane.links.push(StoredLink {
            url: link.url,
            domain,
            first_seen: link.first_seen,
            last_seen: link.last_seen,
            source: AgentLinkSource::Transcript,
        });
    }
    pane.links.sort_by_key(|link| link.last_seen);
    if pane.links.len() > MAX_LINKS {
        pane.links.drain(..pane.links.len() - MAX_LINKS);
    }
    pane.links != before
}

static URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s<>\"'\x00-\x1f]+"#).expect("static URL regex")
});
pub(crate) fn extract_urls(text: &str) -> Vec<String> {
    URL_RE
        .find_iter(text)
        .map(|matched| trim_url_suffix(matched.as_str()).to_string())
        .filter(|url| url_within_bounds(url) && url_domain(url).is_some())
        .collect()
}

fn url_within_bounds(url: &str) -> bool {
    url.len() <= MAX_URL_BYTES
        && url
            .split_once("://")
            .is_some_and(|(scheme, _)| scheme.len() <= MAX_SCHEME_BYTES)
}

fn trim_url_suffix(mut url: &str) -> &str {
    let mut open = [0_usize; 3];
    let mut close = [0_usize; 3];
    for byte in url.bytes() {
        update_delimiter_counts(byte, &mut open, &mut close);
    }
    loop {
        let Some(last) = url.as_bytes().last().copied() else {
            return url;
        };
        if is_always_trimmed_suffix(last) {
            url = &url[..url.len() - 1];
            continue;
        }
        if let Some(index) = closing_delimiter_index(last) {
            if close[index] > open[index] {
                close[index] -= 1;
                url = &url[..url.len() - 1];
                continue;
            }
        }
        return url;
    }
}

fn url_domain(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = if host_port.starts_with('[') {
        host_port.split_once(']')?.0.trim_start_matches('[')
    } else {
        host_port.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {

    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct CollectedLinks {
        output_urls: Vec<String>,
        osc8_urls: Vec<String>,
    }

    struct ParsedLinkHarness {
        gate: Arc<LinkExtractionGate>,
        terminal: Mutex<crate::ghostty::Terminal>,
    }

    impl Default for ParsedLinkHarness {
        fn default() -> Self {
            let gate = Arc::new(LinkExtractionGate::default());
            let gate_for_callback = gate.clone();
            let mut terminal = crate::ghostty::Terminal::new(240, 24, 0).expect("Ghostty terminal");
            terminal.set_parsed_output_callback(move |event| match event {
                crate::ghostty::ParsedOutput::Text(bytes) => {
                    gate_for_callback.observe_parsed_text(bytes);
                }
                crate::ghostty::ParsedOutput::Separator => {
                    gate_for_callback.observe_parsed_separator();
                }
                crate::ghostty::ParsedOutput::Hyperlink(bytes) => {
                    gate_for_callback.observe_parsed_hyperlink(bytes);
                }
                crate::ghostty::ParsedOutput::Boundary => {
                    gate_for_callback.observe_parsed_boundary();
                }
            });
            Self {
                gate,
                terminal: Mutex::new(terminal),
            }
        }
    }

    impl ParsedLinkHarness {
        fn observe_chunk(&self, bytes: &[u8]) {
            let mut terminal = self.terminal.lock().expect("Ghostty terminal");
            terminal
                .set_parsed_output_enabled(true)
                .expect("enable parsed output callback");
            terminal.write(bytes);
            terminal
                .set_parsed_output_enabled(false)
                .expect("disable parsed output callback");
        }
    }

    impl std::ops::Deref for ParsedLinkHarness {
        type Target = LinkExtractionGate;

        fn deref(&self) -> &Self::Target {
            &self.gate
        }
    }

    fn collected_links(mut batches: Vec<ExtractedAgentLinks>) -> CollectedLinks {
        let mut output_urls = Vec::new();
        let mut osc8_urls = Vec::new();
        for batch in &mut batches {
            output_urls.append(&mut batch.output_urls);
            osc8_urls.append(&mut batch.osc8_urls);
        }
        output_urls.sort_unstable();
        output_urls.dedup();
        osc8_urls.sort_unstable();
        osc8_urls.dedup();
        CollectedLinks {
            output_urls,
            osc8_urls,
        }
    }

    fn generated_terminal_stream(seed: u64) -> Vec<u8> {
        let mut pieces = vec![
            format!("title-before\x1b]0;ignore https://hidden-{seed}.example/path\x07title-after ")
                .into_bytes(),
            format!(
                "h\x1b[3{}mttps://visible-{seed}.example/pa\x1b(Bth ",
                seed % 8
            )
            .into_bytes(),
            format!("\x1b]8;;https://osc-{seed}.example/target\x1b\\label-{seed}\x1b]8;;\x1b\\ ")
                .into_bytes(),
            format!(
                "\x1b]8;;https://cancelled-can-{seed}.example/target\x18https://after-can-{seed}.example/path\n"
            )
            .into_bytes(),
            format!(
                "\x1b]8;;https://cancelled-sub-{seed}.example/target\x1ahttps://after-sub-{seed}.example/path\n"
            )
            .into_bytes(),
            format!(
                "\x1b]8;;https://cancelled-esc-{seed}.example/target\x1bchttps://after-esc-{seed}.example/path\n"
            )
            .into_bytes(),
            format!(
                "\x1b]0;cancelled title \x1bPhttps://hidden-dcs-{seed}.example/path\x1b\\ "
            )
            .into_bytes(),
            format!(
                "\x1b]0;cancelled title \x1bXhttps://hidden-sos-{seed}.example/path\x1b\\ "
            )
            .into_bytes(),
            format!(
                "\x1b]0;cancelled title \x1b^https://hidden-pm-{seed}.example/path\x1b\\ "
            )
            .into_bytes(),
            format!(
                "\x1b]0;cancelled title \x1b_https://hidden-apc-{seed}.example/path\x1b\\ "
            )
            .into_bytes(),
        ];
        let mut random = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        for index in (1..pieces.len()).rev() {
            random = random
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            pieces.swap(index, random as usize % (index + 1));
        }
        pieces
            .into_iter()
            .flatten()
            .chain(b"\n".iter().copied())
            .collect()
    }

    fn scan_with_generated_splits(bytes: &[u8], seed: u64) -> CollectedLinks {
        let gate = ParsedLinkHarness::default();
        let mut batches = Vec::new();
        let mut offset = 0;
        let mut random = seed;
        while offset < bytes.len() {
            random = random
                .wrapping_mul(2_862_933_555_777_941_757)
                .wrapping_add(3_037_000_493);
            let end = (offset + 1 + random as usize % 17).min(bytes.len());
            gate.observe_chunk(&bytes[offset..end]);
            offset = end;
            if random & 3 == 0 {
                if let Some(batch) = gate.take_links() {
                    batches.push(batch);
                }
            }
        }
        if let Some(batch) = gate.take_links() {
            batches.push(batch);
        }
        collected_links(batches)
    }

    #[test]
    fn report_round_trips_goal_status_tasks_and_subagents() {
        let pane_id = PaneId::from_raw(7);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    status_text: Some(Some("checking tests".into())),
                    goal: Some(Some("ship MAT-160".into())),
                    tasks: Some(vec![AgentTask {
                        text: "run focused tests".into(),
                        status: AgentTaskStatus::InProgress,
                    }]),
                    subagents: Some(vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: None,
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }]),
                },
                now,
            )
            .expect("valid report");

        let state = store.snapshot(pane_id, AgentStatus::Working);
        assert_eq!(state.goal.as_deref(), Some("ship MAT-160"));
        assert_eq!(state.status_text.as_deref(), Some("checking tests"));
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.subagents.len(), 1);
        assert_eq!(state.status, AgentStatus::Working);
        assert!(state.last_acted_at.is_some());
    }

    #[test]
    fn last_acted_at_uses_latest_report_working_or_transcript_observation() {
        let pane_id = PaneId::from_raw(8);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(pane_id, AgentReportPayload::default(), base)
            .expect("valid report");
        store.observe_working(pane_id, base + std::time::Duration::from_secs(5));
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(10)),
            None,
            None,
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store
                .snapshot(pane_id, AgentStatus::Idle)
                .last_acted_at
                .as_deref(),
            format_rfc3339(base + std::time::Duration::from_secs(10)).as_deref()
        );
    }

    #[test]
    fn last_working_observation_advances_during_a_working_stint_and_survives_stop() {
        let pane_id = PaneId::from_raw(9);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let latest_activity = base + std::time::Duration::from_secs(90);
        let mut store = AgentStateStore::default();

        store.observe_working(pane_id, base);
        store.observe_working(pane_id, latest_activity);

        let working = store.snapshot(pane_id, AgentStatus::Working);
        assert_eq!(
            working.last_acted_at.as_deref(),
            format_rfc3339(latest_activity).as_deref()
        );

        let stopped = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(
            stopped.last_acted_at.as_deref(),
            format_rfc3339(latest_activity).as_deref(),
            "after Working stops, age starts at its latest observation"
        );
    }

    #[test]
    fn chunks_without_scheme_marker_do_not_trigger_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output");
        assert!(!gate.take_dirty());
        assert_eq!(gate.extraction_count(), 0);

        gate.observe_chunk(b"visit https://example.test/path\n");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn marker_free_chunk_does_not_take_pending_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary terminal output without a marker");
        assert_eq!(gate.lock_acquisition_count(), 0);

        gate.observe_chunk(b"visit https://example.test");
        assert_eq!(gate.lock_acquisition_count(), 1);
        gate.observe_chunk(b"/continued\n");
        assert_eq!(gate.lock_acquisition_count(), 2);
    }

    #[test]
    fn scheme_marker_split_across_chunks_triggers_link_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https:");
        assert!(!gate.take_dirty());

        gate.observe_chunk(b"//split.example.test/path\n");
        assert!(gate.take_dirty());
        assert_eq!(gate.extraction_count(), 1);
    }

    #[test]
    fn scheme_prefix_accumulates_across_three_chunks() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ht");
        gate.observe_chunk(b"tps:");
        gate.observe_chunk(b"//split.example.test/path\n");

        let links = gate.take_links().expect("link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn long_scheme_prefixes_survive_chunk_boundaries() {
        let colon_split = LinkExtractionGate::default();
        colon_split.observe_chunk(b"postgresql:");
        colon_split.observe_chunk(b"//db.example/path\n");
        assert_eq!(
            colon_split
                .take_links()
                .expect("colon-split URL")
                .output_urls,
            vec!["postgresql://db.example/path"]
        );

        let scheme_split = LinkExtractionGate::default();
        scheme_split.observe_chunk(b"customscheme");
        scheme_split.observe_chunk(b"://longscheme.example.test/path\n");
        assert_eq!(
            scheme_split
                .take_links()
                .expect("scheme-split URL")
                .output_urls,
            vec!["customscheme://longscheme.example.test/path"]
        );

        let mut max_scheme = "a".to_string();
        while max_scheme.len() < MAX_SCHEME_BYTES {
            max_scheme.push_str("1+.-");
        }
        max_scheme.truncate(MAX_SCHEME_BYTES);
        let max_split = LinkExtractionGate::default();
        max_split.observe_chunk(max_scheme.as_bytes());
        max_split.observe_chunk(b"://maxscheme.example.test/path\n");
        assert_eq!(
            max_split
                .take_links()
                .expect("maximum-length scheme URL")
                .output_urls,
            vec![format!("{max_scheme}://maxscheme.example.test/path")]
        );
    }

    #[test]
    fn unfinished_url_waits_for_terminator_before_extraction() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://split.example.test");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"/path\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn unfinished_url_is_not_published_after_a_quiet_period() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://quiet.example.test/path");

        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://quiet.example.test/path"]);
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn split_osc8_before_scheme_stays_an_osc8_link() {
        let gate = ParsedLinkHarness::default();
        gate.observe_chunk(b"\x1b]8;;htt");
        gate.observe_chunk(b"ps://osc.example.test/path\x1b\\label\x1b]8;;\x1b\\\n");

        let links = gate.take_links().expect("completed OSC 8 link");
        assert!(links.output_urls.is_empty());
        assert_eq!(links.osc8_urls, vec!["https://osc.example.test/path"]);
    }

    #[test]
    fn partial_osc8_introducer_carries_across_chunks() {
        for (first, second) in [
            (
                b"\x1b]8".as_slice(),
                b";;https://osc.example.test/from-eight\x1b\\label\n".as_slice(),
            ),
            (
                b"\x1b]8;".as_slice(),
                b";https://osc.example.test/from-semicolon\x1b\\label\n".as_slice(),
            ),
        ] {
            let gate = ParsedLinkHarness::default();
            gate.observe_chunk(first);
            assert!(gate.take_links().is_none());
            gate.observe_chunk(second);

            let links = gate.take_links().expect("split OSC 8 link");
            assert!(links.output_urls.is_empty());
            assert_eq!(links.osc8_urls.len(), 1);
            assert!(links.osc8_urls[0].starts_with("https://osc.example.test/"));
        }
    }

    #[test]
    fn osc8_target_bound_excludes_split_and_unsplit_st_bytes() {
        let prefix = "https://osc.example/";
        for split_st in [false, true] {
            for extra_bytes in [0, 1] {
                let uri = format!(
                    "{prefix}{}",
                    "a".repeat(MAX_URL_BYTES + extra_bytes - prefix.len())
                );
                let gate = ParsedLinkHarness::default();
                let mut batches = Vec::new();
                if split_st {
                    gate.observe_chunk(format!("\x1b]8;;{uri}\x1b").as_bytes());
                    batches.extend(gate.take_links());
                    gate.observe_chunk(b"\\label\x1b]8;;\x1b\\\n");
                } else {
                    gate.observe_chunk(
                        format!("\x1b]8;;{uri}\x1b\\label\x1b]8;;\x1b\\\n").as_bytes(),
                    );
                }
                batches.extend(gate.take_links());

                let links = collected_links(batches);
                if extra_bytes == 0 {
                    assert!(links.output_urls.is_empty());
                    assert_eq!(links.osc8_urls, vec![uri], "split ST: {split_st}");
                } else {
                    assert!(links.output_urls.is_empty(), "split ST: {split_st}");
                    assert!(links.osc8_urls.is_empty(), "split ST: {split_st}");
                }
            }
        }
    }

    #[test]
    fn completed_link_is_not_blocked_by_open_oversized_url() {
        let gate = LinkExtractionGate::default();
        let mut chunk = b"https://complete.example.test/path\nhttps://oversized.example/".to_vec();
        chunk.extend(std::iter::repeat_n(b'a', MAX_URL_BYTES));
        gate.observe_chunk(&chunk);

        let links = gate.take_links().expect("completed output link");
        assert_eq!(
            links.output_urls,
            vec!["https://complete.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());

        gate.observe_chunk(b"\n");
        assert!(gate.take_links().is_none());
    }

    #[test]
    fn url_and_scheme_bounds_are_enforced_without_prefix_publication() {
        let exact_url = format!("https://bounds.example/{}", "a".repeat(MAX_URL_BYTES - 23));
        assert_eq!(exact_url.len(), MAX_OPEN_SEQUENCE_BYTES);
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(format!("{exact_url}\n").as_bytes());
        assert_eq!(
            gate.take_links().expect("maximum-length URL").output_urls,
            vec![exact_url]
        );

        let overlong_url = format!(
            "https://bounds.example/{}\n",
            "b".repeat(LARGE_TEST_OUTPUT_BYTES)
        );
        gate.observe_chunk(overlong_url.as_bytes());
        assert!(gate.take_links().is_none());

        let overlong_scheme = format!(
            "{}://scheme.example/path\n",
            "a".repeat(MAX_SCHEME_BYTES + 1)
        );
        gate.observe_chunk(overlong_scheme.as_bytes());
        assert!(gate.take_links().is_none());
    }

    #[test]
    fn colon_without_scheme_marker_does_not_take_pending_lock() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"ordinary: terminal output / without a URL");
        assert_eq!(gate.lock_acquisition_count(), 0);
    }

    #[test]
    fn repeated_output_url_keeps_first_seen_and_advances_only_last_seen() {
        let pane_id = PaneId::from_raw(84);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let gate = LinkExtractionGate::default();
        let mut store = AgentStateStore::default();

        gate.observe_chunk(b"https://same.example/path\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("first print").output_urls,
            AgentLinkSource::Output,
            first_seen,
        );

        gate.observe_chunk(b"https://same.example/path");
        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("delayed terminator").output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(2),
        );

        gate.observe_chunk(b"https://same.example/path\n");
        store.observe_links(
            pane_id,
            gate.take_links().expect("third print").output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(3),
        );

        let links = store.snapshot(pane_id, AgentStatus::Idle).links;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].first_seen, format_rfc3339(first_seen).unwrap());
        assert_eq!(
            links[0].last_seen,
            format_rfc3339(first_seen + std::time::Duration::from_secs(3)).unwrap()
        );
    }

    #[test]
    fn interleaved_output_and_osc8_urls_keep_independent_timestamps() {
        let pane_id = PaneId::from_raw(85);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let gate = ParsedLinkHarness::default();
        let mut store = AgentStateStore::default();

        gate.observe_chunk(b"https://open.example/path");
        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\x1b]8;;https://interleaved.example/complete\x1b\\\x1b]8;;\x1b\\");
        let completed = gate.take_links().expect("completed OSC 8 link");
        assert!(completed.output_urls.is_empty());
        store.observe_links(
            pane_id,
            completed.osc8_urls,
            AgentLinkSource::Osc8,
            first_seen,
        );

        gate.observe_chunk(b"/tail\n");
        let open = gate.take_links().expect("terminated output link");
        assert!(open.osc8_urls.is_empty());
        store.observe_links(
            pane_id,
            open.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_secs(2),
        );

        let links = store.snapshot(pane_id, AgentStatus::Idle).links;
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].url, "https://interleaved.example/complete");
        assert_eq!(links[0].first_seen, format_rfc3339(first_seen).unwrap());
        assert_eq!(links[0].last_seen, links[0].first_seen);
        assert_eq!(links[0].source, AgentLinkSource::Osc8);
        assert_eq!(links[1].url, "https://open.example/path/tail");
        assert_eq!(
            links[1].first_seen,
            format_rfc3339(first_seen + std::time::Duration::from_secs(2)).unwrap()
        );
        assert_eq!(links[1].last_seen, links[1].first_seen);
        assert_eq!(links[1].source, AgentLinkSource::Output);
    }

    #[test]
    fn scheme_only_tail_survives_a_detection_tick_between_chunks() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://");

        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"split.example.test/path\n");
        let links = gate.take_links().expect("terminated link extraction");
        assert_eq!(links.output_urls, vec!["https://split.example.test/path"]);
    }

    #[test]
    fn completed_url_batch_carries_trailing_scheme_into_next_chunk() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://split.example.test");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"/path\npostgresql:");
        let completed = gate.take_links().expect("completed URL publication");
        assert_eq!(
            completed.output_urls,
            vec!["https://split.example.test/path"]
        );

        gate.observe_chunk(b"//db.example/path\n");
        let database = gate.take_links().expect("scheme continuation publication");
        assert_eq!(database.output_urls, vec!["postgresql://db.example/path"]);

        let pane_id = PaneId::from_raw(83);
        let first_seen = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store.observe_links(
            pane_id,
            completed.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_millis(300),
        );
        store.observe_links(
            pane_id,
            database.output_urls,
            AgentLinkSource::Output,
            first_seen + std::time::Duration::from_millis(400),
        );

        let urls = store
            .snapshot(pane_id, AgentStatus::Idle)
            .links
            .into_iter()
            .map(|link| link.url)
            .collect::<Vec<_>>();
        assert_eq!(
            urls,
            vec![
                "https://split.example.test/path",
                "postgresql://db.example/path",
            ]
        );
    }

    #[test]
    fn styled_url_ignores_csi_osc_and_charset_select_sequences() {
        let gate = ParsedLinkHarness::default();
        gate.observe_chunk(
            concat!(
                "https://sty",
                "\x1b[31m",
                "led.",
                "\x1b]0;terminal title\x07",
                "example.test",
                "\x1b(B",
                "/path\n"
            )
            .as_bytes(),
        );

        let links = gate.take_links().expect("styled link extraction");
        assert_eq!(links.output_urls, vec!["https://styled.example.test/path"]);
    }

    #[test]
    fn styled_scheme_preserves_prefix_across_detection_cycle() {
        let styled = ParsedLinkHarness::default();
        styled.observe_chunk(b"h\x1b[31mttps:");
        assert!(styled.take_links().is_none());
        styled.observe_chunk(b"//styled-scheme.example/path\n");
        assert_eq!(
            styled.take_links().expect("styled scheme URL").output_urls,
            vec!["https://styled-scheme.example/path"]
        );
    }

    #[test]
    fn non_hyperlink_osc_hides_embedded_url() {
        let hidden = ParsedLinkHarness::default();
        hidden.observe_chunk(
            b"\x1b]0;title https://hidden.example/path\x07https://visible.example/path\n",
        );
        assert_eq!(
            hidden
                .take_links()
                .expect("visible URL after title")
                .output_urls,
            vec!["https://visible.example/path"]
        );
    }

    #[test]
    fn unicode_whitespace_terminates_output_url() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk("https://unicode.example/path\u{00a0}next words\n".as_bytes());

        assert_eq!(
            gate.take_links()
                .expect("URL before non-breaking space")
                .output_urls,
            vec!["https://unicode.example/path"]
        );
    }

    #[test]
    fn punctuated_maximum_length_url_is_published() {
        let prefix = "https://maximum.example/";
        let url = format!("{prefix}{}", "a".repeat(MAX_URL_BYTES - prefix.len()));
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(format!("{url}.\n").as_bytes());

        assert_eq!(
            gate.take_links()
                .expect("punctuated maximum-length URL")
                .output_urls,
            vec![url]
        );
    }

    #[test]
    fn link_queue_evicts_oldest_observation_before_store() {
        let pane_id = PaneId::from_raw(99);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut ordered_store = AgentStateStore::default();
        for index in 0..=MAX_LINKS {
            ordered_store.observe_links(
                pane_id,
                [format!("https://store-order.example/{index:03}")],
                AgentLinkSource::Output,
                base + std::time::Duration::from_secs(index as u64),
            );
        }
        let ordered = ordered_store.snapshot(pane_id, AgentStatus::Idle).links;
        assert_eq!(ordered.len(), MAX_LINKS);
        assert!(!ordered
            .iter()
            .any(|link| link.url == "https://store-order.example/000"));
        assert!(ordered
            .iter()
            .any(|link| link.url == format!("https://store-order.example/{MAX_LINKS:03}")));

        let mut stream = Vec::new();
        for index in 0..MAX_LINKS {
            stream.extend_from_slice(format!("https://z-old.example/{index:03}\n").as_bytes());
        }
        let newest = "https://a-newest.example/path";
        stream.extend_from_slice(format!("{newest}\n").as_bytes());

        let gate = LinkExtractionGate::default();
        gate.observe_chunk(&stream);
        let extracted = gate.take_links().expect("bounded URL batch");
        assert_eq!(extracted.output_urls.len(), MAX_LINKS);
        assert!(extracted.output_urls.iter().any(|url| url == newest));
        assert!(!extracted
            .output_urls
            .iter()
            .any(|url| url == "https://z-old.example/000"));

        let mut batch_store = AgentStateStore::default();
        batch_store.observe_links(
            pane_id,
            extracted.output_urls,
            AgentLinkSource::Output,
            base,
        );
        let stored = batch_store.snapshot(pane_id, AgentStatus::Idle).links;
        assert!(stored.iter().any(|link| link.url == newest));
        assert!(!stored
            .iter()
            .any(|link| link.url == "https://z-old.example/000"));

        let mut mixed_stream = Vec::new();
        for index in 0..=MAX_LINKS {
            if index % 2 == 0 {
                mixed_stream.extend_from_slice(
                    format!("https://mixed-output.example/{index:03}\n").as_bytes(),
                );
            } else {
                mixed_stream.extend_from_slice(
                    format!(
                        "\x1b]8;;https://mixed-osc.example/{index:03}\x1b\\label\x1b]8;;\x1b\\\n"
                    )
                    .as_bytes(),
                );
            }
        }
        mixed_stream.extend_from_slice(b"https://mixed-osc.example/001\n");

        let mixed_gate = ParsedLinkHarness::default();
        mixed_gate.observe_chunk(&mixed_stream);
        let mixed = mixed_gate.take_links().expect("mixed-source URL batch");
        assert_eq!(
            mixed.output_urls.len() + mixed.osc8_urls.len(),
            MAX_LINKS,
            "one shared queue must cap the mixed-source detection cycle"
        );
        assert!(!mixed
            .output_urls
            .iter()
            .any(|url| url == "https://mixed-output.example/000"));
        assert!(mixed
            .output_urls
            .iter()
            .any(|url| { url == &format!("https://mixed-output.example/{MAX_LINKS:03}") }));
        assert!(mixed
            .osc8_urls
            .iter()
            .any(|url| url == "https://mixed-osc.example/001"));
        assert!(!mixed
            .output_urls
            .iter()
            .any(|url| url == "https://mixed-osc.example/001"));
    }

    #[test]
    fn unmatched_suffix_trimming_is_linear_enough_for_pty_ingest() {
        let prefix = "https://suffix.example/";
        let url = format!("{prefix}{}", ")".repeat(MAX_URL_BYTES - prefix.len()));
        let gate = LinkExtractionGate::default();
        let started = std::time::Instant::now();
        gate.observe_chunk(format!("{url}\n").as_bytes());
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "trimming one 8 KiB unmatched suffix took {elapsed:?}"
        );
        assert!(gate.take_links().is_some());
    }

    #[test]
    #[ignore = "manual PTY callback scaling profile"]
    fn short_url_pty_callback_scale_profile() {
        const CALLBACKS_PER_PANE: usize = 25_000;
        const CHUNK: &[u8] = b"https://short.example/x\n";

        for pane_count in [1_usize, 15] {
            let gates = (0..pane_count)
                .map(|_| ParsedLinkHarness::default())
                .collect::<Vec<_>>();
            let started = std::time::Instant::now();
            for _ in 0..CALLBACKS_PER_PANE {
                for gate in &gates {
                    gate.observe_chunk(std::hint::black_box(CHUNK));
                }
            }
            let elapsed = started.elapsed();
            let callbacks = CALLBACKS_PER_PANE * pane_count;
            let nanos_per_callback = elapsed.as_nanos() as f64 / callbacks as f64;
            let mebibytes_per_second =
                callbacks as f64 * CHUNK.len() as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
            let (parsed_events, parsed_bytes) = gates
                .iter()
                .map(|gate| gate.parsed_event_measurement())
                .fold((0, 0), |(events, bytes), (next_events, next_bytes)| {
                    (events + next_events, bytes + next_bytes)
                });
            eprintln!(
                "short-url parser callback: panes={pane_count} writes={callbacks} parsed_events={parsed_events} parsed_bytes={parsed_bytes} ns/write={nanos_per_callback:.1} MiB/s={mebibytes_per_second:.1}"
            );
            assert_eq!(parsed_events, (callbacks * 2) as u64);
            assert_eq!(parsed_bytes, (callbacks * (CHUNK.len() - 1)) as u64);
            assert!(gates.into_iter().all(|gate| {
                gate.take_links()
                    .is_some_and(|links| links.output_urls == ["https://short.example/x"])
            }));
        }
    }

    #[test]
    fn generated_terminal_interleavings_match_visibility_contract() {
        for seed in 0..32 {
            let stream = generated_terminal_stream(seed);
            let mut output_urls = vec![
                format!("https://visible-{seed}.example/path"),
                format!("https://after-can-{seed}.example/path"),
                format!("https://after-sub-{seed}.example/path"),
                format!("https://after-esc-{seed}.example/path"),
            ];
            output_urls.sort_unstable();
            let expected = CollectedLinks {
                output_urls,
                osc8_urls: vec![format!("https://osc-{seed}.example/target")],
            };

            let one_piece = ParsedLinkHarness::default();
            one_piece.observe_chunk(&stream);
            let one_piece = collected_links(one_piece.take_links().into_iter().collect());
            assert_eq!(one_piece, expected, "one-piece input, seed {seed}");

            let chunked = scan_with_generated_splits(&stream, seed + 1);
            assert_eq!(chunked, one_piece, "chunked detection cycles, seed {seed}");
            assert_eq!(chunked, expected, "visibility contract, seed {seed}");
        }
    }

    #[test]
    fn incomplete_osc8_target_resumes_when_terminator_arrives_without_colon() {
        let gate = ParsedLinkHarness::default();
        gate.observe_chunk(b"\x1b]8;;https://osc.example.test/path");
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\x07label\x1b]8;;\x07\n");
        let links = gate.take_links().expect("completed OSC 8 sequence");
        assert!(links.output_urls.is_empty());
        assert_eq!(links.osc8_urls, vec!["https://osc.example.test/path"]);
    }

    #[test]
    fn pending_buffer_cap_extracts_then_keeps_scanning_later_output() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://before-cap.example.test/path\n");
        gate.observe_chunk(&vec![b'x'; LARGE_TEST_OUTPUT_BYTES]);
        gate.observe_chunk(b"\nhttps://after-cap.example.test/path\n");

        let links = gate.take_links().expect("bounded link extraction");
        assert_eq!(
            links.output_urls,
            vec![
                "https://after-cap.example.test/path",
                "https://before-cap.example.test/path",
            ]
        );
    }

    #[test]
    fn pending_buffer_cap_keeps_unterminated_url_from_its_scheme() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-cap.example.test/path\n";
        gate.observe_chunk(first);

        let mut open_url = b"https://at-cap.example.test/".to_vec();
        open_url.resize(1_024, b'a');
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - open_url.len();
        let mut filler = vec![b'x'; filler_len];
        *filler.last_mut().expect("non-empty filler") = b'\n';
        gate.observe_chunk(&filler);
        gate.observe_chunk(&open_url);
        gate.observe_chunk(b"/path\n");

        let mut completed_url = String::from_utf8(open_url).unwrap();
        completed_url.push_str("/path");
        let links = gate.take_links().expect("bounded link extraction");
        assert_eq!(
            links.output_urls,
            vec![
                completed_url,
                "https://before-cap.example.test/path".to_string(),
            ]
        );
    }

    #[test]
    fn ingest_bounds_staging_and_drops_a_cut_url_whole() {
        let gate = LinkExtractionGate::default();
        gate.observe_chunk(b"https://oversized.example/");
        gate.observe_chunk(&vec![b'a'; LARGE_TEST_OUTPUT_BYTES]);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
        assert!(gate.take_links().is_none());

        gate.observe_chunk(b"\nhttps://after-overflow.example/path\n");
        assert_eq!(
            gate.take_links().expect("post-overflow URL").output_urls,
            vec!["https://after-overflow.example/path"]
        );
    }

    #[test]
    fn ingest_extracts_url_wholly_contained_in_evicted_middle() {
        let gate = LinkExtractionGate::default();
        let evicted_url = b"https://evicted.example/path";
        let evicted_at = OLD_STAGING_BOUNDARY_BYTES + 128;
        let mut chunk = b"https://before-overflow.example/path\n".to_vec();
        chunk.resize(evicted_at, b'x');
        chunk.push(b' ');
        chunk.extend_from_slice(evicted_url);
        chunk.push(b'\n');
        chunk.resize(LARGE_TEST_OUTPUT_BYTES + 4_096, b'y');

        gate.observe_chunk(&chunk);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
        assert_eq!(
            gate.take_links()
                .expect("links spanning staging windows")
                .output_urls,
            vec![
                "https://before-overflow.example/path",
                "https://evicted.example/path",
            ]
        );
    }

    #[test]
    fn ingest_extracts_evicted_url_split_across_observe_chunks() {
        let gate = LinkExtractionGate::default();
        let evicted_url = b"https://evicted-in-pieces.example/path";
        let mut first = b"https://before-piecewise-overflow.example/path\n".to_vec();
        first.resize(OLD_STAGING_BOUNDARY_BYTES + 40, b'x');
        first.push(b'\n');
        first.extend_from_slice(evicted_url);
        first.push(b'\n');
        first.resize(LARGE_TEST_OUTPUT_BYTES, b'y');

        gate.observe_chunk(&first);
        gate.observe_chunk(&[b'z'; 64]);
        gate.observe_chunk(&[b'z'; 64]);

        assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
        assert_eq!(
            gate.take_links()
                .expect("piecewise-evicted link")
                .output_urls,
            vec![
                "https://before-piecewise-overflow.example/path",
                "https://evicted-in-pieces.example/path",
            ]
        );
    }

    #[test]
    fn complete_urls_at_old_staging_boundaries_survive_large_prefix_and_suffix() {
        for boundary in (1..=4).map(|multiple| OLD_STAGING_BOUNDARY_BYTES * multiple) {
            let gate = LinkExtractionGate::default();
            let url = format!("https://boundary-{boundary}.example/path");
            let mut chunk = vec![b'x'; boundary];
            *chunk.last_mut().expect("non-empty prefix") = b'\n';
            chunk.extend_from_slice(url.as_bytes());
            chunk.push(b'\n');
            chunk.resize(chunk.len() + LARGE_TEST_OUTPUT_BYTES, b'y');

            gate.observe_chunk(&chunk);

            assert!(
                gate.retained_byte_count()
                    <= MAX_URL_BYTES + MAX_SCHEME_BYTES + MAX_OPEN_SEQUENCE_BYTES,
                "old staging boundary {boundary} retained a multi-MiB window"
            );
            assert_eq!(
                gate.take_links()
                    .unwrap_or_else(|| panic!("URL at old staging boundary {boundary}"))
                    .output_urls,
                vec![url],
                "old staging boundary {boundary}"
            );
        }
    }

    #[test]
    fn unterminated_url_survives_many_detection_cycles_until_terminated() {
        let gate = ParsedLinkHarness::default();
        gate.observe_chunk(b"https://many-cycles.example/");

        let styling = b"\x1b[31m".repeat(LARGE_TEST_OUTPUT_BYTES / 5 + 1);
        let mut max_retained = 0;

        for _ in 0..3 {
            gate.observe_chunk(&styling);
            max_retained = max_retained.max(gate.retained_byte_count());
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"a");
        }

        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\n");
        assert_eq!(
            gate.take_links().expect("terminated URL").output_urls,
            vec![format!("https://many-cycles.example/{}", "a".repeat(3))]
        );
        assert!(
            max_retained <= MAX_URL_BYTES + MAX_SCHEME_BYTES + MAX_OPEN_SEQUENCE_BYTES,
            "a detection cycle retained the full styling burst"
        );
    }

    #[test]
    fn independent_chunk_streams_scan_each_received_byte_once() {
        const PANES: usize = 24;
        const CHUNKS: usize = 128;
        let gates = (0..PANES)
            .map(|_| LinkExtractionGate::default())
            .collect::<Vec<_>>();

        let prefix = b"\x1b]8;;https://linear.example/";
        for gate in &gates {
            gate.observe_chunk(prefix);
            assert!(gate.take_links().is_none());
        }
        for _ in 0..CHUNKS {
            for gate in &gates {
                gate.observe_chunk(b"a");
                assert!(gate.take_links().is_none());
            }
        }

        let received = (prefix.len() + CHUNKS) * PANES;
        let scanned = gates
            .iter()
            .map(|gate| gate.scanned_bytes.load(Ordering::Relaxed) as usize)
            .sum::<usize>();
        assert!(
            scanned <= received,
            "already-scanned bytes were revisited: received {received}, scanned {scanned}"
        );
    }

    #[test]
    fn overflow_suffix_resumes_after_open_osc_terminator() {
        for split_st in [false, true] {
            let gate = ParsedLinkHarness::default();
            let first = b"https://before-overflow.example/path\n";
            gate.observe_chunk(first);

            let osc = b"\x1b]0;title";
            let mut overflow = vec![b'x'; OLD_STAGING_BOUNDARY_BYTES - first.len() - osc.len()];
            overflow.extend_from_slice(osc);
            overflow.resize(LARGE_TEST_OUTPUT_BYTES + 256, b'y');
            overflow.extend_from_slice(b" hidden https://hidden.example/path ");
            if split_st {
                overflow.push(b'\x1b');
                gate.observe_chunk(&overflow);
                gate.observe_chunk(b"\\https://visible.example/path\n");
            } else {
                overflow.extend_from_slice(b"\x07https://visible.example/path\n");
                gate.observe_chunk(&overflow);
            }

            assert!(gate.retained_byte_count() <= MAX_URL_BYTES);
            let links = gate.take_links().expect("visible URL after open OSC");
            assert_eq!(
                links.output_urls,
                vec![
                    "https://before-overflow.example/path",
                    "https://visible.example/path",
                ],
                "split ST: {split_st}"
            );
        }
    }

    #[test]
    fn unfinished_url_scans_each_byte_only_a_constant_number_of_times() {
        let gate = LinkExtractionGate::default();
        let prefix = b"https://incremental.example/";
        gate.observe_chunk(prefix);
        assert!(gate.take_links().is_none());

        for _ in 0..128 {
            gate.observe_chunk(b"a");
            assert!(gate.take_links().is_none());
        }

        let bytes_before_terminator = prefix.len() + 128;
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) as usize <= bytes_before_terminator,
            "unfinished URL was rescanned from its first byte"
        );
        let scanned_before_terminator = gate.scanned_bytes.load(Ordering::Relaxed);

        gate.observe_chunk(b"\n");
        let links = gate.take_links().expect("terminated incremental URL");
        assert_eq!(links.output_urls.len(), 1);
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) - scanned_before_terminator <= 1,
            "terminator reparsed the retained URL"
        );
    }

    #[test]
    fn styled_unfinished_url_does_not_restart_after_each_split_escape() {
        let gate = ParsedLinkHarness::default();
        let prefix = b"https://styled-incremental.example/";
        gate.observe_chunk(prefix);
        assert!(gate.take_links().is_none());

        for _ in 0..64 {
            gate.observe_chunk(b"\x1b[");
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"31m");
            assert!(gate.take_links().is_none());
            gate.observe_chunk(b"a");
            assert!(gate.take_links().is_none());
        }

        let received_bytes = prefix.len() + 64 * 6;
        assert!(
            gate.scanned_bytes.load(Ordering::Relaxed) as usize <= received_bytes * 2,
            "styled URL was reparsed from its first byte after split escapes"
        );

        gate.observe_chunk(b"\n");
        let links = gate.take_links().expect("terminated styled URL");
        assert_eq!(
            links.output_urls,
            vec![format!(
                "https://styled-incremental.example/{}",
                "a".repeat(64)
            )]
        );
    }

    #[test]
    fn split_scheme_continuation_waits_for_marker_finalization() {
        let gate = LinkExtractionGate::default();
        let arrived = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        gate.set_observe_race_hook(Arc::clone(&arrived), Arc::clone(&resume));

        std::thread::scope(|scope| {
            let processing = gate.processing.lock().expect("processing lock");
            let observer = scope.spawn(|| gate.observe_chunk(b"//db.example/path\n"));
            arrived.wait();
            gate.marker_tail.store(b"postgresql:");
            resume.wait();
            drop(processing);
            observer.join().expect("observer thread");
        });

        gate.observe_race_hook.lock().expect("hook lock").take();
        let links = gate.take_links().expect("split URL after finalization");
        assert_eq!(links.output_urls, vec!["postgresql://db.example/path"]);
    }

    #[test]
    fn overlong_csi_recovers_at_csi_terminator_and_keeps_later_url() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-csi.example.test/path\n";
        gate.observe_chunk(first);

        let csi_len = MAX_OPEN_SEQUENCE_BYTES + 1_024;
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - csi_len;
        let mut chunk = vec![b'x'; filler_len];
        *chunk.last_mut().expect("non-empty filler") = b'\n';
        chunk.extend_from_slice(b"\x1b[");
        chunk.resize(chunk.len() + csi_len - 2, b'1');
        gate.observe_chunk(&chunk);
        gate.observe_chunk(b"1");
        assert_eq!(
            gate.take_links()
                .expect("completed link before open CSI")
                .output_urls,
            vec!["https://before-csi.example.test/path"]
        );
        gate.observe_chunk(b"m\nhttps://after-csi.example.test/path\n");

        let links = gate.take_links().expect("link extraction after long CSI");
        assert_eq!(
            links.output_urls,
            vec!["https://after-csi.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn overlong_osc8_target_is_dropped_before_later_plain_url() {
        let gate = LinkExtractionGate::default();
        let first = b"https://before-osc.example.test/path\n";
        gate.observe_chunk(first);

        let osc_len = MAX_OPEN_SEQUENCE_BYTES + 1_024;
        let filler_len = LARGE_TEST_OUTPUT_BYTES - first.len() - osc_len;
        let mut chunk = vec![b'x'; filler_len];
        *chunk.last_mut().expect("non-empty filler") = b'\n';
        let mut control = b"\x1b]8;;https://too-long.example.test/".to_vec();
        control.resize(osc_len, b'a');
        chunk.extend_from_slice(&control);
        gate.observe_chunk(&chunk);
        gate.observe_chunk(b"\x1b");
        assert_eq!(
            gate.take_links()
                .expect("completed link before open OSC")
                .output_urls,
            vec!["https://before-osc.example.test/path"]
        );
        gate.observe_chunk(b"\\\nhttps://after-osc.example.test/path\n");

        let links = gate.take_links().expect("link extraction after long OSC 8");
        assert_eq!(
            links.output_urls,
            vec!["https://after-osc.example.test/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn overlong_osc_discard_preserves_trailing_escape_for_split_st() {
        let gate = LinkExtractionGate::default();
        let mut control = b"\x1b]8;;https://too-long.example.test/".to_vec();
        control.resize(5 + MAX_URL_BYTES + 1, b'a');
        control.push(b'\x1b');
        gate.observe_chunk(&control);

        assert!(gate.take_links().is_none());
        gate.observe_chunk(b"\\\nhttps://after.example/path\n");

        let links = gate.take_links().expect("link extraction after split ST");
        assert_eq!(links.output_urls, vec!["https://after.example/path"]);
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn pending_rollover_preserves_trailing_escape_for_split_st() {
        let gate = LinkExtractionGate::default();
        let mut chunk = b"\x1b]8;;https://too-long.example.test/".to_vec();
        chunk.resize(LARGE_TEST_OUTPUT_BYTES, b'a');
        *chunk.last_mut().expect("non-empty pending buffer") = b'\x1b';
        chunk.extend_from_slice(b"\\\nhttps://after-rollover.example/path\n");

        gate.observe_chunk(&chunk);

        let links = gate
            .take_links()
            .expect("link extraction after pending rollover");
        assert_eq!(
            links.output_urls,
            vec!["https://after-rollover.example/path"]
        );
        assert!(links.osc8_urls.is_empty());
    }

    #[test]
    fn links_dedupe_update_last_seen_and_drop_oldest_over_limit() {
        let pane_id = PaneId::from_raw(9);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base,
        );
        store.observe_links(
            pane_id,
            ["https://example.test/first".into()],
            AgentLinkSource::Output,
            base + std::time::Duration::from_secs(1),
        );
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_ne!(state.links[0].first_seen, state.links[0].last_seen);

        for index in 0..=MAX_LINKS {
            store.observe_links(
                pane_id,
                [format!("https://links.test/{index}")],
                AgentLinkSource::Osc8,
                base + std::time::Duration::from_secs(index as u64 + 2),
            );
        }
        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), MAX_LINKS);
        assert!(state
            .links
            .iter()
            .all(|link| link.url != "https://example.test/first"));
        assert!(state.links.iter().any(|link| {
            link.url == format!("https://links.test/{MAX_LINKS}")
                && link.source == AgentLinkSource::Osc8
        }));
    }

    #[test]
    fn transcript_link_preserves_its_row_times_across_later_rows() {
        let pane_id = PaneId::from_raw(12);
        let first = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let last = first + std::time::Duration::from_secs(3);
        let mut store = AgentStateStore::default();
        let link = ObservedAgentLink {
            url: "https://transcript.test/task".into(),
            first_seen: first,
            last_seen: last,
        };
        store.observe_transcript(
            pane_id,
            Some(last),
            None,
            None,
            Vec::new(),
            vec![link.clone()],
        );
        store.observe_transcript(
            pane_id,
            Some(last + std::time::Duration::from_secs(30)),
            None,
            None,
            Vec::new(),
            vec![link],
        );

        let state = store.snapshot(pane_id, AgentStatus::Idle);
        assert_eq!(state.links.len(), 1);
        assert_eq!(state.links[0].source, AgentLinkSource::Transcript);
        assert_eq!(state.links[0].first_seen, format_rfc3339(first).unwrap());
        assert_eq!(state.links[0].last_seen, format_rfc3339(last).unwrap());
    }

    #[test]
    fn url_extraction_trims_terminal_punctuation() {
        assert_eq!(
            extract_urls("open (https://example.test/path), then git://host/repo."),
            vec!["https://example.test/path", "git://host/repo"]
        );
    }

    #[test]
    fn url_extraction_preserves_balanced_trailing_delimiters() {
        assert_eq!(
            extract_urls(concat!(
                "https://en.wikipedia.org/wiki/Function_(mathematics) ",
                "https://example.test/unbalanced), ",
                "https://example.test/list[item] ",
                "https://example.test/trailing]."
            )),
            vec![
                "https://en.wikipedia.org/wiki/Function_(mathematics)",
                "https://example.test/unbalanced",
                "https://example.test/list[item]",
                "https://example.test/trailing",
            ]
        );
    }

    #[test]
    fn transcript_url_extraction_applies_url_and_scheme_bounds() {
        let valid = format!(
            "https://transcript.example/{}",
            "a".repeat(MAX_URL_BYTES - 27)
        );
        let too_long = format!("https://transcript.example/{}", "b".repeat(MAX_URL_BYTES));
        let long_scheme = format!("{}://transcript.example/path", "s".repeat(65));

        assert_eq!(
            extract_urls(&format!("{valid}\n{too_long}\n{long_scheme}")),
            vec![valid]
        );
    }

    #[test]
    fn newer_reported_tasks_override_older_transcript_tasks() {
        let pane_id = PaneId::from_raw(13);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let transcript_task = AgentTask {
            text: "old transcript task".into(),
            status: AgentTaskStatus::Pending,
        };
        let reported_task = AgentTask {
            text: "new reported task".into(),
            status: AgentTaskStatus::InProgress,
        };
        let mut store = AgentStateStore::default();
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(2)),
            Some(vec![transcript_task]),
            Some(base),
            Vec::new(),
            Vec::new(),
        );
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task.clone()]),
                    ..AgentReportPayload::default()
                },
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid report");

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![reported_task]
        );
    }

    #[test]
    fn newer_transcript_tasks_override_reported_tasks() {
        let pane_id = PaneId::from_raw(14);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let reported_task = AgentTask {
            text: "old reported task".into(),
            status: AgentTaskStatus::Pending,
        };
        let transcript_task = AgentTask {
            text: "new transcript task".into(),
            status: AgentTaskStatus::Completed,
        };
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    tasks: Some(vec![reported_task]),
                    ..AgentReportPayload::default()
                },
                base,
            )
            .expect("valid report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(1)),
            Some(vec![transcript_task.clone()]),
            Some(base + std::time::Duration::from_secs(1)),
            Vec::new(),
            Vec::new(),
        );

        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Working).tasks,
            vec![transcript_task]
        );
    }

    #[test]
    fn partial_report_keeps_fields_it_does_not_mention() {
        let mut store = AgentStateStore::default();
        let pane = PaneId::from_raw(91);
        let now = SystemTime::now();
        store
            .report(
                pane,
                serde_json::from_str::<AgentReportPayload>(
                    r#"{"goal":"ship MAT-160","subagents":[{"name":"reviewer","status":"working"}]}"#,
                )
                .expect("first payload"),
                now,
            )
            .expect("valid report");
        store
            .report(
                pane,
                serde_json::from_str::<AgentReportPayload>(r#"{"status_text":"running tests"}"#)
                    .expect("second payload"),
                now,
            )
            .expect("valid report");

        let snapshot = store.snapshot(pane, AgentStatus::Working);
        assert_eq!(snapshot.goal.as_deref(), Some("ship MAT-160"));
        assert_eq!(snapshot.status_text.as_deref(), Some("running tests"));
        assert_eq!(
            snapshot.subagents.len(),
            1,
            "subagents survive a partial report"
        );

        store
            .report(
                pane,
                serde_json::from_str::<AgentReportPayload>(r#"{"goal":null,"subagents":[]}"#)
                    .expect("clearing payload"),
                now,
            )
            .expect("valid report");
        let snapshot = store.snapshot(pane, AgentStatus::Working);
        assert_eq!(snapshot.goal, None, "explicit null clears the goal");
        assert!(
            snapshot.subagents.is_empty(),
            "explicit [] clears subagents"
        );
    }

    #[test]
    fn explicit_empty_report_and_newer_empty_transcript_clear_tasks() {
        let pane_id = PaneId::from_raw(15);
        let base = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                serde_json::from_str(
                    r#"{"tasks":[{"text":"reported task","status":"in_progress"}]}"#,
                )
                .expect("report payload"),
                base,
            )
            .expect("valid report");
        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[]}"#).expect("empty report payload"),
                base + std::time::Duration::from_secs(1),
            )
            .expect("valid empty report");
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());

        store
            .report(
                pane_id,
                serde_json::from_str(r#"{"tasks":[{"text":"new report","status":"pending"}]}"#)
                    .expect("replacement report payload"),
                base + std::time::Duration::from_secs(2),
            )
            .expect("valid replacement report");
        store.observe_transcript(
            pane_id,
            Some(base + std::time::Duration::from_secs(3)),
            Some(Vec::new()),
            Some(base + std::time::Duration::from_secs(3)),
            Vec::new(),
            Vec::new(),
        );
        assert!(store
            .snapshot(pane_id, AgentStatus::Working)
            .tasks
            .is_empty());
    }

    #[test]
    fn observed_and_reported_subagents_are_both_projected() {
        let pane_id = PaneId::from_raw(10);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    subagents: Some(vec![AgentSubagent {
                        name: "reported reviewer".into(),
                        status: AgentStatus::Blocked,
                        last_active_at: None,
                        pane_id: Some("w1:p2".into()),
                        source: AgentSubagentSource::Observed,
                    }]),
                    ..AgentReportPayload::default()
                },
                now,
            )
            .unwrap();
        store.observe_transcript(
            pane_id,
            Some(now),
            None,
            None,
            vec![AgentSubagent {
                name: "native worker".into(),
                status: AgentStatus::Working,
                last_active_at: format_rfc3339(now),
                pane_id: None,
                source: AgentSubagentSource::Observed,
            }],
            Vec::new(),
        );

        let subagents = store.snapshot(pane_id, AgentStatus::Working).subagents;
        assert_eq!(subagents.len(), 2);
        assert_eq!(subagents[0].source, AgentSubagentSource::Observed);
        assert_eq!(subagents[1].source, AgentSubagentSource::Reported);
    }

    #[test]
    fn invalid_report_is_rejected_without_changing_state() {
        let pane_id = PaneId::from_raw(11);
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_750_000_000);
        let mut store = AgentStateStore::default();
        store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some(Some("keep me".into())),
                    ..AgentReportPayload::default()
                },
                now,
            )
            .expect("valid initial report");

        let error = store
            .report(
                pane_id,
                AgentReportPayload {
                    goal: Some(Some("replace me".into())),
                    subagents: Some(vec![AgentSubagent {
                        name: "reviewer".into(),
                        status: AgentStatus::Working,
                        last_active_at: Some("yesterday".into()),
                        pane_id: None,
                        source: AgentSubagentSource::Reported,
                    }]),
                    ..AgentReportPayload::default()
                },
                now + std::time::Duration::from_secs(1),
            )
            .expect_err("invalid timestamp must reject the entire report");

        assert!(error.contains("RFC3339"));
        assert_eq!(
            store.snapshot(pane_id, AgentStatus::Idle).goal.as_deref(),
            Some("keep me")
        );
    }
}
