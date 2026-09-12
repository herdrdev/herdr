//! Local path links. Parsing is pure. Filesystem checks happen only after a click,
//! never while parsing PTY output or rendering a pane.

use std::path::{Path, PathBuf};

const MAX_TARGET_BYTES: usize = 4096;

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TARGET_BYTES && !value.chars().any(char::is_control)
}

fn windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn allowed_path(value: &str) -> bool {
    if !valid_text(value) || value.starts_with('\\') || value.starts_with("//") {
        return false;
    }
    // Reject URI schemes, drive-relative paths, alternate data streams, device
    // namespaces and network shares before any filesystem lookup.
    let tail = if windows_drive_path(value) {
        &value[2..]
    } else {
        value
    };
    !tail.contains([':', '?', '*', '|', '<', '>'])
        && !tail.split(['/', '\\']).any(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end()
                .to_ascii_uppercase();
            matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
}

fn without_location(value: &str) -> &str {
    let Some((prefix, number)) = value.rsplit_once(':') else {
        return value;
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return value;
    }
    if let Some((path, line)) = prefix.rsplit_once(':') {
        if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) {
            return path;
        }
    }
    prefix
}

fn looks_like_path(value: &str) -> bool {
    let path = without_location(value);
    allowed_path(path)
        && (path.contains(['/', '\\'])
            || Path::new(path).extension().is_some()
            || matches!(
                path,
                "README" | "LICENSE" | "Makefile" | "Dockerfile" | "justfile"
            ))
}

/// Return only a path containing the clicked UTF-8 byte, not a nearby token.
/// Quoted paths may contain spaces. Line and column suffixes are accepted, but
/// opening a file does not imply that its editor supports jumping to a location.
pub(crate) fn path_at_byte(row: &str, clicked: usize) -> Option<&str> {
    if row.len() > 64 * 1024 || !row.is_char_boundary(clicked) || clicked >= row.len() {
        return None;
    }
    let chars: Vec<_> = row.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (start, ch) = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        let quoted = matches!(ch, '\'' | '"' | '`');
        let content_start = if quoted { start + ch.len_utf8() } else { start };
        let mut end_index = index + 1;
        while end_index < chars.len() {
            let current = chars[end_index].1;
            if (quoted && current == ch) || (!quoted && current.is_whitespace()) {
                break;
            }
            end_index += 1;
        }
        if quoted && end_index == chars.len() {
            return None;
        }
        let mut end = chars.get(end_index).map_or(row.len(), |item| item.0);
        let mut begin = content_start;
        if !quoted {
            let raw = &row[begin..end];
            let trimmed = raw.trim_start_matches(['(', '[', '{']);
            begin += raw.len() - trimmed.len();
            // Preserve balanced parentheses inside a real path.
            while end > begin {
                let text = &row[begin..end];
                let last = text.chars().next_back()?;
                let trim = matches!(last, ',' | ';' | '.' | '!' | '"' | '\'' | '`')
                    || [('(', ')'), ('[', ']'), ('{', '}')]
                        .iter()
                        .any(|(open, close)| {
                            last == *close
                                && text.matches(*close).count() > text.matches(*open).count()
                        });
                if !trim {
                    break;
                }
                end -= last.len_utf8();
            }
        }
        if clicked >= begin && clicked < end {
            let candidate = &row[begin..end];
            return (looks_like_path(candidate) || candidate.starts_with("file://"))
                .then_some(candidate);
        }
        index = if quoted {
            end_index.saturating_add(1)
        } else {
            end_index
        };
    }
    None
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_uri_path(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            hex(input.next()?)? * 16 + hex(input.next()?)?
        } else {
            byte
        });
    }
    String::from_utf8(bytes).ok()
}

fn local_file_uri_path(uri: &str, windows: bool) -> Option<String> {
    if !valid_text(uri) || uri.contains(['?', '#', '\\']) {
        return None;
    }
    let remainder = uri.strip_prefix("file://")?;
    let (host, raw_path) = remainder.split_once('/')?;
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    let decoded = decode_uri_path(&format!("/{raw_path}"))?;
    let path = if windows {
        let path = decoded.strip_prefix('/')?;
        if !windows_drive_path(path) {
            return None;
        }
        path.to_owned()
    } else {
        if windows_drive_path(decoded.strip_prefix('/').unwrap_or(&decoded)) {
            return None;
        }
        decoded
    };
    allowed_path(&path).then_some(path)
}

