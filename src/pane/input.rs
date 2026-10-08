/// Build the key event the Ghostty app would build from an OS key event, so
/// libghostty can encode it for the pane's current keyboard modes: physical
/// key, modifiers, the layout text, the unshifted codepoint, and Shift marked
/// consumed when it produced the text. `legacy_pane` is true when the pane
/// negotiated no keyboard protocol.
///
/// Hyper and Meta have no Ghostty equivalent, so those chords are not
/// forwarded rather than typed as their bare key.
pub(super) fn ghostty_key_event_from_terminal_key(
    key: &crate::input::TerminalKey,
    legacy_pane: bool,
) -> Option<crate::ghostty::KeyEvent> {
    use crossterm::event::{KeyCode, KeyModifiers};

    if key
        .modifiers
        .intersects(KeyModifiers::HYPER | KeyModifiers::META)
    {
        return None;
    }
    let mut event = crate::ghostty::KeyEvent::new().ok()?;
    event.set_action(match key.kind {
        crossterm::event::KeyEventKind::Press => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_PRESS
        }
        crossterm::event::KeyEventKind::Release => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_RELEASE
        }
        crossterm::event::KeyEventKind::Repeat => {
            crate::ghostty::ffi::GhosttyKeyAction_GHOSTTY_KEY_ACTION_REPEAT
        }
    });
    let mut mods = ghostty_mods_from_key_modifiers(key.modifiers);
    match key.code {
        KeyCode::Char(c) => {
            // An uppercase letter's key is its lowercase letter on every layout.
            // Nothing else is inferred: Kitty reports already name the unshifted
            // key (`+` on a German layout stays `+`), and legacy input does not
            // say which key produced punctuation.
            let base = unshifted_letter(c);
            if base != c {
                mods |= crate::ghostty::MOD_SHIFT;
            }
            event.set_key(
                ghostty_key_from_char(base)
                    .unwrap_or(crate::ghostty::ffi::GhosttyKey_GHOSTTY_KEY_UNIDENTIFIED),
            );
            event.set_unshifted_codepoint(base as u32);
            let shifted = mods & crate::ghostty::MOD_SHIFT != 0;
            if let Some(text) = key_text(key, base, shifted) {
                // Text the host reported with Shift held already includes Shift
                // (e.g. Shift+7 = "/" on a German layout, where `base` cannot
                // be recovered from the character alone).
                let reported_shifted_text = key.generated_text.is_some() && text != " ";
                // A non-letter given with Shift and no reported shifted character
                // is the produced character itself ("shift+?" is "?"). Kitty
                // hosts report shifted alternates, so a changed character shows
                // up there.
                let given_shifted_char =
                    key.shifted_codepoint.is_none() && !base.is_alphabetic() && text != " ";
                if shifted
                    && (!text.starts_with(base) || reported_shifted_text || given_shifted_char)
                {
                    event.set_consumed_mods(crate::ghostty::MOD_SHIFT);
                }
                // Legacy Alt prefixes the produced text, but libghostty on macOS
                // prefixes the unshifted codepoint for non-ASCII text (it
                // assumes Option translated it). Herdr already decoded Alt, so
                // the text is the key's real output: Alt+Shift+ö is ESC Ö.
                if legacy_pane
                    && key.modifiers.contains(KeyModifiers::ALT)
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    if let Some(produced) = single_char(&text).filter(|c| !c.is_ascii()) {
                        event.set_unshifted_codepoint(produced as u32);
                    }
                }
                event.set_utf8(&text);
            }
        }
        KeyCode::BackTab => {
            // Ghostty represents backtab as Tab with Shift rather than a distinct key.
            mods |= crate::ghostty::MOD_SHIFT;
            event.set_key(crate::ghostty::ffi::GhosttyKey_GHOSTTY_KEY_TAB);
        }
        code => event.set_key(ghostty_key_from_key_code(code)?),
    }
    event.set_mods(mods);
    event.set_composing(key.is_windows_dead_key());

    Some(event)
}

/// Configure `encoder` for a pane's live terminal modes. Herdr has already
/// decoded Alt from the host, so macOS Option must keep its Alt meaning (ESC
/// prefix) instead of being treated as a text modifier.
pub(super) fn configure_key_encoder(
    encoder: &mut crate::ghostty::KeyEncoder,
    terminal: &crate::ghostty::Terminal,
) {
    encoder.set_from_terminal(terminal);
    encoder.set_macos_option_as_alt(true);
}

