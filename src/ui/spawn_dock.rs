use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{
    home::{HomePicker, HomeWorkspace},
    spawn_dock::{SpawnDockField, SpawnDockState},
    state::AppState,
};

const DOCK_HEIGHT: u16 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HitTarget {
    Field(SpawnDockField),
    Spawn,
}

pub(crate) fn hit_test(app: &AppState, column: u16, row: u16) -> Option<HitTarget> {
    let dock = app.spawn_dock.as_ref()?;
    let area = app.view.terminal_area;
    if area.height < 5 || area.width < 30 {
        return None;
    }
    let dock_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(DOCK_HEIGHT.min(area.height)),
        area.width,
        DOCK_HEIGHT.min(area.height),
    );
    if !contains(dock_area, column, row) {
        return None;
    }
    let content_area = Rect::new(
        dock_area.x,
        dock_area.y.saturating_add(1),
        dock_area.width,
        dock_area.height.saturating_sub(1),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(2)])
        .split(content_area);
    let field_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(14),
            Constraint::Percentage(22),
            Constraint::Percentage(16),
            Constraint::Percentage(10),
            Constraint::Percentage(16),
        ])
        .split(rows[0]);
    let fields = [
        SpawnDockField::Project,
        SpawnDockField::Host,
        SpawnDockField::Profile,
        SpawnDockField::Model,
        SpawnDockField::Effort,
        SpawnDockField::Worktree,
    ];
    if let Some((index, _)) = field_areas
        .iter()
        .enumerate()
        .find(|(_, area)| contains(**area, column, row))
    {
        return fields.get(index).copied().map(HitTarget::Field);
    }
    if dock.picker.is_some() || !contains(rows[1], column, row) {
        return None;
    }
    let prompt_area = Block::default()
        .borders(Borders::ALL)
        .padding(ratatui::widgets::Padding::uniform(1))
        .inner(rows[1]);
    let button = format!(
        "⇧⏎ Spawn on {}",
        dock.home
            .machine()
            .map(|machine| machine.name.as_str())
            .unwrap_or("host")
    );
    let width = button.chars().count() as u16 + 3;
    let button_area = Rect::new(
        prompt_area.x + prompt_area.width.saturating_sub(width),
        prompt_area.y + prompt_area.height.saturating_sub(3),
        width.min(prompt_area.width),
        3.min(prompt_area.height),
    );
    if contains(button_area, column, row) {
        Some(HitTarget::Spawn)
    } else {
        Some(HitTarget::Field(SpawnDockField::Prompt))
    }
}

fn contains(rect: Rect, column: u16, row: u16) -> bool {
    column >= rect.x && column < rect.right() && row >= rect.y && row < rect.bottom()
}

pub(super) fn render(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(dock) = app.spawn_dock.as_ref() else {
        return;
    };
    if area.height < 5 || area.width < 30 {
        return;
    }
    let dock_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(DOCK_HEIGHT.min(area.height)),
        area.width,
        DOCK_HEIGHT.min(area.height),
    );
    let title = dock
        .restored_age
        .as_ref()
        .map(|age| format!("draft restored · {age}"));
    frame.render_widget(
        Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(app.palette.surface_dim))
            .title(title.unwrap_or_default())
            .style(Style::default().bg(app.palette.panel_bg)),
        dock_area,
    );
    let content_area = Rect::new(
        dock_area.x,
        dock_area.y.saturating_add(1),
        dock_area.width,
        dock_area.height.saturating_sub(1),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(2)])
        .split(content_area);
    let field_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(22),
            Constraint::Percentage(14),
            Constraint::Percentage(22),
            Constraint::Percentage(16),
            Constraint::Percentage(10),
            Constraint::Percentage(16),
        ])
        .split(rows[0]);
    let fields = [
        SpawnDockField::Project,
        SpawnDockField::Host,
        SpawnDockField::Profile,
        SpawnDockField::Model,
        SpawnDockField::Effort,
        SpawnDockField::Worktree,
    ];
    for (field, rect) in fields.into_iter().zip(field_areas.iter()) {
        render_field(app, dock, field, *rect, frame);
    }
    if let Some(picker) = dock.picker {
        render_picker(app, dock, picker, rows[1], frame);
    } else {
        render_prompt(app, dock, rows[1], frame);
    }
}

