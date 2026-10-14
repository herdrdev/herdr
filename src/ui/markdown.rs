//! Markdown-ish rendering for pull request and Linear ticket bodies.
//!
//! Bodies arrive as GitHub/Linear markdown. Rather than pull in a parser we
//! recognise the constructs that actually carry meaning in a review pane —
//! headings, bullets, emphasis and inline code — and emit styled spans for
//! them. Everything unrecognised falls through as plain text, so a body is
//! never made less readable than the raw source.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::state::Palette;

use super::text::display_width;

/// Inline emphasis carried by a run of characters.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Emphasis {
    bold: bool,
    italic: bool,
    code: bool,
    link: bool,
}

/// Block-level role of a source line, which fixes the base colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    Heading,
    HeadingMuted,
    Body,
    Code,
}

/// One whitespace-delimited word plus the emphasis it was written with.
struct Word {
    text: String,
    emphasis: Emphasis,
    space_before: bool,
    icon: Option<&'static str>,
    marker: bool,
}

/// Render `body` as styled lines wrapped to `width`, prefixing every line with
/// `indent`. Returns a single em dash when the body is absent or blank.
pub(crate) fn body_lines(
    palette: &Palette,
    body: Option<&str>,
    width: usize,
    indent: &str,
) -> Vec<Line<'static>> {
    body_lines_with_list_style(palette, body, width, indent, false)
}

/// Settled transcripts use Claude's literal markers and hanging list rows.
pub(crate) fn transcript_body_lines(
    palette: &Palette,
    body: Option<&str>,
    width: usize,
    indent: &str,
) -> Vec<Line<'static>> {
    body_lines_with_list_style(palette, body, width, indent, true)
}

/// Chat prose uses the shared transcript styling.
pub(crate) fn transcript_chat_body_lines(
    palette: &Palette,
    body: Option<&str>,
    width: usize,
    indent: &str,
) -> Vec<Line<'static>> {
    transcript_body_lines(palette, body, width, indent)
}

fn body_lines_with_list_style(
    palette: &Palette,
    body: Option<&str>,
    width: usize,
    indent: &str,
    hanging_lists: bool,
) -> Vec<Line<'static>> {
    let dash = || {
        vec![Line::from(Span::styled(
            format!("{indent}—"),
            Style::default().fg(palette.text),
        ))]
    };
    let Some(body) = body.filter(|body| !body.trim().is_empty()) else {
        return dash();
    };
    let cleaned = strip_html_comments(body);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let source_lines: Vec<&str> = cleaned.lines().collect();
    let mut index = 0;
    let mut fenced = false;
    let mut discard_fence = false;
    let mut omitted_closing_state = false;
    let mut suppress_blank_after_heading = false;
    while index < source_lines.len() {
        let source_line = source_lines[index];
        index += 1;
        if source_line.trim_start().starts_with("```") {
            if fenced {
                fenced = false;
                discard_fence = false;
                continue;
            }
            fenced = true;
            let info = source_line.trim().trim_start_matches('`').trim();
            discard_fence = info == "closing-state";
            omitted_closing_state |= discard_fence;
            if !discard_fence && !info.is_empty() {
                lines.push(fence_header_line(palette, indent, info, width));
                suppress_blank_after_heading = false;
            }
            continue;
        }
        if fenced {
            if discard_fence {
                continue;
            }
            suppress_blank_after_heading = false;
            let inner_width = width.saturating_sub(display_width(indent));
            let words = split_wide_words(
                parse_words(palette, source_line.trim_end(), Block::Code),
                inner_width,
            );
            let rows = wrap_words(&words, inner_width);
            if rows.is_empty() {
                lines.push(code_line(palette, indent, Vec::new(), width));
            } else {
                for row in rows {
                    lines.push(code_line(palette, indent, row, width));
                }
            }
            continue;
        }

        if source_line.trim_start().starts_with("<task-notification>") {
            let mut paragraph = source_line.to_string();
            while !paragraph.contains("</task-notification>") && index < source_lines.len() {
                paragraph.push('\n');
                paragraph.push_str(source_lines[index]);
                index += 1;
            }
            let summary =
                tag_value(&paragraph, "summary").unwrap_or_else(|| "background task".into());
            let status = tag_value(&paragraph, "status").unwrap_or_default();
            let exit = exit_code(&status)
                .map(|code| format!(" (exit {code})"))
                .unwrap_or_default();
            let mut spans = Vec::new();
            if !indent.is_empty() {
                spans.push(Span::raw(indent.to_string()));
            }
            spans.push(Span::styled(
                format!("⟳ background task {summary}{exit}"),
                Style::default().fg(palette.overlay0),
            ));
            lines.push(Line::from(spans));
            suppress_blank_after_heading = false;
            continue;
        }

        let (block, prefix, content, marker) = classify(source_line, hanging_lists);
        if content.trim().is_empty() && prefix.is_empty() {
            if suppress_blank_after_heading {
                continue;
            }
            if lines.last().is_some_and(|line| !line.spans.is_empty()) {
                lines.push(Line::default());
            }
            continue;
        }
        let heading = matches!(block, Block::Heading | Block::HeadingMuted);
        if heading && lines.last().is_some_and(|line| !line.spans.is_empty()) {
            lines.push(Line::default());
        }

        let mut words = Vec::new();
        let hanging_width = if marker {
            display_width(&prefix) + 1
        } else {
            0
        };
        let has_prefix = !prefix.is_empty();
        if has_prefix {
            words.push(Word {
                text: prefix,
                emphasis: Emphasis::default(),
                space_before: false,
                icon: None,
                marker: true,
            });
        }
        let mut content_words = parse_words(palette, content, block);
        if has_prefix {
            if let Some(first) = content_words.first_mut() {
                first.space_before = true;
            }
        }
        words.extend(content_words);
        let base_width = width.saturating_sub(display_width(indent));
        let line_width = base_width.saturating_sub(hanging_width);
        let words = split_wide_words(words, line_width);
        let rows = wrap_words(&words, line_width);

        for (row_index, row) in rows.into_iter().enumerate() {
            if row_index > 0 && hanging_width > 0 {
                lines.push(styled_line(
                    palette,
                    block,
                    &format!("{indent}{}", " ".repeat(hanging_width)),
                    row,
                ));
            } else {
                lines.push(styled_line(palette, block, indent, row));
            }
        }
        suppress_blank_after_heading = heading;
    }

    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        if omitted_closing_state {
            return Vec::new();
        }
        return dash();
    }
    lines
}