/// Exception table for panes that negotiated no keyboard protocol. Ghostty sends
/// these chords as escape sequences that plain shells do not understand; keep
/// the classic key instead. libghostty still encodes the result (so Alt keeps
/// its ESC prefix).
pub(super) fn legacy_shell_key(key: crate::input::TerminalKey) -> crate::input::TerminalKey {
    use crossterm::event::{KeyCode, KeyModifiers};

    let alt = key.modifiers & KeyModifiers::ALT;
    let chord = key.modifiers - KeyModifiers::ALT;
    let classic = match (key.code, chord) {
        (KeyCode::Enter, chord) if !chord.is_empty() => KeyCode::Enter,
        (KeyCode::Char('m'), KeyModifiers::CONTROL) => KeyCode::Enter,
        (KeyCode::Char('i'), KeyModifiers::CONTROL) | (KeyCode::Tab, KeyModifiers::CONTROL) => {
            KeyCode::Tab
        }
        (KeyCode::Char('['), KeyModifiers::CONTROL) => KeyCode::Esc,
        // Ghostty reports Ctrl+Shift+letter as CSI u; every terminal sends the
        // plain control byte, which is all a shell can read.
        // WezTerm names it Ctrl plus an uppercase letter, without Shift.
        (KeyCode::Char(c), chord)
            if chord - KeyModifiers::SHIFT == KeyModifiers::CONTROL
                && c.is_ascii_alphabetic()
                && (chord.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase()) =>
        {
            // Ctrl+I and Ctrl+M then follow the rows above (Tab, Enter).
            return legacy_shell_key(
                crate::input::TerminalKey::new(
                    KeyCode::Char(c.to_ascii_lowercase()),
                    KeyModifiers::CONTROL | alt,
                )
                .with_kind(key.kind)
                .with_repeat_count(key.repeat_count),
            );
        }
        _ => return key,
    };
    crate::input::TerminalKey::new(classic, alt)
        .with_kind(key.kind)
        .with_repeat_count(key.repeat_count)
}

/// Second exception for panes that negotiated nothing: legacy encoding has no
/// Super, so these chords are not forwarded. Typing the bare key turns Cmd+C
/// into "c" (#3710), and a Kitty report prints as garbage in plain shells
/// (#4356). Ghostty on macOS sends nothing for them either.
pub(super) fn legacy_super_chord(key: &crate::input::TerminalKey) -> bool {
    key.modifiers
        .contains(crossterm::event::KeyModifiers::SUPER)
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Text the key produced on the user's layout, before Ctrl/Alt transformations.
fn key_text(key: &crate::input::TerminalKey, base: char, shifted: bool) -> Option<String> {
    if let Some(text) = key
        .generated_text
        .as_ref()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
    {
        return Some(text.clone());
    }
    // Without a reported shifted character, only a letter's Shift is known.
    let produced = match key.shifted_codepoint.and_then(char::from_u32) {
        Some(shifted_char) if shifted => shifted_char,
        _ if shifted => shifted_letter(base),
        _ => base,
    };
    (!produced.is_control()).then(|| produced.to_string())
}

fn shifted_letter(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(upper), None) if c.is_lowercase() => upper,
        _ => c,
    }
}

fn unshifted_letter(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(lower), None) if c.is_uppercase() => lower,
        _ => c,
    }
}

pub(super) fn ghostty_mods_from_key_modifiers(modifiers: crossterm::event::KeyModifiers) -> u16 {
    let mut ghostty_mods = 0u16;
    if modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
        ghostty_mods |= crate::ghostty::MOD_SHIFT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
        ghostty_mods |= crate::ghostty::MOD_CTRL;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::ALT) {
        ghostty_mods |= crate::ghostty::MOD_ALT;
    }
    if modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
        ghostty_mods |= crate::ghostty::MOD_SUPER;
    }
    ghostty_mods
}

