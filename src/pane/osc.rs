use std::path::PathBuf;

use crate::api::schema::{
    ProgramStatusKind, ProgramStatusRecord, ProgramStatusSnapshot, ProgramStatusSource,
    ProgramStatusState,
};
use base64::Engine;
use tracing::info;

use crate::layout::PaneId;

use super::terminal::GhosttyPaneCore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorQuery {
    Foreground,
    Background,
    Cursor,
}

impl DefaultColorQuery {
    pub(super) fn osc_number(self) -> u8 {
        match self {
            Self::Foreground => 10,
            Self::Background => 11,
            Self::Cursor => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DefaultColorEvent {
    Query(DefaultColorQuery),
    Set(DefaultColorQuery),
    Reset(DefaultColorQuery),
    PaletteQuery(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DefaultColorTrackedEvent {
    pub(super) end_offset: usize,
    pub(super) event: DefaultColorEvent,
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorOscTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DefaultColorOscTrackerState {
    #[default]
    Ground,
    Escape,
    OscBody,
    OscEscape,
    IgnoreString,
    IgnoreStringEscape,
    OversizedOsc,
    OversizedOscEscape,
}

fn is_ignored_string_intro(byte: u8) -> bool {
    matches!(byte, b'P' | b'_' | b'^' | b'X')
}

impl DefaultColorOscTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let mut saw_default_color_set = false;
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                DefaultColorOscTrackerState::Ground | DefaultColorOscTrackerState::IgnoreString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        saw_default_color_set |= is_default_color_set_osc(&self.body);
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }

        saw_default_color_set
    }
}

fn is_default_color_set_osc(body: &[u8]) -> bool {
    parse_default_color_events(body)
        .iter()
        .any(|event| matches!(event, DefaultColorEvent::Set(_)))
}

#[derive(Debug, Default)]
pub(super) struct DefaultColorEventTracker {
    state: DefaultColorOscTrackerState,
    body: Vec<u8>,
    pending: Vec<DefaultColorTrackedEvent>,
}

impl DefaultColorEventTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                DefaultColorOscTrackerState::Ground | DefaultColorOscTrackerState::IgnoreString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let index = cursor;
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                DefaultColorOscTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    }
                }
                DefaultColorOscTrackerState::Escape => {
                    if byte == b']' {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::OscBody;
                    } else if is_ignored_string_intro(byte) {
                        self.body.clear();
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    } else if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::Escape;
                    } else {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OscBody => match byte {
                    0x07 => {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                    0x1b => self.state = DefaultColorOscTrackerState::OscEscape,
                    _ => self.body.push(byte),
                },
                DefaultColorOscTrackerState::OscEscape => {
                    if byte == b'\\' {
                        self.finalize(index + 1);
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else {
                        self.body.push(0x1b);
                        self.body.push(byte);
                        self.state = DefaultColorOscTrackerState::OscBody;
                    }
                }
                DefaultColorOscTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreStringEscape;
                    }
                }
                DefaultColorOscTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::IgnoreString;
                    }
                }
                DefaultColorOscTrackerState::OversizedOsc => {
                    if byte == 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOscEscape;
                    } else if byte == 0x07 {
                        self.state = DefaultColorOscTrackerState::Ground;
                    }
                }
                DefaultColorOscTrackerState::OversizedOscEscape => {
                    if byte == b'\\' {
                        self.state = DefaultColorOscTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = DefaultColorOscTrackerState::OversizedOsc;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = DefaultColorOscTrackerState::OversizedOsc;
            }
        }
    }

    fn finalize(&mut self, end_offset: usize) {
        self.pending.extend(
            parse_default_color_events(&self.body)
                .into_iter()
                .map(|event| DefaultColorTrackedEvent { end_offset, event }),
        );
        self.body.clear();
    }

    pub(super) fn in_progress_event(&self) -> Option<DefaultColorEvent> {
        if !matches!(
            self.state,
            DefaultColorOscTrackerState::OscBody | DefaultColorOscTrackerState::OscEscape
        ) {
            return None;
        }
        let mut events = parse_default_color_events(&self.body);
        (events.len() == 1).then(|| events.remove(0))
    }

    pub(super) fn drain_pending(&mut self) -> Vec<DefaultColorTrackedEvent> {
        std::mem::take(&mut self.pending)
    }
}

fn parse_default_color_events(body: &[u8]) -> Vec<DefaultColorEvent> {
    let single = match body {
        b"10;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Foreground)),
        b"11;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Background)),
        b"12;?" => Some(DefaultColorEvent::Query(DefaultColorQuery::Cursor)),
        b"110" | b"110;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Foreground)),
        b"111" | b"111;" => Some(DefaultColorEvent::Reset(DefaultColorQuery::Background)),
        _ => parse_palette_color_query(body),
    };
    if let Some(event) = single {
        return vec![event];
    }
    parse_default_color_set_events(body)
}

fn parse_palette_color_query(body: &[u8]) -> Option<DefaultColorEvent> {
    let index = body.strip_prefix(b"4;")?.strip_suffix(b";?")?;
    if index.is_empty() || index.len() > 3 || !index.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut value: u16 = 0;
    for &digit in index {
        value = value * 10 + u16::from(digit - b'0');
    }
    u8::try_from(value)
        .ok()
        .map(DefaultColorEvent::PaletteQuery)
}

fn parse_default_color_set_events(body: &[u8]) -> Vec<DefaultColorEvent> {
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return Vec::new();
    };
    let start = match &body[..separator] {
        b"10" => 10,
        b"11" => 11,
        b"12" => 12,
        _ => return Vec::new(),
    };
    body[separator + 1..]
        .split(|byte| *byte == b';')
        .filter(|value| !value.is_empty())
        .enumerate()
        .filter_map(|(offset, value)| {
            if value == b"?" {
                return None;
            }
            let query = match start + offset {
                10 => DefaultColorQuery::Foreground,
                11 => DefaultColorQuery::Background,
                12 => DefaultColorQuery::Cursor,
                _ => return None,
            };
            Some(DefaultColorEvent::Set(query))
        })
        .collect()
}