/// Split a source line into its block role, a literal prefix that must lead the
/// first wrapped row, and the inline-markdown remainder.
fn classify(source: &str, literal_lists: bool) -> (Block, String, &str, bool) {
    let leading_spaces = source.chars().take_while(|ch| *ch == ' ').count();
    let trimmed = source.trim();
    let heading = trimmed.trim_start_matches('#');
    if heading.len() != trimmed.len() {
        let level = trimmed.len() - heading.len();
        return (
            if level >= 6 {
                Block::HeadingMuted
            } else {
                Block::Heading
            },
            String::new(),
            heading.trim_start(),
            false,
        );
    }
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            for task in ["[ ] ", "[x] ", "[X] "] {
                if let Some(item) = rest.strip_prefix(task) {
                    let box_mark = if task.starts_with("[ ]") {
                        "☐"
                    } else {
                        "☑"
                    };
                    return (Block::Body, box_mark.to_string(), item, true);
                }
            }
            let level = (leading_spaces / 2).min(2);
            let bullet = if literal_lists {
                "-"
            } else {
                ["•", "◦", "▪"][level]
            };
            return (Block::Body, bullet.to_string(), rest, true);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return (Block::Body, "│".to_string(), rest, true);
    }
    if let Some((number, rest)) = ordered_marker(trimmed) {
        let marker = if leading_spaces >= 2 {
            lower_alpha_marker(&number).unwrap_or(number)
        } else {
            number
        };
        return (Block::Body, marker, rest, true);
    }
    (Block::Body, String::new(), trimmed, false)
}

