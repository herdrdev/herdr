//! Conservative opt-in recognizer for the owned Claude Code2.1.284 capture.
//! Input compilation validates the complete final dialog, never caller supplied terminal keys.
use crate::api::schema::{InteractionAction, InteractionDialog, InteractionOption};
const FOOTER: &str = "Enter to select · ↑/↓ to navigate · Esc to cancel";

pub(super) fn enabled() -> bool {
    std::env::var("HERDR_GUARDED_CLAUDE_PROFILE").as_deref() == Ok("2.1.284-experimental")
}
fn rule(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 60 && line.chars().all(|c| c == '─')
}
pub(super) fn recognize(text: &str) -> Option<InteractionDialog> {
    if !text.contains("Claude Code v2.1.284") {
        return None;
    }
    let lines: Vec<_> = text.trim_end().lines().collect();
    if lines.last()?.trim() != FOOTER {
        return None;
    }
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
    Some(InteractionDialog {
        profile: "claude-2.1.284-experimental".into(),
        phase: "choose".into(),
        question: question.into(),
        options,
        selected_option_id: format!("choice-{}", selected?),
        supported_actions: vec!["choose".into(), "begin_custom".into()],
    })
}
pub(super) fn compile(
    dialog: &InteractionDialog,
    action: &InteractionAction,
) -> Result<Vec<u8>, &'static str> {
    let option_id = match action {
        InteractionAction::Choose { option_id } | InteractionAction::BeginCustom { option_id } => {
            option_id
        }
        _ => return Err("unsupported_interaction_phase"),
    };
    let target = dialog
        .options
        .iter()
        .position(|o| &o.option_id == option_id)
        .ok_or("unknown_option")?;
    if dialog.options[target].custom != matches!(action, InteractionAction::BeginCustom { .. }) {
        return Err("wrong_option_kind");
    }
    let selected = dialog
        .options
        .iter()
        .position(|o| o.option_id == dialog.selected_option_id)
        .ok_or("unknown_selection")?;
    let mut bytes = Vec::new();
    let key = if target < selected {
        b"\x1b[A"
    } else {
        b"\x1b[B"
    };
    for _ in 0..target.abs_diff(selected) {
        bytes.extend_from_slice(key);
    }
    bytes.push(b'\r');
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn capture() -> String {
        format!("Claude Code v2.1.284\n{}\n ☐ Format\n\nWhat format?\n\n❯ 1. Café\n     Description\n  2. Video call\n     Description\n  3. Async thread\n     Description\n  4. Type something.\n{}\n  5. Chat about this\n\n{FOOTER}\n","─".repeat(80),"─".repeat(80))
    }
    #[test]
    fn interaction_claude_initial_profile_compiles_only_typed_verified_rows() {
        let dialog = recognize(&capture()).unwrap();
        assert_eq!(
            compile(
                &dialog,
                &InteractionAction::BeginCustom {
                    option_id: "choice-4".into()
                }
            )
            .unwrap(),
            b"\x1b[B\x1b[B\x1b[B\r"
        );
        assert_eq!(
            compile(
                &dialog,
                &InteractionAction::Choose {
                    option_id: "choice-2".into()
                }
            )
            .unwrap(),
            b"\x1b[B\r"
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
        ] {
            assert!(recognize(&bad).is_none(), "accepted {bad}");
        }
    }
}