pub(super) fn parse_reported_cwd(value: &[u8]) -> Option<PathBuf> {
    let value = std::str::from_utf8(value).ok()?.trim();
    if value.starts_with("file://") {
        return parse_file_uri_cwd(value);
    }
    let path = value.trim_matches('"');
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Collects complete OSC bodies from a raw byte stream. Consumers receive only
/// bodies, keeping the framing state machine independent from OSC commands.
#[derive(Debug, Default)]
struct OscStreamCollector {
    state: OscStreamState,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum OscStreamState {
    #[default]
    Ground,
    Escape,
    Body,
    BodyEscape,
    IgnoringString,
    IgnoringStringEscape,
    Discarding,
    DiscardingEscape,
}

impl OscStreamCollector {
    const MAX_BODY_BYTES: usize = 4096;

    fn observe(&mut self, bytes: &[u8], mut receive: impl FnMut(&[u8])) {
        self.observe_events(bytes, |body, sequence_bytes| {
            if sequence_bytes > 0 {
                if let Some(body) = body {
                    receive(body);
                }
            }
        });
    }

    fn observe_events(&mut self, bytes: &[u8], mut receive: impl FnMut(Option<&[u8]>, usize)) {
        let mut cursor = 0;
        while cursor < bytes.len() {
            if matches!(
                self.state,
                OscStreamState::Ground | OscStreamState::IgnoringString
            ) {
                let Some(offset) = bytes[cursor..].iter().position(|&byte| byte == 0x1b) else {
                    break;
                };
                cursor += offset;
            }
            let byte = bytes[cursor];
            cursor += 1;
            match self.state {
                OscStreamState::Ground => {
                    if byte == 0x1b {
                        self.state = OscStreamState::Escape;
                    }
                }
                OscStreamState::Escape => match byte {
                    b']' => {
                        self.body.clear();
                        self.state = OscStreamState::Body;
                    }
                    0x1b => self.state = OscStreamState::Escape,
                    b'c' => {
                        receive(None, 2);
                        self.state = OscStreamState::Ground;
                    }
                    byte if is_ignored_string_intro(byte) => {
                        self.state = OscStreamState::IgnoringString;
                    }
                    _ => self.state = OscStreamState::Ground,
                },
                OscStreamState::Body => match byte {
                    0x07 => self.finish(&mut receive, 3),
                    0x1b => self.state = OscStreamState::BodyEscape,
                    _ => self.push(byte),
                },
                OscStreamState::BodyEscape => match byte {
                    b'\\' => self.finish(&mut receive, 4),
                    0x07 => {
                        self.push(0x1b);
                        if matches!(self.state, OscStreamState::Body) {
                            self.finish(&mut receive, 3);
                        } else {
                            receive(Some(&[]), 0);
                            self.state = OscStreamState::Ground;
                        }
                    }
                    0x1b => {
                        self.push(0x1b);
                        self.state = match self.state {
                            OscStreamState::Body => OscStreamState::BodyEscape,
                            OscStreamState::Discarding => OscStreamState::DiscardingEscape,
                            state => state,
                        };
                    }
                    _ => {
                        self.push(0x1b);
                        if matches!(self.state, OscStreamState::Body) {
                            self.push(byte);
                        }
                    }
                },
                OscStreamState::IgnoringString => {
                    if byte == 0x1b {
                        self.state = OscStreamState::IgnoringStringEscape;
                    }
                }
                OscStreamState::IgnoringStringEscape => {
                    if byte == b'\\' {
                        receive(Some(&[]), 0);
                        self.state = OscStreamState::Ground;
                    } else if byte != 0x1b {
                        self.state = OscStreamState::IgnoringString;
                    }
                }
                OscStreamState::Discarding => {
                    if byte == 0x07 {
                        receive(Some(&[]), 0);
                        self.state = OscStreamState::Ground;
                    } else if byte == 0x1b {
                        self.state = OscStreamState::DiscardingEscape;
                    }
                }
                OscStreamState::DiscardingEscape => {
                    if byte == b'\\' {
                        receive(Some(&[]), 0);
                        self.state = OscStreamState::Ground;
                    } else if byte != 0x1b {
                        self.state = OscStreamState::Discarding;
                    }
                }
            }
        }
    }

    fn push(&mut self, byte: u8) {
        self.body.push(byte);
        if self.body.len() > Self::MAX_BODY_BYTES {
            self.body.clear();
            self.state = OscStreamState::Discarding;
        } else {
            self.state = OscStreamState::Body;
        }
    }

    fn finish(&mut self, receive: &mut impl FnMut(Option<&[u8]>, usize), framing: usize) {
        receive(Some(&self.body), self.body.len() + framing);
        self.body.clear();
        self.state = OscStreamState::Ground;
    }
}

/// Maximum retained string length for agent OSC title and progress payloads.
/// Title text is untrusted model output; cap it to bound memory and log size.
const AGENT_OSC_MAX_CHARS: usize = 256;

/// Always-on OSC observer. Title and progress remain detection evidence.
/// Root program status is a separate read-only API fact, with a fixed query reply.
///
/// - `latest_title` — last OSC 0 or OSC 2 payload, sanitized. An empty
///   payload (e.g. `\x1b]0;\x07`) clears the stored value.
/// - `latest_progress` — last OSC 9 payload (the part after `9;`), stored
///   as-is after sanitization. E.g. `"4;3;"` or `"4;0;"`.
#[derive(Debug, Default)]
pub(super) struct AgentOscStateTracker {
    collector: OscStreamCollector,
    latest_title: Option<String>,
    terminal_title: Option<String>,
    latest_progress: Option<String>,
    program_status: Option<ProgramStatusSnapshot>,
    query_replies: usize,
    skip_in_flight_program_status: bool,
}

impl AgentOscStateTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> bool {
        let Self {
            collector,
            latest_title,
            terminal_title,
            latest_progress,
            program_status,
            query_replies,
            skip_in_flight_program_status,
        } = self;
        let mut terminal_title_changed = false;
        collector.observe_events(bytes, |body, sequence_bytes| {
            let Some(body) = body else {
                replace_program_status(program_status, None, true);
                return;
            };
            let skip_status = std::mem::take(skip_in_flight_program_status);
            let Some((command, payload)) = parse_agent_osc_body(body) else {
                return;
            };
            if skip_status && command == b"7501" {
                return;
            }
            match command {
                b"0" | b"2" => {
                    let title = sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS);
                    let title = (!title.is_empty()).then_some(title);
                    terminal_title_changed |= *terminal_title != title;
                    *terminal_title = title.clone();
                    *latest_title = title;
                }
                b"9" => {
                    *latest_progress =
                        Some(sanitize_agent_osc_string(payload, AGENT_OSC_MAX_CHARS));
                }
                b"7501" if sequence_bytes <= 4096 => {
                    if payload == b"?" {
                        *query_replies = query_replies.saturating_add(1);
                    } else if let Some(record) = parse_program_status(payload) {
                        replace_program_status(program_status, record, false);
                    }
                }
                b"133" if payload.split(|b| *b == b';').next() == Some(b"A".as_slice()) => {
                    expire_program_status(program_status);
                }
                _ => {}
            }
        });
        terminal_title_changed
    }

    pub(super) fn program_status_revision(&self) -> u64 {
        self.program_status
            .as_ref()
            .map_or(0, |snapshot| snapshot.revision)
    }

    pub(super) fn program_status(&self) -> Option<ProgramStatusSnapshot> {
        self.program_status.clone()
    }

    pub(super) fn take_query_replies(&mut self) -> usize {
        std::mem::take(&mut self.query_replies)
    }

    pub(super) fn expire_program_status(&mut self) {
        expire_program_status(&mut self.program_status);
    }

    pub(super) fn reset_program_status(&mut self) {
        // Keep title/progress framing, but do not attribute a partial old report
        // to the replacement process when its terminator arrives later.
        self.skip_in_flight_program_status = matches!(
            self.collector.state,
            OscStreamState::Escape | OscStreamState::Body | OscStreamState::BodyEscape
        );
        replace_program_status(&mut self.program_status, None, true);
    }

    pub(super) fn terminal_title(&self) -> Option<&str> {
        self.terminal_title.as_deref()
    }

    #[cfg(unix)]
    pub(super) fn seed_terminal_title(&mut self, title: Option<String>) {
        self.terminal_title = title;
    }

    /// Returns the latest retained OSC title, or `""` if none has been seen or
    /// the last title was an empty clear.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_title(&self) -> &str {
        self.latest_title.as_deref().unwrap_or("")
    }

    /// Returns the latest retained OSC 9 progress payload, or `""` if none.
    #[allow(dead_code)] // used by terminal.rs; full call chain wired in Stage C
    pub(super) fn latest_progress(&self) -> &str {
        self.latest_progress.as_deref().unwrap_or("")
    }

    /// Drops the retained title and progress so a new foreground agent cannot
    /// inherit OSC evidence emitted by a previous process. The in-flight parse
    /// state is kept: a sequence spanning the agent change finalizes normally
    /// and is attributed to the new agent.
    pub(super) fn clear_retained(&mut self) {
        self.latest_title = None;
        self.latest_progress = None;
    }
}