pub(super) fn ghostty_mouse_encoder_for_terminal(
    terminal: &crate::ghostty::Terminal,
    position: crate::input::mouse::Position,
) -> Option<crate::ghostty::MouseEncoder> {
    let mut encoder = crate::ghostty::MouseEncoder::new().ok()?;
    encoder.set_from_terminal(terminal);
    let cols = terminal.cols().ok()? as u32;
    let rows = terminal.rows().ok()? as u32;
    let sgr_pixels = terminal
        .mode_get(crate::ghostty::MODE_MOUSE_SGR_PIXELS)
        .ok()?;
    match position {
        crate::input::mouse::Position::Cell { .. } => {
            if sgr_pixels {
                encoder.set_format(crate::ghostty::MOUSE_FORMAT_SGR);
            }
            encoder.set_size(cols, rows, 1, 1);
        }
        crate::input::mouse::Position::Pixels { .. } => {
            if sgr_pixels {
                encoder.set_format(crate::ghostty::MOUSE_FORMAT_SGR_PIXELS);
            }
            let width_px = terminal.width_px().ok()?;
            let height_px = terminal.height_px().ok()?;
            if width_px == 0 || height_px == 0 || cols == 0 || rows == 0 {
                return None;
            }
            encoder.set_size(width_px, height_px, width_px / cols, height_px / rows);
        }
    }
    Some(encoder)
}

pub(super) fn ghostty_mouse_position_for_terminal(
    position: crate::input::mouse::Position,
) -> Option<(f32, f32)> {
    match position {
        crate::input::mouse::Position::Pixels { x, y } => Some((x as f32, y as f32)),
        crate::input::mouse::Position::Cell { column, row } => Some((column as f32, row as f32)),
    }
}

pub(super) fn ghostty_mouse_event_from_button_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    let (action, button) = match kind {
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_PRESS,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_RELEASE,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_LEFT),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Middle) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_MIDDLE),
        ),
        crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Right) => (
            crate::ghostty::MOUSE_ACTION_MOTION,
            Some(crate::ghostty::MOUSE_BUTTON_RIGHT),
        ),
        _ => return None,
    };
    event.set_action(action);
    if let Some(button) = button {
        event.set_button(button);
    } else {
        event.clear_button();
    }
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

pub(super) fn ghostty_mouse_event_from_motion_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    if kind != crossterm::event::MouseEventKind::Moved {
        return None;
    }

    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    event.set_action(crate::ghostty::MOUSE_ACTION_MOTION);
    event.clear_button();
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

pub(super) fn ghostty_mouse_event_from_wheel_kind(
    kind: crossterm::event::MouseEventKind,
    column: u16,
    row: u16,
    modifiers: crossterm::event::KeyModifiers,
) -> Option<crate::ghostty::MouseEvent> {
    let mut event = crate::ghostty::MouseEvent::new().ok()?;
    event.set_action(crate::ghostty::MOUSE_ACTION_PRESS);
    let button = match kind {
        crossterm::event::MouseEventKind::ScrollUp => crate::ghostty::MOUSE_BUTTON_WHEEL_UP,
        crossterm::event::MouseEventKind::ScrollDown => crate::ghostty::MOUSE_BUTTON_WHEEL_DOWN,
        crossterm::event::MouseEventKind::ScrollLeft => crate::ghostty::MOUSE_BUTTON_WHEEL_LEFT,
        crossterm::event::MouseEventKind::ScrollRight => crate::ghostty::MOUSE_BUTTON_WHEEL_RIGHT,
        _ => return None,
    };
    event.set_button(button);
    event.set_mods(ghostty_mods_from_key_modifiers(modifiers));
    event.set_position(column as f32, row as f32);
    Some(event)
}

