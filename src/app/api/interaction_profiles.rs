//! Conservative opt-in recognizer for the owned Claude Code2.1.284 capture.
//! Input compilation validates the complete final dialog, never caller supplied terminal keys.
use crate::api::schema::{InteractionAction, InteractionDialog, InteractionOption};
const CUSTOM_FOOTER: &str =
    "Enter to select · ↑/↓ to navigate · ctrl+g to edit in nano · Esc to cancel";
const FOOTER: &str = "Enter to select · ↑/↓ to navigate · Esc to cancel";

pub(super) fn enabled() -> bool {
    std::env::var("HERDR_GUARDED_CLAUDE_PROFILE").as_deref() == Ok("2.1.284-custom-v1-experimental")
}
fn rule(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 60 && line.chars().all(|c| c == '─')
}
pub(super) fn recognize(text: &str, ansi: &str) -> Option<InteractionDialog> {
    if !text.contains("Claude Code v2.1.284") {
        return None;
    }
    let lines: Vec<_> = text.trim_end().lines().collect();
    let custom_phase = match lines.last()?.trim() {
        FOOTER => false,
        CUSTOM_FOOTER => true,
        _ => return None,
    };
    let end = lines.len() - 1;
    let header = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("☐ "))?;
    if lines
        .iter()
        .filter(|l| l.trim_start().starts_with("☐ "))
        .count()
        != 1
        || header == 0
        || !rule(lines[header - 1])
    {
        return None;
    }
    if lines.get(header + 1)?.trim() != "" || lines.get(header + 3)?.trim() != "" {
        return None;
    }
    let question = lines.get(header + 2)?.trim();
    if question.is_empty() {
        return None;
    }
    let mut options = Vec::new();
    let mut selected = None;
    let mut custom = false;
    let mut lower_rule = false;
    let mut chat = false;
    for line in &lines[header + 4..end] {
        if line.trim().is_empty() {
            continue;
        }
        if rule(line) {
            if lower_rule || !custom {
                return None;
            }
            lower_rule = true;
            continue;
        }
        let trimmed = line.trim_start();
        if lower_rule {
            if trimmed != "5. Chat about this" || chat {
                return None;
            }
            chat = true;
            continue;
        }
        let active = trimmed.starts_with("❯ ");
        let row = if active {
            trimmed.strip_prefix("❯ ")?
        } else {
            trimmed
        };
        if let Some((number, label)) = row.split_once(". ") {
            if let Ok(number) = number.parse::<usize>() {
                if number != options.len() + 1 || number > 4 || label.is_empty() {
                    return None;
                }
                if active {
                    if selected.is_some() {
                        return None;
                    }
                    selected = Some(number);
                }
                let is_custom = number == 4;
                if is_custom && label != "Type something." {
                    return None;
                }
                custom |= is_custom;
                options.push(InteractionOption {
                    option_id: format!("choice-{number}"),
                    label: label.into(),
                    custom: is_custom,
                });
                continue;
            }
        }
        // Only indented suggestion descriptions are allowed, never arbitrary prompts or rows.
        if custom || options.is_empty() || !line.starts_with("     ") || active {
            return None;
        }
    }
    if options.len() != 4 || !lower_rule || !chat {
        return None;
    }
    if custom_phase != (selected == Some(4)) || !empty_custom_style(ansi, custom_phase) {
        return None;
    }
    Some(InteractionDialog {
        profile: "claude-2.1.284-custom-v1-experimental".into(),
        phase: if custom_phase {
            "custom_entry"
        } else {
            "choose"
        }
        .into(),
        question: question.into(),
        options,
        selected_option_id: format!("choice-{}", selected?),
        supported_actions: vec![if custom_phase {
            "submit_custom"
        } else {
            "begin_custom"
        }
        .into()],
    })
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum CompiledInteraction {
    Key(crossterm::event::KeyEvent),
    CustomText(String),
}