fn replace_program_status(
    slot: &mut Option<ProgramStatusSnapshot>,
    record: Option<ProgramStatusRecord>,
    reset: bool,
) {
    if !reset
        && slot
            .as_ref()
            .is_some_and(|current| current.record == record)
    {
        return;
    }
    let revision = slot
        .as_ref()
        .map_or(1, |current| current.revision.saturating_add(1));
    let source_epoch = slot
        .as_ref()
        .map_or(0, |current| current.source_epoch)
        .saturating_add(u64::from(reset));
    let updated_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0);
    *slot = Some(ProgramStatusSnapshot {
        source: ProgramStatusSource::Osc7501,
        source_epoch,
        revision,
        updated_at_ms,
        record,
    });
}

fn expire_program_status(slot: &mut Option<ProgramStatusSnapshot>) {
    if slot
        .as_ref()
        .and_then(|snapshot| snapshot.record.as_ref())
        .is_some_and(|record| {
            matches!(
                record.state,
                ProgramStatusState::Working
                    | ProgramStatusState::Blocked
                    | ProgramStatusState::Idle
            )
        })
    {
        replace_program_status(slot, None, false);
    }
}

/// None ignores the report. Some(None) is a root clear. Named records are not supported.
fn parse_program_status(payload: &[u8]) -> Option<Option<ProgramStatusRecord>> {
    let mut state = None;
    let mut app = None;
    let mut kind = None;
    let mut progress = None;
    for pair in payload.split(|byte| *byte == b':') {
        let Some(separator) = pair.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let key = pair[..separator].trim_ascii();
        let value = pair[separator + 1..].trim_ascii();
        if key == b"id" {
            return None;
        }
        if key.len() > 16 {
            return None;
        }
        // Check caps before parsing a malformed value or decoding text.
        match key {
            b"msg" if value.len() > 2732 => return None,
            b"title" if value.len() > 256 => return None,
            b"app" if value.len() > 32 => return None,
            _ => {}
        }
        let (Ok(key), Ok(value)) = (std::str::from_utf8(key), std::str::from_utf8(value)) else {
            continue;
        };
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_lowercase())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue;
        }
        match key {
            "state" => state = Some(value),
            "app" => app = Some(value),
            "kind" => kind = Some(value),
            "progress" => progress = Some(value),
            "msg" | "title" => {
                let engine = base64::engine::general_purpose::GeneralPurpose::new(
                    &base64::alphabet::STANDARD,
                    base64::engine::general_purpose::GeneralPurposeConfig::new()
                        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
                );
                let decoded = engine.decode(value).ok()?;
                let limit = if key == "msg" { 2048 } else { 192 };
                if decoded.len() > limit {
                    return None;
                }
                let text = std::str::from_utf8(&decoded).ok()?;
                if text.chars().any(char::is_control) {
                    return None;
                }
            }
            _ => {}
        }
    }
    let state = match state? {
        "clear" => return Some(None),
        "idle" => ProgramStatusState::Idle,
        "working" => ProgramStatusState::Working,
        "blocked" => ProgramStatusState::Blocked,
        "done" => ProgramStatusState::Done,
        "error" => ProgramStatusState::Error,
        _ => return None,
    };
    let kind = if state == ProgramStatusState::Blocked {
        match kind {
            Some("permission") => Some(ProgramStatusKind::Permission),
            Some("question") => Some(ProgramStatusKind::Question),
            Some("auth") => Some(ProgramStatusKind::Auth),
            _ => None,
        }
    } else {
        None
    };
    let progress = if matches!(
        state,
        ProgramStatusState::Working | ProgramStatusState::Blocked
    ) {
        progress
            .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|value| *value <= 100)
    } else {
        None
    };
    let app = app
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
        })
        .map(str::to_string);
    Some(Some(ProgramStatusRecord {
        state,
        kind,
        app,
        progress,
    }))
}

/// Splits an OSC body at the first `;`, returning `(command, payload)`.
/// Returns `None` if there is no `;`.
fn parse_agent_osc_body(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let sep = body.iter().position(|&b| b == b';')?;
    Some((&body[..sep], &body[sep + 1..]))
}

fn sanitize_agent_osc_string(payload: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(payload);
    let mut out = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(max_chars) {
        out.push(ch);
    }
    out
}

