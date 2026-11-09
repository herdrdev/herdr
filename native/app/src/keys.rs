//! Keyboard → wire input. Pure, so it is unit-tested without a window.

use iced::keyboard::{self, key::Named, Key};
use shepherd_app_core::wire::{modifiers, ClientKeyCode};

/// What a key press becomes on the wire: a named/ctrl key, or the text it typed.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Key(ClientKeyCode, u8),
    Text(String),
}

pub fn map_key(key: &Key, mods: keyboard::Modifiers, text: Option<&str>) -> Option<Input> {
    let mut bits = 0u8;
    if mods.shift() {
        bits |= modifiers::SHIFT;
    }
    if mods.control() {
        bits |= modifiers::CONTROL;
    }
    if mods.alt() {
        bits |= modifiers::ALT;
    }
    let code = match key {
        Key::Named(Named::Enter) => Some(ClientKeyCode::Enter),
        Key::Named(Named::Backspace) => Some(ClientKeyCode::Backspace),
        Key::Named(Named::Tab) if mods.shift() => Some(ClientKeyCode::BackTab),
        Key::Named(Named::Tab) => Some(ClientKeyCode::Tab),
        Key::Named(Named::Escape) => Some(ClientKeyCode::Esc),
        Key::Named(Named::ArrowLeft) => Some(ClientKeyCode::Left),
        Key::Named(Named::ArrowRight) => Some(ClientKeyCode::Right),
        Key::Named(Named::ArrowUp) => Some(ClientKeyCode::Up),
        Key::Named(Named::ArrowDown) => Some(ClientKeyCode::Down),
        Key::Named(Named::Home) => Some(ClientKeyCode::Home),
        Key::Named(Named::End) => Some(ClientKeyCode::End),
        Key::Named(Named::PageUp) => Some(ClientKeyCode::PageUp),
        Key::Named(Named::PageDown) => Some(ClientKeyCode::PageDown),
        Key::Named(Named::Delete) => Some(ClientKeyCode::Delete),
        Key::Named(Named::Insert) => Some(ClientKeyCode::Insert),
        Key::Character(c) if mods.control() || mods.alt() => c
            .chars()
            .next()
            .map(|ch| ClientKeyCode::Char(ch.to_ascii_lowercase())),
        _ => None,
    };
    match (code, text) {
        (Some(code), _) => Some(Input::Key(code, bits)),
        (None, Some(text)) if !text.is_empty() && !text.chars().all(char::is_control) => {
            Some(Input::Text(text.to_owned()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::Modifiers;

    #[test]
    fn keys_become_wire_input() {
        let enter = Key::Named(Named::Enter);
        assert_eq!(
            map_key(&enter, Modifiers::empty(), Some("\r")),
            Some(Input::Key(ClientKeyCode::Enter, 0))
        );
        let c = Key::Character("c".into());
        assert_eq!(
            map_key(&c, Modifiers::CTRL, Some("\u{3}")),
            Some(Input::Key(ClientKeyCode::Char('c'), modifiers::CONTROL))
        );
        assert_eq!(
            map_key(&c, Modifiers::empty(), Some("c")),
            Some(Input::Text("c".into()))
        );
        let tab = Key::Named(Named::Tab);
        assert_eq!(
            map_key(&tab, Modifiers::SHIFT, None),
            Some(Input::Key(ClientKeyCode::BackTab, modifiers::SHIFT))
        );
        assert_eq!(
            map_key(&Key::Named(Named::Shift), Modifiers::SHIFT, None),
            None,
            "a bare modifier sends nothing"
        );
    }
}
