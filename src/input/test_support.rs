use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

#[derive(Debug)]
pub(crate) struct KeyboardCorpusCase<'a> {
    pub family: &'a str,
    pub input: Vec<u8>,
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeyEventKind,
    pub shifted_codepoint: Option<u32>,
    pub base_layout_codepoint: Option<u32>,
    pub generated_text: Option<String>,
    pub pane_profile: &'a str,
    pub expected_pane_hex: &'a str,
}

impl KeyboardCorpusCase<'_> {
    pub fn assert_key_semantics(&self, key: &crate::input::TerminalKey) {
        assert_eq!(key.code, self.code, "{} code", self.family);
        assert_eq!(key.modifiers, self.modifiers, "{} modifiers", self.family);
        assert_eq!(key.kind, self.kind, "{} kind", self.family);
        assert_eq!(key.repeat_count, 1, "{} repeat count", self.family);
        assert_eq!(
            key.shifted_codepoint, self.shifted_codepoint,
            "{} shifted codepoint",
            self.family
        );
        assert_eq!(
            key.base_layout_codepoint, self.base_layout_codepoint,
            "{} base layout codepoint",
            self.family
        );
    }
}

pub(crate) fn keyboard_corpus_cases(corpus: &str) -> Vec<KeyboardCorpusCase<'_>> {
    corpus
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }

            let columns: Vec<_> = line.split('\t').collect();
            assert!(
                matches!(columns.len(), 9 | 10),
                "keyboard corpus row must have 9 or 10 columns: {line}"
            );

            Some(KeyboardCorpusCase {
                family: columns[0],
                input: decode_hex(columns[1]),
                code: parse_key_code(columns[2]),
                modifiers: parse_modifiers(columns[3]),
                kind: parse_kind(columns[4]),
                shifted_codepoint: (!columns[5].is_empty())
                    .then(|| columns[5].parse::<u32>().expect("shifted codepoint")),
                base_layout_codepoint: columns
                    .get(9)
                    .filter(|field| !field.is_empty())
                    .map(|field| field.parse::<u32>().expect("base layout codepoint")),
                generated_text: (!columns[6].is_empty() && columns[6] != "-").then(|| {
                    String::from_utf8(decode_hex(columns[6])).expect("generated text must be UTF-8")
                }),
                pane_profile: columns[7],
                expected_pane_hex: columns[8],
            })
        })
        .collect()
}

pub(crate) fn decode_hex(hex: &str) -> Vec<u8> {
    if hex == "empty" {
        return Vec::new();
    }
    assert_eq!(hex.len() % 2, 0, "hex string must have even length: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|idx| u8::from_str_radix(&hex[idx..idx + 2], 16).expect("hex byte"))
        .collect()
}

fn parse_key_code(value: &str) -> KeyCode {
    match value {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "esc" => KeyCode::Esc,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert,
        "delete" => KeyCode::Delete,
        value if value.starts_with("char:") => {
            let mut chars = value.trim_start_matches("char:").chars();
            let ch = chars.next().expect("fixture character");
            assert!(
                chars.next().is_none(),
                "fixture must contain one character: {value}"
            );
            KeyCode::Char(ch)
        }
        value if value.starts_with("f:") => KeyCode::F(
            value
                .trim_start_matches("f:")
                .parse::<u8>()
                .expect("fixture function key"),
        ),
        other => panic!("unsupported fixture key code: {other}"),
    }
}

fn parse_modifiers(value: &str) -> KeyModifiers {
    if value == "-" || value.is_empty() {
        return KeyModifiers::empty();
    }

    let mut modifiers = KeyModifiers::empty();
    for part in value.split('+') {
        modifiers |= match part {
            "shift" => KeyModifiers::SHIFT,
            "alt" => KeyModifiers::ALT,
            "control" => KeyModifiers::CONTROL,
            "super" => KeyModifiers::SUPER,
            "hyper" => KeyModifiers::HYPER,
            "meta" => KeyModifiers::META,
            other => panic!("unsupported fixture modifier: {other}"),
        };
    }
    modifiers
}

fn parse_kind(value: &str) -> KeyEventKind {
    match value {
        "press" => KeyEventKind::Press,
        "repeat" => KeyEventKind::Repeat,
        "release" => KeyEventKind::Release,
        other => panic!("unsupported fixture kind: {other}"),
    }
}

#[test]
fn blank_generated_text_fixture_field_is_absent() {
    let cases = keyboard_corpus_cases("legacy\t61\tchar:a\t-\tpress\t\t\tlegacy\t61");

    assert_eq!(cases.len(), 1);
    assert_eq!(cases[0].generated_text, None);
}
