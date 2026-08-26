use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub(crate) fn display_width_u16(text: &str) -> u16 {
    display_width(text).min(u16::MAX as usize) as u16
}

pub(crate) fn wrap_words(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0;
        for word in paragraph.split_whitespace() {
            let word_width = display_width(word);
            if !current.is_empty() && current_width + 1 + word_width <= max_width {
                current.push(' ');
                current.push_str(word);
                current_width += 1 + word_width;
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }

            if word_width <= max_width {
                current.push_str(word);
                current_width = word_width;
                continue;
            }

            let chunks = split_by_display_width(word, max_width);
            let last = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.into_iter().enumerate() {
                if index == last {
                    current_width = display_width(&chunk);
                    current = chunk;
                } else {
                    lines.push(chunk);
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }

    lines
}

pub(crate) fn truncate_end(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let prefix = take_prefix_width(text, max_width.saturating_sub(1));
    format!("{prefix}…")
}

pub(crate) fn middle_elide(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }

    let content_width = max_width.saturating_sub(1);
    let left_width = content_width / 2;
    let right_width = content_width.saturating_sub(left_width);
    let prefix = take_prefix_width(text, left_width);
    let suffix = take_suffix_width(text, right_width);
    format!("{prefix}…{suffix}")
}

fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

fn take_suffix_width(text: &str, max_width: usize) -> String {
    let mut output = Vec::new();
    let mut width = 0usize;
    for ch in text.chars().rev() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output.into_iter().rev().collect()
}

fn split_by_display_width(text: &str, max_width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if !chunk.is_empty() && width + ch_width > max_width {
            chunks.push(std::mem::take(&mut chunk));
            width = 0;
        }
        chunk.push(ch);
        width += ch_width;
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_end_uses_display_width() {
        let text = truncate_end("提交 herdr 的反馈", 16);

        assert_eq!(text, "提交 herdr 的反…");
        assert!(display_width(&text) <= 16);
    }

    #[test]
    fn middle_elide_uses_display_width() {
        let text = middle_elide("重构用户认证模块并迁移到统一登录服务", 12);

        assert!(text.contains('…'));
        assert!(display_width(&text) <= 12);
    }

    #[test]
    fn wrap_words_keeps_lines_within_display_width() {
        let lines = wrap_words("Review 提交结果 and-this-token-is-long", 12);

        assert_eq!(lines, ["Review", "提交结果", "and-this-tok", "en-is-long"]);
        assert!(lines.iter().all(|line| display_width(line) <= 12));
    }
}
