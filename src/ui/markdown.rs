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
}

/// Block-level role of a source line, which fixes the base colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    Heading,
    Body,
    Code,
}

/// One whitespace-delimited word plus the emphasis it was written with.
struct Word {
    text: String,
    emphasis: Emphasis,
    width: usize,
    space_before: bool,
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
    let mut fenced = false;
    for source_line in cleaned.lines() {
        if source_line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let words = split_wide_words(parse_words(source_line.trim_end(), Block::Code), width);
            for line in wrap_words(&words, width) {
                lines.push(styled_line(palette, Block::Code, indent, line));
            }
            continue;
        }

        let (block, prefix, content) = classify(source_line, hanging_lists);
        if content.trim().is_empty() && prefix.is_empty() {
            if lines.last().is_some_and(|line| line.width() > indent.len()) {
                lines.push(Line::default());
            }
            continue;
        }

        let mut words = Vec::new();
        let hanging_width =
            if hanging_lists && (!prefix.is_empty()) && (prefix == "-" || prefix.ends_with('.')) {
                prefix.len() + 1
            } else {
                0
            };
        let has_prefix = !prefix.is_empty();
        if has_prefix {
            words.push(Word {
                width: display_width(&prefix),
                text: prefix,
                emphasis: Emphasis::default(),
                space_before: false,
            });
        }
        let mut content_words = parse_words(content, block);
        if has_prefix {
            if let Some(first) = content_words.first_mut() {
                first.space_before = true;
            }
        }
        words.extend(content_words);
        let line_width = width.saturating_sub(hanging_width);
        let words = split_wide_words(words, line_width);

        for (row_index, line) in wrap_words(&words, line_width).into_iter().enumerate() {
            if row_index > 0 && hanging_width > 0 {
                lines.push(styled_line(
                    palette,
                    block,
                    &format!("{indent}{}", " ".repeat(hanging_width)),
                    line,
                ));
            } else {
                lines.push(styled_line(palette, block, indent, line));
            }
        }
    }

    while lines.last().is_some_and(|line| line.spans.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return dash();
    }
    lines
}