fn render_field(
    app: &AppState,
    dock: &SpawnDockState,
    field: SpawnDockField,
    rect: Rect,
    frame: &mut Frame,
) {
    let focused = dock.focus == field;
    let value = match field {
        SpawnDockField::Project => dock
            .home
            .project()
            .map(|p| p.label.as_str())
            .unwrap_or("project"),
        SpawnDockField::Host => {
            if dock.auto_host {
                "auto"
            } else {
                dock.home
                    .machine()
                    .map(|m| m.name.as_str())
                    .unwrap_or("auto")
            }
        }
        SpawnDockField::Profile => dock
            .home
            .profiles()
            .iter()
            .find(|p| p.id == dock.home.profile_id())
            .map(|p| p.label.as_str())
            .unwrap_or("claude"),
        SpawnDockField::Model => dock.home.model_display_name(),
        SpawnDockField::Effort => dock.home.effort.as_deref().unwrap_or("auto"),
        SpawnDockField::Worktree => match &dock.home.workspace {
            HomeWorkspace::CurrentCheckout => "current checkout",
            HomeWorkspace::NewWorktree => "new worktree",
            HomeWorkspace::PreviousWorktree(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("worktree"),
        },
        SpawnDockField::Prompt => "",
    };
    let title = match field {
        SpawnDockField::Project => "⌂ project",
        SpawnDockField::Host => "host",
        SpawnDockField::Profile => "profile",
        SpawnDockField::Model => "model",
        SpawnDockField::Effort => "effort",
        SpawnDockField::Worktree => "worktree",
        SpawnDockField::Prompt => "",
    };
    let border = if focused {
        app.palette.accent
    } else {
        app.palette.surface_dim
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(if focused {
            app.palette.selection_bg
        } else {
            app.palette.panel_bg
        }))
        .title(Span::styled(
            title,
            Style::default().fg(app.palette.subtext0),
        ));
    let content = if field == SpawnDockField::Profile {
        let profile = dock
            .home
            .profiles()
            .iter()
            .find(|p| p.id == dock.home.profile_id());
        let icon = profile
            .and_then(|p| crate::ui::icons::agent_label(p.agent, app.nerd_font))
            .unwrap_or("agent");
        let summary = profile.and_then(|p| p.quota.map(|quota| quota.usage(&app.provider_usage)));
        let (left, reset) = usage_summary(summary);
        let account = summary
            .and_then(|usage| usage.account.as_deref())
            .unwrap_or("default");
        format!(
            "{icon} {value}/{account}  {} {}%{}",
            usage_bar(left),
            left,
            reset.map(|r| format!(" · {r}")).unwrap_or_default()
        )
    } else {
        value.to_string()
    };
    frame.render_widget(
        Paragraph::new(content)
            .style(
                Style::default()
                    .fg(app.palette.text)
                    .add_modifier(Modifier::BOLD),
            )
            .block(block),
        rect,
    );
}

fn render_prompt(app: &AppState, dock: &SpawnDockState, rect: Rect, frame: &mut Frame) {
    let focused = dock.focus == SpawnDockField::Prompt;
    let border = if focused {
        app.palette.accent
    } else {
        app.palette.surface_dim
    };
    let button = format!(
        "⇧⏎ Spawn on {}",
        dock.home
            .machine()
            .map(|m| m.name.as_str())
            .unwrap_or("host")
    );
    let width = button.chars().count() as u16 + 3;
    let inner = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .style(Style::default().bg(if focused {
            app.palette.selection_bg
        } else {
            app.palette.panel_bg
        }))
        .padding(ratatui::widgets::Padding::uniform(1));
    let prompt_area = inner.inner(rect);
    frame.render_widget(inner, rect);
    let button_area = Rect::new(
        prompt_area.x + prompt_area.width.saturating_sub(width),
        prompt_area.y + prompt_area.height.saturating_sub(3),
        width.min(prompt_area.width),
        3.min(prompt_area.height),
    );
    frame.render_widget(
        Paragraph::new(button)
            .style(
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(app.palette.accent))
                    .style(Style::default().bg(app.palette.panel_bg)),
            ),
        button_area,
    );
    let text_area = Rect::new(
        prompt_area.x,
        prompt_area.y,
        prompt_area.width.saturating_sub(width),
        prompt_area.height.saturating_sub(2),
    );
    let text = if dock.home.prompt.is_empty() {
        vec![Line::from(Span::styled(
            "What should the agent work on?",
            Style::default().fg(app.palette.subtext0),
        ))]
    } else {
        dock.home.prompt.lines().map(Line::from).collect()
    };
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), text_area);
}