/// Reconstructs selected OSC sequences for local evidence capture while
/// debugging agent title/status behavior. This is intentionally passive:
/// nothing here affects terminal rendering or detection state.
#[derive(Debug)]
pub(super) struct OscDebugTracker {
    enabled: bool,
    collector: OscStreamCollector,
    pending: Vec<OscDebugEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OscDebugEvent {
    pub(super) command: String,
    pub(super) payload: String,
}

impl OscDebugTracker {
    pub(super) fn from_env() -> Self {
        Self {
            enabled: osc_debug_enabled_from_env(),
            collector: OscStreamCollector::default(),
            pending: Vec::new(),
        }
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        let (collector, pending) = (&mut self.collector, &mut self.pending);
        collector.observe(bytes, |body| {
            if let Some(event) = parse_osc_debug_event(body) {
                pending.push(event);
            }
        });
    }

    pub(super) fn drain_pending(&mut self) -> Vec<OscDebugEvent> {
        std::mem::take(&mut self.pending)
    }
}

impl Default for OscDebugTracker {
    fn default() -> Self {
        Self::from_env()
    }
}

fn osc_debug_enabled_from_env() -> bool {
    std::env::var("HERDR_DEBUG_OSC_EVIDENCE")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parse_osc_debug_event(body: &[u8]) -> Option<OscDebugEvent> {
    let separator = body.iter().position(|byte| *byte == b';')?;
    let command = &body[..separator];
    let payload = &body[separator + 1..];
    if !matches!(command, b"0" | b"2" | b"9" | b"21337") {
        return None;
    }
    Some(OscDebugEvent {
        command: std::str::from_utf8(command).ok()?.to_string(),
        payload: sanitized_osc_debug_payload(payload),
    })
}

fn sanitized_osc_debug_payload(payload: &[u8]) -> String {
    const MAX_CHARS: usize = 512;
    let text = String::from_utf8_lossy(payload);
    let mut sanitized = String::new();
    for ch in text.chars().filter(|ch| !ch.is_control()).take(MAX_CHARS) {
        sanitized.push(ch);
    }
    if text.chars().count() > MAX_CHARS {
        sanitized.push_str("...");
    }
    sanitized
}

fn parse_file_uri_cwd(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else if let Some(slash) = rest.find('/') {
        let host = &rest[..slash];
        if !(host.is_empty() || host.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        &rest[slash..]
    } else {
        rest
    };
    let path = percent_decode_utf8(path)?;

    #[cfg(windows)]
    {
        let mut path = path;
        if path.len() >= 3
            && path.as_bytes()[0] == b'/'
            && path.as_bytes()[2] == b':'
            && path.as_bytes()[1].is_ascii_alphabetic()
        {
            path.remove(0);
        }
        Some(PathBuf::from(path.replace('/', "\\")))
    }

    #[cfg(not(windows))]
    Some(PathBuf::from(path))
}

fn percent_decode_utf8(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            let hi = *bytes.get(idx + 1)?;
            let lo = *bytes.get(idx + 2)?;
            output.push(hex_value(hi)? * 16 + hex_value(lo)?);
            idx += 3;
        } else {
            output.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn foreground_job_is_shell(job: &crate::platform::ForegroundJob, shell_pid: u32) -> bool {
    job.processes.iter().any(|process| process.pid == shell_pid)
}

pub(super) fn current_transient_default_color_owner(shell_pid: u32) -> Option<u32> {
    let job = crate::detect::foreground_job(shell_pid)?;
    (!foreground_job_is_shell(&job, shell_pid)).then_some(job.process_group_id)
}

#[cfg(target_os = "macos")]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    let _ = owner_pgid;
    foreground_job_is_shell(foreground_job, shell_pid)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn should_restore_host_terminal_theme(
    owner_pgid: u32,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    if alternate_screen {
        return false;
    }

    let Some(foreground_job) = foreground_job else {
        return false;
    };

    foreground_job.process_group_id != owner_pgid
        && foreground_job_is_shell(foreground_job, shell_pid)
}

pub(super) fn write_host_terminal_theme(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
) {
    write_host_terminal_theme_selective(terminal, theme, true, true);
}

pub(super) fn write_host_terminal_theme_selective(
    terminal: &mut crate::ghostty::Terminal,
    theme: crate::terminal_theme::TerminalTheme,
    foreground: bool,
    background: bool,
) {
    if foreground {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Foreground,
            theme.foreground,
        );
    }
    if background {
        write_host_default_color(
            terminal,
            crate::terminal_theme::DefaultColorKind::Background,
            theme.background,
        );
    }
}

fn write_host_default_color(
    terminal: &mut crate::ghostty::Terminal,
    kind: crate::terminal_theme::DefaultColorKind,
    color: Option<crate::terminal_theme::RgbColor>,
) {
    let sequence = if let Some(color) = color {
        crate::terminal_theme::osc_set_default_color_sequence(kind, color)
    } else {
        crate::terminal_theme::osc_reset_default_color_sequence(kind).to_string()
    };
    terminal.write(sequence.as_bytes());
}

pub(super) fn restore_host_terminal_theme_if_needed(
    core: &mut GhosttyPaneCore,
    pane_id: PaneId,
    shell_pid: u32,
    alternate_screen: bool,
    foreground_job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    let Some(owner_pgid) = core.transient_default_color_owner_pgid else {
        return false;
    };
    if core.host_terminal_theme.is_empty() {
        return false;
    }
    if !should_restore_host_terminal_theme(owner_pgid, shell_pid, alternate_screen, foreground_job)
    {
        return false;
    }

    core.transient_default_color_owner_pgid = None;
    core.child_default_foreground_changed = false;
    core.child_default_background_changed = false;
    write_host_terminal_theme(&mut core.terminal, core.host_terminal_theme);
    info!(
        pane = pane_id.raw(),
        owner_pgid, "restored host terminal default colors after transient override"
    );
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn bulk_osc_scans_match_bytewise_state_and_response_offsets() {
        use super::*;
        let mut input = b"text\x1b_Gm=1;".to_vec();
        input.extend(std::iter::repeat_n(b'A', 8192));
        input.extend_from_slice(b"\x1b\\\x1b]10;?\x07\x1b]11;red\x1b\\\x1bPignored");
        input.extend(0u8..=255);
        input.extend_from_slice(b"\x1b\\\x1b]12;");
        input.extend(std::iter::repeat_n(b'B', 4200));
        input.extend_from_slice(b"\x07\x1b]10;?\x1b\\\x1b]11;?\x07\x1b");
        for chunk_size in [1, 2, 3, 17, 4096, input.len()] {
            let mut bulk = DefaultColorOscTracker::default();
            let mut scalar = DefaultColorOscTracker::default();
            let mut bulk_events = DefaultColorEventTracker::default();
            let mut scalar_events = DefaultColorEventTracker::default();
            let mut bulk_stream = OscStreamCollector::default();
            let mut scalar_stream = OscStreamCollector::default();
            for chunk in input.chunks(chunk_size) {
                let changed = bulk.observe(chunk);
                let mut scalar_changed = false;
                let mut expected_events = Vec::new();
                let mut expected_bodies = Vec::new();
                for (offset, byte) in chunk.iter().enumerate() {
                    let byte = std::slice::from_ref(byte);
                    scalar_changed |= scalar.observe(byte);
                    scalar_events.observe(byte);
                    expected_events.extend(scalar_events.drain_pending().into_iter().map(
                        |mut event| {
                            event.end_offset += offset;
                            event
                        },
                    ));
                    scalar_stream.observe(byte, |body| expected_bodies.push(body.to_vec()));
                }
                bulk_events.observe(chunk);
                let mut bodies = Vec::new();
                bulk_stream.observe(chunk, |body| bodies.push(body.to_vec()));
                assert_eq!(changed, scalar_changed);
                assert_eq!((bulk.state, &bulk.body), (scalar.state, &scalar.body));
                assert_eq!(bulk_events.drain_pending(), expected_events);
                assert_eq!(
                    (bulk_events.state, &bulk_events.body),
                    (scalar_events.state, &scalar_events.body)
                );
                assert_eq!(bodies, expected_bodies);
                assert_eq!(
                    (bulk_stream.state, &bulk_stream.body),
                    (scalar_stream.state, &scalar_stream.body)
                );
            }
        }
    }

    use tokio::sync::mpsc;

    use super::*;
    use crate::layout::PaneId;

    fn pane_default_theme(
        pane: &super::super::GhosttyPaneTerminal,
    ) -> crate::terminal_theme::TerminalTheme {
        let mut core = pane.core.lock().unwrap();
        let super::super::terminal::GhosttyPaneCore {
            terminal,
            render_state,
            ..
        } = &mut *core;
        render_state.update(terminal).unwrap();
        let colors = render_state.colors().unwrap();
        crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: colors.foreground.r,
                g: colors.foreground.g,
                b: colors.foreground.b,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: colors.background.r,
                g: colors.background.g,
                b: colors.background.b,
            }),
            ..Default::default()
        }
    }

