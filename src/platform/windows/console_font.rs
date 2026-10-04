//! Applies the configured console font to the Windows console host.
//!
//! herdr renders its UI with ratatui into the host console, so every glyph
//! reaches the screen through the console's own font table. A coding agent
//! running in a pane draws Nerd Font private-use codepoints (branch, spinner,
//! status icons), and on a stock Windows console those land as fallback shapes:
//! `⑂` becomes a generic box, the spinner disappears entirely.
//!
//! There was no way to point herdr at a font that carries those glyphs, because
//! nothing here ever called the console font APIs. This does, once, at startup,
//! and only when the user asked for it.

use windows_sys::Win32::System::Console::{SetCurrentConsoleFontEx, CONSOLE_FONT_INFOEX, COORD};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};

/// `dwFontFamily` value for TrueType families. The API takes a plain `u32`
/// rather than an enum, so the constant is spelled out here instead of imported.
const FONT_FAMILY_TRUE_TYPE: u32 = 0x0002;

/// The longest face name the console accepts: `FaceName` is a fixed 32-entry
/// UTF-16 buffer and the console copies all of it, so a longer name would be
/// truncated silently into a font the user did not ask for.
const MAX_FACESIZE: usize = 32;

/// Asks the console to use `family` for its output window.
///
/// Best-effort by design: a font that is not installed, a host that refuses the
/// change, or a missing console handle all leave the console exactly as it was.
/// The UI must never fail to start because a glyph table was unavailable.
pub fn apply_console_font(family: &str, size_px: u16) -> Result<(), String> {
	let trimmed = family.trim();
	if trimmed.is_empty() {
		return Err("font_family is empty".to_string());
	}
	let mut face = [0u16; MAX_FACESIZE];
	let encoded = trimmed.encode_utf16();
	let mut written = 0usize;
	for unit in encoded {
		if written + 1 >= MAX_FACESIZE {
			return Err(format!(
				"font_family is longer than the console's {} character limit: {trimmed}",
				MAX_FACESIZE - 1
			));
		}
		face[written] = unit;
		written += 1;
	}

	let size = i16::try_from(size_px.max(2)).map_err(|_| "font_size out of range".to_string())?;
	let info = CONSOLE_FONT_INFOEX {
		cbSize: std::mem::size_of::<CONSOLE_FONT_INFOEX>() as u32,
		// 0 selects the OEM charset; a named font is looked up by family instead,
		// and leaving nFont unset is what the console samples do.
		nFont: 0,
		dwFontSize: COORD { X: 0, Y: size },
		FontFamily: FONT_FAMILY_TRUE_TYPE,
		FontWeight: 400,
		FaceName: face,
	};

	// `bSetDefault`: also persist the choice for new console buffers, so a
	// respawned pane keeps the font rather than snapping back on restart.
	let ok = unsafe { SetCurrentConsoleFontEx(GetStdHandle(STD_OUTPUT_HANDLE), 1, &info) };
	if ok == 0 {
		return Err(format!("the console rejected font_family {trimmed:?}"));
	}
	Ok(())
}