/// Physical key for a non-character key code. Keys libghostty has no name for
/// (some media keys, Hyper/Meta, ISO level shifts) return None; the Ghostty app
/// cannot send them either.
fn ghostty_key_from_key_code(
    code: crossterm::event::KeyCode,
) -> Option<crate::ghostty::ffi::GhosttyKey> {
    use crate::ghostty::ffi;
    use crossterm::event::{KeyCode, MediaKeyCode, ModifierKeyCode};

    match code {
        KeyCode::Backspace => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSPACE),
        KeyCode::Enter => Some(ffi::GhosttyKey_GHOSTTY_KEY_ENTER),
        KeyCode::Left => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_LEFT),
        KeyCode::Right => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_RIGHT),
        KeyCode::Up => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_UP),
        KeyCode::Down => Some(ffi::GhosttyKey_GHOSTTY_KEY_ARROW_DOWN),
        KeyCode::Home => Some(ffi::GhosttyKey_GHOSTTY_KEY_HOME),
        KeyCode::End => Some(ffi::GhosttyKey_GHOSTTY_KEY_END),
        KeyCode::PageUp => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_UP),
        KeyCode::PageDown => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAGE_DOWN),
        KeyCode::Tab | KeyCode::BackTab => Some(ffi::GhosttyKey_GHOSTTY_KEY_TAB),
        KeyCode::Delete => Some(ffi::GhosttyKey_GHOSTTY_KEY_DELETE),
        KeyCode::Insert => Some(ffi::GhosttyKey_GHOSTTY_KEY_INSERT),
        KeyCode::Esc => Some(ffi::GhosttyKey_GHOSTTY_KEY_ESCAPE),
        KeyCode::F(n) => Some(match n {
            1 => ffi::GhosttyKey_GHOSTTY_KEY_F1,
            2 => ffi::GhosttyKey_GHOSTTY_KEY_F2,
            3 => ffi::GhosttyKey_GHOSTTY_KEY_F3,
            4 => ffi::GhosttyKey_GHOSTTY_KEY_F4,
            5 => ffi::GhosttyKey_GHOSTTY_KEY_F5,
            6 => ffi::GhosttyKey_GHOSTTY_KEY_F6,
            7 => ffi::GhosttyKey_GHOSTTY_KEY_F7,
            8 => ffi::GhosttyKey_GHOSTTY_KEY_F8,
            9 => ffi::GhosttyKey_GHOSTTY_KEY_F9,
            10 => ffi::GhosttyKey_GHOSTTY_KEY_F10,
            11 => ffi::GhosttyKey_GHOSTTY_KEY_F11,
            12 => ffi::GhosttyKey_GHOSTTY_KEY_F12,
            13 => ffi::GhosttyKey_GHOSTTY_KEY_F13,
            14 => ffi::GhosttyKey_GHOSTTY_KEY_F14,
            15 => ffi::GhosttyKey_GHOSTTY_KEY_F15,
            16 => ffi::GhosttyKey_GHOSTTY_KEY_F16,
            17 => ffi::GhosttyKey_GHOSTTY_KEY_F17,
            18 => ffi::GhosttyKey_GHOSTTY_KEY_F18,
            19 => ffi::GhosttyKey_GHOSTTY_KEY_F19,
            20 => ffi::GhosttyKey_GHOSTTY_KEY_F20,
            21 => ffi::GhosttyKey_GHOSTTY_KEY_F21,
            22 => ffi::GhosttyKey_GHOSTTY_KEY_F22,
            23 => ffi::GhosttyKey_GHOSTTY_KEY_F23,
            24 => ffi::GhosttyKey_GHOSTTY_KEY_F24,
            25 => ffi::GhosttyKey_GHOSTTY_KEY_F25,
            _ => return None,
        }),
        KeyCode::CapsLock => Some(ffi::GhosttyKey_GHOSTTY_KEY_CAPS_LOCK),
        KeyCode::ScrollLock => Some(ffi::GhosttyKey_GHOSTTY_KEY_SCROLL_LOCK),
        KeyCode::NumLock => Some(ffi::GhosttyKey_GHOSTTY_KEY_NUM_LOCK),
        KeyCode::PrintScreen => Some(ffi::GhosttyKey_GHOSTTY_KEY_PRINT_SCREEN),
        KeyCode::Pause => Some(ffi::GhosttyKey_GHOSTTY_KEY_PAUSE),
        KeyCode::Menu => Some(ffi::GhosttyKey_GHOSTTY_KEY_CONTEXT_MENU),
        KeyCode::KeypadBegin => Some(ffi::GhosttyKey_GHOSTTY_KEY_NUMPAD_BEGIN),
        KeyCode::Media(MediaKeyCode::PlayPause) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_MEDIA_PLAY_PAUSE)
        }
        KeyCode::Media(MediaKeyCode::Stop) => Some(ffi::GhosttyKey_GHOSTTY_KEY_MEDIA_STOP),
        KeyCode::Media(MediaKeyCode::TrackNext) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_MEDIA_TRACK_NEXT)
        }
        KeyCode::Media(MediaKeyCode::TrackPrevious) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_MEDIA_TRACK_PREVIOUS)
        }
        KeyCode::Media(MediaKeyCode::LowerVolume) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_AUDIO_VOLUME_DOWN)
        }
        KeyCode::Media(MediaKeyCode::RaiseVolume) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_AUDIO_VOLUME_UP)
        }
        KeyCode::Media(MediaKeyCode::MuteVolume) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_AUDIO_VOLUME_MUTE)
        }
        KeyCode::Modifier(ModifierKeyCode::LeftShift) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_SHIFT_LEFT)
        }
        KeyCode::Modifier(ModifierKeyCode::RightShift) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_SHIFT_RIGHT)
        }
        KeyCode::Modifier(ModifierKeyCode::LeftControl) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_CONTROL_LEFT)
        }
        KeyCode::Modifier(ModifierKeyCode::RightControl) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_CONTROL_RIGHT)
        }
        KeyCode::Modifier(ModifierKeyCode::LeftAlt) => Some(ffi::GhosttyKey_GHOSTTY_KEY_ALT_LEFT),
        KeyCode::Modifier(ModifierKeyCode::RightAlt) => Some(ffi::GhosttyKey_GHOSTTY_KEY_ALT_RIGHT),
        KeyCode::Modifier(ModifierKeyCode::LeftSuper) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_META_LEFT)
        }
        KeyCode::Modifier(ModifierKeyCode::RightSuper) => {
            Some(ffi::GhosttyKey_GHOSTTY_KEY_META_RIGHT)
        }
        _ => None,
    }
}