/// Split a source line into its block role, a literal prefix that must lead the
/// first wrapped row, and the inline-markdown remainder.
fn classify(source: &str, literal_lists: bool) -> (Block, String, &str) {
    let trimmed = source.trim();
    let heading = trimmed.trim_start_matches('#');
    if heading.len() != trimmed.len() {
        return (Block::Heading, String::new(), heading.trim_start());
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
                    return (Block::Body, box_mark.to_string(), item);
                }
            }
            return (
                Block::Body,
                if literal_lists { "-" } else { "•" }.to_string(),
                rest,
            );
        }
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return (Block::Body, "│".to_string(), rest);
    }
    if let Some((number, rest)) = ordered_marker(trimmed) {
        return (Block::Body, number, rest);
    }
    (Block::Body, String::new(), trimmed)
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
fn parse_words(content: &str, block: Block) -> Vec<Word> {
    let base = Emphasis {
        bold: block == Block::Heading,
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
            width: display_width(current),
            text: std::mem::take(current),
            emphasis,
            space_before: *space_before,
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
        if ch == '`' && !emphasis.code {
            if let Some(end) = chars[index + 1..].iter().position(|c| *c == '`') {
                flush(&mut current, emphasis, &mut words, &mut space_before);
                let literal: String = chars[index + 1..index + 1 + end].iter().collect();
                for (token_index, token) in literal.split_whitespace().enumerate() {
                    words.push(Word {
                        width: display_width(token),
                        text: token.to_string(),
                        emphasis: Emphasis { code: true, ..base },
                        space_before: if token_index == 0 {
                            has_pending_space
                        } else {
                            true
                        },
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
        if word.width <= width {
            out.push(word);
            continue;
        }
        let mut chunk = String::new();
        let mut chunk_width = 0usize;
        let mut first_chunk = true;
        for character in word.text.chars() {
            let character_width = display_width(&character.to_string());
            if chunk_width + character_width > width && !chunk.is_empty() {
                let chunk_space_before = if first_chunk {
                    word.space_before
                } else {
                    false
                };
                out.push(Word {
                    width: chunk_width,
                    text: std::mem::take(&mut chunk),
                    emphasis: word.emphasis,
                    space_before: chunk_space_before,
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
            out.push(Word {
                width: chunk_width,
                text: chunk,
                emphasis: word.emphasis,
                space_before: chunk_space_before,
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
    let mut row_width = 0usize;
    let mut unit_start = 0;
    while unit_start < words.len() {
        let mut unit_end = unit_start + 1;
        while unit_end < words.len() && !words[unit_end].space_before {
            unit_end += 1;
        }
        let unit = &words[unit_start..unit_end];
        let unit_width: usize = unit.iter().map(|word| word.width).sum::<usize>()
            + unit.iter().skip(1).filter(|word| word.space_before).count();
        let separator = usize::from(words[unit_start].space_before && !row.is_empty());
        // Keep a glued unit together when it fits; oversized units can break between words.
        if !row.is_empty() && row_width + separator + unit_width > width {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        for word in unit {
            let separator = usize::from(word.space_before && !row.is_empty());
            if !row.is_empty() && row_width + separator + word.width > width {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            let separator = usize::from(word.space_before && !row.is_empty());
            row_width += separator + word.width;
            row.push(word);
        }
        unit_start = unit_end;
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

/// Turn one packed row into a `Line`, merging neighbouring words that share
/// emphasis so the span count stays proportional to the styling, not the text.
fn styled_line(palette: &Palette, block: Block, indent: &str, row: Vec<&Word>) -> Line<'static> {
    let mut spans = Vec::new();
    if !indent.is_empty() {
        spans.push(Span::raw(indent.to_string()));
    }
    let mut pending: Option<(String, Emphasis)> = None;
    for (position, word) in row.iter().enumerate() {
        match pending.as_mut() {
            Some((text, emphasis)) if *emphasis == word.emphasis => {
                if word.space_before {
                    text.push(' ');
                }
                text.push_str(&word.text);
            }
            Some(_) => {
                let (text, emphasis) = pending.take().expect("pending run");
                spans.push(Span::styled(text, style_for(palette, block, emphasis)));
                if word.space_before {
                    spans.push(Span::styled(
                        " ".to_string(),
                        style_for(palette, block, Emphasis::default()),
                    ));
                }
                pending = Some((word.text.clone(), word.emphasis));
            }
            None => {
                debug_assert_eq!(position, 0, "only the first word starts a run");
                pending = Some((word.text.clone(), word.emphasis));
            }
        }
    }
    if let Some((text, emphasis)) = pending {
        spans.push(Span::styled(text, style_for(palette, block, emphasis)));
    }
    Line::from(spans)
}

fn style_for(palette: &Palette, block: Block, emphasis: Emphasis) -> Style {
    let colour = if emphasis.code {
        palette.mauve
    } else if block == Block::Heading {
        palette.accent
    } else {
        palette.text
    };
    let mut style = Style::default().fg(colour);
    if emphasis.bold || block == Block::Heading {
        style = style.add_modifier(Modifier::BOLD);
    }
    if emphasis.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
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
    fn headings_are_accented_and_bold_across_every_span() {
        let lines = body_lines(&palette(), Some("## what it does"), 40, "");
        assert_eq!(text(&lines), vec!["what it does"]);
        assert!(lines[0]
            .spans
            .iter()
            .all(|span| span.style.add_modifier.contains(Modifier::BOLD)
                && span.style.fg == Some(palette().accent)));
    }

    #[test]
    fn inline_code_keeps_its_own_colour_and_drops_the_backticks() {
        let lines = body_lines(&palette(), Some("call `render_home` here"), 40, "");
        assert_eq!(text(&lines), vec!["call render_home here"]);
        let code: Vec<&str> = lines[0]
            .spans
            .iter()
            .filter(|span| span.style.fg == Some(palette().mauve))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(code, vec!["render_home"]);
    }

    #[test]
    fn inline_spans_stay_glued_to_adjacent_source_text() {
        let p = palette();
        for (source, expected) in [
            (
                "using `restore-snapshot.sh`, verifying",
                "using restore-snapshot.sh, verifying",
            ),
            (
                "a **canary strategy**, routing",
                "a canary strategy, routing",
            ),
            ("(`v2`)", "(v2)"),
            ("foo**bar**", "foobar"),
        ] {
            let lines = body_lines(&p, Some(source), 80, "");
            assert_eq!(text(&lines), vec![expected], "{source}");
        }
        let lines = body_lines(&p, Some("`restore-snapshot.sh`,"), 80, "");
        assert_eq!(lines[0].spans[0].style.fg, Some(p.mauve));
        assert_eq!(lines[0].spans[1].content, ",");
        assert_eq!(lines[0].spans[1].style.fg, Some(p.text));
    }

    #[test]
    fn glued_words_wrap_as_one_unit() {
        let lines = body_lines(&palette(), Some("ab x`cd`,"), 5, "");
        assert_eq!(text(&lines), vec!["ab", "xcd,"]);
    }

    #[test]
    fn an_over_wide_glued_unit_breaks_inside_itself() {
        let lines = body_lines(&palette(), Some("abc`de`, x"), 4, "");
        assert_eq!(text(&lines), vec!["abc", "de,", "x"]);
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
        assert_eq!(text(&lines), vec!["before", "just check", "after"]);
        assert_eq!(lines[1].spans[0].style.fg, Some(palette().mauve));
    }

    #[test]
    fn a_long_url_is_broken_so_it_never_overflows_its_line() {
        let url = "https://scalablev2-git-feat-studio-resize-window-blend-scalableso.vercel.app/auth?x-vercel-protection-bypass=abc";
        let lines = body_lines(&palette(), Some(url), 30, "");

        assert!(lines.len() > 1, "an over-wide URL must be broken up");
        assert!(
            lines.iter().all(|line| line.width() <= 30),
            "{:?}",
            lines.iter().map(|line| line.width()).collect::<Vec<_>>()
        );
        // Broken across rows, but not a character lost.
        let rejoined: String = text(&lines).join("");
        assert_eq!(rejoined, url);
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