fn render_picker(
    app: &AppState,
    dock: &SpawnDockState,
    picker: HomePicker,
    rect: Rect,
    frame: &mut Frame,
) {
    let items: Vec<(String, Line<'static>)> = match picker {
        HomePicker::Project => dock
            .home
            .projects()
            .iter()
            .enumerate()
            .map(|(index, p)| {
                let icon = p.label.chars().next().unwrap_or('⌂');
                let path = p
                    .repos
                    .first()
                    .map(|repo| repo.path.display().to_string())
                    .unwrap_or_default();
                let count = dock
                    .project_agent_counts
                    .get(index)
                    .copied()
                    .unwrap_or_default();
                let search = format!("{icon} {}  {path}  agents:{count}", p.label);
                let line = Line::from(vec![
                    Span::styled(
                        format!(" {icon} "),
                        Style::default()
                            .fg(app.palette.panel_bg)
                            .bg(app.palette.blue),
                    ),
                    Span::raw(format!(" {}  {path}  agents:{count}", p.label)),
                ]);
                (search, line)
            })
            .collect(),
        HomePicker::Agent => {
            dock.home
                .profiles()
                .iter()
                .map(|p| {
                    let summary = p.quota.map(|q| q.usage(&app.provider_usage));
                    let (left, reset) = usage_summary(summary);
                    let account = summary
                        .and_then(|usage| usage.account.as_deref())
                        .unwrap_or("default");
                    let usage = format!(
                        "/{account}  {} {}%{}",
                        usage_bar(left),
                        left,
                        reset.map(|r| format!(" · {r}")).unwrap_or_default()
                    );
                    picker_row(format!(
                        "{}  {}{}  models:{}",
                        match p.agent {
                            crate::detect::Agent::Claude => "✳",
                            crate::detect::Agent::Codex => "◎",
                            _ => crate::ui::icons::agent_label(p.agent, app.nerd_font)
                                .unwrap_or("agent"),
                        },
                        p.label,
                        usage,
                        dock.home.profile_models(p.agent)
                    ))
                })
                .collect()
        }
        HomePicker::Model => dock
            .home
            .model_options()
            .iter()
            .map(|m| picker_row(m.display_name.clone()))
            .collect(),
        HomePicker::Effort => dock
            .home
            .effort_options()
            .iter()
            .cloned()
            .map(picker_row)
            .collect(),
        HomePicker::Workspace => dock
            .home
            .workspace_options()
            .iter()
            .map(HomeWorkspace::label)
            .map(picker_row)
            .collect(),
        _ => Vec::new(),
    };
    let query = dock.filter.to_lowercase();
    let matches: Vec<(usize, &(String, Line<'static>))> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.0.to_lowercase().contains(&query))
        .collect();
    let selected = dock.home.picker_selected;
    let lines = matches
        .iter()
        .take(rect.height.saturating_sub(1) as usize)
        .enumerate()
        .map(|(i, (_, (_, line)))| {
            let active = i == selected;
            let style = if active {
                Style::default()
                    .fg(app.palette.text)
                    .bg(app.palette.selection_bg)
            } else {
                Style::default().fg(app.palette.text)
            };
            let mut line = line.clone();
            line.style = style;
            line
        })
        .collect::<Vec<_>>();
    let title = format!(
        "{}  {}",
        match picker {
            HomePicker::Project => "project",
            HomePicker::Agent => "profile",
            HomePicker::Model => "model",
            HomePicker::Effort => "effort",
            _ => "worktree",
        },
        dock.filter
    );
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.palette.accent))
                .style(Style::default().bg(app.palette.panel_bg)),
        ),
        rect,
    );
}

fn picker_row(label: String) -> (String, Line<'static>) {
    (label.clone(), Line::from(label))
}

fn usage_summary(usage: Option<&crate::provider_usage::AccountUsage>) -> (u8, Option<String>) {
    let Some(usage) = usage else {
        return (0, None);
    };
    let window = [usage.five_hour, usage.seven_day]
        .into_iter()
        .flatten()
        .max_by_key(|w| w.used_percent);
    let remaining = window
        .map(|w| 100u8.saturating_sub(w.used_percent))
        .unwrap_or(0);
    let reset = window
        .and_then(|w| w.resets_at)
        .and_then(|at| crate::provider_usage::reset_label(at, app_now()));
    (remaining, reset)
}

fn app_now() -> i64 {
    crate::provider_usage::now_unix().unwrap_or_default()
}

fn usage_bar(left: u8) -> String {
    let filled = usize::from(left.saturating_add(9) / 10).min(10);
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}