    fn shell_job(shell_pid: u32) -> crate::platform::ForegroundJob {
        crate::platform::ForegroundJob {
            process_group_id: shell_pid,
            processes: vec![crate::platform::ForegroundProcess {
                pid: shell_pid,
                name: "zsh".to_string(),
                argv0: Some("zsh".to_string()),
                argv: Some(vec!["zsh".to_string()]),
                cmdline: Some("zsh".to_string()),
            }],
        }
    }

    fn tracked_default_color_events(
        events: Vec<DefaultColorTrackedEvent>,
    ) -> Vec<DefaultColorEvent> {
        events.into_iter().map(|event| event.event).collect()
    }

    fn enabled_osc_debug_tracker() -> OscDebugTracker {
        OscDebugTracker {
            enabled: true,
            collector: OscStreamCollector::default(),
            pending: Vec::new(),
        }
    }

    #[test]
    fn osc_stream_collector_ignores_strings_and_preserves_escaped_bytes() {
        let mut collector = OscStreamCollector::default();
        let mut bodies = Vec::new();

        collector.observe(
            b"\x1bPignored\x1b]0;not-osc\x07\x1b\\\x1b]9;a\x1b",
            |body| bodies.push(body.to_vec()),
        );
        collector.observe(b"\x1b\\\x1b]2;b\x1b\x07", |body| bodies.push(body.to_vec()));

        assert_eq!(bodies, vec![b"9;a\x1b".to_vec(), b"2;b\x1b".to_vec()]);
    }

