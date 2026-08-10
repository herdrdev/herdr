mod encode;
mod model;
pub(crate) mod mouse;
mod parse;
#[cfg(test)]
pub(crate) mod test_support;

// Preserve mouse encoder re-exports for facade consumers even though Herdr's
// runtime routes mouse encoding through terminal backends.
#[allow(unused_imports)]
pub use encode::{encode_mouse_button, encode_mouse_scroll};
#[cfg(not(windows))]
pub use model::ime_compatible_keyboard_enhancement_flags;
pub use model::{
    host_modify_other_keys_mode, KeyIdentity, KeyboardProtocol, ModifyOtherKeysMode,
    MouseProtocolEncoding, MouseProtocolMode, TerminalKey, TextCommit, WindowsKeyRecord,
};
pub use parse::parse_terminal_key_sequence;
