use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::{app::AppState, planning_lock::DialogFlow};

pub(crate) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
    let snapshot = app.planning_lock.snapshot(now);
    if let Some(snapshot) = snapshot.as_ref().filter(|snapshot| snapshot.locked) {
        let focused_is_discussion = app
            .active
            .and_then(|workspace_idx| {
                let workspace = app.workspaces.get(workspace_idx)?;
                let tab_idx = workspace.active_tab_index();
                let tab_number = workspace.public_tab_number(tab_idx)?;
                Some(crate::workspace::public_tab_id_for_number(
                    &workspace.id,
                    tab_number,
                ))
            })
            .is_some_and(|tab| tab == snapshot.discussion_tab_id);
        dim_background(app, frame, area, focused_is_discussion);
        let discussion = app
            .workspaces
            .iter()
            .find_map(|workspace| {
                workspace
                    .tabs
                    .iter()
                    .enumerate()
                    .find_map(|(tab_idx, tab)| {
                        let id = workspace.id.as_str();
                        let tab_number = workspace.public_tab_number(tab_idx)?;
                        let public_id = crate::workspace::public_tab_id_for_number(id, tab_number);
                        (public_id == snapshot.discussion_tab_id).then(|| {
                            format!(
                                "{} · {}",
                                workspace.custom_name.as_deref().unwrap_or("Workspace"),
                                tab.custom_name.as_deref().unwrap_or("discussion")
                            )
                        })
                    })
            })
            .unwrap_or_else(|| "discussion tab unavailable".into());
        let title = if snapshot.discussion_tab_id.is_empty() {
            "Planning lock config needs reset".to_owned()
        } else {
            format!("Planning lock · {discussion}")
        };
        let status_area = if app.view.sidebar_rect.width >= 8 {
            Rect::new(
                app.view.sidebar_rect.x,
                app.view.sidebar_rect.y,
                app.view.sidebar_rect.width,
                app.view.sidebar_rect.height.min(4),
            )
        } else {
            Rect::default()
        };
        if status_area.width > 0 && status_area.height > 0 {
            frame.render_widget(
                Paragraph::new("Other Herdr sessions are dimmed. Ctrl+Alt+U to unlock.")
                    .style(
                        Style::default()
                            .fg(app.palette.text)
                            .bg(app.palette.panel_bg),
                    )
                    .block(
                        Block::default()
                            .title(title)
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(app.palette.accent))
                            .style(
                                Style::default()
                                    .fg(app.palette.text)
                                    .bg(app.palette.panel_bg),
                            ),
                    )
                    .wrap(Wrap { trim: true }),
                status_area,
            );
        }
    } else if let Some(deadline) = snapshot.and_then(|snapshot| snapshot.unlock_until_unix_s) {
        let remaining = deadline.saturating_sub(now);
        let label = format!("🔓 {}:{:02}", remaining / 60, remaining % 60);
        let width = u16::try_from(label.chars().count())
            .unwrap_or(12)
            .min(area.width);
        if width > 0 && area.height > 0 {
            frame.render_widget(
                Paragraph::new(label).style(
                    Style::default()
                        .fg(app.palette.yellow)
                        .bg(app.palette.panel_bg),
                ),
                Rect::new(area.right().saturating_sub(width), area.y, width, 1),
            );
        }
    }

    if let Some(dialog) = app.planning_lock_dialog.as_ref() {
        let Some(popup) = crate::ui::widgets::centered_popup_rect(area, 66, 16) else {
            return;
        };
        frame.render_widget(Clear, popup);
        let title = match dialog.flow {
            DialogFlow::SetupChooseDiscussionTab => "Choose discussion session",
            DialogFlow::SetupPassword => "Set planning lock password",
            DialogFlow::SetupConfirmPassword => "Confirm password",
            DialogFlow::UnlockPassword => "Unlock planning lock",
            DialogFlow::ChooseDuration => "Unlock duration",
            DialogFlow::Manage => "Planning lock",
            DialogFlow::ChooseDiscussionTab => "Change discussion session",
            DialogFlow::DisablePassword => "Turn off planning lock",
            DialogFlow::DisableConfirmation => "Confirm turn off",
        };
        let mut lines = vec![Line::from(title).style(Style::default().fg(app.palette.accent))];
        match dialog.flow {
            DialogFlow::SetupChooseDiscussionTab | DialogFlow::ChooseDiscussionTab => {
                for (index, (tab_id, label)) in
                    app.planning_lock_tab_projection().iter().enumerate()
                {
                    let marker = if index == dialog.selected_tab {
                        "› "
                    } else {
                        "  "
                    };
                    lines.push(Line::from(format!("{marker}{label} ({tab_id})")));
                }
                lines.push(Line::from("↑/↓ select · Enter continue"));
            }
            DialogFlow::ChooseDuration => {
                let options = crate::planning_lock::UNLOCK_MINUTES
                    .iter()
                    .enumerate()
                    .map(|(index, minutes)| {
                        let label = if index == dialog.selected_tab {
                            format!("[ {minutes} min ]")
                        } else {
                            format!("  {minutes} min  ")
                        };
                        Span::styled(label, Style::default().fg(app.palette.text))
                    })
                    .collect::<Vec<_>>();
                lines.push(Line::from(options));
                lines.push(Line::from("←/→ choose · Enter unlock"));
            }
            DialogFlow::Manage => {
                lines.push(Line::from("u · unlock      d · change discussion tab"));
                lines.push(Line::from("x · turn off    Esc · close"));
            }
            _ => {
                let prompt = if matches!(dialog.flow, DialogFlow::DisableConfirmation) {
                    "Type turn off"
                } else if matches!(
                    dialog.flow,
                    DialogFlow::SetupPassword | DialogFlow::SetupConfirmPassword
                ) {
                    "Type at least 24 characters"
                } else {
                    "Type password"
                };
                let visible = if matches!(dialog.flow, DialogFlow::DisableConfirmation) {
                    dialog.input.clone()
                } else {
                    "•".repeat(dialog.input.chars().count())
                };
                lines.push(Line::from(prompt));
                lines.push(Line::from(visible));
                lines.push(Line::from(
                    "Paste is disabled — type it · Enter to continue",
                ));
            }
        }
        if let Some(error) = dialog.error.as_deref() {
            lines.push(Line::from(error).style(Style::default().fg(app.palette.red)));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .title("Planning lock")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(app.palette.accent))
                        .style(
                            Style::default()
                                .fg(app.palette.text)
                                .bg(app.palette.panel_bg),
                        ),
                )
                .wrap(Wrap { trim: true }),
            popup,
        );
    }
}

fn dim_background(app: &AppState, frame: &mut Frame, area: Rect, preserve_terminal_area: bool) {
    let fallback_fg = app.palette.subtext0;
    let fallback_bg = app.palette.surface1;
    let buffer = frame.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if preserve_terminal_area
                && x >= app.view.terminal_area.x
                && x < app.view.terminal_area.right()
                && y >= app.view.terminal_area.y
                && y < app.view.terminal_area.bottom()
            {
                continue;
            }
            let cell = &mut buffer[(x, y)];
            let original = cell.style();
            let fg = dim_color(original.fg.unwrap_or(app.palette.text), fallback_fg);
            let bg = dim_color(original.bg.unwrap_or(app.palette.panel_bg), fallback_bg);
            cell.set_style(original.fg(fg).bg(bg).add_modifier(Modifier::DIM));
        }
    }
}

fn dim_color(color: Color, fallback: Color) -> Color {
    let Some(rgb) =
        crate::app::state::color_rgb(color).or_else(|| crate::app::state::color_rgb(fallback))
    else {
        return Color::Rgb(24, 24, 30);
    };
    Color::Rgb(rgb.r / 4, rgb.g / 4, rgb.b / 4)
}