/// Recognise `1. item` / `12) item` and return its rendered marker.
fn ordered_marker(trimmed: &str) -> Option<(String, &str)> {
    let digits = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .filter(|end| *end > 0 && *end <= 3)?;
    let rest = &trimmed[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((format!("{}.", &trimmed[..digits]), rest))
}

/// Parse inline markdown into whitespace-delimited words. Headings and code
/// blocks carry their emphasis from the block role instead.
fn parse_words(palette: &Palette, content: &str, block: Block) -> Vec<Word> {
    let base = Emphasis {
        code: block == Block::Code,
        ..Emphasis::default()
    };
    let chars: Vec<char> = content.chars().collect();
    let mut words: Vec<Word> = Vec::new();
    let mut current = String::new();
    let mut emphasis = base;
    let mut index = 0usize;
    let mut space_before = false;
    let mut has_pending_space = false;

    let flush = |current: &mut String,
                 emphasis: Emphasis,
                 words: &mut Vec<Word>,
                 space_before: &mut bool| {
        if current.is_empty() {
            return;
        }
        words.push(Word {
            text: std::mem::take(current),
            emphasis,
            space_before: *space_before,
            icon: None,
            marker: false,
        });
        *space_before = false;
    };

    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            flush(&mut current, emphasis, &mut words, &mut space_before);
            has_pending_space = true;
            index += 1;
            continue;
        }
        if !emphasis.code && ch == '[' {
            if let Some((label, target, end)) = markdown_link(&chars, index) {
                flush(&mut current, emphasis, &mut words, &mut space_before);
                let icon = link_icon(palette, &target, &label);
                append_link_text(&label, icon, has_pending_space, &mut words);
                has_pending_space = false;
                index = end;
                continue;
            }
        }
        if !emphasis.code && starts_with(&chars, index, "https://") {
            flush(&mut current, emphasis, &mut words, &mut space_before);
            let end = chars[index..]
                .iter()
                .position(|character| character.is_whitespace())
                .map_or(chars.len(), |offset| index + offset);
            let mut url_end = end;
            while url_end > index
                && matches!(
                    chars[url_end - 1],
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']'
                )
            {
                url_end -= 1;
            }
            if url_end > index {
                let url: String = chars[index..url_end].iter().collect();
                let label = shortened_url(&url);
                let icon = link_icon(palette, &url, &label);
                append_link_text(&label, icon, has_pending_space, &mut words);
                if url_end < end {
                    let punctuation: String = chars[url_end..end].iter().collect();
                    words.push(Word {
                        text: punctuation,
                        emphasis: base,
                        space_before: false,
                        icon: None,
                        marker: false,
                    });
                }
                has_pending_space = false;
                index = end;
                continue;
            }
        }
        if ch == '`' && !emphasis.code {
            if let Some(end) = chars[index + 1..].iter().position(|c| *c == '`') {
                flush(&mut current, emphasis, &mut words, &mut space_before);
                let literal: String = chars[index + 1..index + 1 + end].iter().collect();
                for (token_index, token) in literal.split_whitespace().enumerate() {
                    words.push(Word {
                        text: token.to_string(),
                        emphasis: Emphasis { code: true, ..base },
                        space_before: if token_index == 0 {
                            has_pending_space
                        } else {
                            true
                        },
                        icon: None,
                        marker: false,
                    });
                    has_pending_space = false;
                }
                index += end + 2;
                continue;
            }
        }
        if !emphasis.code && (ch == '*' || ch == '_') {
            let doubled = chars.get(index + 1).is_some_and(|next| *next == ch);
            // A lone `_` is left literal so snake_case identifiers survive.
            if doubled {
                flush(&mut current, emphasis, &mut words, &mut space_before);
                emphasis.bold = !emphasis.bold;
                index += 2;
                continue;
            }
            if ch == '*' {
                flush(&mut current, emphasis, &mut words, &mut space_before);
                emphasis.italic = !emphasis.italic;
                index += 1;
                continue;
            }
        }
        if current.is_empty() {
            space_before = has_pending_space;
        }
        has_pending_space = false;
        current.push(ch);
        index += 1;
    }
    flush(&mut current, emphasis, &mut words, &mut space_before);
    words
}

fn markdown_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let label_end = chars[start + 1..]
        .iter()
        .position(|character| *character == ']')?
        + start
        + 1;
    if chars.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let target_start = label_end + 2;
    let target_end = chars[target_start..]
        .iter()
        .position(|character| *character == ')')?
        + target_start;
    let label = chars[start + 1..label_end].iter().collect();
    let target = chars[target_start..target_end].iter().collect();
    Some((label, target, target_end + 1))
}

fn starts_with(chars: &[char], start: usize, needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    chars.get(start..start + needle.len()) == Some(needle.as_slice())
}

fn append_link_text(label: &str, icon: &'static str, space_before: bool, words: &mut Vec<Word>) {
    for (index, text) in label.split_whitespace().enumerate() {
        let icon = (index == 0).then_some(icon);
        words.push(Word {
            text: text.to_string(),
            emphasis: Emphasis {
                link: true,
                ..Emphasis::default()
            },
            space_before: if index == 0 { space_before } else { true },
            icon,
            marker: false,
        });
    }
}

fn shortened_url(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let path_end = without_scheme
        .find(['?', '#'])
        .unwrap_or(without_scheme.len());
    let visible = &without_scheme[..path_end];
    truncate_cells(visible, 48)
}

fn truncate_cells(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut output = String::new();
    let mut used = 0;
    for character in value.chars() {
        let character_width = display_width(&character.to_string());
        if used + character_width + 1 > max_width {
            break;
        }
        output.push(character);
        used += character_width;
    }
    output.push('…');
    output
}

fn link_icon(palette: &Palette, target: &str, label: &str) -> &'static str {
    let host = url_host(target);
    let dark = palette.appearance() == Some(crate::terminal_theme::HostAppearance::Dark);
    if host_matches(&host, "github.com") || is_github_reference(label) {
        crate::ui::icons::GITHUB
    } else if host_matches(&host, "linear.app") {
        crate::ui::icons::LINEAR
    } else if host_matches(&host, "sentry.io") {
        crate::ui::icons::SENTRY
    } else if host_matches(&host, "vercel.com") || host_matches(&host, "vercel.app") {
        if dark {
            crate::ui::icons::VERCEL_DARK
        } else {
            crate::ui::icons::VERCEL
        }
    } else if host_matches(&host, "supabase.com") || host_matches(&host, "supabase.co") {
        if dark {
            crate::ui::icons::SUPABASE_DARK
        } else {
            crate::ui::icons::SUPABASE
        }
    } else if host_matches(&host, "stripe.com") {
        if dark {
            crate::ui::icons::STRIPE_DARK
        } else {
            crate::ui::icons::STRIPE
        }
    } else if host_matches(&host, "obsidian.md") {
        if dark {
            crate::ui::icons::OBSIDIAN_LINK_DARK
        } else {
            crate::ui::icons::OBSIDIAN_LINK
        }
    } else if host_matches(&host, "slack.com") {
        if dark {
            crate::ui::icons::SLACK_DARK
        } else {
            crate::ui::icons::SLACK
        }
    } else if host_matches(&host, "youtube.com") {
        if dark {
            crate::ui::icons::YOUTUBE_DARK
        } else {
            crate::ui::icons::YOUTUBE
        }
    } else {
        crate::ui::icons::LINK
    }
}