fn ghostty_key_from_char(base: char) -> Option<crate::ghostty::ffi::GhosttyKey> {
    use crate::ghostty::ffi;

    match base {
        'a' => Some(ffi::GhosttyKey_GHOSTTY_KEY_A),
        'b' => Some(ffi::GhosttyKey_GHOSTTY_KEY_B),
        'c' => Some(ffi::GhosttyKey_GHOSTTY_KEY_C),
        'd' => Some(ffi::GhosttyKey_GHOSTTY_KEY_D),
        'e' => Some(ffi::GhosttyKey_GHOSTTY_KEY_E),
        'f' => Some(ffi::GhosttyKey_GHOSTTY_KEY_F),
        'g' => Some(ffi::GhosttyKey_GHOSTTY_KEY_G),
        'h' => Some(ffi::GhosttyKey_GHOSTTY_KEY_H),
        'i' => Some(ffi::GhosttyKey_GHOSTTY_KEY_I),
        'j' => Some(ffi::GhosttyKey_GHOSTTY_KEY_J),
        'k' => Some(ffi::GhosttyKey_GHOSTTY_KEY_K),
        'l' => Some(ffi::GhosttyKey_GHOSTTY_KEY_L),
        'm' => Some(ffi::GhosttyKey_GHOSTTY_KEY_M),
        'n' => Some(ffi::GhosttyKey_GHOSTTY_KEY_N),
        'o' => Some(ffi::GhosttyKey_GHOSTTY_KEY_O),
        'p' => Some(ffi::GhosttyKey_GHOSTTY_KEY_P),
        'q' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Q),
        'r' => Some(ffi::GhosttyKey_GHOSTTY_KEY_R),
        's' => Some(ffi::GhosttyKey_GHOSTTY_KEY_S),
        't' => Some(ffi::GhosttyKey_GHOSTTY_KEY_T),
        'u' => Some(ffi::GhosttyKey_GHOSTTY_KEY_U),
        'v' => Some(ffi::GhosttyKey_GHOSTTY_KEY_V),
        'w' => Some(ffi::GhosttyKey_GHOSTTY_KEY_W),
        'x' => Some(ffi::GhosttyKey_GHOSTTY_KEY_X),
        'y' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Y),
        'z' => Some(ffi::GhosttyKey_GHOSTTY_KEY_Z),
        '0' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_0),
        '1' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_1),
        '2' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_2),
        '3' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_3),
        '4' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_4),
        '5' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_5),
        '6' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_6),
        '7' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_7),
        '8' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_8),
        '9' => Some(ffi::GhosttyKey_GHOSTTY_KEY_DIGIT_9),
        '`' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKQUOTE),
        '\\' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BACKSLASH),
        '[' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_LEFT),
        ']' => Some(ffi::GhosttyKey_GHOSTTY_KEY_BRACKET_RIGHT),
        ',' => Some(ffi::GhosttyKey_GHOSTTY_KEY_COMMA),
        '=' => Some(ffi::GhosttyKey_GHOSTTY_KEY_EQUAL),
        '-' => Some(ffi::GhosttyKey_GHOSTTY_KEY_MINUS),
        '.' => Some(ffi::GhosttyKey_GHOSTTY_KEY_PERIOD),
        '\'' => Some(ffi::GhosttyKey_GHOSTTY_KEY_QUOTE),
        ';' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SEMICOLON),
        '/' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SLASH),
        ' ' => Some(ffi::GhosttyKey_GHOSTTY_KEY_SPACE),
        _ => None,
    }
}