pub(super) fn compile(
    dialog: &InteractionDialog,
    action: &InteractionAction,
) -> Result<CompiledInteraction, &'static str> {
    match action {
        InteractionAction::BeginCustom { option_id } => {
            if dialog.phase != "choose" || dialog.selected_option_id == "choice-4" {
                return Err("unsupported_interaction_phase");
            }
            let row = dialog
                .options
                .iter()
                .find(|o| &o.option_id == option_id)
                .ok_or("unknown_option")?;
            if !row.custom || row.option_id != "choice-4" {
                return Err("wrong_option_kind");
            }
            Ok(CompiledInteraction::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('4'),
                crossterm::event::KeyModifiers::NONE,
            )))
        }
        InteractionAction::SubmitCustom { text, .. } => {
            if dialog.phase != "custom_entry" || dialog.selected_option_id != "choice-4" {
                return Err("unsupported_interaction_phase");
            }
            if text.len() > 4096 || text.trim().is_empty() || text.chars().any(char::is_control) {
                return Err("invalid_custom_text");
            }
            Ok(CompiledInteraction::CustomText(text.clone()))
        }
        _ => Err("unsupported_interaction_phase"),
    }
}

#[derive(Clone, Copy, Default)]
struct Style {
    dim: bool,
    inverse: bool,
    foreground: Option<[u16; 3]>,
}
fn styled_chars(line: &str) -> Option<Vec<(char, Style)>> {
    let mut chars = line.chars().peekable();
    let mut style = Style::default();
    let mut out = Vec::new();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            continue;
        }
        if ch != '\x1b' {
            out.push((ch, style));
            continue;
        }
        if chars.next()? != '[' {
            return None;
        }
        let mut codes = String::new();
        loop {
            let c = chars.next()?;
            if c == 'm' {
                break;
            }
            if !c.is_ascii_digit() && c != ';' {
                return None;
            }
            codes.push(c);
        }
        let codes: Vec<u16> = if codes.is_empty() {
            vec![0]
        } else {
            codes
                .split(';')
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?
        };
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => style = Style::default(),
                1 => {}
                2 => style.dim = true,
                22 => style.dim = false,
                7 => style.inverse = true,
                27 => style.inverse = false,
                39 => style.foreground = None,
                49 => {}
                38 | 48 => {
                    let foreground = codes[i] == 38;
                    match *codes.get(i + 1)? {
                        2 => {
                            let rgb = [
                                u8::try_from(*codes.get(i + 2)?).ok()?,
                                u8::try_from(*codes.get(i + 3)?).ok()?,
                                u8::try_from(*codes.get(i + 4)?).ok()?,
                            ];
                            if foreground {
                                style.foreground = Some(rgb.map(u16::from))
                            }
                            i += 4;
                        }
                        5 => {
                            codes.get(i + 2)?;
                            if foreground {
                                style.foreground = Some([256, *codes.get(i + 2)?, 0])
                            }
                            i += 2;
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
            i += 1;
        }
    }
    Some(out)
}
/// Exact captured empty placeholder styles discriminate a same-text nonempty draft. These
/// styles are version/theme scoped; absent or changed styling disables this experimental profile.
fn empty_custom_style(ansi: &str, focused: bool) -> bool {
    // Bind the last captured custom row; never fall back to an older styled row when the
    // current row is filled, malformed, or has an unsupported style.
    let Some(line) = ansi.lines().rev().find(|line| line.contains("4. ")) else {
        return false;
    };
    {
        let Some(chars) = styled_chars(line) else {
            return false;
        };
        let text: String = chars.iter().map(|(c, _)| c).collect();
        let expected = if focused {
            "❯ 4. Type something."
        } else {
            "4. Type something."
        };
        if text.trim() != expected {
            return false;
        }
        let Some(offset) = text.find("Type something.") else {
            return false;
        };
        let index = text[..offset].chars().count();
        let label = &chars[index..];
        if focused {
            label[0].1.inverse
                && !label[0].1.dim
                && label[0].1.foreground.is_none()
                && label[1..]
                    .iter()
                    .all(|(_, s)| s.dim && !s.inverse && s.foreground.is_none())
        } else {
            label
                .iter()
                .all(|(_, s)| !s.dim && !s.inverse && s.foreground == Some([153, 153, 153]))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn capture() -> String {
        format!("Claude Code v2.1.284\n{}\n ☐ Format\n\nWhat format?\n\n❯ 1. Café\n     Description\n  2. Video call\n     Description\n  3. Async thread\n     Description\n  4. Type something.\n{}\n  5. Chat about this\n\n{FOOTER}\n","─".repeat(80),"─".repeat(80))
    }
    fn initial_ansi() -> String {
        "\x1b[38;2;153;153;153m  4. Type something.\x1b[0m".into()
    }
    fn custom_ansi() -> String {
        "❯ 4. \x1b[7mT\x1b[0m\x1b[2mype something.\x1b[0m".into()
    }
    fn custom_text() -> String {
        capture()
            .replace("❯ 1.", "  1.")
            .replace("  4.", "❯ 4.")
            .replace(FOOTER, CUSTOM_FOOTER)
    }
    #[test]
    fn interaction_custom_phase_requires_exact_empty_styles_and_bounded_text() {
        let text = custom_text();
        let dialog = recognize(&text, &custom_ansi()).unwrap();
        assert_eq!(dialog.phase, "custom_entry");
        assert!(
            recognize(&text, "❯ 4. \x1b[7mT\x1b[0mype something.").is_none(),
            "same-text draft must reject"
        );
        assert!(
            recognize(&capture(), "  4. Type something.").is_none(),
            "unfocused same-text draft must reject"
        );
        for bad in [
            "",
            "  ",
            "bad\nanswer",
            "bad\ranswer",
            "bad\x1b[201~",
            "bad\x7f",
            &"a".repeat(4097),
        ] {
            assert_eq!(
                compile(
                    &dialog,
                    &InteractionAction::SubmitCustom {
                        text: bad.into(),
                        parent_operation_id: "parent".into()
                    }
                ),
                Err("invalid_custom_text")
            );
        }
        assert_eq!(
            compile(
                &dialog,
                &InteractionAction::SubmitCustom {
                    text: "A library circle with optional video dial-in.".into(),
                    parent_operation_id: "parent".into()
                }
            ),
            Ok(CompiledInteraction::CustomText(
                "A library circle with optional video dial-in.".into()
            ))
        );
    }
    #[test]
    fn interaction_style_guard_never_falls_back_to_an_older_empty_row() {
        let ansi = format!("{}\n❯ 4. \x1b[7mT\x1b[0mype something.", custom_ansi());
        assert!(recognize(&custom_text(), &ansi).is_none());
    }
    #[test]
    fn interaction_claude_initial_profile_compiles_only_typed_verified_rows() {
        let dialog = recognize(&capture(), &initial_ansi()).unwrap();
        assert_eq!(
            compile(
                &dialog,
                &InteractionAction::BeginCustom {
                    option_id: "choice-4".into()
                }
            )
            .unwrap(),
            CompiledInteraction::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('4'),
                crossterm::event::KeyModifiers::NONE
            ))
        );
        assert_eq!(
            compile(
                &dialog,
                &InteractionAction::Choose {
                    option_id: "choice-2".into()
                }
            ),
            Err("unsupported_interaction_phase")
        );
        assert!(compile(
            &dialog,
            &InteractionAction::Choose {
                option_id: "choice-4".into()
            }
        )
        .is_err());
        assert!(compile(
            &dialog,
            &InteractionAction::FreeText {
                text: "real answer".into()
            }
        )
        .is_err());
    }
    #[test]
    fn interaction_claude_initial_profile_rejects_ambiguous_or_changed_affordances() {
        let c = capture();
        for bad in [
            c.replace("2.1.284", "2.1.285"),
            c.replace("↑/↓", "j/k"),
            c.replace("  2.", "❯ 2."),
            c.replace("  3.", "  7."),
            c.replace("Type something.", "Other"),
            format!("{c}❯ user prompt"),
            c.replace("What format?", "What format?\nextra prompt"),
            c.replace("  5. Chat about this", "❯ 5. Chat about this"),
            c.replace("❯ 1.", "  1.").replace("  4.", "❯ 4."),
        ] {
            assert!(recognize(&bad, &initial_ansi()).is_none(), "accepted {bad}");
        }
    }
}