fn url_host(url: &str) -> String {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_ascii_lowercase()
}

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

fn is_github_reference(label: &str) -> bool {
    let reference = label.trim();
    if let Some(number) = reference.strip_prefix('#') {
        return !number.is_empty() && number.chars().all(|character| character.is_ascii_digit());
    }
    let Some((repository, number)) = reference.rsplit_once('#') else {
        return false;
    };
    let mut segments = repository.split('/');
    let Some(owner) = segments.next() else {
        return false;
    };
    let Some(name) = segments.next() else {
        return false;
    };
    segments.next().is_none()
        && !owner.is_empty()
        && !name.is_empty()
        && owner
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_.-".contains(ch))
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || "_.-".contains(ch))
        && !number.is_empty()
        && number.chars().all(|character| character.is_ascii_digit())
}

fn lower_alpha_marker(marker: &str) -> Option<String> {
    let number = marker.strip_suffix('.')?.parse::<u8>().ok()?;
    let letter = b'a'.checked_add(number.checked_sub(1)?)?;
    letter
        .is_ascii_lowercase()
        .then(|| format!("{}.", char::from(letter)))
}

/// Break any word wider than the line into chunks that fit.
///
/// A long URL has no space to break at, so without this it overflows its row
/// and is clipped by the surrounding fixed-height rect.
fn split_wide_words(words: Vec<Word>, width: usize) -> Vec<Word> {
    if width == 0 {
        return words;
    }
    let mut out = Vec::with_capacity(words.len());
    for word in words {
        let icon_width = if word.icon.is_some() { 2 } else { 0 };
        let code_padding = usize::from(word.emphasis.code) * 2;
        let content_width = display_width(&word.text);
        if content_width + icon_width + code_padding <= width {
            out.push(word);
            continue;
        }
        let chunk_limit = width.saturating_sub(icon_width + code_padding);
        if chunk_limit == 0 {
            out.push(word);
            continue;
        }
        let mut chunk = String::new();
        let mut chunk_width = 0usize;
        let mut first_chunk = true;
        for character in word.text.chars() {
            let character_width = display_width(&character.to_string());
            if chunk_width + character_width > chunk_limit && !chunk.is_empty() {
                let chunk_space_before = if first_chunk {
                    word.space_before
                } else {
                    false
                };
                let icon = if first_chunk { word.icon } else { None };
                out.push(Word {
                    text: std::mem::take(&mut chunk),
                    emphasis: word.emphasis,
                    space_before: chunk_space_before,
                    icon,
                    marker: word.marker,
                });
                first_chunk = false;
                chunk_width = 0;
            }
            chunk.push(character);
            chunk_width += character_width;
        }
        if !chunk.is_empty() {
            let chunk_space_before = if first_chunk {
                word.space_before
            } else {
                false
            };
            let icon = if first_chunk { word.icon } else { None };
            out.push(Word {
                text: chunk,
                emphasis: word.emphasis,
                space_before: chunk_space_before,
                icon,
                marker: word.marker,
            });
        }
    }
    out
}

