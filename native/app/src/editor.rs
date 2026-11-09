//! A text editor with line numbers. iced's `text_editor` has no gutter, so the numbers are a column beside it that
//! uses the same line height: for the two to stay aligned the editor does not wrap lines and does not scroll by
//! itself (it is as tall as its text); the whole thing scrolls, both ways, inside one `scrollable`.

use crate::theme::{self, DIM, FG};
use crate::Message;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{column, container, row, scrollable, text, text_editor};
use iced::{Element, Length, Pixels};

/// Height of one line, in pixels, for the numbers and for the text.
const LINE_H: f32 = 20.0;
/// Space around the text inside the editor.
const PAD: f32 = 10.0;
const FONT_SIZE: f32 = 13.0;
/// Advance of one character of the mono font at `FONT_SIZE` (0.6 em).
const CHAR_W: f32 = FONT_SIZE * 0.6;
const GUTTER_W: f32 = 52.0;
const MIN_TEXT_W: f32 = 640.0;

/// Width the text needs so that its longest line fits without wrapping.
pub fn text_width(longest_line_chars: usize) -> f32 {
    (longest_line_chars as f32 * CHAR_W + 2.0 * PAD + 24.0).max(MIN_TEXT_W)
}

/// `1..=lines` as a column of right-aligned numbers, the line of the cursor lit.
fn gutter<'a>(lines: usize, current: usize) -> Element<'a, Message> {
    let mut numbers = column![].width(GUTTER_W);
    for n in 0..lines.max(1) {
        numbers = numbers.push(
            container(
                text((n + 1).to_string())
                    .size(12)
                    .color(if n == current { FG } else { DIM })
                    .width(Length::Fill)
                    .align_x(iced::alignment::Horizontal::Right),
            )
            .height(LINE_H)
            .center_y(LINE_H)
            .padding([0, 10]),
        );
    }
    container(numbers)
        .padding(iced::Padding {
            top: PAD,
            bottom: PAD,
            left: 0.0,
            right: 0.0,
        })
        .style(theme::bar_style)
        .into()
}

/// The editor, with its numbers. `on_action` receives what the user does in it.
pub fn view<'a>(
    content: &'a text_editor::Content,
    on_action: impl Fn(text_editor::Action) -> Message + 'a,
) -> Element<'a, Message> {
    let longest = content
        .lines()
        .map(|l| l.text.chars().count())
        .max()
        .unwrap_or(0);
    let editor = text_editor(content)
        .on_action(on_action)
        .wrapping(Wrapping::None)
        .line_height(LineHeight::Absolute(Pixels(LINE_H)))
        .size(FONT_SIZE)
        .padding(PAD)
        .width(text_width(longest))
        .height(Length::Shrink)
        .style(theme::editor_style);
    scrollable(row![
        gutter(content.line_count(), content.cursor().position.line),
        editor
    ])
    .direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default(),
        horizontal: scrollable::Scrollbar::default(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_is_wide_enough_for_its_longest_line_and_never_narrow() {
        assert_eq!(text_width(0), MIN_TEXT_W);
        assert_eq!(text_width(10), MIN_TEXT_W);
        let wide = text_width(300);
        assert!(wide >= 300.0 * CHAR_W);
    }
}
