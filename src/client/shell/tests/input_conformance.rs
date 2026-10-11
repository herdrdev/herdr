//! Keyboard transparency conformance.
//!
//! Herdr should be invisible to the application in a pane: for every keystroke,
//! the pane must receive the bytes it would receive running directly in the
//! host terminal. The oracle is libghostty's encoder fed the same key event the
//! Ghostty app builds from an OS key event. The Herdr path is the real one:
//! host bytes -> host framer -> client shell routing -> server pane input ->
//! pane encoder, with the pane's modes set by application output.
//!
//! Keystrokes whose host bytes cannot distinguish keys the pane would see
//! differently are counted as host-lossy, not as Herdr failures.

// The Unix-host checks are compiled out on Windows, leaving helpers that only
// they use.
#![cfg_attr(windows, allow(dead_code))]

use std::collections::{BTreeMap, HashMap};

use super::*;

use crate::ghostty::{self, ffi};

#[derive(Clone, Copy)]
struct KeyDef {
    name: &'static str,
    key: ffi::GhosttyKey,
    /// US layout text: (unshifted, shifted).
    text: Option<(char, char)>,
}

const fn text(name: &'static str, key: ffi::GhosttyKey, base: char, shifted: char) -> KeyDef {
    KeyDef {
        name,
        key,
        text: Some((base, shifted)),
    }
}

const fn func(name: &'static str, key: ffi::GhosttyKey) -> KeyDef {
    KeyDef {
        name,
        key,
        text: None,
    }
}

const KEYS: &[KeyDef] = &[
    text("a", ffi::GhosttyKey_GHOSTTY_KEY_A, 'a', 'A'),
    text("b", ffi::GhosttyKey_GHOSTTY_KEY_B, 'b', 'B'),
    text("c", ffi::GhosttyKey_GHOSTTY_KEY_C, 'c', 'C'),
    text("d", ffi::GhosttyKey_GHOSTTY_KEY_D, 'd', 'D'),
    text("e", ffi::GhosttyKey_GHOSTTY_KEY_E, 'e', 'E'),
    text("f", ffi::GhosttyKey_GHOSTTY_KEY_F, 'f', 'F'),
    text("g", ffi::GhosttyKey_GHOSTTY_KEY_G, 'g', 'G'),
    text("h", ffi::GhosttyKey_GHOSTTY_KEY_H, 'h', 'H'),
    text("i", ffi::GhosttyKey_GHOSTTY_KEY_I, 'i', 'I'),
    text("j", ffi::GhosttyKey_GHOSTTY_KEY_J, 'j', 'J'),
    text("k", ffi::GhosttyKey_GHOSTTY_KEY_K, 'k', 'K'),
    text("l", ffi::GhosttyKey_GHOSTTY_KEY_L, 'l', 'L'),
    text("m", ffi::GhosttyKey_GHOSTTY_KEY_M, 'm', 'M'),
    text("n", ffi::GhosttyKey_GHOSTTY_KEY_N, 'n', 'N'),
    text("o", ffi::GhosttyKey_GHOSTTY_KEY_O, 'o', 'O'),
    text("p", ffi::GhosttyKey_GHOSTTY_KEY_P, 'p', 'P'),
    text("q", ffi::GhosttyKey_GHOSTTY_KEY_Q, 'q', 'Q'),
    text("r", ffi::GhosttyKey_GHOSTTY_KEY_R, 'r', 'R'),
    text("s", ffi::GhosttyKey_GHOSTTY_KEY_S, 's', 'S'),
    text("t", ffi::GhosttyKey_GHOSTTY_KEY_T, 't', 'T'),
    text("u", ffi::GhosttyKey_GHOSTTY_KEY_U, 'u', 'U'),
    text("v", ffi::GhosttyKey_GHOSTTY_KEY_V, 'v', 'V'),
    text("w", ffi::GhosttyKey_GHOSTTY_KEY_W, 'w', 'W'),
    text("x", ffi::GhosttyKey_GHOSTTY_KEY_X, 'x', 'X'),
    text("y", ffi::GhosttyKey_GHOSTTY_KEY_Y, 'y', 'Y'),
    text("z", ffi::GhosttyKey_GHOSTTY_KEY_Z, 'z', 'Z'),
    text("0", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0, '0', ')'),
    text("1", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_1, '1', '!'),
    text("2", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_2, '2', '@'),
    text("3", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_3, '3', '#'),
    text("4", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_4, '4', '$'),
    text("5", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_5, '5', '%'),
    text("6", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_6, '6', '^'),
    text("7", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_7, '7', '&'),
    text("8", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_8, '8', '*'),
    text("9", ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_9, '9', '('),
    text("`", ffi::GhosttyKey_GHOSTTY_KEY_BACKQUOTE, '`', '~'),
    text("-", ffi::GhosttyKey_GHOSTTY_KEY_MINUS, '-', '_'),
    text("=", ffi::GhosttyKey_GHOSTTY_KEY_EQUAL, '=', '+'),
    text("[", ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT, '[', '{'),
    text("]", ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT, ']', '}'),
    text("\\", ffi::GhosttyKey_GHOSTTY_KEY_BACKSLASH, '\\', '|'),
    text(";", ffi::GhosttyKey_GHOSTTY_KEY_SEMICOLON, ';', ':'),
    text("'", ffi::GhosttyKey_GHOSTTY_KEY_QUOTE, '\'', '"'),
    text(",", ffi::GhosttyKey_GHOSTTY_KEY_COMMA, ',', '<'),
    text(".", ffi::GhosttyKey_GHOSTTY_KEY_PERIOD, '.', '>'),
    text("/", ffi::GhosttyKey_GHOSTTY_KEY_SLASH, '/', '?'),
    text("space", ffi::GhosttyKey_GHOSTTY_KEY_SPACE, ' ', ' '),
    func("enter", ffi::GhosttyKey_GHOSTTY_KEY_ENTER),
    func("tab", ffi::GhosttyKey_GHOSTTY_KEY_TAB),
    func("backspace", ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE),
    func("escape", ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE),
    func("up", ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP),
    func("down", ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN),
    func("left", ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT),
    func("right", ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT),
    func("home", ffi::GhosttyKey_GHOSTTY_KEY_HOME),
    func("end", ffi::GhosttyKey_GHOSTTY_KEY_END),
    func("pageup", ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP),
    func("pagedown", ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN),
    func("insert", ffi::GhosttyKey_GHOSTTY_KEY_INSERT),
    func("delete", ffi::GhosttyKey_GHOSTTY_KEY_DELETE),
    func("f1", ffi::GhosttyKey_GHOSTTY_KEY_F1),
    func("f2", ffi::GhosttyKey_GHOSTTY_KEY_F2),
    func("f3", ffi::GhosttyKey_GHOSTTY_KEY_F3),
    func("f4", ffi::GhosttyKey_GHOSTTY_KEY_F4),
    func("f5", ffi::GhosttyKey_GHOSTTY_KEY_F5),
    func("f6", ffi::GhosttyKey_GHOSTTY_KEY_F6),
    func("f7", ffi::GhosttyKey_GHOSTTY_KEY_F7),
    func("f8", ffi::GhosttyKey_GHOSTTY_KEY_F8),
    func("f9", ffi::GhosttyKey_GHOSTTY_KEY_F9),
    func("f10", ffi::GhosttyKey_GHOSTTY_KEY_F10),
    func("f11", ffi::GhosttyKey_GHOSTTY_KEY_F11),
    func("f12", ffi::GhosttyKey_GHOSTTY_KEY_F12),
];

/// Modes an application can request by writing to its terminal.
const PANE_MODES: &[(&str, &[u8])] = &[
    ("legacy", b""),
    ("legacy+decckm", b"\x1b[?1h"),
    ("modifyOtherKeys1", b"\x1b[>4;1m"),
    ("modifyOtherKeys2", b"\x1b[>4;2m"),
    ("kitty1", b"\x1b[>1u"),
    ("kitty3", b"\x1b[>3u"),
    ("kitty7", b"\x1b[>7u"),
    ("kitty11", b"\x1b[>11u"),
    ("kitty15", b"\x1b[>15u"),
    ("kitty31", b"\x1b[>31u"),
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum HostProfile {
    /// Kitty keyboard host (Ghostty, kitty, WezTerm, foot...). Herdr pushes 7,
    /// or 31 while the pane wants all keys reported.
    Kitty,
    /// Host without an enhanced keyboard protocol.
    Legacy,
}

impl HostProfile {
    fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty-host",
            Self::Legacy => "legacy-host",
        }
    }

    fn setup(self, pane_mode: &[u8]) -> &'static [u8] {
        match self {
            Self::Kitty if pane_mode == b"\x1b[>11u" || pane_mode == b"\x1b[>15u" => b"\x1b[>31u",
            Self::Kitty if pane_mode == b"\x1b[>31u" => b"\x1b[>31u",
            Self::Kitty => b"\x1b[>7u",
            Self::Legacy => b"",
        }
    }
}

const MODS: [(u16, &str); 4] = [
    (ghostty::MOD_CTRL, "ctrl"),
    (ghostty::MOD_ALT, "alt"),
    (ghostty::MOD_SHIFT, "shift"),
    (ghostty::MOD_SUPER, "super"),
];