/// Greedily pack glued word units into rows no wider than `width`.
fn wrap_words(words: &[Word], width: usize) -> Vec<Vec<&Word>> {
    if width == 0 || words.is_empty() {
        return Vec::new();
    }
    let mut rows: Vec<Vec<&Word>> = Vec::new();
    let mut row: Vec<&Word> = Vec::new();
    let mut unit_start = 0;
    while unit_start < words.len() {
        let mut unit_end = unit_start + 1;
        while unit_end < words.len()
            && (!words[unit_end].space_before || words[unit_end - 1].marker)
        {
            unit_end += 1;
        }
        let unit = &words[unit_start..unit_end];
        // Keep a glued unit together when it fits; oversized units can break between words.
        let mut candidate = row.clone();
        candidate.extend(unit.iter());
        if !row.is_empty() && row_display_width(&candidate) > width {
            rows.push(std::mem::take(&mut row));
        }
        for word in unit {
            let mut candidate = row.clone();
            candidate.push(word);
            if !row.is_empty() && row_display_width(&candidate) > width {
                rows.push(std::mem::take(&mut row));
            }
            row.push(word);
        }
        unit_start = unit_end;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn row_display_width(row: &[&Word]) -> usize {
    let mut width = 0;
    let mut previous_code = false;
    for word in row {
        if previous_code && !word.emphasis.code {
            width += 1;
        }
        if word.space_before && width > 0 {
            width += 1;
        }
        if word.icon.is_some() {
            width += 2;
        }
        if word.emphasis.code && !previous_code {
            width += 1;
        }
        width += display_width(&word.text);
        previous_code = word.emphasis.code;
    }
    if previous_code {
        width += 1;
    }
    width
}

/// Turn one packed row into a `Line`, merging neighbouring words that share
/// emphasis so the span count stays proportional to the styling, not the text.
fn styled_line(palette: &Palette, block: Block, indent: &str, row: Vec<&Word>) -> Line<'static> {
    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push(if block == Block::Code {
            Span::styled(
                indent.to_string(),
                Style::default()
                    .fg(palette.subtext0)
                    .bg(palette.surface_dim),
            )
        } else {
            Span::raw(indent.to_string())
        });
    }
    let mut pending: Option<(String, Emphasis, bool)> = None;
    let mut has_content = false;
    for word in &row {
        if let Some(icon) = word.icon {
            flush_styled_run(palette, block, &mut spans, pending.take());
            if word.space_before && has_content {
                spans.push(Span::styled(
                    " ",
                    style_for(palette, block, Emphasis::default()),
                ));
            }
            let icon_color = if icon == crate::ui::icons::LINK {
                palette.overlay1
            } else {
                palette.blue
            };
            spans.push(Span::styled(
                format!("{icon} "),
                Style::default().fg(icon_color),
            ));
            pending = Some((word.text.clone(), word.emphasis, word.marker));
            has_content = true;
            continue;
        }
        let matching_run = pending.as_ref().is_some_and(|(_, emphasis, marker)| {
            *emphasis == word.emphasis && *marker == word.marker
        });
        if matching_run {
            let (text, _, _) = pending.as_mut().expect("matching styled run");
            if word.space_before {
                text.push(' ');
            }
            text.push_str(&word.text);
            has_content = true;
            continue;
        }
        flush_styled_run(palette, block, &mut spans, pending.take());
        if word.space_before && has_content {
            spans.push(Span::styled(
                " ",
                style_for(palette, block, Emphasis::default()),
            ));
        }
        let text = if word.emphasis.code && block != Block::Code {
            format!(" {}", word.text)
        } else {
            word.text.clone()
        };
        pending = Some((text, word.emphasis, word.marker));
        has_content = true;
    }
    flush_styled_run(palette, block, &mut spans, pending.take());
    Line::from(spans)
}

fn flush_styled_run(
    palette: &Palette,
    block: Block,
    spans: &mut Vec<Span<'static>>,
    run: Option<(String, Emphasis, bool)>,
) {
    let Some((mut text, emphasis, marker)) = run else {
        return;
    };
    if emphasis.code && block != Block::Code {
        text.push(' ');
    }
    let mut style = style_for(palette, block, emphasis);
    if marker {
        style = style.fg(palette.overlay1);
    }
    spans.push(Span::styled(text, style));
}

fn style_for(palette: &Palette, block: Block, emphasis: Emphasis) -> Style {
    let colour = if block == Block::Code {
        palette.subtext0
    } else if emphasis.link {
        palette.blue
    } else if emphasis.italic || block == Block::HeadingMuted {
        palette.subtext0
    } else {
        palette.text
    };
    let mut style = Style::default().fg(colour);
    if block == Block::Code {
        style = style.bg(palette.surface_dim);
    } else if emphasis.code {
        style = style.fg(palette.text).bg(palette.surface0);
    }
    if !emphasis.code && (emphasis.bold || matches!(block, Block::Heading | Block::HeadingMuted)) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if emphasis.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

fn fence_header_line(palette: &Palette, indent: &str, label: &str, width: usize) -> Line<'static> {
    let inner_width = width.saturating_sub(display_width(indent));
    let label = truncate_cells(label, inner_width);
    let mut spans = vec![Span::styled(
        indent.to_string(),
        Style::default()
            .fg(palette.overlay0)
            .bg(palette.surface_dim),
    )];
    let label_width = display_width(&label);
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(palette.overlay0)
            .bg(palette.surface_dim),
    ));
    if label_width < inner_width {
        spans.push(Span::styled(
            " ".repeat(inner_width - label_width),
            Style::default().bg(palette.surface_dim),
        ));
    }
    Line::from(spans)
}

fn code_line(palette: &Palette, indent: &str, row: Vec<&Word>, width: usize) -> Line<'static> {
    let mut line = styled_line(palette, Block::Code, indent, row);
    let remaining = width.saturating_sub(line.width());
    if remaining > 0 {
        line.spans.push(Span::styled(
            " ".repeat(remaining),
            Style::default().bg(palette.surface_dim),
        ));
    }
    line
}

fn tag_value(text: &str, tag: &str) -> Option<String> {
    let start = text.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = text[start..].find(&format!("</{tag}>"))? + start;
    Some(text[start..end].trim().to_string())
}

