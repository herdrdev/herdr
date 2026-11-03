use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::{
    config::{ActionKeybinds, IndexedKeybind, Keybinds},
    input::TerminalKey,
};

pub(crate) type KeybindHelpEntry = (String, Cow<'static, str>);
pub(crate) type KeybindHelpGroup = (&'static str, Vec<KeybindHelpEntry>);

/// The character a key types, for overlay shortcuts and text fields.
pub(crate) fn keybind_help_text_char(key: &TerminalKey) -> Option<char> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
        return None;
    }
    // Text the host says the key produced is authoritative: kitty hosts may
    // report Shift+/ as `/` with text `?` and no shifted alternate.
    if let Some(text) = key.generated_text.as_deref() {
        let mut chars = text.chars();
        if let (Some(character), None) = (chars.next(), chars.next()) {
            if !character.is_control() {
                return Some(character);
            }
        }
    }
    if let Some(character) = key.shifted_codepoint.and_then(char::from_u32) {
        return Some(character);
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    Some(character)
}

fn entry(key: impl Into<String>, label: &'static str) -> KeybindHelpEntry {
    (key.into(), Cow::Borrowed(label))
}

fn binding_label(bindings: &ActionKeybinds) -> String {
    bindings
        .label()
        .unwrap_or_else(|| "não definido".to_owned())
}

fn indexed_label(bindings: &[IndexedKeybind]) -> String {
    if bindings.is_empty() {
        return "não definido".to_owned();
    }
    let mut parts = Vec::new();
    let mut index = 0;
    while index < bindings.len() {
        if let Some(prefix) = indexed_range_prefix(&bindings[index..]) {
            parts.push(format!("{prefix}1..9"));
            index += 9;
        } else {
            parts.push(bindings[index].label.clone());
            index += 1;
        }
    }
    parts.join(" / ")
}

fn indexed_range_prefix(bindings: &[IndexedKeybind]) -> Option<&str> {
    let run = bindings.get(..9)?;
    let prefix = run[0].label.strip_suffix('1')?;
    for (offset, binding) in run.iter().enumerate() {
        let digit = char::from(b'1' + offset as u8);
        if binding.label.strip_suffix(digit) != Some(prefix) {
            return None;
        }
    }
    Some(prefix)
}

pub(crate) fn keybind_help_groups(
    keybinds: &Keybinds,
    prefixes: &[crate::config::KeyCombo],
) -> Vec<KeybindHelpGroup> {
    let mut groups = vec![
        (
            "global",
            vec![
                entry(
                    crate::config::format_prefix_combos(prefixes),
                    "modo de prefixo",
                ),
                entry(binding_label(&keybinds.help), "atalhos"),
                entry(binding_label(&keybinds.settings), "configurações"),
                entry(binding_label(&keybinds.detach), "desanexar"),
                entry(
                    binding_label(&keybinds.reload_config),
                    "recarregar configuração",
                ),
                entry(
                    binding_label(&keybinds.open_notification_target),
                    "abrir destino da notificação",
                ),
            ],
        ),
        (
            "navegação",
            vec![
                entry("esc", "voltar"),
                entry(
                    format!(
                        "{} / {}",
                        binding_label(&keybinds.navigate.workspace_up),
                        binding_label(&keybinds.navigate.workspace_down)
                    ),
                    "lista de espaços",
                ),
                entry(
                    format!(
                        "{} / {} / {} / {} / left / right",
                        binding_label(&keybinds.navigate.pane_left),
                        binding_label(&keybinds.navigate.pane_down),
                        binding_label(&keybinds.navigate.pane_up),
                        binding_label(&keybinds.navigate.pane_right)
                    ),
                    "mover foco",
                ),
                entry("tab / shift+tab", "alternar painel"),
                entry("enter", "abrir espaço"),
                entry("1..9", "trocar espaço"),
            ],
        ),
        (
            "espaços / abas",
            vec![
                entry(binding_label(&keybinds.workspace_picker), "navegar espaços"),
                entry(binding_label(&keybinds.goto), "navegar sessões"),
                entry(binding_label(&keybinds.new_workspace), "novo espaço"),
                entry(
                    binding_label(&keybinds.new_worktree),
                    "nova árvore de trabalho",
                ),
                entry(
                    binding_label(&keybinds.open_worktree),
                    "abrir árvore de trabalho",
                ),
                entry(
                    binding_label(&keybinds.remove_worktree),
                    "excluir árvore de trabalho",
                ),
                entry(binding_label(&keybinds.rename_workspace), "renomear espaço"),
                entry(binding_label(&keybinds.close_workspace), "fechar espaço"),
                entry(
                    binding_label(&keybinds.previous_workspace),
                    "espaço anterior",
                ),
                entry(binding_label(&keybinds.next_workspace), "próximo espaço"),
                entry(
                    indexed_label(&keybinds.switch_workspace),
                    "trocar espaço 1-9",
                ),
                entry(binding_label(&keybinds.previous_agent), "agente anterior"),
                entry(binding_label(&keybinds.next_agent), "próximo agente"),
                entry(indexed_label(&keybinds.focus_agent), "focar agente 1-9"),
                entry(binding_label(&keybinds.new_tab), "nova aba"),
                entry(binding_label(&keybinds.rename_tab), "renomear aba"),
                entry(binding_label(&keybinds.previous_tab), "aba anterior"),
                entry(binding_label(&keybinds.next_tab), "próxima aba"),
                entry(
                    binding_label(&keybinds.move_tab_previous),
                    "mover aba à esquerda",
                ),
                entry(
                    binding_label(&keybinds.move_tab_next),
                    "mover aba à direita",
                ),
                entry(indexed_label(&keybinds.switch_tab), "trocar aba 1-9"),
                entry(binding_label(&keybinds.close_tab), "fechar aba"),
            ],
        ),
        (
            "painéis",
            vec![
                entry(
                    binding_label(&keybinds.split_vertical),
                    "dividir na vertical",
                ),
                entry(
                    binding_label(&keybinds.split_horizontal),
                    "dividir na horizontal",
                ),
                entry(binding_label(&keybinds.close_pane), "fechar painel"),
                entry(binding_label(&keybinds.rename_pane), "renomear painel"),
                entry(binding_label(&keybinds.edit_scrollback), "editar histórico"),
                entry(binding_label(&keybinds.clear_pane), "limpar painel"),
                entry(binding_label(&keybinds.copy_mode), "modo de cópia"),
                entry(binding_label(&keybinds.zoom), "ampliar painel"),
                entry(
                    binding_label(&keybinds.resize_mode),
                    "modo de redimensionamento",
                ),
                entry(
                    binding_label(&keybinds.resize_pane_left),
                    "redimensionar painel à esquerda",
                ),
                entry(
                    binding_label(&keybinds.resize_pane_down),
                    "redimensionar painel para baixo",
                ),
                entry(
                    binding_label(&keybinds.resize_pane_up),
                    "redimensionar painel para cima",
                ),
                entry(
                    binding_label(&keybinds.resize_pane_right),
                    "redimensionar painel à direita",
                ),
                entry(
                    binding_label(&keybinds.toggle_sidebar),
                    "alternar barra lateral",
                ),
                entry(
                    binding_label(&keybinds.focus_pane_left),
                    "focar painel à esquerda",
                ),
                entry(
                    binding_label(&keybinds.focus_pane_down),
                    "focar painel abaixo",
                ),
                entry(binding_label(&keybinds.focus_pane_up), "focar painel acima"),
                entry(
                    binding_label(&keybinds.focus_pane_right),
                    "focar painel à direita",
                ),
                entry(binding_label(&keybinds.cycle_pane_next), "próximo painel"),
                entry(
                    binding_label(&keybinds.cycle_pane_previous),
                    "painel anterior",
                ),
                entry(binding_label(&keybinds.last_pane), "último painel"),
            ],
        ),
    ];

    if !keybinds.custom_commands.is_empty() {
        groups.push((
            "personalizados",
            keybinds
                .custom_commands
                .iter()
                .map(|binding| {
                    (
                        binding.label.clone(),
                        binding
                            .description
                            .clone()
                            .map(Cow::Owned)
                            .unwrap_or(Cow::Borrowed("comando personalizado")),
                    )
                })
                .collect(),
        ));
    }
    groups
}

pub(crate) fn filter_keybind_help_groups(
    groups: Vec<KeybindHelpGroup>,
    query: &str,
) -> Vec<KeybindHelpGroup> {
    if query.is_empty() {
        return groups;
    }
    let query = query.to_lowercase();
    groups
        .into_iter()
        .filter_map(|(group, entries)| {
            let entries = entries
                .into_iter()
                .filter(|(key, label)| {
                    key.to_lowercase().contains(&query) || label.to_lowercase().contains(&query)
                })
                .collect::<Vec<_>>();
            (!entries.is_empty()).then_some((group, entries))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> Vec<KeybindHelpGroup> {
        vec![
            (
                "espaços / abas",
                vec![entry("w", "navegar espaços"), entry("c", "nova aba")],
            ),
            (
                "painéis",
                vec![
                    entry("v", "dividir na vertical"),
                    entry("x", "fechar painel"),
                ],
            ),
        ]
    }

    #[test]
    fn text_char_prefers_generated_text_then_alternate_then_code() {
        let parse = |bytes: &str| {
            crate::input::parse_terminal_key_sequence(bytes)
                .unwrap_or_else(|| panic!("{bytes:?} parses"))
        };
        for (bytes, want) in [
            // Shift+/ typing `?` with no shifted alternate.
            ("\x1b[47;2;63u", Some('?')),
            ("\x1b[47u", Some('/')),
            ("/", Some('/')),
            // Portuguese Shift+7: alternate and text agree.
            ("\x1b[55:47;2;47u", Some('/')),
            ("\x1b[55:47;2u", Some('/')),
            ("\x1b[55;2u", Some('7')),
            ("\x1b[47;5u", None),
            ("\x1b[47;3u", None),
        ] {
            assert_eq!(keybind_help_text_char(&parse(bytes)), want, "{bytes:?}");
        }
    }

    #[test]
    fn filter_matches_labels_and_shortcuts_case_insensitively() {
        let filtered = filter_keybind_help_groups(groups(), "EsPaÇ");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1[0].1, "navegar espaços");

        let filtered = filter_keybind_help_groups(groups(), "x");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1[0].1, "fechar painel");
        assert!(filter_keybind_help_groups(groups(), "painéis").is_empty());
    }

    #[test]
    fn busca_portuguesa_encontra_atalho_real_de_fechar() {
        let grupos = keybind_help_groups(&Keybinds::default(), &[]);
        let encontrados = filter_keybind_help_groups(grupos, "fechar painel");
        assert_eq!(encontrados.len(), 1);
        assert_eq!(encontrados[0].0, "painéis");
        assert_eq!(encontrados[0].1[0].1, "fechar painel");
        assert_eq!(
            encontrados[0].1[0].0,
            binding_label(&Keybinds::default().close_pane)
        );
    }

    #[test]
    fn descricao_personalizada_e_tecla_permanecem_intactas() {
        let mut keybinds = Keybinds::default();
        keybinds
            .custom_commands
            .push(crate::config::CustomCommandKeybind {
                bindings: keybinds.help.clone(),
                label: "ctrl+x".into(),
                command: "printf example".into(),
                action: crate::config::CustomCommandAction::Shell,
                description: Some("Custom: ação própria 😀".into()),
                width: None,
                height: None,
            });
        let grupos = keybind_help_groups(&keybinds, &[]);
        let resultado = filter_keybind_help_groups(grupos, "ação própria");
        assert_eq!(resultado.len(), 1);
        assert_eq!(resultado[0].1[0].0, "ctrl+x");
        assert_eq!(resultado[0].1[0].1, "Custom: ação própria 😀");
    }

    #[test]
    fn help_lists_every_configured_prefix() {
        let groups = keybind_help_groups(
            &Keybinds::default(),
            &[
                (KeyCode::Char(' '), KeyModifiers::CONTROL),
                (KeyCode::Char('s'), KeyModifiers::CONTROL),
            ],
        );
        let global = &groups[0].1;
        assert_eq!(global[0].0, "ctrl+space / ctrl+s");
        assert_eq!(global[0].1, "modo de prefixo");
    }
}