pub(crate) fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    local_file_uri_path(uri, cfg!(windows)).map(PathBuf::from)
}

fn explicit_path_start(value: &str) -> bool {
    windows_drive_path(value)
        || value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with("~/")
        || value.starts_with("~\\")
        || value.split_once('/').is_some_and(|(head, _)| {
            !head.is_empty()
                && head
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
        || value.split_once('\\').is_some_and(|(head, _)| {
            !head.is_empty()
                && head
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        })
}

/// Pi removes Markdown code-span delimiters, so paths containing spaces can
/// arrive without quotes. Check a bounded set of complete path candidates on
/// click, preferring the longest existing path containing the clicked byte.
pub(crate) fn resolve_visible_path(
    row: &str,
    clicked: usize,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if row.len() > MAX_TARGET_BYTES || !row.is_char_boundary(clicked) || clicked >= row.len() {
        return None;
    }
    // Start only at explicit path prefixes, never midway through a URL, UNC
    // path, or device name. Ordinary prose must not become an arbitrary path.
    for (start, _) in row
        .char_indices()
        .filter(|(index, _)| {
            *index <= clicked
                && (*index == 0
                    || row[..*index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
        })
        .take(32)
    {
        if !explicit_path_start(&row[start..]) {
            continue;
        }
        let end = row[start..]
            .find("  ")
            .map_or(row.len(), |index| start + index);
        if clicked >= end {
            continue;
        }
        let text = row[start..end].trim_end();
        if text.len() > MAX_TARGET_BYTES {
            continue;
        }
        let mut ends = vec![text.len()];
        ends.extend(
            text.char_indices()
                .rev()
                .filter_map(|(index, ch)| {
                    (ch.is_whitespace() && start + index > clicked).then_some(index)
                })
                .take(16),
        );
        for end in ends {
            let candidate = text[..end].trim_end_matches([',', ';']);
            if start + candidate.len() <= clicked {
                continue;
            }
            if let Some(path) = resolve_path(candidate, cwd, home) {
                return Some(path);
            }
        }
    }
    resolve_path(path_at_byte(row, clicked)?, cwd, home)
}

/// A deliberately narrow default-app policy. Scripts and executable formats are
/// opened as text in an editor, never passed to their executable file association.
pub(crate) fn is_document(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pdf" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
            )
        })
}

pub(crate) fn path_to_file_uri(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    if !path.is_absolute() || !allowed_path(raw) {
        return None;
    }
    let raw = if cfg!(windows) {
        raw.replace('\\', "/")
    } else {
        raw.to_owned()
    };
    let mut uri = if cfg!(windows) { "file:///" } else { "file://" }.to_owned();
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    Some(uri)
}

/// Resolve a clicked local file reference against the producing pane's cwd.
/// This accepts only existing regular files or directories on local paths.
pub(crate) fn resolve_path(
    value: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if !valid_text(value) {
        return None;
    }
    let path = if value.starts_with("file://") {
        file_uri_to_path(value)?
    } else {
        let value = without_location(value);
        if !allowed_path(value) {
            return None;
        }
        if let Some(tail) = value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\"))
        {
            home?.join(tail)
        } else {
            let path = PathBuf::from(value);
            if !cfg!(windows) && (windows_drive_path(value) || value.contains('\\')) {
                return None;
            }
            if cfg!(windows) && value.starts_with('/') {
                return None;
            }
            if path.is_absolute() {
                path
            } else {
                cwd?.join(path)
            }
        }
    };
    if !path.is_absolute() || !allowed_path(path.to_str()?) {
        return None;
    }
    let metadata = path.metadata().ok()?;
    (metadata.is_file() || metadata.is_dir()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_tokens_use_clicked_bytes_and_preserve_spaces() {
        for (text, clicked, expected) in [
            ("read `src/main.rs:42:3` now", "main", "src/main.rs:42:3"),
            (
                "read \"C:\\my project\\file.ts\" now",
                "project",
                "C:\\my project\\file.ts",
            ),
            ("open (/tmp/report.txt).", "report", "/tmp/report.txt"),
            ("open /tmp/a(b).txt,", "a(b)", "/tmp/a(b).txt"),
            ("文档 ./报告.txt", "报告", "./报告.txt"),
            (
                "open file:///C:/my%20project/test.txt",
                "project",
                "file:///C:/my%20project/test.txt",
            ),
            ("see README.md", "README", "README.md"),
        ] {
            assert_eq!(
                path_at_byte(text, text.find(clicked).unwrap()),
                Some(expected)
            );
        }
        for text in [
            "ordinary",
            "https://example.test/a.rs",
            "javascript:alert(1)",
            "\\\\server\\share\\file.txt",
        ] {
            assert_eq!(path_at_byte(text, 0), None, "{text}");
        }
        assert_eq!(path_at_byte("src/main.rs now", 11), None);
        assert_eq!(path_at_byte("`src/main.rs`", 0), None);
    }

    #[test]
    fn file_uri_conversion_is_local_and_decodes_exactly_once() {
        assert_eq!(
            local_file_uri_path("file:///C:/my%20project/%E6%96%87.txt", true).as_deref(),
            Some("C:/my project/文.txt")
        );
        assert_eq!(
            local_file_uri_path("file://localhost/tmp/a%2520.txt", false).as_deref(),
            Some("/tmp/a%20.txt")
        );
        for uri in [
            "file://server/share/a",
            "file:relative",
            "file:///C:/a%00.txt",
            "file:///C:/a%1B.txt",
            "file:///C:/a%GG.txt",
            "file:///C:/a?cmd=1",
            "file:///C:/a#x",
            "file:////server/share/a",
            "file:///C:/a:stream",
            "file:///C:/NUL.txt",
            "file:///C:/a%5Cb:stream",
        ] {
            assert_eq!(local_file_uri_path(uri, true), None, "{uri}");
        }
    }

    #[test]
    fn path_security_rejects_devices_networks_and_commands() {
        for value in [
            "\\\\server\\share\\file.txt",
            "//server/share/file.txt",
            "\\\\?\\C:\\file.txt",
            "C:relative.txt",
            "C:\\file.txt:stream",
            "a\u{0}.txt",
            "file.txt|whoami",
            "https://example.test",
            "NUL",
            "aux.txt",
            "a/COM1.txt",
        ] {
            assert!(!allowed_path(value), "{value}");
        }
        assert!(allowed_path("C:\\source\\file.rs"));
        assert!(allowed_path("./src/main.rs"));
        assert_eq!(
            without_location("C:\\src\\main.rs:42:3"),
            "C:\\src\\main.rs"
        );
    }

    #[test]
    fn unquoted_pi_code_paths_with_spaces_resolve_without_guessing_a_url() {
        let cwd = std::env::current_dir().unwrap();
        let base = cwd
            .join(".working/tmp")
            .join(format!("path-link-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("file with spaces.txt");
        std::fs::write(&file, "fixture").unwrap();
        for text in [
            file.display().to_string(),
            "./file with spaces.txt:42:3".into(),
        ] {
            let click = text.find("spaces").unwrap();
            assert_eq!(
                resolve_visible_path(&text, click, Some(&base), None),
                Some(file.clone())
            );
        }
        let text = "https://example.test/file with spaces.txt";
        assert!(
            resolve_visible_path(text, text.find("spaces").unwrap(), Some(&base), None).is_none()
        );
    }

    #[test]
    fn relative_file_resolution_uses_supplied_directory() {
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.join("Cargo.toml");
        assert_eq!(
            resolve_path("Cargo.toml:42", Some(&cwd), None),
            Some(expected.clone())
        );
        let uri = path_to_file_uri(&expected).unwrap();
        assert_eq!(resolve_path(&uri, None, None), Some(expected));
        assert!(resolve_path("Cargo.toml", None, None).is_none());
        assert!(resolve_path("./no-herdr-test-file-84973.txt", Some(&cwd), None).is_none());
        assert_eq!(
            resolve_path("~/Cargo.toml", None, Some(&cwd)),
            Some(cwd.join("Cargo.toml"))
        );
    }
}