fn mods_name(mods: u16) -> String {
    let mut parts: Vec<&str> = MODS
        .iter()
        .filter(|(bit, _)| mods & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if parts.is_empty() {
        parts.push("plain");
    }
    parts.join("+")
}

/// Host bytes for one keystroke: (press, release).
type HostKeystroke = (Vec<u8>, Vec<u8>);

struct Oracle {
    _terminal: ghostty::Terminal,
    encoder: ghostty::KeyEncoder,
}

impl Oracle {
    fn new(app_output: &[u8]) -> Self {
        let mut terminal = ghostty::Terminal::new(80, 24, 0).expect("terminal");
        terminal.write(app_output);
        let mut encoder = ghostty::KeyEncoder::new().expect("encoder");
        encoder.set_from_terminal(&terminal);
        // Keystrokes here carry an interpreted Alt, as on Linux; keep the oracle
        // identical on macOS, where libghostty would otherwise treat Option as text.
        encoder.set_macos_option_as_alt(true);
        Self {
            _terminal: terminal,
            encoder,
        }
    }

    /// Bytes for one keystroke (press then release), built like the Ghostty app
    /// builds key events from the OS: physical key, modifiers, layout text, the
    /// unshifted codepoint, and Shift consumed when it produced the text.
    fn keystroke(&mut self, def: KeyDef, mods: u16) -> HostKeystroke {
        let press = self.encode(def, mods, ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS);
        let release = self.encode(def, mods, ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE);
        (press, release)
    }

    fn encode(&mut self, def: KeyDef, mods: u16, action: ffi::GhosttyKeyAction) -> Vec<u8> {
        let mut event = ghostty::KeyEvent::new().expect("key event");
        event.set_key(def.key);
        event.set_mods(mods);
        event.set_action(action);
        let utf8;
        if let Some((base, shifted)) = def.text {
            let shift = mods & ghostty::MOD_SHIFT != 0;
            utf8 = if shift { shifted } else { base }.to_string();
            event.set_utf8(&utf8);
            event.set_unshifted_codepoint(base as u32);
            if shift && shifted != base {
                event.set_consumed_mods(ghostty::MOD_SHIFT);
            }
        }
        self.encoder.encode(&event).expect("encode")
    }
}

/// Panes that negotiated nothing. Like Ghostty, modifyOtherKeys level 1 counts
/// as nothing.
fn is_legacy_pane(pane_mode: &[u8]) -> bool {
    matches!(pane_mode, b"" | b"\x1b[?1h" | b"\x1b[>4;1m")
}

/// Agreed exception table: panes that negotiated no keyboard protocol keep the
/// classic bytes for keys where Ghostty's escape sequences break shells, and
/// get no Super chords (see `is_legacy_pane` use above).
fn legacy_shell_exception(pane_mode: &[u8], key: &str, mods: u16) -> Option<Vec<u8>> {
    if !is_legacy_pane(pane_mode) {
        return None;
    }
    let alt = mods & ghostty::MOD_ALT != 0;
    let base = mods & !ghostty::MOD_ALT;
    let classic: &[u8] = match (key, base) {
        ("enter", m) if m != 0 => b"\r",
        ("m", ghostty::MOD_CTRL) => b"\r",
        ("i", ghostty::MOD_CTRL) | ("tab", ghostty::MOD_CTRL) => b"\t",
        ("[", ghostty::MOD_CTRL) => b"\x1b",
        // Real terminals (lab: Alacritty, WezTerm) send the control byte.
        (letter, m)
            if m == ghostty::MOD_CTRL | ghostty::MOD_SHIFT
                && letter.len() == 1
                && letter.as_bytes()[0].is_ascii_lowercase() =>
        {
            let control = [letter.as_bytes()[0] & 0x1f];
            return Some(if alt {
                [b"\x1b".as_slice(), &control].concat()
            } else {
                control.to_vec()
            });
        }
        _ => return None,
    };
    Some(if alt {
        [b"\x1b", classic].concat()
    } else {
        classic.to_vec()
    })
}

struct HerdrPath {
    state: ClientShellState,
    framer: crate::raw_input::RawInputByteFramer,
    runtime: crate::terminal::TerminalRuntime,
    rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    host: HostProfile,
    host_reports_all: bool,
}

impl HerdrPath {
    fn new(host: HostProfile, app_output: &[u8]) -> Self {
        // Mirrors the client: report-all is pushed to the host while the pane
        // asks for it (`HostProfile::setup` picks 31 for those panes).
        let host_reports_all = matches!(app_output, b"\x1b[>11u" | b"\x1b[>15u" | b"\x1b[>31u");
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 0, b"", 4096,
            );
        runtime.test_process_pty_bytes(app_output);
        while rx.try_recv().is_ok() {}
        Self {
            state: Self::fresh_state(host, host_reports_all),
            framer: Self::fresh_framer(host),
            runtime,
            rx,
            host,
            host_reports_all,
        }
    }

    fn fresh_state(host: HostProfile, host_reports_all: bool) -> ClientShellState {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        // Mirrors the client: Herdr pushes Kitty event types on Kitty hosts.
        state.set_host_reports_key_releases(host == HostProfile::Kitty);
        state.set_host_reports_all_keys(host_reports_all);
        state.set_snapshot(Box::new(snapshot()));
        state.set_pane_surface(surface());
        state
    }

    fn fresh_framer(host: HostProfile) -> crate::raw_input::RawInputByteFramer {
        #[allow(unused_mut)] // only mutated on unix
        let mut framer = crate::raw_input::RawInputByteFramer::for_host_input();
        #[cfg(unix)]
        framer.set_host_escape_disambiguation_active(host == HostProfile::Kitty);
        #[cfg(not(unix))]
        let _ = host;
        framer
    }

    /// Collect pane input from one client outcome, the way the client loop does:
    /// side requests (pane focus) are ignored, link lookups get a no-link answer
    /// and the held mouse events are replayed. Returns true when Herdr kept the
    /// input for itself.
    fn absorb(
        &mut self,
        outcome: ClientShellInput,
        events: &mut Vec<ClientPaneInputEvent>,
    ) -> bool {
        let mut owned = outcome.detach;
        for request in outcome.requests {
            if let ClientMessage::ClientShellPaneInput { events: batch, .. } = request {
                events.extend(batch);
            }
        }
        let mut replay = Vec::new();
        for action in outcome.actions {
            match action {
                ClientShellAction::Endpoint {
                    boot_id, request, ..
                } if matches!(
                    request.method,
                    crate::api::schema::Method::PaneLinkActivate(_)
                ) =>
                {
                    let (_, actions) = self.state.handle_endpoint_result(
                        &boot_id,
                        &request.id,
                        Ok(crate::api::schema::ResponseResult::PaneLinkActivated {
                            url: None,
                            handled: false,
                        }),
                    );
                    for action in actions {
                        match action {
                            ClientShellAction::ReplayMouse(mouse) => replay.extend(mouse),
                            _ => owned = true,
                        }
                    }
                }
                ClientShellAction::Endpoint { .. } => {}
                ClientShellAction::ReplayMouse(mouse) => replay.extend(mouse),
                _ => owned = true,
            }
        }
        if !replay.is_empty() {
            let outcome = self.state.replay_mouse_events(replay);
            owned |= self.absorb(outcome, events);
        }
        owned
    }

    fn reset_client(&mut self) {
        self.state = Self::fresh_state(self.host, self.host_reports_all);
        self.framer = Self::fresh_framer(self.host);
    }

    /// Feed one host write, then let idle flushes run. Returns None when Herdr
    /// itself consumed the key (prefix, binding, mode change).
    fn feed(&mut self, host_bytes: &[u8]) -> Option<Vec<u8>> {
        let mut chunks = self.framer.push(host_bytes);
        for _ in 0..3 {
            if !self.framer.has_pending_input() {
                break;
            }
            chunks.extend(self.framer.flush_timeout());
        }
        let outcomes = chunks
            .iter()
            .map(|chunk| self.state.handle_input_bytes(chunk))
            .collect();
        self.deliver(outcomes)
    }

    /// Route client outcomes through server pane input and collect the bytes the
    /// pane receives. None when Herdr kept the input for itself.
    fn deliver(&mut self, outcomes: Vec<ClientShellInput>) -> Option<Vec<u8>> {
        let mut events = Vec::new();
        let mut herdr_owned = false;
        for outcome in outcomes {
            herdr_owned |= self.absorb(outcome, &mut events);
        }
        herdr_owned |= self.state.mode != ClientShellMode::Terminal || self.state.overlay.is_some();
        if herdr_owned {
            return None;
        }
        let mut out = Vec::new();
        if let Err(err) =
            crate::server::pane_input::test_apply_client_pane_input_events(&self.runtime, &events)
        {
            out.extend_from_slice(format!("<server error: {err}>").as_bytes());
        }
        while let Ok(bytes) = self.rx.try_recv() {
            out.extend_from_slice(&bytes);
        }
        Some(out)
    }
}