fn exit_code(status: &str) -> Option<i32> {
    status.split_whitespace().find_map(|part| {
        part.trim_matches(|ch: char| !ch.is_ascii_digit() && ch != '-')
            .parse()
            .ok()
    })
}

fn strip_html_comments(text: &str) -> String {
    let mut remaining = text;
    let mut output = String::new();
    while let Some(start) = remaining.find("<!--") {
        output.push_str(&remaining[..start]);
        let after_start = &remaining[start + 4..];
        let Some(end) = after_start.find("-->") else {
            return output;
        };
        remaining = &after_start[end + 3..];
    }
    output.push_str(remaining);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Palette;

    fn palette() -> Palette {
        Palette::catppuccin()
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    type SpanSnapshot = (
        String,
        Option<ratatui::style::Color>,
        Option<ratatui::style::Color>,
        bool,
        bool,
    );

    fn styled_snapshot(lines: &[Line<'static>]) -> Vec<Vec<SpanSnapshot>> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| {
                        (
                            span.content.to_string(),
                            span.style.fg,
                            span.style.bg,
                            span.style.add_modifier.contains(Modifier::BOLD),
                            span.style.add_modifier.contains(Modifier::ITALIC),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_missing_body_renders_a_single_dash() {
        assert_eq!(text(&body_lines(&palette(), None, 40, " ")), vec![" —"]);
        assert_eq!(
            text(&body_lines(&palette(), Some("   \n\n"), 40, " ")),
            vec![" —"]
        );
    }

    #[test]
    fn bold_runs_become_bold_spans_without_their_markers() {
        let lines = body_lines(&palette(), Some("ships **the fix** today"), 40, "");
        assert_eq!(text(&lines), vec!["ships the fix today"]);
        let bold: Vec<&str> = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(bold, vec!["the fix"]);
    }

    #[test]
    fn headings_use_text_colour_and_separate_from_the_previous_row() {
        let p = palette();
        let lines = body_lines(&p, Some("paragraph\n## what it does\n\nnext"), 40, "");
        assert_eq!(text(&lines), vec!["paragraph", "", "what it does", "next"]);
        assert_eq!(
            styled_snapshot(&lines)[2],
            vec![("what it does".into(), Some(p.text), None, true, false)]
        );
        let h6 = body_lines(&p, Some("###### small heading"), 40, "");
        assert_eq!(
            styled_snapshot(&h6),
            vec![vec![(
                "small heading".into(),
                Some(p.subtext0),
                None,
                true,
                false
            )]]
        );
    }

    #[test]
    fn body_is_regular_weight_and_only_strong_runs_are_bold() {
        let p = palette();
        let lines = body_lines(&p, Some("ships **the fix** today"), 40, "");
        assert_eq!(
            styled_snapshot(&lines),
            vec![vec![
                ("ships".into(), Some(p.text), None, false, false),
                (" ".into(), Some(p.text), None, false, false),
                ("the fix".into(), Some(p.text), None, true, false),
                (" ".into(), Some(p.text), None, false, false),
                ("today".into(), Some(p.text), None, false, false),
            ]]
        );
    }

    #[test]
    fn italic_runs_use_muted_text_colour() {
        let p = palette();
        let lines = body_lines(&p, Some("a *quiet note* here"), 40, "");
        assert_eq!(
            styled_snapshot(&lines),
            vec![vec![
                ("a".into(), Some(p.text), None, false, false),
                (" ".into(), Some(p.text), None, false, false),
                ("quiet note".into(), Some(p.subtext0), None, false, true),
                (" ".into(), Some(p.text), None, false, false),
                ("here".into(), Some(p.text), None, false, false),
            ]]
        );
    }

    #[test]
    fn heading_stays_bold_and_has_no_blank_row_after_it() {
        let lines = body_lines(&palette(), Some("## what it does"), 40, "");
        assert_eq!(text(&lines), vec!["what it does"]);
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn inline_code_is_a_padded_foreground_chip() {
        let p = palette();
        let lines = body_lines(&p, Some("call `render_home` here"), 40, "");
        assert_eq!(
            styled_snapshot(&lines),
            vec![vec![
                ("call".into(), Some(p.text), None, false, false),
                (" ".into(), Some(p.text), None, false, false),
                (
                    " render_home ".into(),
                    Some(p.text),
                    Some(p.surface0),
                    false,
                    false
                ),
                (" ".into(), Some(p.text), None, false, false),
                ("here".into(), Some(p.text), None, false, false),
            ]]
        );
    }

    #[test]
    fn inline_spans_stay_glued_to_adjacent_source_text() {
        let p = palette();
        for (source, expected) in [
            (
                "using `restore-snapshot.sh`, verifying",
                "using  restore-snapshot.sh , verifying",
            ),
            (
                "a **canary strategy**, routing",
                "a canary strategy, routing",
            ),
            ("(`v2`)", "( v2 )"),
            ("foo**bar**", "foobar"),
        ] {
            let lines = body_lines(&p, Some(source), 80, "");
            assert_eq!(text(&lines), vec![expected], "{source}");
        }
        let lines = body_lines(&p, Some("`restore-snapshot.sh`,"), 80, "");
        assert_eq!(lines[0].spans[0].content, " restore-snapshot.sh ");
        assert_eq!(lines[0].spans[0].style.fg, Some(p.text));
        assert_eq!(lines[0].spans[0].style.bg, Some(p.surface0));
        assert_eq!(lines[0].spans[1].content, ",");
        assert_eq!(lines[0].spans[1].style.fg, Some(p.text));
    }

    #[test]
    fn glued_words_wrap_as_one_unit() {
        let lines = body_lines(&palette(), Some("ab x`cd`,"), 5, "");
        assert_eq!(text(&lines), vec!["ab", "x cd ", ","]);
    }

    #[test]
    fn an_over_wide_glued_unit_breaks_inside_itself() {
        let lines = body_lines(&palette(), Some("abc`de`, x"), 4, "");
        assert_eq!(text(&lines), vec!["abc", " de ", ", x"]);
    }

    #[test]
    fn a_lone_underscore_stays_literal_so_identifiers_survive() {
        let lines = body_lines(&palette(), Some("set work_index.enabled now"), 40, "");
        assert_eq!(text(&lines), vec!["set work_index.enabled now"]);
    }

    #[test]
    fn bullets_and_tasks_get_markers_and_wrap_under_their_width() {
        let lines = body_lines(
            &palette(),
            Some("- first item\n- [ ] open task\n- [x] done task\n1. ordered"),
            40,
            "",
        );
        assert_eq!(
            text(&lines),
            vec!["• first item", "☐ open task", "☑ done task", "1. ordered"]
        );
    }

    #[test]
    fn long_lines_wrap_within_the_requested_width() {
        let lines = body_lines(&palette(), Some("alpha beta gamma delta"), 11, "");
        assert_eq!(text(&lines), vec!["alpha beta", "gamma delta"]);
        assert!(lines.iter().all(|line| line.width() <= 11));
    }

    #[test]
    fn fenced_code_drops_its_fences_and_keeps_the_code_colour() {
        let lines = body_lines(
            &palette(),
            Some("before\n```sh\njust check\n```\nafter"),
            40,
            "",
        );
        assert_eq!(
            text(&lines),
            vec![
                String::from("before"),
                format!("sh{}", " ".repeat(38)),
                format!("just check{}", " ".repeat(30)),
                String::from("after")
            ]
        );
        assert_eq!(lines[1].spans[0].style.fg, Some(palette().overlay0));
        assert_eq!(lines[1].spans[0].style.bg, Some(palette().surface_dim));
        assert_eq!(lines[2].spans[0].style.fg, Some(palette().subtext0));
        assert_eq!(lines[2].spans[0].style.bg, Some(palette().surface_dim));
    }

    #[test]
    fn bare_urls_drop_the_scheme_and_shorten_to_48_cells() {
        let p = palette();
        let url = "https://example.com/a/really/long/path/that/keeps/going/and/going?token=hidden";
        let lines = body_lines(&p, Some(url), 80, "");
        let visible = text(&lines).concat();
        let label = visible
            .strip_prefix(&format!("{} ", crate::ui::icons::LINK))
            .unwrap();
        assert_eq!(display_width(label), 48);
        assert!(label.ends_with('…'));
        assert!(label.starts_with("example.com/"));
        assert_eq!(lines[0].spans[0].style.fg, Some(p.overlay1));
        assert_eq!(lines[0].spans[1].style.fg, Some(p.blue));
    }

    #[test]
    fn markdown_links_show_labels_with_host_brand_icons() {
        let p = palette();
        let lines = body_lines(
            &p,
            Some("[PR 12](https://github.com/acme/repo/pull/12) and [issue](https://linear.app/acme/issue/1)"),
            80,
            "",
        );
        assert_eq!(
            text(&lines),
            vec![format!(
                "{} PR 12 and {} issue",
                crate::ui::icons::GITHUB,
                crate::ui::icons::LINEAR
            )]
        );
        assert_eq!(
            lines[0].spans[0].content,
            format!("{} ", crate::ui::icons::GITHUB)
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(p.blue));
        assert_eq!(lines[0].spans[1].content, "PR 12");
        assert_eq!(lines[0].spans[1].style.fg, Some(p.blue));
    }

    #[test]
    fn every_supported_link_brand_uses_the_theme_variant() {
        let dark = Palette::catppuccin();
        let light = Palette::catppuccin_latte();
        let cases = [
            (
                "https://github.com/a/b",
                crate::ui::icons::GITHUB,
                crate::ui::icons::GITHUB,
            ),
            (
                "https://linear.app/a",
                crate::ui::icons::LINEAR,
                crate::ui::icons::LINEAR,
            ),
            (
                "https://sentry.io/a",
                crate::ui::icons::SENTRY,
                crate::ui::icons::SENTRY,
            ),
            (
                "https://vercel.app/a",
                crate::ui::icons::VERCEL_DARK,
                crate::ui::icons::VERCEL,
            ),
            (
                "https://supabase.co/a",
                crate::ui::icons::SUPABASE_DARK,
                crate::ui::icons::SUPABASE,
            ),
            (
                "https://stripe.com/a",
                crate::ui::icons::STRIPE_DARK,
                crate::ui::icons::STRIPE,
            ),
            (
                "https://obsidian.md/a",
                crate::ui::icons::OBSIDIAN_LINK_DARK,
                crate::ui::icons::OBSIDIAN_LINK,
            ),
            (
                "https://slack.com/a",
                crate::ui::icons::SLACK_DARK,
                crate::ui::icons::SLACK,
            ),
            (
                "https://youtube.com/a",
                crate::ui::icons::YOUTUBE_DARK,
                crate::ui::icons::YOUTUBE,
            ),
        ];
        for (url, dark_icon, light_icon) in cases {
            for (p, icon) in [(&dark, dark_icon), (&light, light_icon)] {
                let lines = body_lines(p, Some(&format!("[site]({url})")), 40, "");
                assert_eq!(lines[0].spans[0].content, format!("{icon} "), "{url}");
            }
        }
    }

    #[test]
    fn github_reference_glyph_requires_a_markdown_link() {
        let linked = body_lines(
            &palette(),
            Some("[#123](#123) [acme/herdr#42](url)"),
            60,
            "",
        );
        assert_eq!(
            text(&linked),
            vec![format!(
                "{} #123 {} acme/herdr#42",
                crate::ui::icons::GITHUB,
                crate::ui::icons::GITHUB
            )]
        );
        let bare = body_lines(&palette(), Some("acme/herdr#42 #123"), 60, "");
        assert_eq!(text(&bare), vec!["acme/herdr#42 #123"]);
        assert!(bare[0]
            .spans
            .iter()
            .all(|span| span.style.fg == Some(palette().text)));
    }

    #[test]
    fn list_levels_and_nested_ordered_markers_wrap_with_hanging_indents() {
        let lines = body_lines(
            &palette(),
            Some("- first item that wraps\n  - nested item\n    - third\n1. one\n  1. two"),
            16,
            "",
        );
        assert!(lines.iter().all(|line| line.width() <= 16));
        let rendered = text(&lines);
        assert!(rendered.contains(&"• first item".to_string()));
        assert!(rendered.contains(&"  that wraps".to_string()));
        assert!(rendered.contains(&"◦ nested item".to_string()));
        assert!(rendered.contains(&"▪ third".to_string()));
        assert!(rendered.contains(&"1. one".to_string()));
        assert!(rendered.contains(&"a. two".to_string()));
        assert_eq!(lines[0].spans[0].style.fg, Some(palette().overlay1));
    }

    #[test]
    fn transcript_keeps_literal_claude_list_markers() {
        let lines = transcript_body_lines(&palette(), Some("- first\n  - second"), 30, "");
        assert_eq!(text(&lines), vec!["- first", "- second"]);
    }

    #[test]
    fn task_notifications_collapse_to_one_muted_status_row() {
        let p = palette();
        let lines = body_lines(
            &p,
            Some("<task-notification>\n<summary>Run checks</summary>\n<status>exit code 2</status>\n</task-notification>"),
            80,
            "",
        );
        assert_eq!(text(&lines), vec!["⟳ background task Run checks (exit 2)"]);
        assert_eq!(
            styled_snapshot(&lines),
            vec![vec![(
                "⟳ background task Run checks (exit 2)".into(),
                Some(p.overlay0),
                None,
                false,
                false,
            )]]
        );
    }

    #[test]
    fn closing_state_fences_render_no_rows() {
        assert!(body_lines(
            &palette(),
            Some("```closing-state\nprivate state\n```"),
            40,
            ""
        )
        .is_empty());
    }

    #[test]
    fn inline_code_chip_padding_is_included_when_wrapping() {
        let p = palette();
        let lines = body_lines(&p, Some("x `abcdefghijk` y"), 10, "");
        assert!(lines.iter().all(|line| line.width() <= 10));
        let code: Vec<&Span<'static>> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.bg == Some(p.surface0))
            .collect();
        assert_eq!(code.len(), 2);
        assert_eq!(code[0].content, " abcdefgh ");
        assert_eq!(code[1].content, " ijk ");
    }

    #[test]
    fn html_comments_are_stripped_before_rendering() {
        let lines = body_lines(&palette(), Some("keep <!-- drop me --> this"), 40, "");
        assert_eq!(text(&lines), vec!["keep this"]);
    }

    #[test]
    fn a_zero_width_body_renders_nothing_rather_than_panicking() {
        assert_eq!(
            text(&body_lines(&palette(), Some("anything at all"), 0, "")),
            vec!["—"]
        );
    }
}