    #[test]
    fn default_color_tracker_detects_split_osc_11_sequences() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]11;rgb:11/22"));
        assert!(tracker.observe(b"/33\x1b\\"));
    }

    #[test]
    fn default_color_tracker_ignores_osc_queries() {
        let mut tracker = DefaultColorOscTracker::default();

        assert!(!tracker.observe(b"\x1b]10;?\x1b\\"));
        assert!(!tracker.observe(b"\x1b]11;?\x07"));
    }

    #[test]
    fn reported_cwd_parses_file_uri_and_bare_paths() {
        assert_eq!(
            parse_reported_cwd(b"file:///tmp/herdr%20repo"),
            Some(std::path::PathBuf::from("/tmp/herdr repo"))
        );
        assert_eq!(
            parse_reported_cwd(b"C:\\Users\\herdr\\src\\herdr"),
            Some(std::path::PathBuf::from("C:\\Users\\herdr\\src\\herdr"))
        );
        assert_eq!(
            parse_reported_cwd(b"\"C:\\my proj\""),
            Some(std::path::PathBuf::from("C:\\my proj"))
        );
    }

    #[test]
    fn reported_cwd_rejects_invalid_or_empty_values() {
        assert_eq!(parse_reported_cwd(b""), None);
        assert_eq!(parse_reported_cwd(b"\xff"), None);
        assert_eq!(parse_reported_cwd(b"file://remote/tmp"), None);
    }

    // -----------------------------------------------------------------------

    #[test]
    fn program_status_pi_wire_survives_every_byte_split() {
        let bytes = b"\x1b]7501;state=blocked:app=pi:kind=permission:msg=QXBwcm92ZQ==\x1b\\";
        for split in 0..=bytes.len() {
            let mut tracker = AgentOscStateTracker::default();
            tracker.observe(&bytes[..split]);
            tracker.observe(&bytes[split..]);
            let snapshot = tracker.program_status().unwrap();
            assert_eq!(snapshot.revision, 1);
            let record = snapshot.record.unwrap();
            assert_eq!(record.state, ProgramStatusState::Blocked);
            assert_eq!(record.kind, Some(ProgramStatusKind::Permission));
            assert_eq!(record.app.as_deref(), Some("pi"));
        }
    }

    #[test]
    fn program_status_queries_are_fixed_replies_without_state() {
        let mut tracker = AgentOscStateTracker::default();
        for byte in b"\x1b]7501;?\x1b\\\x1b]7501;?\x07" {
            tracker.observe(&[*byte]);
        }
        assert_eq!(tracker.take_query_replies(), 2);
        assert_eq!(tracker.take_query_replies(), 0);
        assert_eq!(tracker.program_status(), None);
    }

    #[test]
    fn program_status_replaces_keys_and_clear_keeps_revision_fence() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]7501;state=blocked:kind=auth:app=pi:progress=23\x07");
        let first = tracker.program_status().unwrap();
        assert_eq!(first.record.unwrap().kind, Some(ProgramStatusKind::Auth));
        tracker.observe(b"\x1b]7501;state=working\x07");
        let second = tracker.program_status().unwrap();
        assert_eq!(second.revision, 2);
        let record = second.record.unwrap();
        assert_eq!(record.state, ProgramStatusState::Working);
        assert_eq!(
            (record.kind, record.app, record.progress),
            (None, None, None)
        );
        tracker.observe(b"\x1b]7501;state=working\x07");
        assert_eq!(tracker.program_status().unwrap().revision, 2);
        tracker.observe(b"\x1b]7501;state=clear\x07");
        assert_eq!(tracker.program_status().unwrap().revision, 3);
        assert_eq!(tracker.program_status().unwrap().record, None);
    }

    #[test]
    fn program_status_invalid_reports_leave_record_unchanged() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]7501;state=working:app=pi\x07");
        let original = tracker.program_status();
        for payload in [
            "state=oops",
            "app=pi",
            "state=done:id=child",
            "state=clear:id=",
            "state=done:id=bad!",
            "state=done:msg=not_base64",
            "state=done:msg=AA==",
            "state=done:title=woA=",
            "state=done:msg=/w==",
        ] {
            tracker.observe(format!("\x1b]7501;{payload}\x1b\\").as_bytes());
            assert_eq!(tracker.program_status(), original, "{payload}");
        }
        for payload in [
            format!("state=done:app={}", "a".repeat(33)),
            format!("state=done:msg={}", "A".repeat(2733)),
            format!("state=done:title={}", "A".repeat(257)),
            format!("state=done:{}=a", "a".repeat(17)),
            format!("state=done:{}", "x".repeat(4096)),
        ] {
            tracker.observe(format!("\x1b]7501;{payload}\x07").as_bytes());
            assert_eq!(tracker.program_status(), original);
        }
        tracker.observe(b"\x1b]7501;state=done\x07");
        assert_eq!(
            tracker.program_status().unwrap().record.unwrap().state,
            ProgramStatusState::Done
        );
    }

    #[test]
    fn program_status_sequence_cap_includes_framing_and_recovers() {
        for terminator in ["\x07", "\x1b\\"] {
            let prefix = "\x1b]7501;state=done:extension=";
            let valid = format!(
                "{prefix}{}{terminator}",
                "x".repeat(4096 - prefix.len() - terminator.len())
            );
            let mut tracker = AgentOscStateTracker::default();
            tracker.observe(valid.as_bytes());
            assert_eq!(tracker.program_status().unwrap().revision, 1);
            let oversized = format!(
                "{prefix}{}{terminator}",
                "x".repeat(4097 - prefix.len() - terminator.len())
            );
            tracker.observe(oversized.as_bytes());
            assert_eq!(tracker.program_status().unwrap().revision, 1);
        }
    }

    #[test]
    fn program_status_malformed_pairs_unknown_keys_and_last_value() {
        let record = parse_program_status(
            b" bad :state=idle:state=blocked:kind=question:progress=100:future=okay:app=pi",
        )
        .unwrap()
        .unwrap();
        assert_eq!(record.state, ProgramStatusState::Blocked);
        assert_eq!(record.kind, Some(ProgramStatusKind::Question));
        assert_eq!(record.progress, Some(100));
        let record = parse_program_status(b"state=working:kind=auth:progress=101:app=bad,name")
            .unwrap()
            .unwrap();
        assert_eq!(
            (record.kind, record.progress, record.app),
            (None, None, None)
        );
        assert!(parse_program_status(b"state=done:msg=YQ").is_some());
        assert!(parse_program_status(b"state=done:bad=\xff").is_some());
        assert!(parse_program_status(b"state=done:id=\xff").is_none());
        assert!(parse_program_status(b"state=done:msg=YQ==").is_some());
    }

    #[test]
    fn program_status_lifetime_prompt_exit_full_and_soft_reset() {
        for state in ["idle", "working", "blocked", "done", "error"] {
            for prompt in [false, true] {
                let mut tracker = AgentOscStateTracker::default();
                tracker.observe(format!("\x1b]7501;state={state}\x07").as_bytes());
                // Alternate-screen switches and soft reset do not affect records.
                tracker.observe(b"\x1b[?1049h\x1b[!p\x1b[?1049l");
                assert!(tracker.program_status().unwrap().record.is_some());
                if prompt {
                    tracker.observe(b"\x1b]133;A;prompt=1\x07");
                } else {
                    tracker.expire_program_status();
                }
                let survives = matches!(state, "done" | "error");
                assert_eq!(tracker.program_status().unwrap().record.is_some(), survives);
                tracker.observe(b"\x1b");
                tracker.observe(b"c");
                let reset = tracker.program_status().unwrap();
                assert_eq!(reset.record, None);
                assert_eq!(reset.source_epoch, 1);
                assert!(reset.revision >= 2);
            }
        }
    }

    #[test]
    fn program_status_replacement_rejects_partial_old_report_but_keeps_title_framing() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]7501;state=working");
        tracker.reset_program_status();
        tracker.observe(b":app=pi\x07");
        assert_eq!(tracker.program_status().unwrap().record, None);
        assert_eq!(tracker.program_status().unwrap().revision, 1);
        tracker.observe(b"\x1b]7501;state=idle:app=pi\x07");
        assert_eq!(
            tracker.program_status().unwrap().record.unwrap().state,
            ProgramStatusState::Idle
        );
        tracker.observe(b"\x1b]2;new title");
        tracker.reset_program_status();
        tracker.observe(b"\x07\x1b]7501;state=working\x07");
        assert_eq!(tracker.terminal_title(), Some("new title"));
        assert_eq!(
            tracker.program_status().unwrap().record.unwrap().state,
            ProgramStatusState::Working
        );
        let mut oversized = b"state=done:msg=".to_vec();
        oversized.extend(vec![0xff; 2733]);
        assert!(parse_program_status(&oversized).is_none());
        tracker.observe(b"\x1b]7501;state=working:x=");
        tracker.reset_program_status();
        let suffix = format!("{}\x07\x1b]7501;state=done\x07", "x".repeat(5000));
        tracker.observe(suffix.as_bytes());
        assert_eq!(
            tracker.program_status().unwrap().record.unwrap().state,
            ProgramStatusState::Done
        );
    }

    #[test]
    fn program_status_ignored_strings_do_not_report_or_reset() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe(b"\x1b]7501;state=working\x07");
        let original = tracker.program_status();
        tracker.observe(b"\x1bP\x1bc\x1b]7501;state=done\x07\x1b\\");
        assert_eq!(tracker.program_status(), original);
        tracker.reset_program_status();
        assert_eq!(tracker.program_status().unwrap().source_epoch, 1);
        assert_eq!(tracker.program_status().unwrap().record, None);
    }

    // AgentOscStateTracker tests
    // -----------------------------------------------------------------------

    #[test]
    fn agent_osc_osc0_title_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]0;braille title\x07world".as_bytes());
        assert_eq!(t.latest_title(), "braille title");
        assert_eq!(t.terminal_title(), Some("braille title"));
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_osc2_title_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe("hello\x1b]2;static title\x1b\\world".as_bytes());
        assert_eq!(t.latest_title(), "static title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_empty_osc0_clears_title() {
        let mut t = AgentOscStateTracker::default();
        // First set a title.
        t.observe(b"\x1b]0;some title\x07");
        assert_eq!(t.latest_title(), "some title");
        // Then clear it with an empty payload (Codex pattern).
        t.observe(b"\x1b]0;\x07");
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.terminal_title(), None);
    }

    #[test]
    fn clearing_agent_evidence_preserves_the_terminal_title() {
        let mut tracker = AgentOscStateTracker::default();
        tracker.observe("\x1b]2;✳ 修复🙂标题\x1b\\".as_bytes());

        tracker.clear_retained();

        assert_eq!(tracker.latest_title(), "");
        assert_eq!(tracker.terminal_title(), Some("✳ 修复🙂标题"));
    }

    #[cfg(unix)]
    #[test]
    fn handoff_seed_does_not_restore_agent_detection_evidence() {
        let mut tracker = AgentOscStateTracker::default();

        tracker.seed_terminal_title(Some("✳ restored title".into()));

        assert_eq!(tracker.terminal_title(), Some("✳ restored title"));
        assert_eq!(tracker.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_sets_progress_with_bel() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        assert_eq!(t.latest_title(), "");
    }

    #[test]
    fn agent_osc_osc9_clear_progress_with_st() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3;\x07");
        assert_eq!(t.latest_progress(), "4;3;");
        t.observe(b"\x1b]9;4;0;\x1b\\");
        assert_eq!(t.latest_progress(), "4;0;");
    }

    #[test]
    fn agent_osc_split_sequence_across_chunks() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]9;4;3");
        assert_eq!(t.latest_progress(), "");
        t.observe(b";\x07");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_bel_and_st_terminators_both_work() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;title-bel\x07");
        assert_eq!(t.latest_title(), "title-bel");
        t.observe(b"\x1b]0;title-st\x1b\\");
        assert_eq!(t.latest_title(), "title-st");
    }

    #[test]
    fn agent_osc_oversized_payload_is_discarded_and_recovers() {
        let mut t = AgentOscStateTracker::default();
        // Set a title first.
        t.observe(b"\x1b]0;before\x07");
        assert_eq!(t.latest_title(), "before");

        // Feed an oversized OSC body (> 4096 bytes).
        let mut oversized = Vec::from(b"\x1b]0;".as_slice());
        oversized.extend(std::iter::repeat_n(b'x', 4097));
        oversized.push(0x07);
        t.observe(&oversized);
        // The oversized body is dropped; the previously stored title is kept.
        assert_eq!(t.latest_title(), "before");

        // After recovery, subsequent valid sequences are captured normally.
        t.observe(b"\x1b]0;after\x07");
        assert_eq!(t.latest_title(), "after");
    }

    #[test]
    fn agent_osc_cap_length_is_respected() {
        let mut t = AgentOscStateTracker::default();
        // Build a title of AGENT_OSC_MAX_CHARS + 50 ASCII chars.
        let long_title: String = "a".repeat(AGENT_OSC_MAX_CHARS + 50);
        let seq = format!("\x1b]0;{long_title}\x07");
        t.observe(seq.as_bytes());
        assert_eq!(t.latest_title().len(), AGENT_OSC_MAX_CHARS);
    }

    #[test]
    fn agent_osc_control_chars_stripped() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;before\x01after\x07");
        assert_eq!(t.latest_title(), "beforeafter");
    }

    #[test]
    fn agent_osc_unrelated_osc_does_not_overwrite_title() {
        let mut t = AgentOscStateTracker::default();
        t.observe(b"\x1b]0;my title\x07");
        // OSC 4 (palette color), OSC 52 (clipboard) — should not touch title/progress.
        t.observe(b"\x1b]4;1;rgb:aa/bb/cc\x07");
        t.observe(b"\x1b]52;c;aGVsbG8=\x07");
        assert_eq!(t.latest_title(), "my title");
        assert_eq!(t.latest_progress(), "");
    }

    #[test]
    fn agent_osc_interleaved_sequences() {
        let mut t = AgentOscStateTracker::default();
        // OSC 0 title, then OSC 9 progress, then OSC 2 title update.
        t.observe(b"\x1b]0;first\x07\x1b]9;4;3;\x07\x1b]2;second\x07");
        assert_eq!(t.latest_title(), "second");
        assert_eq!(t.latest_progress(), "4;3;");
    }

    #[test]
    fn agent_osc_default_state_is_empty() {
        let t = AgentOscStateTracker::default();
        assert_eq!(t.latest_title(), "");
        assert_eq!(t.latest_progress(), "");
    }

    // -----------------------------------------------------------------------
    // OscDebugTracker tests (existing)
    // -----------------------------------------------------------------------

    #[test]
    fn osc_debug_tracker_detects_title_with_bel() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]0;✻ working title\x07world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "✻ working title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_title_with_st() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe("hello\x1b]2;static title\x1b\\world".as_bytes());

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "2".to_string(),
                payload: "static title".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_detects_split_status_sequences() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]9;4;3");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\x07\x1b]21337;status=working\x1b\\");

        assert_eq!(
            tracker.drain_pending(),
            vec![
                OscDebugEvent {
                    command: "9".to_string(),
                    payload: "4;3".to_string(),
                },
                OscDebugEvent {
                    command: "21337".to_string(),
                    payload: "status=working".to_string(),
                },
            ]
        );
    }

    #[test]
    fn osc_debug_tracker_ignores_untracked_osc_commands() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]52;c;SGVsbG8=\x07\x1b]7;file:///tmp\x07");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn osc_debug_tracker_sanitizes_control_characters() {
        let mut tracker = enabled_osc_debug_tracker();

        tracker.observe(b"\x1b]0;before\x01after\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "beforeafter".to_string(),
            }]
        );
    }

    #[test]
    fn osc_debug_tracker_recovers_after_oversized_payload() {
        let mut tracker = enabled_osc_debug_tracker();
        let oversized = vec![b'a'; 4097];

        tracker.observe(b"\x1b]0;");
        tracker.observe(&oversized);
        tracker.observe(b"\x07\x1b]0;ok\x07");

        assert_eq!(
            tracker.drain_pending(),
            vec![OscDebugEvent {
                command: "0".to_string(),
                payload: "ok".to_string(),
            }]
        );
    }

    #[test]
    fn default_color_event_tracker_detects_queries_sets_and_resets() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(
            b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07\x1b]4;0;?\x07\x1b]10;rgb:11/22/33\x07\x1b]111\x07",
        );

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![
                DefaultColorEvent::Query(DefaultColorQuery::Foreground),
                DefaultColorEvent::Query(DefaultColorQuery::Background),
                DefaultColorEvent::Query(DefaultColorQuery::Cursor),
                DefaultColorEvent::PaletteQuery(0),
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
                DefaultColorEvent::Reset(DefaultColorQuery::Background),
            ]
        );
    }

    #[test]
    fn default_color_event_tracker_tracks_each_multi_value_set() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(
            b"\x1b]10;rgb:11/22/33;rgb:44/55/66\x1b\\\x1b]10;?;rgb:77/88/99\x1b\\\x1b]10;;rgb:aa/bb/cc\x1b\\",
        );

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
                DefaultColorEvent::Set(DefaultColorQuery::Background),
                DefaultColorEvent::Set(DefaultColorQuery::Background),
                DefaultColorEvent::Set(DefaultColorQuery::Foreground),
            ]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_default_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]11");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b";?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn default_color_event_tracker_handles_split_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;25");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"5;?\x1b");
        assert!(tracker.drain_pending().is_empty());
        tracker.observe(b"\\");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(255)]
        );
    }

    #[test]
    fn default_color_event_tracker_rejects_malformed_palette_color_queries() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]4;;?\x07");
        tracker.observe(b"\x1b]4;-1;?\x07");
        tracker.observe(b"\x1b]4;256;?\x07");
        tracker.observe(b"\x1b]4;0;?;1;?\x07");
        tracker.observe(b"\x1b]4;0;rgb:1111/2222/3333\x07");
        tracker.observe(b"\x1b]4;0;?\x07");

        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::PaletteQuery(0)]
        );
    }

    #[test]
    fn default_color_event_tracker_ignores_other_osc_and_dcs_payloads() {
        let mut tracker = DefaultColorEventTracker::default();

        tracker.observe(b"\x1b]0;title\x07");
        tracker.observe(b"\x1b]52;c;?\x07");
        tracker.observe(b"\x1bPtmux;\x1b\x1b]11;?\x07\x1b\\");
        tracker.observe(b"\x1bPtmux;payload\x07\x1b]11;?\x07\x1b\\");

        assert!(tracker.drain_pending().is_empty());
    }

    #[test]
    fn default_color_event_tracker_ignores_oversized_osc_until_terminator() {
        let mut tracker = DefaultColorEventTracker::default();
        let mut oversized = Vec::from(b"\x1b]11;".as_slice());
        oversized.extend(std::iter::repeat_n(b'a', 1025));
        oversized.extend_from_slice(b"\x1b]11;?\x07");

        tracker.observe(&oversized);
        assert!(tracker.drain_pending().is_empty());

        tracker.observe(b"\x1b]11;?\x07");
        assert_eq!(
            tracked_default_color_events(tracker.drain_pending()),
            vec![DefaultColorEvent::Query(DefaultColorQuery::Background)]
        );
    }

    #[test]
    fn host_theme_restore_waits_for_shell_and_non_alternate_screen() {
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            true,
            Some(&shell_job(7)),
        ));
        assert!(!should_restore_host_terminal_theme(42, 7, false, None));
        assert!(!should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&crate::platform::ForegroundJob {
                process_group_id: 42,
                processes: vec![crate::platform::ForegroundProcess {
                    pid: 42,
                    name: "droid".to_string(),
                    argv0: Some("droid".to_string()),
                    argv: Some(vec!["droid".to_string()]),
                    cmdline: Some("droid".to_string()),
                }],
            }),
        ));
        assert!(should_restore_host_terminal_theme(
            42,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(target_os = "macos")]
        assert!(should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));

        #[cfg(not(target_os = "macos"))]
        assert!(!should_restore_host_terminal_theme(
            7,
            7,
            false,
            Some(&shell_job(7)),
        ));
    }

    #[test]
    fn restore_host_terminal_theme_reapplies_cached_colors() {
        let (tx, _rx) = mpsc::channel(4);
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let pane = super::super::GhosttyPaneTerminal::new(terminal, tx).unwrap();
        let pane_id = PaneId::from_raw(1);
        let shell_pid = 7;
        let host_theme = crate::terminal_theme::TerminalTheme {
            foreground: Some(crate::terminal_theme::RgbColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            }),
            background: Some(crate::terminal_theme::RgbColor {
                r: 0x11,
                g: 0x22,
                b: 0x33,
            }),
            ..Default::default()
        };

        pane.apply_host_terminal_theme(host_theme);
        {
            let mut core = pane.core.lock().unwrap();
            core.transient_default_color_owner_pgid = Some(42);
            core.terminal.write(b"\x1b]11;rgb:dd/ee/ff\x1b\\");
        }
        assert_eq!(
            pane_default_theme(&pane).background,
            Some(crate::terminal_theme::RgbColor {
                r: 0xdd,
                g: 0xee,
                b: 0xff,
            })
        );

        {
            let mut core = pane.core.lock().unwrap();
            assert!(restore_host_terminal_theme_if_needed(
                &mut core,
                pane_id,
                shell_pid,
                false,
                Some(&shell_job(shell_pid)),
            ));
        }

        assert_eq!(pane_default_theme(&pane).background, host_theme.background);
        assert_eq!(pane_default_theme(&pane).foreground, host_theme.foreground);
    }
}