/// Kitty's default event type is press, so `;mods:1` and `;mods` are the same
/// report. Strip the explicit form so equivalent encodings compare equal.
fn normalize_kitty_press(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b":1")
            && bytes
                .get(i + 2)
                .is_some_and(|b| matches!(b, b'u' | b'~' | b';') || b.is_ascii_uppercase())
        {
            i += 2;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

#[derive(Default)]
struct Tally {
    scored: usize,
    passed: usize,
    equivalent: usize,
    host_lossy: usize,
    herdr_owned: usize,
}

struct Failure {
    host: &'static str,
    pane: &'static str,
    keystroke: String,
    expected: Vec<u8>,
    actual: Vec<u8>,
}

fn show(bytes: &[u8]) -> String {
    bytes.escape_ascii().to_string()
}

struct Report {
    by_cell: BTreeMap<(&'static str, &'static str, &'static str), Tally>,
    failures: Vec<Failure>,
    owned: Vec<String>,
}

fn run_keyboard_conformance() -> Report {
    let mut report = Report {
        by_cell: BTreeMap::new(),
        failures: Vec::new(),
        owned: Vec::new(),
    };
    for host in [HostProfile::Kitty, HostProfile::Legacy] {
        for &(pane_name, pane_mode) in PANE_MODES {
            let mut host_oracle = Oracle::new(host.setup(pane_mode));
            let mut direct_oracle = Oracle::new(pane_mode);

            let mut cases = Vec::new();
            for def in KEYS {
                for mods in 0u16..16 {
                    let mods = MODS
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mods & (1 << i) != 0)
                        .fold(0, |acc, (_, (bit, _))| acc | bit);
                    let host_bytes = host_oracle.keystroke(*def, mods);
                    let (mut press, release) = direct_oracle.keystroke(*def, mods);
                    if is_legacy_pane(pane_mode) && mods & ghostty::MOD_SUPER != 0 {
                        press = Vec::new();
                    } else if let Some(classic) = legacy_shell_exception(pane_mode, def.name, mods)
                    {
                        press = classic;
                    }
                    // A host that never reports releases gives nobody a release to forward.
                    let expected = if host_bytes.1.is_empty() {
                        press
                    } else {
                        [press, release].concat()
                    };
                    cases.push((*def, mods, host_bytes, expected));
                }
            }

            let mut expected_by_host_bytes: HashMap<&HostKeystroke, Vec<&Vec<u8>>> = HashMap::new();
            for (_, mods, host_bytes, expected) in &cases {
                if host == HostProfile::Legacy && mods & ghostty::MOD_SUPER != 0 {
                    continue;
                }
                expected_by_host_bytes
                    .entry(host_bytes)
                    .or_default()
                    .push(expected);
            }

            let mut herdr = HerdrPath::new(host, pane_mode);
            for (def, mods, host_bytes, expected) in &cases {
                let group = if mods & ghostty::MOD_SUPER != 0 {
                    "super"
                } else {
                    "core"
                };
                let tally = report
                    .by_cell
                    .entry((host.name(), pane_name, group))
                    .or_default();
                // Legacy hosts cannot report Super at all (they drop it or send
                // the chord without it), so nothing downstream can recover it.
                let super_unreportable =
                    host == HostProfile::Legacy && mods & ghostty::MOD_SUPER != 0;
                // Plain text names a character, not a key: which key and Shift state
                // produced punctuation depends on the layout. Only a letter's Shift
                // is layout independent (its case). Legacy hosts never report
                // Shift for non-letters; Kitty hosts send unmodified punctuation
                // presses as plain text.
                let non_letter_text = def.text.is_some_and(|(base, _)| !base.is_alphabetic());
                let plain_text_press =
                    host_bytes.0.len() == 1 && host_bytes.0[0].is_ascii_graphic();
                let layout_unreportable = non_letter_text
                    && mods & ghostty::MOD_SHIFT != 0
                    && (host == HostProfile::Legacy || plain_text_press);
                let ambiguous = expected_by_host_bytes
                    .get(host_bytes)
                    .is_some_and(|distinct| distinct.iter().any(|other| *other != expected));
                if super_unreportable || layout_unreportable || ambiguous {
                    tally.host_lossy += 1;
                    continue;
                }
                let press = herdr.feed(&host_bytes.0);
                let release = herdr.feed(&host_bytes.1);
                let page_key_scrolls_herdr = *mods == 0
                    && matches!(def.name, "pageup" | "pagedown")
                    && herdr.runtime.plain_page_keys_use_host_scrollback() == Some(true);
                let (Some(press), Some(release), false) = (press, release, page_key_scrolls_herdr)
                else {
                    report.owned.push(format!(
                        "{}\t{}\t{}+{}",
                        host.name(),
                        pane_name,
                        mods_name(*mods),
                        def.name
                    ));
                    tally.herdr_owned += 1;
                    herdr.reset_client();
                    continue;
                };
                let actual = [press, release].concat();
                tally.scored += 1;
                if &actual == expected {
                    tally.passed += 1;
                } else if normalize_kitty_press(&actual) == normalize_kitty_press(expected) {
                    tally.passed += 1;
                    tally.equivalent += 1;
                } else {
                    report.failures.push(Failure {
                        host: host.name(),
                        pane: pane_name,
                        keystroke: format!("{}+{}", mods_name(*mods), def.name),
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
        }
    }
    report
}

fn print_report(report: &Report) {
    let mut total = Tally::default();
    println!(
        "\n{:<12} {:<17} {:<6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "host", "pane", "keys", "scored", "passed", "score", "equiv", "lossy", "owned"
    );
    for ((host, pane, group), tally) in &report.by_cell {
        println!(
            "{:<12} {:<17} {:<6} {:>7} {:>7} {:>6.1}% {:>7} {:>7} {:>7}",
            host,
            pane,
            group,
            tally.scored,
            tally.passed,
            100.0 * tally.passed as f64 / tally.scored.max(1) as f64,
            tally.equivalent,
            tally.host_lossy,
            tally.herdr_owned
        );
        total.scored += tally.scored;
        total.passed += tally.passed;
        total.equivalent += tally.equivalent;
        total.host_lossy += tally.host_lossy;
        total.herdr_owned += tally.herdr_owned;
    }
    println!(
        "{:<37} {:>7} {:>7} {:>6.1}% {:>7} {:>7} {:>7}",
        "TOTAL",
        total.scored,
        total.passed,
        100.0 * total.passed as f64 / total.scored.max(1) as f64,
        total.equivalent,
        total.host_lossy,
        total.herdr_owned
    );

    if let Ok(path) = std::env::var("HERDR_INPUT_CONFORMANCE_FAILURES") {
        let lines: Vec<String> = report
            .failures
            .iter()
            .map(|failure| {
                format!(
                    "{}\t{}\t{}\texpected={}\tactual={}",
                    failure.host,
                    failure.pane,
                    failure.keystroke,
                    show(&failure.expected),
                    show(&failure.actual)
                )
            })
            .chain(report.owned.iter().map(|owned| format!("owned\t{owned}")))
            .collect();
        std::fs::write(path, lines.join("\n")).expect("write failures");
    }
}

// Ratchet: failure counts may only go down. Lower them when a change fixes
// cases; a rise means a regression in Herdr's input transparency.
// Remaining: Alt+], Alt+Shift+P/X, Alt+^, Alt+_ behind legacy hosts. Their bytes
// also start host replies, so they are never forwarded and the reply-tail
// discard can swallow the next keystroke (#344).
const KEYBOARD_FAILURES_BASELINE: usize = 60;
const MOUSE_FAILURES_BASELINE: usize = 96;
// Remaining: legacy hosts split right after ESC[, which is also Alt+[.
const SPLIT_IDLE_MISMATCH_BASELINE: usize = 207;

// ---------------------------------------------------------------------------
// Mouse
// ---------------------------------------------------------------------------

const MOUSE_TRACKING: &[(&str, &[u8])] = &[
    ("off", b""),
    ("x10", b"\x1b[?9h"),
    ("normal", b"\x1b[?1000h"),
    ("button", b"\x1b[?1002h"),
    ("any", b"\x1b[?1003h"),
];

const MOUSE_FORMATS: &[(&str, &[u8])] = &[
    ("default", b""),
    ("utf8", b"\x1b[?1005h"),
    ("sgr", b"\x1b[?1006h"),
    ("urxvt", b"\x1b[?1015h"),
];

/// What Herdr asks the host for while it captures the mouse.
const HOST_MOUSE_SETUP: &[u8] = b"\x1b[?1003h\x1b[?1006h";

#[derive(Clone, Copy)]
struct MouseStep {
    action: ffi::GhosttyMouseAction,
    button: Option<ffi::GhosttyMouseButton>,
    /// Offset added to the gesture's start cell.
    dx: u16,
    dy: u16,
}

const fn step(
    action: ffi::GhosttyMouseAction,
    button: Option<ffi::GhosttyMouseButton>,
    dx: u16,
    dy: u16,
) -> MouseStep {
    MouseStep {
        action,
        button,
        dx,
        dy,
    }
}

const GESTURES: &[(&str, &[MouseStep])] = &[
    (
        "click-left",
        &[
            step(
                ghostty::MOUSE_ACTION_PRESS,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                0,
                0,
            ),
            step(
                ghostty::MOUSE_ACTION_RELEASE,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                0,
                0,
            ),
        ],
    ),
    (
        "click-middle",
        &[
            step(
                ghostty::MOUSE_ACTION_PRESS,
                Some(ghostty::MOUSE_BUTTON_MIDDLE),
                0,
                0,
            ),
            step(
                ghostty::MOUSE_ACTION_RELEASE,
                Some(ghostty::MOUSE_BUTTON_MIDDLE),
                0,
                0,
            ),
        ],
    ),
    (
        "click-right",
        &[
            step(
                ghostty::MOUSE_ACTION_PRESS,
                Some(ghostty::MOUSE_BUTTON_RIGHT),
                0,
                0,
            ),
            step(
                ghostty::MOUSE_ACTION_RELEASE,
                Some(ghostty::MOUSE_BUTTON_RIGHT),
                0,
                0,
            ),
        ],
    ),
    (
        "drag-left",
        &[
            step(
                ghostty::MOUSE_ACTION_PRESS,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                0,
                0,
            ),
            step(
                ghostty::MOUSE_ACTION_MOTION,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                1,
                0,
            ),
            step(
                ghostty::MOUSE_ACTION_MOTION,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                2,
                1,
            ),
            step(
                ghostty::MOUSE_ACTION_RELEASE,
                Some(ghostty::MOUSE_BUTTON_LEFT),
                2,
                1,
            ),
        ],
    ),
    (
        "move",
        &[
            step(ghostty::MOUSE_ACTION_MOTION, None, 0, 0),
            step(ghostty::MOUSE_ACTION_MOTION, None, 1, 1),
        ],
    ),
    (
        "wheel-up",
        &[step(
            ghostty::MOUSE_ACTION_PRESS,
            Some(ghostty::MOUSE_BUTTON_WHEEL_UP),
            0,
            0,
        )],
    ),
    (
        "wheel-down",
        &[step(
            ghostty::MOUSE_ACTION_PRESS,
            Some(ghostty::MOUSE_BUTTON_WHEEL_DOWN),
            0,
            0,
        )],
    ),
    (
        "wheel-left",
        &[step(
            ghostty::MOUSE_ACTION_PRESS,
            Some(ghostty::MOUSE_BUTTON_WHEEL_LEFT),
            0,
            0,
        )],
    ),
    (
        "wheel-right",
        &[step(
            ghostty::MOUSE_ACTION_PRESS,
            Some(ghostty::MOUSE_BUTTON_WHEEL_RIGHT),
            0,
            0,
        )],
    ),
];

const MOUSE_MODS: [(u16, &str); 4] = [
    (0, "plain"),
    (ghostty::MOD_SHIFT, "shift"),
    (ghostty::MOD_ALT, "alt"),
    (ghostty::MOD_CTRL, "ctrl"),
];

struct MouseOracle {
    _terminal: ghostty::Terminal,
    encoder: ghostty::MouseEncoder,
}

impl MouseOracle {
    fn new(app_output: &[u8], cols: u16, rows: u16) -> Self {
        let mut terminal = ghostty::Terminal::new(cols, rows, 0).expect("terminal");
        terminal.write(app_output);
        let mut encoder = ghostty::MouseEncoder::new().expect("encoder");
        encoder.set_from_terminal(&terminal);
        encoder.set_size(u32::from(cols), u32::from(rows), 1, 1);
        Self {
            _terminal: terminal,
            encoder,
        }
    }

    fn encode(&mut self, step: MouseStep, mods: u16, column: u16, row: u16) -> Vec<u8> {
        let mut event = ghostty::MouseEvent::new().expect("mouse event");
        event.set_action(step.action);
        match step.button {
            Some(button) => event.set_button(button),
            None => event.clear_button(),
        }
        event.set_mods(mods);
        event.set_position(f32::from(column), f32::from(row));
        self.encoder.encode(&event).expect("encode mouse")
    }
}

const MOUSE_PANE_COLS: u16 = 40;
const MOUSE_PANE_ROWS: u16 = 10;

fn mouse_surface(mouse_reporting: bool) -> PaneSurfaceFrame {
    let mut surface = surface();
    let pane = &mut surface.panes[0];
    for rect in [&mut pane.rect, &mut pane.inner_rect] {
        rect.width = MOUSE_PANE_COLS;
        rect.height = MOUSE_PANE_ROWS;
    }
    pane.mouse_reporting = mouse_reporting;
    surface
}

fn run_mouse_conformance() -> Report {
    let mut report = Report {
        by_cell: BTreeMap::new(),
        failures: Vec::new(),
        owned: Vec::new(),
    };
    for &(tracking_name, tracking) in MOUSE_TRACKING {
        for &(format_name, format) in MOUSE_FORMATS {
            let pane_mode = [tracking, format].concat();
            let pane_name: &'static str =
                Box::leak(format!("{tracking_name}/{format_name}").into_boxed_str());
            let tally = report
                .by_cell
                .entry(("kitty-host", pane_name, "mouse"))
                .or_default();

            let mut herdr = HerdrPath::new(HostProfile::Kitty, &pane_mode);
            herdr
                .state
                .set_pane_surface(mouse_surface(!tracking.is_empty()));
            herdr.state.compose(106, 20).expect("composed frame");
            let inner = herdr.state.hits.panes[0].inner_rect;
            let mut direct = MouseOracle::new(&pane_mode, 80, 24);
            let mut host = MouseOracle::new(HOST_MOUSE_SETUP, 106, 20);

            let starts = [
                (0u16, 0u16),
                (inner.width / 2, inner.height / 2),
                (
                    inner.width.saturating_sub(3),
                    inner.height.saturating_sub(2),
                ),
            ];
            for &(gesture_name, steps) in GESTURES {
                for &(mods, mods_label) in &MOUSE_MODS {
                    for &(column, row) in &starts {
                        let mut expected = Vec::new();
                        let mut actual = Vec::new();
                        let mut owned = false;
                        for step in steps {
                            let (c, r) = (column + step.dx, row + step.dy);
                            expected.extend(direct.encode(*step, mods, c, r));
                            let host_bytes = host.encode(*step, mods, inner.x + c, inner.y + r);
                            match herdr.feed(&host_bytes) {
                                Some(bytes) => actual.extend(bytes),
                                None => owned = true,
                            }
                        }
                        if owned {
                            tally.herdr_owned += 1;
                            report.owned.push(format!(
                                "{pane_name}\t{mods_label}+{gesture_name}@{column},{row}"
                            ));
                            herdr.reset_client();
                            herdr
                                .state
                                .set_pane_surface(mouse_surface(!tracking.is_empty()));
                            herdr.state.compose(106, 20).expect("composed frame");
                            continue;
                        }
                        tally.scored += 1;
                        if actual == expected {
                            tally.passed += 1;
                        } else {
                            report.failures.push(Failure {
                                host: "kitty-host",
                                pane: pane_name,
                                keystroke: format!("{mods_label}+{gesture_name}@{column},{row}"),
                                expected,
                                actual,
                            });
                        }
                    }
                }
            }
        }
    }
    report
}

// ---------------------------------------------------------------------------
// Split reads
// ---------------------------------------------------------------------------

fn host_input_corpus() -> Vec<(HostProfile, Vec<u8>)> {
    let mut corpus = Vec::new();
    for (host, setup) in [
        (HostProfile::Kitty, &b"\x1b[>7u"[..]),
        (HostProfile::Kitty, &b"\x1b[>31u"[..]),
        (HostProfile::Legacy, &b""[..]),
    ] {
        let mut oracle = Oracle::new(setup);
        for def in KEYS {
            for mods in [
                0,
                ghostty::MOD_SHIFT,
                ghostty::MOD_CTRL,
                ghostty::MOD_ALT,
                ghostty::MOD_CTRL | ghostty::MOD_SHIFT,
                ghostty::MOD_CTRL | ghostty::MOD_ALT,
            ] {
                let (press, release) = oracle.keystroke(*def, mods);
                corpus.push((host, press));
                if !release.is_empty() {
                    corpus.push((host, release));
                }
            }
        }
    }
    let mut mouse = MouseOracle::new(HOST_MOUSE_SETUP, 300, 100);
    for &(_, steps) in GESTURES {
        for &(mods, _) in &MOUSE_MODS {
            for step in steps {
                corpus.push((HostProfile::Kitty, mouse.encode(*step, mods, 7, 3)));
                corpus.push((HostProfile::Kitty, mouse.encode(*step, mods, 150, 42)));
            }
        }
    }
    corpus.sort();
    corpus.dedup();
    corpus.retain(|(_, bytes)| bytes.len() > 1);
    corpus
}

/// Replies Herdr asks the host for at startup and on focus (colors, palette,
/// cell size, appearance).
const HOST_REPLIES: &[&[u8]] = &[
    b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\",
    b"\x1b]11;rgb:1e1e/1e1e/2e2e\x07",
    b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\",
    b"\x1b]4;1;rgb:cdcd/0000/0000\x1b\\",
    b"\x1b]4;255;rgb:eeee/eeee/eeee\x07",
    b"\x1b[6;20;10t",
    b"\x1b[?997;1n",
    b"\x1b[?997;2n",
];

/// A network hiccup between two reads of one sequence (SSH, slow links).
const SPLIT_PAUSE_MS: i32 = 50;

fn framed_events(host: HostProfile, pieces: &[&[u8]], pause_ms: Option<i32>) -> String {
    framed_events_with(host, pieces, pause_ms, false)
}

/// Feed `pieces` as separate reads. With `pause_ms`, the reads are that far
/// apart: like the client loop, the framer is flushed only when the pause
/// outlasts the wait it asked for.
fn framed_events_with(
    host: HostProfile,
    pieces: &[&[u8]],
    pause_ms: Option<i32>,
    awaiting_replies: bool,
) -> String {
    let mut framer = HerdrPath::fresh_framer(host);
    if awaiting_replies {
        // Mirrors the Unix client right after it sent its host queries.
        framer.host_color_query_sent();
        framer.enable_host_color_scheme_change_tracking();
        framer.enable_host_appearance_query_on_focus();
        framer.host_cell_size_query_sent();
    }
    let mut chunks = Vec::new();
    for (index, piece) in pieces.iter().enumerate() {
        chunks.extend(framer.push(piece));
        let Some(pause_ms) = pause_ms.filter(|_| index + 1 < pieces.len()) else {
            continue;
        };
        let mut waited = 0;
        for _ in 0..3 {
            let wait = framer.idle_flush_timeout_ms(true);
            if !framer.has_pending_input() || waited + wait > pause_ms {
                break;
            }
            waited += wait;
            chunks.extend(framer.flush_timeout());
        }
    }
    for _ in 0..3 {
        if !framer.has_pending_input() {
            break;
        }
        chunks.extend(framer.flush_timeout());
    }
    let events: Vec<_> = chunks
        .iter()
        .flat_map(|chunk| crate::raw_input::parse_raw_input_bytes_sync(chunk))
        .collect();
    format!("{events:?}")
}

// The keyboard, mouse and split-read checks model a Unix host sending terminal
// bytes. Windows input arrives as native records; see `windows_records`.
#[cfg(unix)]
#[test]
fn split_read_robustness() {
    let corpus = host_input_corpus();
    let mut splits = 0usize;
    let mut burst_mismatch = Vec::new();
    let mut idle_mismatch = Vec::new();
    for (host, bytes) in &corpus {
        let whole = framed_events(*host, &[bytes], None);
        for cut in 1..bytes.len() {
            splits += 1;
            let pieces = [&bytes[..cut], &bytes[cut..]];
            if framed_events(*host, &pieces, None) != whole {
                burst_mismatch.push(format!(
                    "{}\t{}|{}",
                    host.name(),
                    show(pieces[0]),
                    show(pieces[1])
                ));
            }
            if framed_events(*host, &pieces, Some(SPLIT_PAUSE_MS)) != whole {
                idle_mismatch.push(format!(
                    "{}\t{}|{}",
                    host.name(),
                    show(pieces[0]),
                    show(pieces[1])
                ));
            }
        }
    }
    let mut reply_splits = 0usize;
    let mut reply_burst_mismatch = Vec::new();
    let mut reply_idle_mismatch = Vec::new();
    for reply in HOST_REPLIES {
        let whole = framed_events_with(HostProfile::Kitty, &[reply], None, true);
        for cut in 1..reply.len() {
            reply_splits += 1;
            let pieces = [&reply[..cut], &reply[cut..]];
            let line = format!("reply\t{}|{}", show(pieces[0]), show(pieces[1]));
            if framed_events_with(HostProfile::Kitty, &pieces, None, true) != whole {
                reply_burst_mismatch.push(line.clone());
            }
            if framed_events_with(HostProfile::Kitty, &pieces, Some(SPLIT_PAUSE_MS), true) != whole
            {
                reply_idle_mismatch.push(line);
            }
        }
    }
    println!(
        "\nhost replies: {} replies, {} splits\n  burst (no pause): {} differ\n  {SPLIT_PAUSE_MS}ms pause:      {} differ",
        HOST_REPLIES.len(),
        reply_splits,
        reply_burst_mismatch.len(),
        reply_idle_mismatch.len()
    );

    let pct = |bad: usize| 100.0 * (splits - bad) as f64 / splits.max(1) as f64;
    println!(
        "\nsplit reads: {} sequences, {} splits\n  burst (no pause): {:.1}% identical ({} differ)\n  {SPLIT_PAUSE_MS}ms pause:      {:.1}% identical ({} differ)",
        corpus.len(),
        splits,
        pct(burst_mismatch.len()),
        burst_mismatch.len(),
        pct(idle_mismatch.len()),
        idle_mismatch.len()
    );
    if let Ok(path) = std::env::var("HERDR_INPUT_CONFORMANCE_FAILURES") {
        let lines: Vec<String> = burst_mismatch
            .iter()
            .chain(&reply_burst_mismatch)
            .map(|line| format!("burst\t{line}"))
            .chain(
                idle_mismatch
                    .iter()
                    .chain(&reply_idle_mismatch)
                    .map(|line| format!("idle\t{line}")),
            )
            .collect();
        std::fs::write(path, lines.join("\n")).expect("write failures");
    }
    assert!(
        burst_mismatch.is_empty() && reply_burst_mismatch.is_empty(),
        "split reads without a pause changed the decoded input"
    );
    assert!(
        idle_mismatch.len() <= SPLIT_IDLE_MISMATCH_BASELINE,
        "split-read robustness regressed: {} > {SPLIT_IDLE_MISMATCH_BASELINE}",
        idle_mismatch.len()
    );
    assert!(
        reply_idle_mismatch.is_empty(),
        "split host replies changed after a {SPLIT_PAUSE_MS}ms pause: {reply_idle_mismatch:?}"
    );
}

// ---------------------------------------------------------------------------
// Reporter captures: exact bytes and timing from real terminals in issues.
// ---------------------------------------------------------------------------

/// (issue, host, reads, gap between reads in ms, Herdr awaiting host replies)
type SplitCapture = (
    &'static str,
    HostProfile,
    &'static [&'static [u8]],
    i32,
    bool,
);

#[cfg(unix)]
#[test]
fn reporter_split_captures_decode_like_the_unsplit_input() {
    let captures: &[SplitCapture] = &[
        // Alacritty 0.17 on Windows over SSH: the space release split after
        // `ESC[32;1`, the tail arrived 36 ms later (client log in the issue).
        (
            "#4856",
            HostProfile::Kitty,
            &[b"\x1b[115;1:3u\x1b[32;1", b":3u"],
            36,
            false,
        ),
        // tmux 3.6 relays each OSC 4 palette reply from the outer terminal
        // with arbitrary gaps; the tail of `e4e4` gray entries leaked.
        (
            "#4025",
            HostProfile::Kitty,
            &[b"\x1b]4;254;rgb:e4", b"e4/e4e4/e4e4\x1b\\"],
            200,
            true,
        ),
        (
            "#4025",
            HostProfile::Legacy,
            &[b"\x1b", b"]4;254;rgb:e4e4/e4e4/e4e4\x07"],
            200,
            true,
        ),
    ];
    for &(issue, host, reads, gap_ms, awaiting) in captures {
        let whole = reads.concat();
        assert_eq!(
            framed_events_with(host, reads, Some(gap_ms), awaiting),
            framed_events_with(host, &[&whole], None, awaiting),
            "{issue}: {} read(s) {gap_ms}ms apart",
            reads.len()
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn ime_committed_text_reaches_every_pane_mode_exactly_once() {
    // IME commits (CJK, Korean syllables) arrive as plain UTF-8 text on every
    // host, with no key releases. Split reads can cut inside a character.
    // (Spaces between words are the space key, not part of a commit.)
    let text = "日本語한국어中文";
    let bytes = text.as_bytes();
    for host in [HostProfile::Kitty, HostProfile::Legacy] {
        for &(pane_name, pane_mode) in PANE_MODES {
            for cut in [None, Some(1), Some(4), Some(bytes.len() - 2)] {
                let mut herdr = HerdrPath::new(host, pane_mode);
                let got = match cut {
                    None => herdr.feed(bytes),
                    Some(cut) => {
                        let mut chunks = herdr.framer.push(&bytes[..cut]);
                        chunks.extend(herdr.framer.push(&bytes[cut..]));
                        for _ in 0..3 {
                            chunks.extend(herdr.framer.flush_timeout());
                        }
                        let outcomes = chunks
                            .iter()
                            .map(|chunk| herdr.state.handle_input_bytes(chunk))
                            .collect();
                        herdr.deliver(outcomes)
                    }
                }
                .expect("text reaches the pane");
                assert_eq!(
                    String::from_utf8_lossy(&got),
                    text,
                    "{} {pane_name} cut {cut:?}",
                    host.name()
                );
                // Losing focus must not replay releases for committed text.
                let after = herdr.feed(b"\x1b[O").unwrap_or_default();
                assert!(
                    !String::from_utf8_lossy(&after).contains(":3u"),
                    "{} {pane_name}: focus loss sent {}",
                    host.name(),
                    show(&after)
                );
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn lab_kitty_function_keys_reach_the_pane() {
    // #4403, recorded from kitty 0.47.1 through real key presses: with Herdr's
    // keyboard flags pushed, unmodified F1, F2 and F4 arrive as bare CSI P/Q/S.
    for (host_bytes, plain_shell) in [
        (&b"\x1b[P"[..], &b"\x1bOP"[..]),
        (b"\x1b[Q", b"\x1bOQ"),
        (b"\x1b[13~", b"\x1bOR"),
        (b"\x1b[S", b"\x1bOS"),
    ] {
        let mut herdr = HerdrPath::new(HostProfile::Kitty, b"");
        let got = herdr
            .feed(host_bytes)
            .expect("function key reaches the pane");
        assert_eq!(show(&got), show(plain_shell), "{}", show(host_bytes));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn reporter_text_key_release_reaches_kitty_event_pane() {
    // #4184: an app asking for event types gets typed-letter releases.
    // (pane mode, kitty host bytes for `a` press then release, pane bytes)
    for (pane_mode, press, want) in [
        // Event types only: Herdr keeps the host at flags 7, so the press
        // arrives as text and its release as a report (the reporter's capture).
        (&b"\x1b[>3u"[..], &b"a"[..], &b"a\x1b[97;1:3u"[..]),
        // All keys too: Herdr switches the host to report-all (31), so kitty
        // reports the press with its text.
        (b"\x1b[>11u", b"\x1b[97;;97u", b"\x1b[97u\x1b[97;1:3u"),
    ] {
        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        let press = herdr.feed(press).expect("press reaches the pane");
        let release = herdr
            .feed(b"\x1b[97;1:3u")
            .expect("release reaches the pane");
        assert_eq!(
            show(&[press, release].concat()),
            show(want),
            "{}",
            show(pane_mode)
        );
    }
}

/// A key on a non-US layout, as the Ghostty app reports it: the physical key
/// and the layout's (unshifted, shifted) text.
const fn layout_key(key: ffi::GhosttyKey, base: char, shifted: char) -> KeyDef {
    text("layout", key, base, shifted)
}

fn layout_keystroke_through_herdr(
    host: HostProfile,
    pane_mode: &[u8],
    def: KeyDef,
    mods: u16,
) -> (HostKeystroke, String, String) {
    let keystroke = Oracle::new(host.setup(pane_mode)).keystroke(def, mods);
    let (press, release) = Oracle::new(pane_mode).keystroke(def, mods);
    let direct = if keystroke.1.is_empty() {
        press
    } else {
        [press, release].concat()
    };
    let mut herdr = HerdrPath::new(host, pane_mode);
    let pane_press = herdr.feed(&keystroke.0).expect("press reaches the pane");
    let pane_release = herdr.feed(&keystroke.1).expect("release reaches the pane");
    (
        keystroke,
        show(&direct),
        show(&[pane_press, pane_release].concat()),
    )
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn layout_ctrl_chords_match_ghostty_direct() {
    // #1079. Keystrokes from a Kitty host on Russian, German, Turkish, French,
    // Nordic and Polish layouts, built like Ghostty builds them. `ghostty` is
    // what Ghostty sends the app directly (recorded from libghostty, and
    // checked against it below); `herdr` is what the pane gets through Herdr.
    use ffi::*;
    const CTRL: u16 = ghostty::MOD_CTRL;
    const ALT: u16 = ghostty::MOD_ALT;
    const SHIFT: u16 = ghostty::MOD_SHIFT;
    let ru_w = layout_key(GhosttyKey_GHOSTTY_KEY_W, '\u{446}', '\u{426}');
    let ru_c = layout_key(GhosttyKey_GHOSTTY_KEY_C, '\u{441}', '\u{421}');
    let ru_i = layout_key(GhosttyKey_GHOSTTY_KEY_I, '\u{448}', '\u{428}');
    let ru_bracket = layout_key(GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT, '\u{445}', '\u{425}');
    let de = layout_key(GhosttyKey_GHOSTTY_KEY_SEMICOLON, '\u{f6}', '\u{d6}');
    let tr = layout_key(GhosttyKey_GHOSTTY_KEY_I, '\u{131}', 'I');
    let fr = layout_key(GhosttyKey_GHOSTTY_KEY_DIGIT_2, '\u{e9}', '2');
    let no = layout_key(GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT, '\u{e5}', '\u{c5}');
    let pl = layout_key(GhosttyKey_GHOSTTY_KEY_E, '\u{119}', '\u{118}');
    let keys = [
        ("ru ctrl+w", ru_w, CTRL),
        ("ru ctrl+c", ru_c, CTRL),
        ("ru ctrl+i", ru_i, CTRL),
        ("ru ctrl+[", ru_bracket, CTRL),
        ("ru ctrl+shift+[", ru_bracket, CTRL | SHIFT),
        ("ru alt+w", ru_w, ALT),
        ("ru ctrl+alt+w", ru_w, CTRL | ALT),
        ("ru w", ru_w, 0),
        ("ru shift+w", ru_w, SHIFT),
        ("de ctrl+\u{f6}", de, CTRL),
        ("tr ctrl+\u{131}", tr, CTRL),
        ("fr ctrl+\u{e9}", fr, CTRL),
        ("no ctrl+\u{e5}", no, CTRL),
        ("pl altgr \u{119}", pl, CTRL | ALT),
    ];
    // (pane, key, ghostty, herdr)
    let table: &[(&str, &str, &str, &str)] = &[
        // Plain shell. Ctrl alone maps to the physical key's control byte
        // when Ghostty has one; Ctrl+I and Ctrl+[ keep the layout character.
        ("legacy", "ru ctrl+w", "\\x17", "\\x17"),
        ("legacy", "ru ctrl+c", "\\x03", "\\x03"),
        ("legacy", "ru ctrl+i", "\\x1b[1096;5u", "\\x1b[1096;5u"),
        ("legacy", "ru ctrl+[", "\\x1b[1093;5u", "\\x1b[1093;5u"),
        (
            "legacy",
            "ru ctrl+shift+[",
            "\\x1b[1061;5u",
            "\\x1b[1061;5u",
        ),
        ("legacy", "ru alt+w", "\\x1b\\xd1\\x86", "\\x1b\\xd1\\x86"),
        ("legacy", "ru w", "\\xd1\\x86", "\\xd1\\x86"),
        ("legacy", "ru shift+w", "\\xd0\\xa6", "\\xd0\\xa6"),
        ("legacy", "de ctrl+\u{f6}", "\\x1b[246;5u", "\\x1b[246;5u"),
        ("legacy", "tr ctrl+\u{131}", "\\x1b[305;5u", "\\x1b[305;5u"),
        ("legacy", "no ctrl+\u{e5}", "\\x1b[229;5u", "\\x1b[229;5u"),
        // Ghostty maps these by physical key too, but Ctrl+Alt can be AltGr
        // on Windows hosts and Latin keys are shortcuts of their own, so
        // Herdr keeps the layout character.
        ("legacy", "ru ctrl+alt+w", "\\x1b\\x17", "\\x1b[1094;7u"),
        ("legacy", "fr ctrl+\u{e9}", "\\x00", "\\x1b[233;5u"),
        ("legacy", "pl altgr \u{119}", "\\x1b\\x05", "\\x1b[281;7u"),
        // Kitty apps. A rewritten chord reaches them as the physical key, the
        // key the Kitty spec says to match shortcuts on.
        ("kitty1", "ru ctrl+w", "\\x1b[1094;5u", "\\x1b[119;5u"),
        ("kitty1", "ru ctrl+c", "\\x1b[1089;5u", "\\x1b[99;5u"),
        ("kitty1", "ru ctrl+i", "\\x1b[1096;5u", "\\x1b[1096;5u"),
        ("kitty1", "ru ctrl+[", "\\x1b[1093;5u", "\\x1b[1093;5u"),
        (
            "kitty1",
            "ru ctrl+shift+[",
            "\\x1b[1093;6u",
            "\\x1b[1093;6u",
        ),
        ("kitty1", "ru alt+w", "\\x1b[1094;3u", "\\x1b[1094;3u"),
        ("kitty1", "ru ctrl+alt+w", "\\x1b[1094;7u", "\\x1b[1094;7u"),
        ("kitty1", "ru w", "\\xd1\\x86", "\\xd1\\x86"),
        ("kitty1", "ru shift+w", "\\xd0\\xa6", "\\xd0\\xa6"),
        ("kitty1", "de ctrl+\u{f6}", "\\x1b[246;5u", "\\x1b[246;5u"),
        ("kitty1", "tr ctrl+\u{131}", "\\x1b[305;5u", "\\x1b[305;5u"),
        ("kitty1", "fr ctrl+\u{e9}", "\\x1b[233;5u", "\\x1b[233;5u"),
        ("kitty1", "no ctrl+\u{e5}", "\\x1b[229;5u", "\\x1b[229;5u"),
        ("kitty1", "pl altgr \u{119}", "\\x1b[281;7u", "\\x1b[281;7u"),
        (
            "kitty3",
            "ru ctrl+w",
            "\\x1b[1094;5u\\x1b[1094;5:3u",
            "\\x1b[119;5u\\x1b[119;5:3u",
        ),
        (
            "kitty3",
            "ru w",
            "\\xd1\\x86\\x1b[1094;1:3u",
            "\\xd1\\x86\\x1b[1094;1:3u",
        ),
        // Alternate keys: the pane input wire has no base-layout slot, so
        // Herdr drops that field for every non-ASCII key (as on master).
        ("kitty5", "ru ctrl+w", "\\x1b[1094::119;5u", "\\x1b[119;5u"),
        ("kitty5", "ru ctrl+c", "\\x1b[1089::99;5u", "\\x1b[99;5u"),
        (
            "kitty5",
            "ru ctrl+shift+[",
            "\\x1b[1093:1061:91;6u",
            "\\x1b[1093:1061;6u",
        ),
        ("kitty5", "ru alt+w", "\\x1b[1094::119;3u", "\\x1b[1094;3u"),
        ("kitty5", "ru w", "\\xd1\\x86", "\\xd1\\x86"),
        (
            "kitty5",
            "de ctrl+\u{f6}",
            "\\x1b[246::59;5u",
            "\\x1b[246;5u",
        ),
    ];
    let panes: HashMap<&str, &[u8]> = [
        ("legacy", &b""[..]),
        ("kitty1", b"\x1b[>1u"),
        ("kitty3", b"\x1b[>3u"),
        ("kitty5", b"\x1b[>5u"),
    ]
    .into_iter()
    .collect();
    for &(pane, name, ghostty, want) in table {
        let &(_, def, mods) = keys.iter().find(|(key, ..)| *key == name).unwrap();
        let (host, direct, got) =
            layout_keystroke_through_herdr(HostProfile::Kitty, panes[pane], def, mods);
        assert_eq!(direct, ghostty, "{pane} {name}: Ghostty direct");
        assert_eq!(got, want, "{pane} {name}: host sent {}", show(&host.0));
    }
    // Every key is covered in the plain shell and a Kitty app.
    for (name, ..) in keys {
        for pane in ["legacy", "kitty1"] {
            assert!(
                table.iter().any(|row| row.0 == pane && row.1 == name),
                "{pane} {name}"
            );
        }
    }

    // Plain Cyrillic typing from a host without the Kitty protocol.
    for (pane, pane_mode) in [("legacy", &b""[..]), ("kitty1", b"\x1b[>1u")] {
        for mods in [0, SHIFT] {
            let (_, direct, got) =
                layout_keystroke_through_herdr(HostProfile::Legacy, pane_mode, ru_w, mods);
            assert_eq!(got, direct, "legacy host {pane} {}", mods_name(mods));
        }
    }

    // Hosts that name no base-layout key: nothing is known about the physical
    // key, so the layout character goes through as reported.
    for (pane_mode, host_bytes, want) in [
        (&b""[..], &b"\x1b[1094;5u"[..], "\\x1b[1094;5u"),
        (b"\x1b[>1u", b"\x1b[1094;5u", "\\x1b[1094;5u"),
        (b"", b"\x1b[1094;3u", "\\x1b\\xd1\\x86"),
        (b"\x1b[>1u", b"\x1b[1089:1057;6u", "\\x1b[1089;6u"),
    ] {
        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        let got = herdr.feed(host_bytes).expect("reaches the pane");
        assert_eq!(show(&got), want, "{}", show(host_bytes));
    }

    // Ctrl let go before the key: the release names the layout character
    // without Ctrl, and is paired with the physical-key press.
    let mut herdr = HerdrPath::new(HostProfile::Kitty, b"\x1b[>3u");
    let press = herdr.feed(b"\x1b[1094::119;5u").expect("press");
    let release = herdr.feed(b"\x1b[1094::119;1:3u").expect("release");
    assert_eq!(
        show(&[press, release].concat()),
        "\\x1b[119;5u\\x1b[119;1:3u"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn layout_chord_releases_pair_when_modifiers_change_first() {
    // A modifier let go before the key changes whether the chord is Ctrl
    // alone, so the press and the release can name different keys (the layout
    // character or the physical key). The release must still reach the pane
    // as the key it saw pressed, and must not stay leased.
    use ffi::*;
    const CTRL: u16 = ghostty::MOD_CTRL;
    const ALT: u16 = ghostty::MOD_ALT;
    const SHIFT: u16 = ghostty::MOD_SHIFT;
    const PRESS: ffi::GhosttyKeyAction = ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS;
    const RELEASE: ffi::GhosttyKeyAction = ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE;
    let ru_w = layout_key(GhosttyKey_GHOSTTY_KEY_W, '\u{446}', '\u{426}');
    // (case, press mods, release mods, Ghostty direct, Herdr)
    let cases = [
        (
            "ctrl+shift, shift let go first",
            CTRL | SHIFT,
            CTRL,
            "\\x1b[1094;6u\\x1b[1094;5:3u",
            "\\x1b[1094;6u\\x1b[1094;5:3u",
        ),
        (
            "ctrl+alt, alt let go first",
            CTRL | ALT,
            CTRL,
            "\\x1b[1094;7u\\x1b[1094;5:3u",
            "\\x1b[1094;7u\\x1b[1094;5:3u",
        ),
        (
            "ctrl+shift, ctrl let go first",
            CTRL | SHIFT,
            SHIFT,
            "\\x1b[1094;6u\\x1b[1094;2:3u",
            "\\x1b[1094;6u\\x1b[1094;2:3u",
        ),
        // A Ctrl chord reaches Kitty apps as its physical key (see
        // `layout_ctrl_chords_match_ghostty_direct`); its release follows.
        (
            "ctrl, ctrl let go first",
            CTRL,
            0,
            "\\x1b[1094;5u\\x1b[1094;1:3u",
            "\\x1b[119;5u\\x1b[119;1:3u",
        ),
        (
            "ctrl, shift added before release",
            CTRL,
            CTRL | SHIFT,
            "\\x1b[1094;5u\\x1b[1094;6:3u",
            "\\x1b[119;5u\\x1b[119;6:3u",
        ),
    ];
    let pane_mode = b"\x1b[>3u";
    for (name, press_mods, release_mods, ghostty, want) in cases {
        let mut host = Oracle::new(HostProfile::Kitty.setup(pane_mode));
        let host_press = host.encode(ru_w, press_mods, PRESS);
        let host_release = host.encode(ru_w, release_mods, RELEASE);
        let mut direct = Oracle::new(pane_mode);
        let direct_bytes = [
            direct.encode(ru_w, press_mods, PRESS),
            direct.encode(ru_w, release_mods, RELEASE),
        ]
        .concat();
        assert_eq!(show(&direct_bytes), ghostty, "{name}: Ghostty direct");

        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        let press = herdr.feed(&host_press).expect("press reaches the pane");
        let release = herdr.feed(&host_release).expect("release reaches the pane");
        assert_eq!(
            show(&[press, release].concat()),
            want,
            "{name}: host sent {} {}",
            show(&host_press),
            show(&host_release)
        );
        // Nothing stays leased: losing focus releases no further key.
        let after = herdr.feed(b"\x1b[O").unwrap_or_default();
        assert!(
            !show(&after).contains(":3u"),
            "{name}: focus loss sent {}",
            show(&after)
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn overlapping_layout_and_us_keys_release_exactly() {
    // A US `w` held together with a Russian chord on the same physical key.
    // Each release must end its own press, in either order, however the
    // chord's modifiers changed in between.
    use ffi::*;
    const CTRL: u16 = ghostty::MOD_CTRL;
    const SHIFT: u16 = ghostty::MOD_SHIFT;
    const PRESS: ffi::GhosttyKeyAction = ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS;
    const RELEASE: ffi::GhosttyKeyAction = ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE;
    let us_w = *KEYS.iter().find(|def| def.name == "w").unwrap();
    let ru_w = layout_key(GhosttyKey_GHOSTTY_KEY_W, '\u{446}', '\u{426}');
    type Step = (KeyDef, u16, ffi::GhosttyKeyAction);
    // (case, steps, Ghostty direct, Herdr)
    let cases: [(&str, Vec<Step>, &str, &str); 4] = [
        (
            "ctrl+shift+\u{446}, shift let go, \u{446} then w released",
            vec![
                (us_w, 0, PRESS),
                (ru_w, CTRL | SHIFT, PRESS),
                (ru_w, CTRL, RELEASE),
                (us_w, CTRL, RELEASE),
            ],
            "w\\x1b[1094;6u\\x1b[1094;5:3u\\x1b[119;5:3u",
            "w\\x1b[1094;6u\\x1b[1094;5:3u\\x1b[119;5:3u",
        ),
        (
            "ctrl+shift+\u{446}, shift let go, w then \u{446} released",
            vec![
                (us_w, 0, PRESS),
                (ru_w, CTRL | SHIFT, PRESS),
                (us_w, CTRL, RELEASE),
                (ru_w, CTRL, RELEASE),
            ],
            "w\\x1b[1094;6u\\x1b[119;5:3u\\x1b[1094;5:3u",
            "w\\x1b[1094;6u\\x1b[119;5:3u\\x1b[1094;5:3u",
        ),
        // A Ctrl chord reaches Kitty apps as its physical key (see
        // `layout_ctrl_chords_match_ghostty_direct`); its release follows.
        (
            "ctrl+\u{446}, ctrl let go, \u{446} then w released",
            vec![
                (us_w, 0, PRESS),
                (ru_w, CTRL, PRESS),
                (ru_w, 0, RELEASE),
                (us_w, 0, RELEASE),
            ],
            "w\\x1b[1094;5u\\x1b[1094;1:3u\\x1b[119;1:3u",
            "w\\x1b[119;5u\\x1b[119;1:3u\\x1b[119;1:3u",
        ),
        (
            "ctrl+\u{446}, ctrl let go, w then \u{446} released",
            vec![
                (us_w, 0, PRESS),
                (ru_w, CTRL, PRESS),
                (us_w, 0, RELEASE),
                (ru_w, 0, RELEASE),
            ],
            "w\\x1b[1094;5u\\x1b[119;1:3u\\x1b[1094;1:3u",
            "w\\x1b[119;5u\\x1b[119;1:3u\\x1b[119;1:3u",
        ),
    ];
    let pane_mode = b"\x1b[>3u";
    for (name, steps, ghostty, want) in cases {
        let mut host = Oracle::new(HostProfile::Kitty.setup(pane_mode));
        let mut direct = Oracle::new(pane_mode);
        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        let mut direct_bytes = Vec::new();
        let mut pane_bytes = Vec::new();
        let mut host_bytes = Vec::new();
        for (def, mods, action) in steps {
            let report = host.encode(def, mods, action);
            direct_bytes.extend(direct.encode(def, mods, action));
            pane_bytes.extend(herdr.feed(&report).expect("reaches the pane"));
            host_bytes.push(show(&report));
        }
        assert_eq!(show(&direct_bytes), ghostty, "{name}: Ghostty direct");
        assert_eq!(show(&pane_bytes), want, "{name}: host sent {host_bytes:?}");
        // Nothing stays leased: losing focus releases no further key.
        let after = herdr.feed(b"\x1b[O").unwrap_or_default();
        assert!(
            !show(&after).contains(":3u"),
            "{name}: focus loss sent {}",
            show(&after)
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn goto_search_opens_on_shifted_slash_from_a_report_all_pane() {
    // #4963, recorded from Ghostty 1.3.1 with a Portuguese layout, where `/` is
    // Shift+7. With a plain pane Herdr keeps the host at flags 7 and the press
    // arrives as text; when the focused pane asks for all keys Herdr pushes 31
    // and the press arrives as a report carrying the shifted key and its text.
    for (pane_mode, press, release) in [
        (&b""[..], &b"/"[..], &b"\x1b[55:47;2:3u"[..]),
        (b"\x1b[>31u", b"\x1b[55:47;2;47u", b"\x1b[55:47;2:3u"),
    ] {
        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        herdr.state.open_navigator_overlay();
        assert_eq!(herdr.feed(press), None, "{}", show(pane_mode));
        assert_eq!(herdr.feed(release), None, "{}", show(pane_mode));
        assert!(
            matches!(
                &herdr.state.overlay,
                Some(ClientShellOverlay::Navigator(navigator)) if navigator.search_focused
            ),
            "goto search did not activate for pane {}",
            show(pane_mode)
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn goto_search_opens_only_for_a_typed_slash() {
    // (pane mode, host bytes, whether goto search opens)
    for (pane_mode, press, opens) in [
        // Unshifted `/` reported as a kitty key.
        (&b"\x1b[>31u"[..], &b"\x1b[47u"[..], true),
        // Shift+/ typing `?` without a shifted alternate is not `/`.
        (b"\x1b[>31u", b"\x1b[47;2;63u", false),
        // Shift+/ without text or alternate says nothing about `/`.
        (b"\x1b[>31u", b"\x1b[47;2u", false),
        // Shift+7 with a `/` alternate and no text is `/`.
        (b"\x1b[>31u", b"\x1b[55:47;2u", true),
        // Ctrl+/ and Alt+/ are not text.
        (b"\x1b[>31u", b"\x1b[47;5u", false),
        (b"\x1b[>31u", b"\x1b[47;3u", false),
        (b"", b"\x1b/", false),
    ] {
        let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
        herdr.state.open_navigator_overlay();
        let _ = herdr.feed(press);
        let focused = matches!(
            &herdr.state.overlay,
            Some(ClientShellOverlay::Navigator(navigator)) if navigator.search_focused
        );
        assert_eq!(focused, opens, "{}", show(press));
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn wezterm_escape_tap_reaches_the_pane_with_doubled_escape_preserved() {
    // #1266: WezTerm with `enable_kitty_keyboard` sends an Escape press as a
    // bare ESC and its release as `CSI 27;1:3u`. The macOS host policy keeps
    // legacy `ESC ESC` whole, which used to swallow quick taps.
    const PRESS: &[u8] = b"\x1b";
    const RELEASE: &[u8] = b"\x1b[27;1:3u";
    let mouse: &[u8] = b"\x1b[?1000h\x1b[?1006h";
    for (pane_mode, want) in [
        (&b""[..], &b"\x1b"[..]),
        (mouse, b"\x1b"),
        (b"\x1b[>1u", b"\x1b[27u"),
    ] {
        for (reads, label) in [
            (vec![[PRESS, RELEASE].concat()], "one read"),
            (vec![PRESS.to_vec(), RELEASE.to_vec()], "two reads"),
        ] {
            let mut herdr = HerdrPath::new(HostProfile::Kitty, pane_mode);
            herdr.framer = crate::raw_input::RawInputByteFramer::with_host_input_policy(true);
            herdr.framer.set_host_escape_disambiguation_active(true);
            let mut chunks = Vec::new();
            for read in &reads {
                chunks.extend(herdr.framer.push(read));
            }
            for _ in 0..3 {
                chunks.extend(herdr.framer.flush_timeout());
            }
            let outcomes = chunks
                .iter()
                .map(|chunk| herdr.state.handle_input_bytes(chunk))
                .collect();
            let got = herdr.deliver(outcomes).expect("escape reaches the pane");
            assert_eq!(show(&got), show(want), "{label}, pane {}", show(pane_mode));
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn mouse_transparency_conformance() {
    let report = run_mouse_conformance();
    print_report(&report);
    assert!(
        report.failures.len() <= MOUSE_FAILURES_BASELINE,
        "mouse transparency regressed: {} failures > {MOUSE_FAILURES_BASELINE}",
        report.failures.len()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn keyboard_transparency_conformance() {
    let report = run_keyboard_conformance();
    print_report(&report);
    assert!(
        report.failures.len() <= KEYBOARD_FAILURES_BASELINE,
        "keyboard transparency regressed: {} failures > {KEYBOARD_FAILURES_BASELINE}",
        report.failures.len()
    );
}

// ---------------------------------------------------------------------------
// Windows: native key records
// ---------------------------------------------------------------------------
//
// In Windows Terminal each keypress reaches ConPTY as a native key record
// (win32-input-mode). Herdr's pane is a ConPTY too, so for a pane that
// negotiated no keyboard protocol, transparency means the pane's ConPTY gets
// the same records Herdr received. Records are built from the real keyboard
// layouts with ToUnicodeEx, the way Windows produces them.

#[cfg(windows)]
mod windows_records {
    use super::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardLayout, LoadKeyboardLayoutW, MapVirtualKeyExW, ToUnicodeEx, HKL,
        KLF_NOTELLSHELL, MAPVK_VK_TO_VSC,
    };

    const RIGHT_ALT_PRESSED: u32 = 0x0001;
    const LEFT_ALT_PRESSED: u32 = 0x0002;
    const LEFT_CTRL_PRESSED: u32 = 0x0008;
    const SHIFT_PRESSED: u32 = 0x0010;
    const ENHANCED_KEY: u32 = 0x0100;

    const VK_SHIFT: usize = 0x10;
    const VK_CONTROL: usize = 0x11;
    const VK_MENU: usize = 0x12;
    const VK_LSHIFT: usize = 0xa0;
    const VK_LCONTROL: usize = 0xa2;
    const VK_LMENU: usize = 0xa4;
    const VK_RMENU: usize = 0xa5;

    const LAYOUTS: &[(&str, &str)] = &[("us", "00000409"), ("de", "00000407")];

    #[derive(Clone, Copy)]
    struct Chord {
        name: &'static str,
        shift: bool,
        ctrl: bool,
        left_alt: bool,
        /// AltGr: Windows reports it as Right Alt plus Left Ctrl.
        alt_gr: bool,
    }

    const fn chord(
        name: &'static str,
        shift: bool,
        ctrl: bool,
        left_alt: bool,
        alt_gr: bool,
    ) -> Chord {
        Chord {
            name,
            shift,
            ctrl,
            left_alt,
            alt_gr,
        }
    }

    const CHORDS: &[Chord] = &[
        chord("plain", false, false, false, false),
        chord("shift", true, false, false, false),
        chord("ctrl", false, true, false, false),
        chord("alt", false, false, true, false),
        chord("ctrl+shift", true, true, false, false),
        chord("alt+shift", true, false, true, false),
        chord("ctrl+alt", false, true, true, false),
        chord("altgr", false, false, false, true),
        chord("altgr+shift", true, false, false, true),
    ];

    fn virtual_keys() -> Vec<(String, u16, bool)> {
        let mut keys: Vec<(String, u16, bool)> = Vec::new();
        for vk in (0x41..=0x5a).chain(0x30..=0x39) {
            keys.push((format!("vk{vk:02x}"), vk, false));
        }
        for vk in [
            0xba, 0xbb, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0, 0xdb, 0xdc, 0xdd, 0xde, 0xe2,
        ] {
            keys.push((format!("oem{vk:02x}"), vk, false));
        }
        for (name, vk) in [
            ("space", 0x20),
            ("enter", 0x0d),
            ("tab", 0x09),
            ("backspace", 0x08),
            ("escape", 0x1b),
        ] {
            keys.push((name.to_owned(), vk, false));
        }
        for (name, vk) in [
            ("pageup", 0x21),
            ("pagedown", 0x22),
            ("end", 0x23),
            ("home", 0x24),
            ("left", 0x25),
            ("up", 0x26),
            ("right", 0x27),
            ("down", 0x28),
            ("insert", 0x2d),
            ("delete", 0x2e),
        ] {
            keys.push((name.to_owned(), vk, true));
        }
        for n in 0..12u16 {
            keys.push((format!("f{}", n + 1), 0x70 + n, false));
        }
        keys
    }

    /// Sessions without a desktop (SSH) can only use the current layout.
    fn load_layout(klid: &str) -> Option<HKL> {
        let wide: Vec<u16> = klid.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated layout id that outlives the call.
        let loaded = unsafe { LoadKeyboardLayoutW(wide.as_ptr(), KLF_NOTELLSHELL) };
        if !loaded.is_null() {
            return Some(loaded);
        }
        // SAFETY: querying the calling thread's layout has no preconditions.
        let current = unsafe { GetKeyboardLayout(0) };
        let current_id = (current as usize & 0xffff) as u32;
        (u32::from_str_radix(klid, 16).ok()? & 0xffff == current_id).then_some(current)
    }

    /// The press and release records Windows produces for `vk` under `chord`,
    /// or None for multi-unit output.
    fn key_records(
        layout: HKL,
        vk: u16,
        enhanced: bool,
        chord: Chord,
    ) -> Option<(
        crate::input::WindowsKeyRecord,
        crate::input::WindowsKeyRecord,
    )> {
        // SAFETY: pure layout lookup.
        let scan = unsafe { MapVirtualKeyExW(u32::from(vk), MAPVK_VK_TO_VSC, layout) } as u16;
        let mut state = [0u8; 256];
        let mut control_key_state = if enhanced { ENHANCED_KEY } else { 0 };
        if chord.shift {
            state[VK_SHIFT] = 0x80;
            state[VK_LSHIFT] = 0x80;
            control_key_state |= SHIFT_PRESSED;
        }
        if chord.ctrl || chord.alt_gr {
            state[VK_CONTROL] = 0x80;
            state[VK_LCONTROL] = 0x80;
            control_key_state |= LEFT_CTRL_PRESSED;
        }
        if chord.left_alt {
            state[VK_MENU] = 0x80;
            state[VK_LMENU] = 0x80;
            control_key_state |= LEFT_ALT_PRESSED;
        }
        if chord.alt_gr {
            state[VK_MENU] = 0x80;
            state[VK_RMENU] = 0x80;
            control_key_state |= RIGHT_ALT_PRESSED;
        }
        let mut buf = [0u16; 8];
        // SAFETY: buffers are valid for the given lengths. Flag 0x4 leaves the
        // kernel dead-key state untouched.
        let written = unsafe {
            ToUnicodeEx(
                u32::from(vk),
                u32::from(scan),
                state.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as i32,
                0x4,
                layout,
            )
        };
        let unicode = match written {
            0 => 0,
            1 => buf[0],
            // A dead key has not committed a character yet.
            n if n < 0 => 0,
            _ => return None,
        };
        let press = crate::input::WindowsKeyRecord {
            key_down: true,
            repeat_count: 1,
            virtual_key_code: vk,
            virtual_scan_code: scan,
            unicode,
            control_key_state,
        };
        Some((
            press,
            crate::input::WindowsKeyRecord {
                key_down: false,
                ..press
            },
        ))
    }

    fn win32_input_mode(record: crate::input::WindowsKeyRecord) -> Vec<u8> {
        format!(
            "\x1b[{};{};{};{};{};{}_",
            record.virtual_key_code,
            record.virtual_scan_code,
            record.unicode,
            u8::from(record.key_down),
            record.control_key_state,
            record.repeat_count
        )
        .into_bytes()
    }

    fn deliver_record(
        herdr: &mut HerdrPath,
        input: &mut crate::client::input::windows_vti::TestWindowsInput,
        record: crate::input::WindowsKeyRecord,
        layout: HKL,
    ) -> Option<Vec<u8>> {
        // The generated record belongs to this layout, independently of the
        // foreground window's layout while the test runs.
        let mut events = input.key_in_layout(record, layout);
        events.extend(input.idle());
        let outcome = herdr.state.handle_client_events(&events);
        herdr.deliver(vec![outcome])
    }

    fn run() -> Report {
        let mut report = Report {
            by_cell: BTreeMap::new(),
            failures: Vec::new(),
            owned: Vec::new(),
        };
        for &(layout_name, klid) in LAYOUTS {
            let Some(layout) = load_layout(klid) else {
                println!(
                    "keyboard layout {layout_name} ({klid}) unavailable in this session; skipped"
                );
                continue;
            };
            let mut herdr = HerdrPath::new(HostProfile::Legacy, b"");
            let mut input = crate::client::input::windows_vti::TestWindowsInput::default();
            for (key_name, vk, enhanced) in virtual_keys() {
                for chord in CHORDS {
                    let tally = report
                        .by_cell
                        .entry(("windows", layout_name, chord.name))
                        .or_default();
                    let Some((press, release)) = key_records(layout, vk, enhanced, *chord) else {
                        tally.host_lossy += 1;
                        continue;
                    };
                    let expected = [win32_input_mode(press), win32_input_mode(release)].concat();
                    let pressed = deliver_record(&mut herdr, &mut input, press, layout);
                    let released = deliver_record(&mut herdr, &mut input, release, layout);
                    let page_key_scrolls_herdr = chord.name == "plain"
                        && matches!(key_name.as_str(), "pageup" | "pagedown")
                        && herdr.runtime.plain_page_keys_use_host_scrollback() == Some(true);
                    let (Some(pressed), Some(released), false) =
                        (pressed, released, page_key_scrolls_herdr)
                    else {
                        tally.herdr_owned += 1;
                        herdr.reset_client();
                        input = Default::default();
                        continue;
                    };
                    let actual = [pressed, released].concat();
                    tally.scored += 1;
                    if actual == expected {
                        tally.passed += 1;
                    } else {
                        report.failures.push(Failure {
                            host: "windows",
                            pane: layout_name,
                            keystroke: format!(
                                "{}+{key_name} vk={vk:#x} uc={:#x}",
                                chord.name, press.unicode
                            ),
                            expected,
                            actual,
                        });
                    }
                }
            }
        }
        report
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn windows_key_record_transparency() {
        let report = run();
        print_report(&report);
        assert!(
            report.failures.len() <= WINDOWS_RECORD_FAILURES_BASELINE,
            "windows key record transparency regressed: {} failures > {WINDOWS_RECORD_FAILURES_BASELINE}",
            report.failures.len()
        );
    }

    // Ratchet (may only go down). Remaining: Ctrl+[ is forwarded as a plain
    // Escape record rather than its own key record.
    const WINDOWS_RECORD_FAILURES_BASELINE: usize = 1;
}
