use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use super::list_view::put_str;
use super::list_view::text_width;
use crate::app::state::Palette;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormField {
    pub label: String,
    pub value: String,
    pub placeholder: String,
}

impl FormField {
    pub fn new(label: &str, placeholder: &str) -> Self {
        Self {
            label: label.to_string(),
            value: String::new(),
            placeholder: placeholder.to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineFormHits {
    pub field_rects: Vec<(Rect, usize)>,
    pub submit_button: Option<Rect>,
    pub cancel_button: Option<Rect>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineFormState {
    pub title: String,
    pub fields: Vec<FormField>,
    pub active_field: usize,
    pub error: Option<String>,
    pub success: Option<String>,
}

impl InlineFormState {
    pub fn new(title: &str, fields: Vec<FormField>) -> Self {
        Self {
            title: title.to_string(),
            fields,
            active_field: 0,
            error: None,
            success: None,
        }
    }

    pub fn cycle_forward(&mut self) {
        if !self.fields.is_empty() {
            self.active_field = (self.active_field + 1) % self.fields.len();
        }
    }

    pub fn cycle_backward(&mut self) {
        if !self.fields.is_empty() {
            self.active_field = if self.active_field == 0 {
                self.fields.len() - 1
            } else {
                self.active_field - 1
            };
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if let Some(field) = self.fields.get_mut(self.active_field) {
            field.value.push(c);
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        if let Some(field) = self.fields.get_mut(self.active_field) {
            field.value.push_str(s);
        }
    }

    pub fn delete_char(&mut self) {
        if let Some(field) = self.fields.get_mut(self.active_field) {
            field.value.pop();
        }
    }

    pub fn active_value(&self) -> &str {
        self.fields
            .get(self.active_field)
            .map(|f| f.value.as_str())
            .unwrap_or("")
    }
}

pub fn render_inline_form(
    buffer: &mut Buffer,
    area: Rect,
    form: &InlineFormState,
    submit_label: &str,
    cancel_label: &str,
    is_valid: bool,
    palette: &Palette,
) -> InlineFormHits {
    let mut hits = InlineFormHits::default();
    if area.width < 10 || area.height < 5 {
        return hits;
    }

    let mut current_y = area.y;

    // Title
    put_str(
        buffer,
        area.x,
        current_y,
        area.width,
        &form.title,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    current_y += 1;

    let sep = "─".repeat(area.width as usize);
    put_str(
        buffer,
        area.x,
        current_y,
        area.width,
        &sep,
        Style::default().fg(palette.surface_dim),
    );
    current_y += 2;

    // Render each field
    for (i, field) in form.fields.iter().enumerate() {
        if current_y + 2 >= area.bottom() {
            break;
        }

        let is_active = i == form.active_field;

        // Label
        let label_style = if is_active {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_str(
            buffer,
            area.x,
            current_y,
            area.width,
            &field.label,
            label_style,
        );
        current_y += 1;

        // Input box
        let box_w = area.width.saturating_sub(2);
        let box_rect = Rect::new(area.x, current_y, box_w, 1);
        hits.field_rects.push((box_rect, i));

        let box_bg = if is_active {
            palette.surface0
        } else {
            palette.panel_bg
        };

        // Draw input box background
        for x in area.x..area.x + box_w {
            let cell = &mut buffer[(x, current_y)];
            cell.set_char(' ');
            cell.set_bg(box_bg);
        }

        put_str(
            buffer,
            area.x,
            current_y,
            1,
            "[",
            Style::default().fg(palette.surface1),
        );

        if field.value.is_empty() {
            put_str(
                buffer,
                area.x + 2,
                current_y,
                box_w.saturating_sub(4),
                &field.placeholder,
                Style::default().fg(palette.overlay0).bg(box_bg),
            );
        } else {
            put_str(
                buffer,
                area.x + 2,
                current_y,
                box_w.saturating_sub(4),
                &field.value,
                Style::default().fg(palette.text).bg(box_bg),
            );
        }

        // Active field cursor
        if is_active {
            let cursor_x = area.x + 2 + (text_width(&field.value)).min(box_w.saturating_sub(5));
            if cursor_x < area.x + box_w.saturating_sub(1) {
                let cell = &mut buffer[(cursor_x, current_y)];
                cell.set_char('█');
                cell.set_fg(palette.accent);
            }
        }

        put_str(
            buffer,
            area.x + box_w.saturating_sub(1),
            current_y,
            1,
            "]",
            Style::default().fg(palette.surface1),
        );

        current_y += 2;
    }

    // Validation feedback
    if let Some(ref err) = form.error {
        if current_y < area.bottom() {
            put_str(
                buffer,
                area.x,
                current_y,
                area.width,
                &format!("⚠ Error: {err}"),
                Style::default()
                    .fg(palette.red)
                    .add_modifier(Modifier::BOLD),
            );
            current_y += 2;
        }
    } else if let Some(ref ok_msg) = form.success {
        if current_y < area.bottom() {
            put_str(
                buffer,
                area.x,
                current_y,
                area.width,
                &format!("✓ {ok_msg}"),
                Style::default()
                    .fg(palette.green)
                    .add_modifier(Modifier::BOLD),
            );
            current_y += 2;
        }
    }

    // Action buttons at the bottom
    let btn_y = area.bottom().saturating_sub(2);
    if current_y <= btn_y {
        let mut btn_x = area.x;

        // Submit button
        let submit_text = format!("[ {submit_label} ]");
        let sub_w = text_width(&submit_text);
        let submit_rect = Rect::new(btn_x, btn_y, sub_w, 1);
        hits.submit_button = Some(submit_rect);

        let sub_style = if is_valid {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_str(buffer, btn_x, btn_y, sub_w, &submit_text, sub_style);
        btn_x += sub_w + 3;

        // Cancel button
        let cancel_text = format!("[ {cancel_label} ]");
        let can_w = text_width(&cancel_text);
        let cancel_rect = Rect::new(btn_x, btn_y, can_w, 1);
        hits.cancel_button = Some(cancel_rect);
        put_str(
            buffer,
            btn_x,
            btn_y,
            can_w,
            &cancel_text,
            Style::default().fg(palette.overlay0),
        );
    }

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_inline_form_state_machine() {
        let fields = vec![
            FormField::new("Title:", "Enter title"),
            FormField::new("Spec:", "Enter description"),
        ];
        let mut form = InlineFormState::new("Create Task", fields);

        assert_eq!(form.active_field, 0);
        form.insert_char('H');
        form.insert_str("ello");
        assert_eq!(form.active_value(), "Hello");

        form.cycle_forward();
        assert_eq!(form.active_field, 1);
        form.insert_str("World");
        assert_eq!(form.active_value(), "World");

        form.cycle_backward();
        assert_eq!(form.active_field, 0);
        form.delete_char();
        assert_eq!(form.active_value(), "Hell");
    }

    #[test]
    fn test_inline_form_pure_render_and_hits() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 50, 15);
        let mut buffer = Buffer::empty(area);

        let fields = vec![
            FormField::new("Title:", "Enter title"),
            FormField::new("Spec:", "Enter description"),
        ];
        let mut form = InlineFormState::new("NEW TASK", fields);
        form.fields[0].value = "Fix parser".into();

        let hits = render_inline_form(
            &mut buffer,
            area,
            &form,
            "Save (Enter)",
            "Cancel (Esc)",
            true,
            &palette,
        );

        assert_eq!(hits.field_rects.len(), 2);
        assert!(hits.submit_button.is_some());
        assert!(hits.cancel_button.is_some());

        // Check title in buffer
        assert_eq!(buffer[(0, 0)].symbol(), "N");
    }
}
